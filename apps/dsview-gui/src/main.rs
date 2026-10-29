#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use dsview_core::{
    CaptureBatch, CaptureCoordinator, CaptureEvent, CaptureSession, CaptureSource,
    DeviceDescriptor, DeviceId,
};
use dsview_recording::{CompletionStatus, RecordingWriter};
use dsview_usb::{
    apply_dscope_configuration, auto_calibrate_dscope, discover_supported_devices, prepare_device,
    read_dscope_calibration, write_dscope_calibration, DeviceModel, DiscoveredDevice,
    DsCopeBandwidthLimit, DsCopeCalibrationValues, DsCopeCaptureConfig, DsCopeCaptureSource,
    DsCopeChannelConfig, DsCopeCoupling, DsCopeOperationMode, DsCopeTriggerConfig,
    DsCopeTriggerSlope, DsCopeTriggerSource, DsLogicCaptureConfig, DsLogicCaptureSource,
    LinkStatus, DSCOPE_SAMPLE_COUNTS, DSCOPE_SAMPLE_RATES_HZ, DSCOPE_TRIGGER_HOLDOFF_MAX_NS,
    DSCOPE_TRIGGER_LEVEL_MAX, DSCOPE_TRIGGER_LEVEL_MIN, DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX,
    DSCOPE_TRIGGER_POSITION_MAX_PERCENT, DSCOPE_TRIGGER_POSITION_MIN_PERCENT,
    DSCOPE_VERTICAL_OFFSET_CENTER, DSCOPE_VERTICAL_OFFSET_MAX, DSCOPE_VERTICAL_OFFSET_MIN,
    DSCOPE_VOLTS_PER_DIV_MV,
};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const SAMPLE_RATE_HZ: u64 = 1_000_000;
const SAMPLE_COUNT: u64 = 1_000_000;
const LOGIC_CHANNEL_COUNT: usize = 16;
// Keep the complete largest hardware capture in memory. Rendering is already
// decimated to the plot width, so retaining the samples does not increase the
// number of painted points. This also keeps the hardware trigger region in the
// waveform instead of displaying only the final tail of a capture.
const MAX_WAVEFORM_SAMPLES: usize = 2_000_000;
const WAVEFORM_STATUS_HEIGHT: f32 = 20.0;
const TIME_RULER_HEIGHT: f32 = 24.0;
const SCOPE_HORIZONTAL_DIVISIONS: usize = 10;
const SCOPE_VERTICAL_DIVISIONS: usize = 8;
const SCOPE_CONFIGURATION_DEBOUNCE: Duration = Duration::from_millis(350);
const TOOLTIP_DELAY_SECONDS: f32 = 0.1;
const TIMEBASE_STEPS_SECONDS: [f64; 30] = [
    2.0e-9, 5.0e-9, 10.0e-9, 20.0e-9, 50.0e-9, 100.0e-9, 200.0e-9, 500.0e-9, 1.0e-6, 2.0e-6,
    5.0e-6, 10.0e-6, 20.0e-6, 50.0e-6, 100.0e-6, 200.0e-6, 500.0e-6, 1.0e-3, 2.0e-3, 5.0e-3,
    10.0e-3, 20.0e-3, 50.0e-3, 100.0e-3, 200.0e-3, 500.0e-3, 1.0, 2.0, 5.0, 10.0,
];
const BACKGROUND: Color32 = Color32::from_rgb(12, 17, 25);
const PANEL: Color32 = Color32::from_rgb(20, 27, 38);
const GRID: Color32 = Color32::from_rgb(43, 55, 72);
const TEXT_MUTED: Color32 = Color32::from_rgb(145, 160, 180);
const ACCENT: Color32 = Color32::from_rgb(70, 210, 185);
const SCOPE_COLORS: [Color32; 2] = [
    Color32::from_rgb(255, 204, 74),
    Color32::from_rgb(99, 179, 255),
];

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([920.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DSView",
        options,
        Box::new(|creation_context| Ok(Box::new(DsViewApp::new(creation_context)))),
    )
}

struct DeviceEntry {
    device: DiscoveredDevice,
    selected: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeChannelSettings {
    enabled: bool,
    volts_per_div_mv: u16,
    coupling: DsCopeCoupling,
    probe_attenuation: u8,
    vertical_offset: u16,
}

impl Default for ScopeChannelSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            volts_per_div_mv: 1_000,
            coupling: DsCopeCoupling::Dc,
            probe_attenuation: 1,
            vertical_offset: DSCOPE_VERTICAL_OFFSET_CENTER,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TriggerHoldoffUnit {
    Microseconds,
    Milliseconds,
    Seconds,
}

impl TriggerHoldoffUnit {
    fn label(self) -> &'static str {
        match self {
            Self::Microseconds => "µs",
            Self::Milliseconds => "ms",
            Self::Seconds => "s",
        }
    }

    fn nanoseconds(self) -> f64 {
        match self {
            Self::Microseconds => 1_000.0,
            Self::Milliseconds => 1_000_000.0,
            Self::Seconds => 1_000_000_000.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeTriggerSettings {
    position_percent: u8,
    source: DsCopeTriggerSource,
    auto_channel: u8,
    slope: DsCopeTriggerSlope,
    holdoff_ns: u64,
    holdoff_unit: TriggerHoldoffUnit,
    noise_sensitivity: u8,
    levels: [u8; 2],
}

impl Default for ScopeTriggerSettings {
    fn default() -> Self {
        Self::from_capture_config(DsCopeTriggerConfig::default())
    }
}

impl ScopeTriggerSettings {
    fn capture_config(self) -> Result<DsCopeTriggerConfig, String> {
        DsCopeTriggerConfig::new(self.source, self.slope)
            .with_position_percent(self.position_percent)
            .and_then(|trigger| trigger.with_auto_channel(self.auto_channel))
            .and_then(|trigger| trigger.with_holdoff_ns(self.holdoff_ns))
            .and_then(|trigger| trigger.with_noise_sensitivity(self.noise_sensitivity))
            .and_then(|trigger| trigger.with_levels(self.levels))
            .map_err(|error| error.to_string())
    }

    fn from_capture_config(config: DsCopeTriggerConfig) -> Self {
        let holdoff_unit = if config.holdoff_ns() >= 1_000_000_000 {
            TriggerHoldoffUnit::Seconds
        } else if config.holdoff_ns() >= 1_000_000 {
            TriggerHoldoffUnit::Milliseconds
        } else {
            TriggerHoldoffUnit::Microseconds
        };
        Self {
            position_percent: config.position_percent(),
            source: config.source(),
            auto_channel: config.auto_channel(),
            slope: config.slope(),
            holdoff_ns: config.holdoff_ns(),
            holdoff_unit,
            noise_sensitivity: config.noise_sensitivity(),
            levels: config.levels(),
        }
    }

    fn source_is_available(self, channels: [bool; 2]) -> bool {
        match self.source {
            DsCopeTriggerSource::Auto => channels[usize::from(self.auto_channel)],
            DsCopeTriggerSource::ChannelZero => channels[0],
            DsCopeTriggerSource::ChannelOne => channels[1],
            DsCopeTriggerSource::ChannelZeroAndOne => channels[0] && channels[1],
            DsCopeTriggerSource::ChannelZeroOrOne => channels[0] || channels[1],
        }
    }

    fn clamp_to_channels(&mut self, channels: [bool; 2]) {
        if !channels[usize::from(self.auto_channel)] {
            self.auto_channel = if channels[0] { 0 } else { 1 };
        }
        if !self.source_is_available(channels) {
            self.source = DsCopeTriggerSource::Auto;
        }
    }

    fn hardware_differs_from(self, other: Self) -> bool {
        self.position_percent != other.position_percent
            || self.source != other.source
            || self.auto_channel != other.auto_channel
            || self.slope != other.slope
            || self.holdoff_ns != other.holdoff_ns
            || self.noise_sensitivity != other.noise_sensitivity
            || self.levels != other.levels
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeCaptureSettings {
    sample_rate_hz: u64,
    sample_count: u64,
    channels: [ScopeChannelSettings; 2],
    trigger: ScopeTriggerSettings,
    operation_mode: DsCopeOperationMode,
    bandwidth_limit: DsCopeBandwidthLimit,
}

impl Default for ScopeCaptureSettings {
    fn default() -> Self {
        Self {
            sample_rate_hz: SAMPLE_RATE_HZ,
            sample_count: SAMPLE_COUNT,
            channels: [ScopeChannelSettings::default(); 2],
            trigger: ScopeTriggerSettings::default(),
            operation_mode: DsCopeOperationMode::Normal,
            bandwidth_limit: DsCopeBandwidthLimit::Full,
        }
    }
}

impl ScopeCaptureSettings {
    fn capture_config(self) -> Result<DsCopeCaptureConfig, String> {
        let mut channels = [DsCopeChannelConfig::default(); 2];
        for (destination, source) in channels.iter_mut().zip(self.channels) {
            *destination = DsCopeChannelConfig::new(
                source.volts_per_div_mv,
                source.coupling,
                source.probe_attenuation,
            )
            .map_err(|error| error.to_string())?
            .with_vertical_offset(source.vertical_offset)
            .map_err(|error| error.to_string())?
            .with_enabled(source.enabled);
        }
        DsCopeCaptureConfig::with_channel_configs(self.sample_rate_hz, self.sample_count, channels)
            .map_err(|error| error.to_string())?
            .with_trigger_config(self.trigger.capture_config()?)
            .map_err(|error| error.to_string())
            .map(|config| config.with_options(self.operation_mode, self.bandwidth_limit))
    }

    fn enabled_channel_count(self) -> usize {
        self.channels
            .iter()
            .filter(|channel| channel.enabled)
            .count()
    }

    fn maximum_sample_rate_hz(self) -> u64 {
        if self.enabled_channel_count() == 1 {
            1_000_000_000
        } else {
            500_000_000
        }
    }

    fn maximum_sample_count(self) -> u64 {
        if self.enabled_channel_count() == 1 {
            2_000_000
        } else {
            1_000_000
        }
    }

    fn clamp_to_channel_limits(&mut self) {
        self.sample_rate_hz = self.sample_rate_hz.min(self.maximum_sample_rate_hz());
        self.sample_count = self.sample_count.min(self.maximum_sample_count());
        self.trigger
            .clamp_to_channels(self.channels.map(|channel| channel.enabled));
    }

    fn frontend_differs_from(self, other: Self) -> bool {
        self.sample_rate_hz != other.sample_rate_hz
            || self.operation_mode != other.operation_mode
            || self.bandwidth_limit != other.bandwidth_limit
            || self
                .channels
                .iter()
                .zip(other.channels)
                .any(|(left, right)| {
                    left.enabled != right.enabled
                        || left.volts_per_div_mv != right.volts_per_div_mv
                        || left.coupling != right.coupling
                        || left.vertical_offset != right.vertical_offset
                })
            || self.trigger.hardware_differs_from(other.trigger)
    }

    fn from_capture_config(config: DsCopeCaptureConfig) -> Self {
        Self {
            sample_rate_hz: config.sample_rate_hz(),
            sample_count: config.sample_count(),
            channels: config.channels().map(|channel| ScopeChannelSettings {
                enabled: channel.enabled(),
                volts_per_div_mv: channel.volts_per_div_mv(),
                coupling: channel.coupling(),
                probe_attenuation: channel.probe_attenuation(),
                vertical_offset: channel.vertical_offset(),
            }),
            trigger: ScopeTriggerSettings::from_capture_config(config.trigger()),
            operation_mode: config.operation_mode(),
            bandwidth_limit: config.bandwidth_limit(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScopeCalibrationAction {
    Auto,
    Manual,
}

struct ManualCalibrationEditor {
    values: DsCopeCalibrationValues,
    range: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureSettingsTab {
    DsLogic,
    DsCope,
}

impl CaptureSettingsTab {
    fn label(self) -> &'static str {
        match self {
            Self::DsLogic => "DSLogic",
            Self::DsCope => "DSCope",
        }
    }
}

fn capture_settings_tabs(
    has_logic_analyzer: bool,
    has_oscilloscope: bool,
) -> Vec<CaptureSettingsTab> {
    let mut tabs = Vec::with_capacity(2);
    if has_logic_analyzer {
        tabs.push(CaptureSettingsTab::DsLogic);
    }
    if has_oscilloscope {
        tabs.push(CaptureSettingsTab::DsCope);
    }
    tabs
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaveformTab {
    LogicAnalyzer,
    Oscilloscope,
    Combined,
}

impl WaveformTab {
    fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "Logic Analyzer",
            Self::Oscilloscope => "Oscilloscope",
            Self::Combined => "Combined",
        }
    }
}

fn waveform_tabs(has_logic_analyzer: bool, has_oscilloscope: bool) -> Vec<WaveformTab> {
    let mut tabs = Vec::with_capacity(3);
    if has_logic_analyzer {
        tabs.push(WaveformTab::LogicAnalyzer);
    }
    if has_oscilloscope {
        tabs.push(WaveformTab::Oscilloscope);
    }
    if has_logic_analyzer && has_oscilloscope {
        tabs.push(WaveformTab::Combined);
    }
    tabs
}

fn default_logic_channel_colors() -> [Color32; LOGIC_CHANNEL_COUNT] {
    [
        Color32::from_rgb(90, 220, 145),
        Color32::from_rgb(255, 204, 74),
        Color32::from_rgb(74, 210, 230),
        Color32::from_rgb(238, 112, 214),
        Color32::from_rgb(255, 145, 77),
        Color32::from_rgb(99, 179, 255),
        Color32::from_rgb(180, 230, 80),
        Color32::from_rgb(255, 120, 160),
        Color32::from_rgb(70, 210, 185),
        Color32::from_rgb(245, 177, 66),
        Color32::from_rgb(174, 130, 255),
        Color32::from_rgb(100, 205, 255),
        Color32::from_rgb(245, 100, 100),
        Color32::from_rgb(105, 235, 190),
        Color32::from_rgb(225, 215, 90),
        Color32::from_rgb(195, 155, 255),
    ]
}

#[derive(Default)]
struct DeviceStats {
    frames: u64,
    samples: u64,
    first_timestamp: Option<Duration>,
    last_timestamp: Option<Duration>,
}

#[derive(Default)]
struct WaveformStore {
    logic_samples: VecDeque<u32>,
    logic_channels: u8,
    logic_rate_hz: u64,
    logic_sample_offset: u64,
    scope_channels: Vec<VecDeque<f32>>,
    scope_rate_hz: u64,
    scope_sample_offset: u64,
    scope_trigger_sample_index: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WaveformViewport {
    start: f32,
    span: f32,
}

impl Default for WaveformViewport {
    fn default() -> Self {
        Self {
            start: 0.0,
            span: 1.0,
        }
    }
}

impl WaveformViewport {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn time_per_division(self, sample_count: usize, sample_rate_hz: u64) -> Option<f64> {
        if sample_count == 0 || sample_rate_hz == 0 {
            return None;
        }
        Some(
            self.span as f64 * sample_count as f64
                / sample_rate_hz as f64
                / SCOPE_HORIZONTAL_DIVISIONS as f64,
        )
    }

    fn step_timebase(
        &mut self,
        scroll_delta: f32,
        sample_count: usize,
        sample_rate_hz: u64,
        cursor_fraction: f32,
    ) {
        let cursor_fraction = cursor_fraction.clamp(0.0, 1.0);
        let sample_fraction = self.start + cursor_fraction * self.span;
        self.step_timebase_at_sample(
            scroll_delta,
            sample_count,
            sample_rate_hz,
            sample_fraction,
            cursor_fraction,
        );
    }

    fn sample_range(self, sample_count: usize) -> Range<usize> {
        if sample_count == 0 {
            return 0..0;
        }
        let visible_start = self.start.max(0.0);
        let visible_end = (self.start + self.span).min(1.0);
        if visible_start >= visible_end {
            return 0..0;
        }
        let start = ((visible_start * sample_count as f32).floor() as usize).min(sample_count - 1);
        let mut end = (visible_end * sample_count as f32).ceil() as usize;
        end = end.clamp(start + 1, sample_count);
        start..end
    }

    fn zoom_factor(self) -> f32 {
        1.0 / self.span
    }

    fn move_screen_anchor(&mut self, old_fraction: f32, new_fraction: f32) {
        let sample_anchor = self.start + old_fraction.clamp(0.0, 1.0) * self.span;
        self.start = self.clamp_start(sample_anchor - new_fraction.clamp(0.0, 1.0) * self.span);
    }

    fn align_sample_to_screen(&mut self, sample_fraction: f32, screen_fraction: f32) {
        self.start =
            self.clamp_start(sample_fraction - screen_fraction.clamp(0.0, 1.0) * self.span);
    }

    fn clamp_start(self, start: f32) -> f32 {
        let first = 0.0_f32.min(1.0 - self.span);
        let last = 0.0_f32.max(1.0 - self.span);
        start.clamp(first, last)
    }

    fn step_timebase_at_sample(
        &mut self,
        scroll_delta: f32,
        sample_count: usize,
        sample_rate_hz: u64,
        sample_fraction: f32,
        screen_fraction: f32,
    ) {
        if scroll_delta == 0.0 {
            return;
        }
        let Some(current_timebase) = self.time_per_division(sample_count, sample_rate_hz) else {
            return;
        };
        let Some(timebase) = adjacent_timebase(current_timebase, scroll_delta > 0.0) else {
            return;
        };
        let full_duration = sample_count as f64 / sample_rate_hz as f64;
        let minimum_span = (1.0 / sample_count as f64).min(1.0);
        let maximum_span = TIMEBASE_STEPS_SECONDS[TIMEBASE_STEPS_SECONDS.len() - 1]
            * SCOPE_HORIZONTAL_DIVISIONS as f64
            / full_duration;
        self.span = (timebase * SCOPE_HORIZONTAL_DIVISIONS as f64 / full_duration)
            .clamp(minimum_span, maximum_span.max(minimum_span)) as f32;
        self.align_sample_to_screen(sample_fraction, screen_fraction);
    }
}

fn adjacent_timebase(current_seconds: f64, zoom_in: bool) -> Option<f64> {
    const COMPARISON_EPSILON: f64 = 1.0e-6;
    if zoom_in {
        TIMEBASE_STEPS_SECONDS
            .iter()
            .rev()
            .copied()
            .find(|step| *step < current_seconds * (1.0 - COMPARISON_EPSILON))
    } else {
        TIMEBASE_STEPS_SECONDS
            .iter()
            .copied()
            .find(|step| *step > current_seconds * (1.0 + COMPARISON_EPSILON))
    }
}

impl WaveformStore {
    fn ingest(&mut self, batch: &CaptureBatch, scope_channels: [bool; 2]) {
        match batch {
            CaptureBatch::Logic(logic) => {
                self.logic_channels = logic.channel_count;
                self.logic_rate_hz = logic.sample_rate_hz;
                self.logic_sample_offset +=
                    extend_bounded(&mut self.logic_samples, logic.samples.iter().copied()) as u64;
            }
            CaptureBatch::Scope(scope) => {
                self.scope_rate_hz = scope.sample_rate_hz;
                if self.scope_channels.len() != scope_channels.len() {
                    self.scope_channels = vec![VecDeque::new(); scope_channels.len()];
                    self.scope_sample_offset = 0;
                }
                if let Some(trigger_sample_index) = scope.trigger_sample_index {
                    self.scope_trigger_sample_index = Some(trigger_sample_index);
                }
                let mut removed_samples = 0;
                for (source, physical_channel) in scope.channels.iter().zip(
                    scope_channels
                        .into_iter()
                        .enumerate()
                        .filter_map(|(index, enabled)| enabled.then_some(index)),
                ) {
                    let destination = &mut self.scope_channels[physical_channel];
                    removed_samples =
                        removed_samples.max(extend_bounded(destination, source.iter().copied()));
                }
                self.scope_sample_offset += removed_samples as u64;
            }
        }
    }
}

fn extend_bounded<T>(destination: &mut VecDeque<T>, values: impl IntoIterator<Item = T>) -> usize {
    destination.extend(values);
    let excess = destination.len().saturating_sub(MAX_WAVEFORM_SAMPLES);
    destination.drain(..excess);
    excess
}

struct ActiveCapture {
    session: CaptureSession,
    scope_config: DsCopeCaptureConfig,
    stats: HashMap<DeviceId, DeviceStats>,
    stopped: HashSet<DeviceId>,
    waveforms: WaveformStore,
    recorder: Option<FileRecorder>,
    recording_path: Option<String>,
}

struct CompletedCapture {
    stats: HashMap<DeviceId, DeviceStats>,
    waveforms: WaveformStore,
    recording_path: Option<String>,
}

type FileRecorder = RecordingWriter<BufWriter<File>>;

enum WorkerSuccess {
    Devices(Vec<DiscoveredDevice>),
    Capture {
        session: CaptureSession,
        scope_config: DsCopeCaptureConfig,
    },
    ScopeConfigured(usize),
    ScopeAutoCalibrated,
    ScopeCalibrationLoaded(DsCopeCalibrationValues),
    ScopeCalibrationSaved,
}

type WorkerResult = Result<WorkerSuccess, String>;

struct DsViewApp {
    resource_directory: String,
    settings_open: bool,
    auto_calibration_confirmation_open: bool,
    manual_calibration: Option<ManualCalibrationEditor>,
    record_capture: bool,
    recording_path: String,
    devices: Vec<DeviceEntry>,
    active_capture_settings_tab: CaptureSettingsTab,
    scope_settings: ScopeCaptureSettings,
    active_waveform_tab: Option<WaveformTab>,
    combined_logic_ratio: f32,
    logic_channel_colors: [Color32; LOGIC_CHANNEL_COUNT],
    scope_channel_colors: [Color32; 2],
    logic_viewport: WaveformViewport,
    scope_viewport: WaveformViewport,
    scope_configuration_due: Option<Instant>,
    pending: Option<Receiver<WorkerResult>>,
    capture: Option<ActiveCapture>,
    completed_capture: Option<CompletedCapture>,
    status: String,
    last_error: Option<String>,
}

impl DsViewApp {
    fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        configure_style(&creation_context.egui_ctx);
        let mut app = Self {
            resource_directory: default_resource_directory(),
            settings_open: false,
            auto_calibration_confirmation_open: false,
            manual_calibration: None,
            record_capture: false,
            recording_path: "capture.dsv".to_owned(),
            devices: Vec::new(),
            active_capture_settings_tab: CaptureSettingsTab::DsLogic,
            scope_settings: ScopeCaptureSettings::default(),
            active_waveform_tab: None,
            combined_logic_ratio: 0.55,
            logic_channel_colors: default_logic_channel_colors(),
            scope_channel_colors: SCOPE_COLORS,
            logic_viewport: WaveformViewport::default(),
            scope_viewport: WaveformViewport::default(),
            scope_configuration_due: None,
            pending: None,
            capture: None,
            completed_capture: None,
            status: "Ready".to_owned(),
            last_error: None,
        };
        app.begin_refresh();
        app
    }

    fn begin_refresh(&mut self) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.completed_capture = None;
        self.status = "Discovering USB instruments…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = discover_supported_devices()
                .map(|report| WorkerSuccess::Devices(report.devices))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn begin_capture(&mut self) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        let devices = self
            .devices
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.device.clone())
            .collect::<Vec<_>>();
        if devices.is_empty() {
            self.last_error = Some("Select at least one supported instrument".to_owned());
            return;
        }
        if self.record_capture && self.recording_path.trim().is_empty() {
            self.last_error = Some("Enter a recording file path".to_owned());
            return;
        }
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };

        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.completed_capture = None;
        self.status = format!("Preparing {} instrument(s)…", devices.len());
        self.last_error = None;
        thread::spawn(move || {
            let result = start_capture(devices, &resource_directory, scope_config).map(|session| {
                WorkerSuccess::Capture {
                    session,
                    scope_config,
                }
            });
            let _ = sender.send(result);
        });
    }

    fn begin_scope_configuration(&mut self) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        self.scope_configuration_due = None;
        let devices = self
            .devices
            .iter()
            .filter(|entry| {
                entry.selected && entry.device.profile.model == DeviceModel::DsCopeU3P100
            })
            .map(|entry| entry.device.clone())
            .collect::<Vec<_>>();
        if devices.is_empty() {
            self.status = "DSCope settings will be applied when capture starts".to_owned();
            return;
        }
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let device_count = devices.len();
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.status = format!("Applying DSCope configuration to {device_count} instrument(s)…");
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                for device in devices {
                    let bitstream_path =
                        resource_directory.join(device.profile.fpga_bitstream_name);
                    prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                    apply_dscope_configuration(&device, scope_config)
                        .map_err(|error| error.to_string())?;
                }
                Ok(WorkerSuccess::ScopeConfigured(device_count))
            })();
            let _ = sender.send(result);
        });
    }

    fn selected_scope_device(&mut self) -> Option<DiscoveredDevice> {
        let devices = self
            .devices
            .iter()
            .filter(|entry| {
                entry.selected && entry.device.profile.model == DeviceModel::DsCopeU3P100
            })
            .map(|entry| entry.device.clone())
            .collect::<Vec<_>>();
        if devices.len() != 1 {
            self.last_error =
                Some("Select exactly one connected DSCope U3P100 for calibration".to_owned());
            return None;
        }
        devices.into_iter().next()
    }

    fn begin_auto_calibration(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device() else {
            return;
        };
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.status = "Auto-calibrating DSCope across all input ranges…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                auto_calibrate_dscope(&device).map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeAutoCalibrated)
            })();
            let _ = sender.send(result);
        });
    }

    fn begin_manual_calibration(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device() else {
            return;
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.status = "Loading DSCope calibration values…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                let values = read_dscope_calibration(&device).map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeCalibrationLoaded(values))
            })();
            let _ = sender.send(result);
        });
    }

    fn begin_save_manual_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device() else {
            return;
        };
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.status = "Saving DSCope calibration values to device NVM…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                write_dscope_calibration(&device, values).map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeCalibrationSaved)
            })();
            let _ = sender.send(result);
        });
    }

    fn stop_capture(&mut self) {
        let Some(mut active) = self.capture.take() else {
            return;
        };
        self.status = "Stopping capture…".to_owned();
        let stop_result = active.session.stop();
        let completion = if stop_result.is_ok() {
            CompletionStatus::Cancelled
        } else {
            CompletionStatus::Failed
        };
        let recording_result = finish_gui_recording(&mut active.recorder, completion);
        self.completed_capture = Some(CompletedCapture {
            stats: active.stats,
            waveforms: active.waveforms,
            recording_path: active.recording_path,
        });
        match (stop_result, recording_result) {
            (Ok(()), Ok(())) => self.status = "Capture stopped".to_owned(),
            (Err(error), Ok(())) => self.set_error(error.to_string()),
            (Ok(()), Err(error)) => self.set_error(error),
            (Err(stop_error), Err(recording_error)) => self.set_error(format!(
                "{stop_error}; additionally failed to finish recording: {recording_error}"
            )),
        }
    }

    fn poll_worker(&mut self) {
        let Some(receiver) = self.pending.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err("background operation ended unexpectedly".to_owned())
            }
        };
        self.pending = None;
        match result {
            Ok(WorkerSuccess::Devices(devices)) => {
                let selected = self
                    .devices
                    .iter()
                    .filter(|entry| entry.selected)
                    .map(|entry| entry.device.location.stable_id())
                    .collect::<HashSet<_>>();
                self.devices = devices
                    .into_iter()
                    .map(|device| {
                        let stable_id = device.location.stable_id();
                        let selected = if selected.is_empty() {
                            device.link_status() != LinkStatus::Unsupported
                        } else {
                            selected.contains(&stable_id)
                        };
                        DeviceEntry { device, selected }
                    })
                    .collect();
                self.status = if self.devices.is_empty() {
                    "No supported instruments detected".to_owned()
                } else {
                    format!("Detected {} supported instrument(s)", self.devices.len())
                };
            }
            Ok(WorkerSuccess::Capture {
                session,
                scope_config,
            }) => {
                let count = session.descriptors().len();
                let recording_path = self
                    .record_capture
                    .then(|| self.recording_path.trim().to_owned());
                let recorder = match recording_path.as_deref() {
                    Some(path) => match start_gui_recording(
                        Path::new(path),
                        session.descriptors(),
                        scope_config,
                    ) {
                        Ok(recorder) => Some(recorder),
                        Err(error) => {
                            let cleanup = session.stop().err();
                            self.set_error(match cleanup {
                                Some(cleanup) => {
                                    format!("{error}; additionally failed to stop: {cleanup}")
                                }
                                None => error,
                            });
                            return;
                        }
                    },
                    None => None,
                };
                self.capture = Some(ActiveCapture {
                    session,
                    scope_config,
                    stats: HashMap::new(),
                    stopped: HashSet::new(),
                    waveforms: WaveformStore::default(),
                    recorder,
                    recording_path: recording_path.clone(),
                });
                self.status = match recording_path {
                    Some(path) => {
                        format!("Capturing {count} instrument(s) to {path}")
                    }
                    None => format!("Capturing from {count} instrument(s)"),
                };
            }
            Ok(WorkerSuccess::ScopeConfigured(device_count)) => {
                self.status =
                    format!("Applied DSCope configuration to {device_count} instrument(s)");
            }
            Ok(WorkerSuccess::ScopeAutoCalibrated) => {
                self.status = "DSCope auto calibration completed and verified".to_owned();
            }
            Ok(WorkerSuccess::ScopeCalibrationLoaded(values)) => {
                self.manual_calibration = Some(ManualCalibrationEditor { values, range: 0 });
                self.status = "Loaded DSCope calibration values".to_owned();
            }
            Ok(WorkerSuccess::ScopeCalibrationSaved) => {
                self.manual_calibration = None;
                self.status = "DSCope manual calibration saved and verified".to_owned();
            }
            Err(error) => self.set_error(error),
        }
    }

    fn poll_capture(&mut self) {
        let mut finish_status = None::<(Result<String, String>, CompletionStatus)>;
        if let Some(active) = self.capture.as_mut() {
            loop {
                match active.session.try_recv() {
                    Ok(event) => {
                        if let Some(recorder) = active.recorder.as_mut() {
                            if let Err(error) = recorder.write_event(&event) {
                                active.session.request_stop();
                                finish_status = Some((
                                    Err(format!("failed to write recording: {error}")),
                                    CompletionStatus::Failed,
                                ));
                                break;
                            }
                        }
                        match event {
                            CaptureEvent::Frame(frame) => {
                                let entry = active.stats.entry(frame.device_id).or_default();
                                entry.frames += 1;
                                entry.samples += frame.payload.sample_count() as u64;
                                entry.first_timestamp.get_or_insert(frame.captured_at);
                                entry.last_timestamp = Some(frame.captured_at);
                                active.waveforms.ingest(
                                    &frame.payload,
                                    active
                                        .scope_config
                                        .channels()
                                        .map(|channel| channel.enabled()),
                                );
                            }
                            CaptureEvent::SourceFailed { device_id, error } => {
                                active.session.request_stop();
                                finish_status = Some((
                                    Err(format!("{device_id}: {error}")),
                                    CompletionStatus::Failed,
                                ));
                                break;
                            }
                            CaptureEvent::SourceStopped { device_id } => {
                                active.stopped.insert(device_id);
                                if active.stopped.len() == active.session.descriptors().len() {
                                    finish_status = Some((
                                        Ok("Capture complete".to_owned()),
                                        CompletionStatus::Complete,
                                    ));
                                    break;
                                }
                            }
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finish_status = Some((
                            Err("capture event stream disconnected".to_owned()),
                            CompletionStatus::Failed,
                        ));
                        break;
                    }
                }
            }
        }

        if let Some((result, completion)) = finish_status {
            if let Some(mut active) = self.capture.take() {
                let stop_result = active.session.stop();
                let recording_completion = if stop_result.is_ok() {
                    completion
                } else {
                    CompletionStatus::Failed
                };
                let recording_result =
                    finish_gui_recording(&mut active.recorder, recording_completion);
                let mut error = result.as_ref().err().cloned();
                if let Err(stop_error) = stop_result {
                    append_error(&mut error, format!("failed to stop capture: {stop_error}"));
                }
                if let Err(recording_error) = recording_result {
                    append_error(
                        &mut error,
                        format!("failed to finish recording: {recording_error}"),
                    );
                }
                self.completed_capture = Some(CompletedCapture {
                    stats: active.stats,
                    waveforms: active.waveforms,
                    recording_path: active.recording_path,
                });
                match error {
                    Some(error) => self.set_error(error),
                    None => {
                        self.status = result.ok().unwrap_or_else(|| "Capture complete".to_owned());
                    }
                }
            }
        }
    }

    fn set_error(&mut self, error: String) {
        self.status = "Operation failed".to_owned();
        self.last_error = Some(error);
    }

    fn available_waveform_tabs(&self) -> Vec<WaveformTab> {
        let has_logic_analyzer = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model == DeviceModel::DsLogicU3Pro16);
        let has_oscilloscope = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model == DeviceModel::DsCopeU3P100);
        waveform_tabs(has_logic_analyzer, has_oscilloscope)
    }

    fn available_capture_settings_tabs(&self) -> Vec<CaptureSettingsTab> {
        let has_logic_analyzer = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model == DeviceModel::DsLogicU3Pro16);
        let has_oscilloscope = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model == DeviceModel::DsCopeU3P100);
        capture_settings_tabs(has_logic_analyzer, has_oscilloscope)
    }

    fn reconcile_capture_settings_tab(&mut self) {
        let available = self.available_capture_settings_tabs();
        if available.contains(&self.active_capture_settings_tab) {
            return;
        }
        if let Some(tab) = available.first() {
            self.active_capture_settings_tab = *tab;
        }
    }

    fn reconcile_waveform_tab(&mut self) {
        let available = self.available_waveform_tabs();
        if self
            .active_waveform_tab
            .is_some_and(|active| available.contains(&active))
        {
            return;
        }
        self.active_waveform_tab = if available.contains(&WaveformTab::Combined) {
            Some(WaveformTab::Combined)
        } else {
            available.first().copied()
        };
    }

    fn is_busy(&self) -> bool {
        self.pending.is_some() || self.capture.is_some()
    }
}

fn format_sample_count(sample_count: u64) -> String {
    if sample_count >= 1_000_000 {
        format!("{}M samples", sample_count / 1_000_000)
    } else if sample_count >= 1_000 {
        format!("{}k samples", sample_count / 1_000)
    } else {
        format!("{sample_count} samples")
    }
}

fn format_input_scale(volts_per_div_mv: u16) -> String {
    if volts_per_div_mv >= 1_000 {
        format!("{} V/div", volts_per_div_mv / 1_000)
    } else {
        format!("{volts_per_div_mv} mV/div")
    }
}

fn step_scope_input_range(current: u16, steps: i32) -> u16 {
    let current_index = DSCOPE_VOLTS_PER_DIV_MV
        .iter()
        .position(|candidate| *candidate == current)
        .unwrap_or(0) as i32;
    let maximum_index = DSCOPE_VOLTS_PER_DIV_MV.len() as i32 - 1;
    DSCOPE_VOLTS_PER_DIV_MV[(current_index + steps).clamp(0, maximum_index) as usize]
}

fn paint_scope_input_range_encoder(
    ui: &mut egui::Ui,
    channel_index: usize,
    volts_per_div_mv: &mut u16,
    channel_color: &mut Color32,
) -> bool {
    let mut steps = 0;
    ui.horizontal(|ui| {
        let encoder_id = ui.make_persistent_id(("scope_range_encoder", channel_index));
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(58.0), Sense::click_and_drag());
        let response = response
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_text(
                "Drag the knob vertically or scroll to rotate it · double-click to change color",
            );
        if response.clicked() {
            response.request_focus();
        }
        let popup_id = ui.make_persistent_id(("scope_channel_color_popup", channel_index));
        if response.double_clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }
        if response.hovered() {
            let scroll_delta = ui.input_mut(|input| {
                let delta = input.raw_scroll_delta.y;
                input.raw_scroll_delta.y = 0.0;
                delta
            });
            if scroll_delta.abs() > f32::EPSILON {
                steps += scroll_delta.signum() as i32;
            }
        }
        if response.dragged() {
            let pointer_delta = ui.input(|input| input.pointer.delta());
            let rotation_delta = pointer_delta.x - pointer_delta.y;
            let drag_steps = ui.data_mut(|data| {
                let accumulator = data.get_temp_mut_or_default::<f32>(encoder_id);
                *accumulator += rotation_delta;
                let whole_steps = (*accumulator / 12.0).trunc() as i32;
                *accumulator -= whole_steps as f32 * 12.0;
                whole_steps
            });
            steps += drag_steps;
        }
        if response.drag_stopped() {
            ui.data_mut(|data| data.remove_temp::<f32>(encoder_id));
        }
        if response.has_focus() {
            ui.input(|input| {
                if input.key_pressed(egui::Key::ArrowUp) || input.key_pressed(egui::Key::ArrowRight)
                {
                    steps += 1;
                }
                if input.key_pressed(egui::Key::ArrowDown)
                    || input.key_pressed(egui::Key::ArrowLeft)
                {
                    steps -= 1;
                }
            });
        }

        let current_index = DSCOPE_VOLTS_PER_DIV_MV
            .iter()
            .position(|candidate| *candidate == *volts_per_div_mv)
            .unwrap_or(0);
        let fraction = current_index as f32 / (DSCOPE_VOLTS_PER_DIV_MV.len() - 1) as f32;
        let center = rect.center();
        let radius = 22.0;
        let painter = ui.painter();
        painter.circle_filled(center, radius, channel_color.gamma_multiply(0.14));
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(1.4_f32, channel_color.gamma_multiply(0.7)),
        );
        for tick_index in 0..DSCOPE_VOLTS_PER_DIV_MV.len() {
            let tick_fraction = tick_index as f32 / (DSCOPE_VOLTS_PER_DIV_MV.len() - 1) as f32;
            let angle = std::f32::consts::PI * (0.75 + tick_fraction * 1.5);
            let direction = Vec2::new(angle.cos(), angle.sin());
            painter.line_segment(
                [center + direction * 25.0, center + direction * 28.0],
                Stroke::new(1.0_f32, channel_color.gamma_multiply(0.55)),
            );
        }
        let angle = std::f32::consts::PI * (0.75 + fraction * 1.5);
        let direction = Vec2::new(angle.cos(), angle.sin());
        painter.line_segment(
            [center, center + direction * 16.0],
            Stroke::new(2.8_f32, *channel_color),
        );
        painter.circle_filled(center, 2.5, *channel_color);

        ui.vertical(|ui| {
            ui.label(
                RichText::new(format_input_scale(*volts_per_div_mv))
                    .strong()
                    .color(*channel_color),
            );
            ui.small(RichText::new("drag / scroll").color(TEXT_MUTED));
        });

        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &response,
            egui::popup::PopupCloseBehavior::CloseOnClickOutside,
            |popup_ui| {
                popup_ui.set_min_width(280.0);
                popup_ui.label(RichText::new(format!("CH{channel_index} color")).strong());
                egui::color_picker::color_picker_color32(
                    popup_ui,
                    channel_color,
                    egui::color_picker::Alpha::Opaque,
                );
                if popup_ui.button("Reset color").clicked() {
                    *channel_color = SCOPE_COLORS[channel_index];
                }
            },
        );
    });

    let next = step_scope_input_range(*volts_per_div_mv, steps);
    let changed = next != *volts_per_div_mv;
    *volts_per_div_mv = next;
    changed
}

fn coupling_label(coupling: DsCopeCoupling) -> &'static str {
    match coupling {
        DsCopeCoupling::Dc => "DC",
        DsCopeCoupling::Ac => "AC",
    }
}

fn trigger_source_label(source: DsCopeTriggerSource) -> &'static str {
    match source {
        DsCopeTriggerSource::Auto => "Auto",
        DsCopeTriggerSource::ChannelZero => "CH0",
        DsCopeTriggerSource::ChannelOne => "CH1",
        DsCopeTriggerSource::ChannelZeroAndOne => "CH0 AND CH1",
        DsCopeTriggerSource::ChannelZeroOrOne => "CH0 OR CH1",
    }
}

fn trigger_slope_label(slope: DsCopeTriggerSlope) -> &'static str {
    match slope {
        DsCopeTriggerSlope::Rising => "Rising edge",
        DsCopeTriggerSlope::Falling => "Falling edge",
    }
}

fn paint_scope_trigger_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeTriggerSettings,
    channels: [bool; 2],
    enabled: bool,
) {
    egui::CollapsingHeader::new(RichText::new("Trigger").strong())
        .default_open(false)
        .show(ui, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                egui::Grid::new("dscope_trigger_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Source");
                        egui::ComboBox::from_id_salt("scope_trigger_source")
                            .selected_text(trigger_source_label(settings.source))
                            .show_ui(ui, |ui| {
                                for source in [
                                    DsCopeTriggerSource::Auto,
                                    DsCopeTriggerSource::ChannelZero,
                                    DsCopeTriggerSource::ChannelOne,
                                    DsCopeTriggerSource::ChannelZeroAndOne,
                                    DsCopeTriggerSource::ChannelZeroOrOne,
                                ] {
                                    let mut candidate = *settings;
                                    candidate.source = source;
                                    ui.add_enabled_ui(
                                        candidate.source_is_available(channels),
                                        |ui| {
                                            ui.selectable_value(
                                                &mut settings.source,
                                                source,
                                                trigger_source_label(source),
                                            );
                                        },
                                    );
                                }
                            });
                        ui.end_row();

                        if settings.source == DsCopeTriggerSource::Auto {
                            ui.label("Auto channel");
                            egui::ComboBox::from_id_salt("scope_trigger_auto_channel")
                                .selected_text(format!("CH{}", settings.auto_channel))
                                .show_ui(ui, |ui| {
                                    for (channel, channel_enabled) in
                                        channels.into_iter().enumerate()
                                    {
                                        ui.add_enabled_ui(channel_enabled, |ui| {
                                            ui.selectable_value(
                                                &mut settings.auto_channel,
                                                channel as u8,
                                                format!("CH{channel}"),
                                            );
                                        });
                                    }
                                });
                            ui.end_row();
                        }

                        ui.label("Edge");
                        egui::ComboBox::from_id_salt("scope_trigger_slope")
                            .selected_text(trigger_slope_label(settings.slope))
                            .show_ui(ui, |ui| {
                                for slope in
                                    [DsCopeTriggerSlope::Rising, DsCopeTriggerSlope::Falling]
                                {
                                    ui.selectable_value(
                                        &mut settings.slope,
                                        slope,
                                        trigger_slope_label(slope),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Holdoff");
                        ui.horizontal(|ui| {
                            let multiplier = settings.holdoff_unit.nanoseconds();
                            let mut value = settings.holdoff_ns as f64 / multiplier;
                            let maximum = DSCOPE_TRIGGER_HOLDOFF_MAX_NS as f64 / multiplier;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut value)
                                        .range(0.0..=maximum)
                                        .speed(0.1)
                                        .max_decimals(3),
                                )
                                .changed()
                            {
                                settings.holdoff_ns = (value * multiplier).round() as u64;
                            }
                            egui::ComboBox::from_id_salt("scope_trigger_holdoff_unit")
                                .selected_text(settings.holdoff_unit.label())
                                .show_ui(ui, |ui| {
                                    for unit in [
                                        TriggerHoldoffUnit::Microseconds,
                                        TriggerHoldoffUnit::Milliseconds,
                                        TriggerHoldoffUnit::Seconds,
                                    ] {
                                        ui.selectable_value(
                                            &mut settings.holdoff_unit,
                                            unit,
                                            unit.label(),
                                        );
                                    }
                                });
                        });
                        ui.end_row();

                        ui.label("Noise sensitivity");
                        ui.add(egui::Slider::new(
                            &mut settings.noise_sensitivity,
                            0..=DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX,
                        ));
                        ui.end_row();
                    });
            });
        });
}

fn paint_scope_channel_settings(
    ui: &mut egui::Ui,
    channel_index: usize,
    settings: &mut ScopeChannelSettings,
    channel_color: &mut Color32,
    other_channel_enabled: bool,
    capture_settings_enabled: bool,
) {
    egui::Frame::none()
        .fill(PANEL.gamma_multiply(0.72))
        .rounding(egui::Rounding::same(5.0))
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let can_toggle_enable = !settings.enabled || other_channel_enabled;
            let enable_control =
                egui::Checkbox::new(&mut settings.enabled, format!("CH{channel_index} enabled"));
            ui.add_enabled(
                capture_settings_enabled && can_toggle_enable,
                enable_control,
            )
            .on_hover_text("At least one oscilloscope channel must remain enabled");
            ui.add_enabled_ui(capture_settings_enabled && settings.enabled, |ui| {
                ui.label("Input range");
                paint_scope_input_range_encoder(
                    ui,
                    channel_index,
                    &mut settings.volts_per_div_mv,
                    channel_color,
                );
                ui.add_space(12.0);
                egui::Grid::new(("scope_channel_secondary_settings", channel_index))
                    .num_columns(2)
                    .spacing([8.0, 5.0])
                    .show(ui, |ui| {
                        ui.label("Coupling");
                        egui::ComboBox::from_id_salt(("scope_coupling", channel_index))
                            .selected_text(coupling_label(settings.coupling))
                            .show_ui(ui, |ui| {
                                for coupling in [DsCopeCoupling::Dc, DsCopeCoupling::Ac] {
                                    ui.selectable_value(
                                        &mut settings.coupling,
                                        coupling,
                                        coupling_label(coupling),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Probe");
                        egui::ComboBox::from_id_salt(("scope_probe", channel_index))
                            .selected_text(format!("{}×", settings.probe_attenuation))
                            .show_ui(ui, |ui| {
                                for attenuation in [1, 10] {
                                    ui.selectable_value(
                                        &mut settings.probe_attenuation,
                                        attenuation,
                                        format!("{attenuation}×"),
                                    );
                                }
                            });
                        ui.end_row();
                    });
            });
        });
}

fn paint_capture_settings(
    ui: &mut egui::Ui,
    tab: CaptureSettingsTab,
    scope_settings: &mut ScopeCaptureSettings,
    scope_channel_colors: &mut [Color32; 2],
    enabled: bool,
    calibration_action: &mut Option<ScopeCalibrationAction>,
) {
    let card_width = ui.available_width();
    egui::Frame::none()
        .fill(BACKGROUND)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            let content_width = (card_width - 20.0).max(0.0);
            ui.set_width(content_width);
            ui.label(
                RichText::new(match tab {
                    CaptureSettingsTab::DsLogic => "DSLogic U3Pro16",
                    CaptureSettingsTab::DsCope => "DSCope U3P100",
                })
                .strong(),
            );
            ui.add_space(6.0);
            match tab {
                CaptureSettingsTab::DsLogic => {
                    egui::Grid::new("dslogic_capture_settings")
                        .num_columns(2)
                        .min_col_width(90.0)
                        .max_col_width(((content_width - 12.0) * 0.5).max(90.0))
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Sample rate");
                            ui.monospace("1 MHz");
                            ui.end_row();
                            ui.label("Buffer");
                            ui.monospace("1M samples");
                            ui.end_row();
                            ui.label("Channels");
                            ui.monospace("D0–D15");
                            ui.end_row();
                            ui.label("Display");
                            ui.monospace("Per-channel colors");
                            ui.end_row();
                        });
                }
                CaptureSettingsTab::DsCope => {
                    let maximum_sample_rate_hz = scope_settings.maximum_sample_rate_hz();
                    let maximum_sample_count = scope_settings.maximum_sample_count();
                    if scope_settings.sample_rate_hz > maximum_sample_rate_hz {
                        scope_settings.sample_rate_hz = maximum_sample_rate_hz;
                    }
                    if scope_settings.sample_count > maximum_sample_count {
                        scope_settings.sample_count = maximum_sample_count;
                    }
                    ui.add_enabled_ui(enabled, |ui| {
                        egui::Grid::new("dscope_common_capture_settings")
                            .num_columns(2)
                            .spacing([8.0, 5.0])
                            .show(ui, |ui| {
                                ui.label("Sample rate");
                                egui::ComboBox::from_id_salt("scope_sample_rate")
                                    .selected_text(format_sample_rate(
                                        scope_settings.sample_rate_hz,
                                    ))
                                    .show_ui(ui, |ui| {
                                        for rate in DSCOPE_SAMPLE_RATES_HZ
                                            .into_iter()
                                            .filter(|rate| *rate <= maximum_sample_rate_hz)
                                        {
                                            ui.selectable_value(
                                                &mut scope_settings.sample_rate_hz,
                                                rate,
                                                format_sample_rate(rate),
                                            );
                                        }
                                    });
                                ui.end_row();

                                ui.label("Buffer");
                                egui::ComboBox::from_id_salt("scope_sample_count")
                                    .selected_text(format_sample_count(scope_settings.sample_count))
                                    .show_ui(ui, |ui| {
                                        for count in DSCOPE_SAMPLE_COUNTS
                                            .into_iter()
                                            .filter(|count| *count <= maximum_sample_count)
                                        {
                                            ui.selectable_value(
                                                &mut scope_settings.sample_count,
                                                count,
                                                format_sample_count(count),
                                            );
                                        }
                                    });
                                ui.end_row();

                                ui.label("Maximum");
                                ui.monospace(format_sample_rate(maximum_sample_rate_hz));
                                ui.end_row();
                            });
                    });
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(RichText::new("Options").strong())
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.add_enabled_ui(enabled, |ui| {
                                egui::Grid::new("dscope_options")
                                    .num_columns(2)
                                    .spacing([8.0, 6.0])
                                    .show(ui, |ui| {
                                        ui.label("Operating mode");
                                        egui::ComboBox::from_id_salt("scope_operation_mode")
                                            .selected_text(match scope_settings.operation_mode {
                                                DsCopeOperationMode::Normal => "Normal",
                                                DsCopeOperationMode::InternalTest => {
                                                    "Internal test"
                                                }
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut scope_settings.operation_mode,
                                                    DsCopeOperationMode::Normal,
                                                    "Normal",
                                                );
                                                ui.selectable_value(
                                                    &mut scope_settings.operation_mode,
                                                    DsCopeOperationMode::InternalTest,
                                                    "Internal test",
                                                );
                                            });
                                        ui.end_row();

                                        ui.label("Bandwidth");
                                        egui::ComboBox::from_id_salt("scope_bandwidth_limit")
                                            .selected_text(match scope_settings.bandwidth_limit {
                                                DsCopeBandwidthLimit::Full => "Full",
                                                DsCopeBandwidthLimit::Mhz20 => "20 MHz",
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut scope_settings.bandwidth_limit,
                                                    DsCopeBandwidthLimit::Full,
                                                    "Full",
                                                );
                                                ui.selectable_value(
                                                    &mut scope_settings.bandwidth_limit,
                                                    DsCopeBandwidthLimit::Mhz20,
                                                    "20 MHz",
                                                );
                                            });
                                        ui.end_row();
                                    });
                                ui.add_space(6.0);
                                ui.label(RichText::new("Calibration").color(TEXT_MUTED));
                                ui.horizontal(|ui| {
                                    if ui.button("Auto…").clicked() {
                                        *calibration_action = Some(ScopeCalibrationAction::Auto);
                                    }
                                    if ui.button("Manual…").clicked() {
                                        *calibration_action = Some(ScopeCalibrationAction::Manual);
                                    }
                                });
                            });
                        });
                    ui.add_space(8.0);
                    let channel_count = scope_settings.channels.len();
                    let channel_enabled = scope_settings.channels.map(|channel| channel.enabled);
                    paint_scope_trigger_settings(
                        ui,
                        &mut scope_settings.trigger,
                        channel_enabled,
                        enabled,
                    );
                    ui.add_space(8.0);
                    for (index, channel) in scope_settings.channels.iter_mut().enumerate() {
                        paint_scope_channel_settings(
                            ui,
                            index,
                            channel,
                            &mut scope_channel_colors[index],
                            channel_enabled[1 - index],
                            enabled,
                        );
                        if index + 1 < channel_count {
                            ui.add_space(6.0);
                        }
                    }
                }
            }
        });
    scope_settings.clamp_to_channel_limits();
}

fn paint_instrument_settings(
    ui: &mut egui::Ui,
    devices: &mut [DeviceEntry],
    selection_enabled: bool,
) {
    if devices.is_empty() {
        ui.label(RichText::new("No supported devices").color(TEXT_MUTED));
        return;
    }

    for entry in devices {
        ui.add_space(4.0);
        egui::Frame::none()
            .fill(BACKGROUND)
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(10.0))
            .show(ui, |ui| {
                ui.add_enabled(
                    selection_enabled,
                    egui::Checkbox::new(
                        &mut entry.selected,
                        RichText::new(entry.device.profile.display_name).strong(),
                    ),
                );
                ui.small(RichText::new(entry.device.location.stable_id()).color(TEXT_MUTED));
                let (link_text, link_color) = match entry.device.link_status() {
                    LinkStatus::Optimal => ("USB 3 · optimal", ACCENT),
                    LinkStatus::ReducedPerformance => {
                        ("USB 2 · reduced performance", SCOPE_COLORS[0])
                    }
                    LinkStatus::Unsupported => ("Unsupported link speed", Color32::LIGHT_RED),
                };
                ui.small(RichText::new(link_text).color(link_color));
            });
    }
}

impl eframe::App for DsViewApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();
        self.reconcile_capture_settings_tab();
        self.reconcile_waveform_tab();
        self.poll_capture();

        let mut refresh_clicked = false;
        let mut start_clicked = false;
        let mut stop_clicked = false;
        let mut calibration_action = None;
        let capture_settings_enabled = !self.is_busy();
        let scope_settings_before_ui = self.scope_settings;

        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(12.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("DSVIEW").strong().size(18.0).color(ACCENT));
                    ui.separator();
                    ui.menu_button("App", |ui| {
                        if ui.button("Settings…").clicked() {
                            self.settings_open = true;
                            ui.close_menu();
                        }
                    });
                    ui.separator();
                    ui.label(RichText::new("USB capture workbench").color(TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        stop_clicked = ui
                            .add_enabled(self.capture.is_some(), egui::Button::new("■ Stop"))
                            .clicked();
                        start_clicked = ui
                            .add_enabled(
                                !self.is_busy() && self.devices.iter().any(|entry| entry.selected),
                                egui::Button::new("▶ Capture"),
                            )
                            .clicked();
                        refresh_clicked = ui
                            .add_enabled(!self.is_busy(), egui::Button::new("↻ Refresh"))
                            .clicked();
                    });
                });
            });

        egui::SidePanel::left("capture_setup_fixed")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(context, |ui| {
                let available_capture_settings_tabs = self.available_capture_settings_tabs();
                let content_width = ui.available_width();
                egui::ScrollArea::vertical()
                    .max_width(content_width)
                    .show(ui, |ui| {
                        ui.set_width(content_width);
                        ui.label(RichText::new("CAPTURE SETUP").strong().color(TEXT_MUTED));
                        ui.add_space(8.0);
                        if available_capture_settings_tabs.len() > 1 {
                            ui.horizontal(|ui| {
                                for tab in &available_capture_settings_tabs {
                                    ui.selectable_value(
                                        &mut self.active_capture_settings_tab,
                                        *tab,
                                        tab.label(),
                                    );
                                }
                            });
                            ui.add_space(6.0);
                        }
                        if available_capture_settings_tabs.is_empty() {
                            ui.label(
                                RichText::new(
                                    "Connect a supported instrument to configure capture.",
                                )
                                .color(TEXT_MUTED),
                            );
                        } else {
                            paint_capture_settings(
                                ui,
                                self.active_capture_settings_tab,
                                &mut self.scope_settings,
                                &mut self.scope_channel_colors,
                                capture_settings_enabled,
                                &mut calibration_action,
                            );
                        }

                        ui.add_space(14.0);
                        ui.label(RichText::new("RECORDING").strong().color(TEXT_MUTED));
                        ui.add_space(6.0);
                        ui.add_enabled(
                            !self.is_busy(),
                            egui::Checkbox::new(
                                &mut self.record_capture,
                                "Record complete capture",
                            ),
                        );
                        if self.record_capture {
                            ui.label("Recording file");
                            ui.add_enabled(
                                !self.is_busy(),
                                egui::TextEdit::singleline(&mut self.recording_path)
                                    .desired_width(f32::INFINITY),
                            );
                            ui.small(
                                RichText::new(
                                    "Lossless .dsv file; existing files are not overwritten",
                                )
                                .color(TEXT_MUTED),
                            );
                        }
                        if let Some(active) = &self.capture {
                            ui.add_space(18.0);
                            ui.label(RichText::new("SESSION").strong().color(TEXT_MUTED));
                            ui.add_space(6.0);
                            for descriptor in active.session.descriptors() {
                                let stats = active.stats.get(&descriptor.id);
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("●").color(ACCENT));
                                    ui.label(&descriptor.name);
                                });
                                if let Some(stats) = stats {
                                    ui.small(
                                        RichText::new(format!(
                                            "{} frames · {} samples",
                                            stats.frames, stats.samples
                                        ))
                                        .color(TEXT_MUTED),
                                    );
                                }
                            }
                            if let Some(skew) = capture_start_skew(&active.stats) {
                                ui.add_space(6.0);
                                ui.label(format!("Start skew: {skew:?}"));
                            }
                        } else if let Some(completed) = &self.completed_capture {
                            ui.add_space(18.0);
                            ui.label(RichText::new("LAST CAPTURE").strong().color(TEXT_MUTED));
                            if let Some(path) = &completed.recording_path {
                                ui.small(
                                    RichText::new(format!("Recording file: {path}")).color(ACCENT),
                                );
                            }
                            for (device_id, stats) in &completed.stats {
                                ui.add_space(4.0);
                                ui.label(device_id.to_string());
                                ui.small(
                                    RichText::new(format!(
                                        "{} frames · {} samples",
                                        stats.frames, stats.samples
                                    ))
                                    .color(TEXT_MUTED),
                                );
                            }
                            if let Some(skew) = capture_start_skew(&completed.stats) {
                                ui.add_space(6.0);
                                ui.label(format!("Start skew: {skew:?}"));
                            }
                        }
                    });
            });

        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(9.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    let color = if self.last_error.is_some() {
                        Color32::LIGHT_RED
                    } else if self.is_busy() {
                        ACCENT
                    } else {
                        TEXT_MUTED
                    };
                    ui.label(RichText::new("●").color(color));
                    ui.label(&self.status);
                    if let Some(error) = &self.last_error {
                        ui.separator();
                        ui.label(RichText::new(error).color(Color32::LIGHT_RED));
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::same(16.0)),
            )
            .show(context, |ui| {
                let available_tabs = self.available_waveform_tabs();
                if available_tabs.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(
                                "Connect a supported logic analyzer or oscilloscope, then Refresh",
                            )
                            .color(TEXT_MUTED),
                        );
                    });
                    return;
                }

                ui.horizontal(|ui| {
                    for tab in &available_tabs {
                        if ui
                            .selectable_label(self.active_waveform_tab == Some(*tab), tab.label())
                            .clicked()
                        {
                            self.active_waveform_tab = Some(*tab);
                        }
                    }
                });
                ui.separator();
                ui.add_space(8.0);

                let empty_waveforms = WaveformStore::default();
                let waveforms = self
                    .capture
                    .as_ref()
                    .map(|active| &active.waveforms)
                    .or_else(|| {
                        self.completed_capture
                            .as_ref()
                            .map(|capture| &capture.waveforms)
                    })
                    .unwrap_or(&empty_waveforms);
                let mut combined_logic_ratio = self.combined_logic_ratio;
                let mut logic_channel_colors = self.logic_channel_colors;
                let mut logic_viewport = self.logic_viewport;
                let mut scope_viewport = self.scope_viewport;
                let mut scope_vertical_offsets = self
                    .scope_settings
                    .channels
                    .map(|channel| channel.vertical_offset);
                let mut scope_trigger_settings = self.scope_settings.trigger;
                let scope_settings = self
                    .capture
                    .as_ref()
                    .map(|active| ScopeCaptureSettings::from_capture_config(active.scope_config))
                    .unwrap_or(self.scope_settings);
                match self.active_waveform_tab {
                    Some(WaveformTab::LogicAnalyzer) => {
                        waveform_card_fill(
                            ui,
                            "DIGITAL",
                            "DSLogic · channels 0–15",
                            |ui, rect| {
                                paint_logic(
                                    ui,
                                    rect,
                                    waveforms,
                                    &mut logic_channel_colors,
                                    &mut logic_viewport,
                                )
                            },
                        );
                    }
                    Some(WaveformTab::Oscilloscope) => {
                        waveform_card_fill(
                            ui,
                            "OSCILLOSCOPE",
                            "DSCope · CH0 / CH1",
                            |ui, rect| {
                                paint_scope(
                                    ui,
                                    rect,
                                    waveforms,
                                    &mut scope_viewport,
                                    ScopeWaveformOptions {
                                        settings: scope_settings,
                                        channel_colors: self.scope_channel_colors,
                                        vertical_offsets: &mut scope_vertical_offsets,
                                        trigger_settings: &mut scope_trigger_settings,
                                        offset_controls_enabled: capture_settings_enabled,
                                    },
                                )
                            },
                        );
                    }
                    Some(WaveformTab::Combined) => {
                        combined_waveform_cards(
                            ui,
                            waveforms,
                            &mut combined_logic_ratio,
                            &mut logic_channel_colors,
                            &mut logic_viewport,
                            &mut scope_viewport,
                            ScopeWaveformOptions {
                                settings: scope_settings,
                                channel_colors: self.scope_channel_colors,
                                vertical_offsets: &mut scope_vertical_offsets,
                                trigger_settings: &mut scope_trigger_settings,
                                offset_controls_enabled: capture_settings_enabled,
                            },
                        );
                    }
                    None => {}
                }
                self.combined_logic_ratio = combined_logic_ratio;
                self.logic_channel_colors = logic_channel_colors;
                self.logic_viewport = logic_viewport;
                self.scope_viewport = scope_viewport;
                for (channel, vertical_offset) in self
                    .scope_settings
                    .channels
                    .iter_mut()
                    .zip(scope_vertical_offsets)
                {
                    channel.vertical_offset = vertical_offset;
                }
                self.scope_settings.trigger = scope_trigger_settings;
            });

        let mut settings_open = self.settings_open;
        let mut close_settings = false;
        let settings_locked = self.is_busy();
        egui::Window::new("Settings")
            .open(&mut settings_open)
            .collapsible(false)
            .resizable(true)
            .default_size([560.0, 620.0])
            .min_width(480.0)
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(RichText::new("FPGA RESOURCES").strong().color(ACCENT));
                    ui.add_space(8.0);
                    ui.label("Resource directory");
                    ui.add_enabled(
                        !settings_locked,
                        egui::TextEdit::singleline(&mut self.resource_directory)
                            .desired_width(f32::INFINITY),
                    );
                    ui.small(
                        RichText::new("Contains DSLogicU3Pro16.bin and DSCopeU3P100.bin")
                            .color(TEXT_MUTED),
                    );
                    ui.add_space(10.0);

                    let resource_directory = Path::new(self.resource_directory.trim());
                    if has_required_fpga_resources(resource_directory) {
                        ui.label(RichText::new("✓ Required FPGA bitstreams found").color(ACCENT));
                    } else {
                        ui.label(
                            RichText::new(
                                "Required FPGA bitstreams were not found in this directory",
                            )
                            .color(Color32::LIGHT_RED),
                        );
                    }

                    ui.add_space(10.0);
                    if ui
                        .add_enabled(!settings_locked, egui::Button::new("Use bundled resources"))
                        .clicked()
                    {
                        self.resource_directory = default_resource_directory();
                    }

                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("INSTRUMENTS").strong().color(ACCENT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(!settings_locked, egui::Button::new("↻ Refresh"))
                                .clicked()
                            {
                                refresh_clicked = true;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    paint_instrument_settings(ui, &mut self.devices, !settings_locked);

                    if settings_locked {
                        ui.add_space(8.0);
                        ui.small(
                            RichText::new("Settings are locked while an operation is running")
                                .color(TEXT_MUTED),
                        );
                    }

                    ui.add_space(16.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close_settings = true;
                        }
                    });
                });
            });
        if close_settings {
            settings_open = false;
        }
        self.settings_open = settings_open;

        let mut auto_confirmation_open = self.auto_calibration_confirmation_open;
        let mut auto_confirmed = false;
        let mut auto_cancelled = false;
        egui::Window::new("DSCope Auto Calibration")
            .open(&mut auto_confirmation_open)
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label(RichText::new("Disconnect all probes before continuing.").strong());
                ui.add_space(6.0);
                ui.label(
                    "DSView will measure and adjust both channels across all input ranges, then write and verify the results in device NVM.",
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Do not disconnect the oscilloscope while calibration is running.")
                        .color(Color32::LIGHT_RED),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        auto_cancelled = true;
                    }
                    if ui.button("Start calibration").clicked() {
                        auto_confirmed = true;
                    }
                });
            });
        if auto_confirmed || auto_cancelled {
            auto_confirmation_open = false;
        }
        self.auto_calibration_confirmation_open = auto_confirmation_open;

        let mut save_manual_values = None;
        let mut close_manual = false;
        if let Some(editor) = self.manual_calibration.as_mut() {
            let mut manual_open = true;
            egui::Window::new("DSCope Manual Calibration")
                .open(&mut manual_open)
                .collapsible(false)
                .resizable(false)
                .default_width(430.0)
                .show(context, |ui| {
                    ui.label(
                        "Adjust the stored offset and VGA gain for one input range. Changes reach device NVM only after Save.",
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("Input range");
                        egui::ComboBox::from_id_salt("manual_calibration_range")
                            .selected_text(format!(
                                "{} mV/div",
                                DSCOPE_VOLTS_PER_DIV_MV[editor.range]
                            ))
                            .show_ui(ui, |ui| {
                                for (range, value) in
                                    DSCOPE_VOLTS_PER_DIV_MV.into_iter().enumerate()
                                {
                                    ui.selectable_value(
                                        &mut editor.range,
                                        range,
                                        format!("{value} mV/div"),
                                    );
                                }
                            });
                    });
                    ui.add_space(8.0);
                    for channel in 0..2 {
                        ui.group(|ui| {
                            ui.label(RichText::new(format!("CH{channel}")).strong());
                            let mut offset = editor
                                .values
                                .preoffset(channel, editor.range)
                                .expect("manual calibration selection is valid");
                            ui.horizontal(|ui| {
                                ui.label("Offset");
                                ui.add(egui::Slider::new(&mut offset, 0..=1_023));
                            });
                            let _ = editor.values.set_preoffset(channel, editor.range, offset);

                            let mut gain = editor
                                .values
                                .vga_gain(channel, editor.range)
                                .expect("manual calibration selection is valid");
                            ui.horizontal(|ui| {
                                ui.label("VGA gain");
                                ui.add(
                                    egui::DragValue::new(&mut gain)
                                        .range(1..=0x1fff)
                                        .speed(1),
                                );
                            });
                            let _ = editor.values.set_vga_gain(channel, editor.range, gain);
                        });
                        ui.add_space(6.0);
                    }
                    ui.label(
                        RichText::new(
                            "Incorrect calibration values can distort measurements. Save only values established with a known reference.",
                        )
                        .color(Color32::LIGHT_RED),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close_manual = true;
                        }
                        if ui.button("Save to device").clicked() {
                            save_manual_values = Some(editor.values);
                        }
                    });
                });
            if !manual_open {
                close_manual = true;
            }
        }
        if close_manual {
            self.manual_calibration = None;
        }

        if refresh_clicked {
            self.begin_refresh();
        }
        if start_clicked {
            self.begin_capture();
        }
        if stop_clicked {
            self.stop_capture();
        }
        match calibration_action {
            Some(ScopeCalibrationAction::Auto) => {
                self.auto_calibration_confirmation_open = true;
            }
            Some(ScopeCalibrationAction::Manual) => self.begin_manual_calibration(),
            None => {}
        }
        if auto_confirmed {
            self.begin_auto_calibration();
        }
        if let Some(values) = save_manual_values {
            self.begin_save_manual_calibration(values);
        }
        if self
            .scope_settings
            .frontend_differs_from(scope_settings_before_ui)
        {
            self.scope_configuration_due = Some(Instant::now() + SCOPE_CONFIGURATION_DEBOUNCE);
            self.status = "Adjusting DSCope configuration…".to_owned();
        }
        if !refresh_clicked
            && !start_clicked
            && !stop_clicked
            && !self.is_busy()
            && self
                .scope_configuration_due
                .is_some_and(|due| Instant::now() >= due)
        {
            self.begin_scope_configuration();
        }
        if self.pending.is_some() || self.capture.is_some() {
            context.request_repaint_after(Duration::from_millis(16));
        } else if let Some(due) = self.scope_configuration_due {
            context.request_repaint_after(due.saturating_duration_since(Instant::now()));
        }
    }
}

fn default_resource_directory() -> String {
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(executable_directory) = executable.parent() {
            #[cfg(target_os = "macos")]
            if executable_directory
                .file_name()
                .and_then(|name| name.to_str())
                == Some("MacOS")
            {
                if let Some(contents_directory) = executable_directory.parent() {
                    candidates.push(contents_directory.join("Resources/fpga"));
                }
            }
            candidates.push(executable_directory.join("resources/fpga"));
        }
    }
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("resources/fpga"),
    );
    candidates.push(PathBuf::from("resources/fpga"));
    let selected = candidates
        .into_iter()
        .find(|candidate| has_required_fpga_resources(candidate))
        .unwrap_or_else(|| PathBuf::from("resources/fpga"));
    selected
        .canonicalize()
        .unwrap_or(selected)
        .to_string_lossy()
        .into_owned()
}

fn has_required_fpga_resources(directory: &Path) -> bool {
    directory.join("DSLogicU3Pro16.bin").is_file() && directory.join("DSCopeU3P100.bin").is_file()
}

fn start_gui_recording(
    path: &Path,
    devices: &[DeviceDescriptor],
    scope_config: DsCopeCaptureConfig,
) -> Result<FileRecorder, String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to create recording {} without overwriting an existing file: {error}",
                path.display()
            )
        })?;
    let mut metadata = BTreeMap::new();
    metadata.insert("capture.command".to_owned(), "gui".to_owned());
    metadata.insert(
        "logic.sample_rate_hz".to_owned(),
        SAMPLE_RATE_HZ.to_string(),
    );
    metadata.insert("logic.sample_count".to_owned(), SAMPLE_COUNT.to_string());
    metadata.insert(
        "scope.sample_rate_hz".to_owned(),
        scope_config.sample_rate_hz().to_string(),
    );
    metadata.insert(
        "scope.sample_count".to_owned(),
        scope_config.sample_count().to_string(),
    );
    let trigger = scope_config.trigger();
    metadata.insert(
        "scope.trigger.position_percent".to_owned(),
        trigger.position_percent().to_string(),
    );
    metadata.insert(
        "scope.trigger.source".to_owned(),
        match trigger.source() {
            DsCopeTriggerSource::Auto => "auto",
            DsCopeTriggerSource::ChannelZero => "ch0",
            DsCopeTriggerSource::ChannelOne => "ch1",
            DsCopeTriggerSource::ChannelZeroAndOne => "ch0_and_ch1",
            DsCopeTriggerSource::ChannelZeroOrOne => "ch0_or_ch1",
        }
        .to_owned(),
    );
    metadata.insert(
        "scope.trigger.auto_channel".to_owned(),
        trigger.auto_channel().to_string(),
    );
    metadata.insert(
        "scope.trigger.slope".to_owned(),
        match trigger.slope() {
            DsCopeTriggerSlope::Rising => "rising",
            DsCopeTriggerSlope::Falling => "falling",
        }
        .to_owned(),
    );
    metadata.insert(
        "scope.trigger.holdoff_ns".to_owned(),
        trigger.holdoff_ns().to_string(),
    );
    metadata.insert(
        "scope.trigger.noise_sensitivity".to_owned(),
        trigger.noise_sensitivity().to_string(),
    );
    metadata.insert(
        "scope.trigger.level".to_owned(),
        trigger.level().to_string(),
    );
    for (channel, level) in trigger.levels().into_iter().enumerate() {
        metadata.insert(
            format!("scope.trigger.ch{channel}.level"),
            level.to_string(),
        );
    }
    for (index, channel) in scope_config.channels().iter().enumerate() {
        metadata.insert(
            format!("scope.ch{index}.enabled"),
            channel.enabled().to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.volts_per_div_mv"),
            channel.volts_per_div_mv().to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.coupling"),
            coupling_label(channel.coupling()).to_ascii_lowercase(),
        );
        metadata.insert(
            format!("scope.ch{index}.probe_attenuation"),
            channel.probe_attenuation().to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.vertical_offset"),
            channel.vertical_offset().to_string(),
        );
    }
    RecordingWriter::new(BufWriter::new(file), devices, metadata).map_err(|error| error.to_string())
}

fn finish_gui_recording(
    recorder: &mut Option<FileRecorder>,
    status: CompletionStatus,
) -> Result<(), String> {
    if let Some(recorder) = recorder {
        recorder.finish(status).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn append_error(error: &mut Option<String>, additional: String) {
    match error {
        Some(error) => {
            error.push_str("; ");
            error.push_str(&additional);
        }
        None => *error = Some(additional),
    }
}

fn start_capture(
    devices: Vec<DiscoveredDevice>,
    resource_directory: &Path,
    scope_config: DsCopeCaptureConfig,
) -> Result<CaptureSession, String> {
    let logic_config = DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)
        .map_err(|error| error.to_string())?;
    let mut sources = Vec::<Box<dyn CaptureSource>>::with_capacity(devices.len());
    for device in devices {
        if device.link_status() == LinkStatus::Unsupported {
            return Err(format!(
                "{} is connected at an unsupported speed",
                device.profile.display_name
            ));
        }
        let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
        prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
        match device.profile.model {
            DeviceModel::DsLogicU3Pro16 => sources.push(Box::new(
                DsLogicCaptureSource::new(device, logic_config)
                    .map_err(|error| error.to_string())?,
            )),
            DeviceModel::DsCopeU3P100 => sources.push(Box::new(
                DsCopeCaptureSource::new(device, scope_config)
                    .map_err(|error| error.to_string())?,
            )),
        }
    }
    CaptureCoordinator::start(sources).map_err(|error| error.to_string())
}

fn configure_style(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BACKGROUND;
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    visuals.widgets.active.bg_fill = ACCENT.gamma_multiply(0.6);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(45, 61, 77);
    context.set_visuals(visuals);
    context.style_mut(|style| {
        style.interaction.tooltip_delay = TOOLTIP_DELAY_SECONDS;
    });
}

const WAVEFORM_CARD_CHROME_HEIGHT: f32 = 58.0;
const COMBINED_DIVIDER_HEIGHT: f32 = 10.0;
const MIN_LOGIC_PANE_HEIGHT: f32 = 260.0;
const MIN_SCOPE_PANE_HEIGHT: f32 = 165.0;

struct ScopeWaveformOptions<'a> {
    settings: ScopeCaptureSettings,
    channel_colors: [Color32; 2],
    vertical_offsets: &'a mut [u16; 2],
    trigger_settings: &'a mut ScopeTriggerSettings,
    offset_controls_enabled: bool,
}

fn waveform_card_fill(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    paint: impl FnOnce(&egui::Ui, Rect),
) {
    let plot_height = (ui.available_height() - WAVEFORM_CARD_CHROME_HEIGHT).max(64.0);
    waveform_card(ui, title, subtitle, plot_height, paint);
}

fn combined_pane_heights(total_height: f32, logic_ratio: f32) -> (f32, f32) {
    let usable_height = (total_height - COMBINED_DIVIDER_HEIGHT).max(0.0);
    if usable_height == 0.0 {
        return (0.0, 0.0);
    }
    let minimum_total = MIN_LOGIC_PANE_HEIGHT + MIN_SCOPE_PANE_HEIGHT;
    let minimum_scale = (usable_height / minimum_total).min(1.0);
    let minimum_logic_height = MIN_LOGIC_PANE_HEIGHT * minimum_scale;
    let minimum_scope_height = MIN_SCOPE_PANE_HEIGHT * minimum_scale;
    let logic_height = (usable_height * logic_ratio.clamp(0.0, 1.0))
        .clamp(minimum_logic_height, usable_height - minimum_scope_height);
    (logic_height, usable_height - logic_height)
}

fn combined_waveform_cards(
    ui: &mut egui::Ui,
    waveforms: &WaveformStore,
    logic_ratio: &mut f32,
    logic_channel_colors: &mut [Color32; LOGIC_CHANNEL_COUNT],
    logic_viewport: &mut WaveformViewport,
    scope_viewport: &mut WaveformViewport,
    scope_options: ScopeWaveformOptions<'_>,
) {
    let available = ui.available_rect_before_wrap();
    let (logic_height, _) = combined_pane_heights(available.height(), *logic_ratio);
    let divider_y = available.top() + logic_height;
    let divider_hit_rect = Rect::from_min_max(
        Pos2::new(available.left(), divider_y),
        Pos2::new(
            available.right(),
            (divider_y + COMBINED_DIVIDER_HEIGHT).min(available.bottom()),
        ),
    );
    let divider_response = ui
        .interact(
            divider_hit_rect,
            ui.make_persistent_id("combined_waveform_divider"),
            Sense::drag(),
        )
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if divider_response.dragged() {
        if let Some(pointer) = divider_response.interact_pointer_pos() {
            let usable_height = (available.height() - COMBINED_DIVIDER_HEIGHT).max(1.0);
            let minimum_total = MIN_LOGIC_PANE_HEIGHT + MIN_SCOPE_PANE_HEIGHT;
            let minimum_scale = (usable_height / minimum_total).min(1.0);
            let minimum_logic_ratio = MIN_LOGIC_PANE_HEIGHT * minimum_scale / usable_height;
            let minimum_scope_ratio = MIN_SCOPE_PANE_HEIGHT * minimum_scale / usable_height;
            *logic_ratio = ((pointer.y - available.top()) / usable_height)
                .clamp(minimum_logic_ratio, 1.0 - minimum_scope_ratio);
        }
    }

    let (logic_height, scope_height) = combined_pane_heights(available.height(), *logic_ratio);
    let logic_rect = Rect::from_min_size(available.min, Vec2::new(available.width(), logic_height));
    let scope_rect = Rect::from_min_size(
        Pos2::new(
            available.left(),
            logic_rect.bottom() + COMBINED_DIVIDER_HEIGHT,
        ),
        Vec2::new(available.width(), scope_height),
    );
    ui.advance_cursor_after_rect(available);

    let mut logic_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("combined_logic_waveform")
            .max_rect(logic_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    waveform_card_fill(
        &mut logic_ui,
        "DIGITAL",
        "DSLogic · channels 0–15",
        |ui, rect| paint_logic(ui, rect, waveforms, logic_channel_colors, logic_viewport),
    );

    let mut scope_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("combined_scope_waveform")
            .max_rect(scope_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    waveform_card_fill(
        &mut scope_ui,
        "OSCILLOSCOPE",
        "DSCope · CH0 / CH1",
        |ui, rect| paint_scope(ui, rect, waveforms, scope_viewport, scope_options),
    );

    let divider_color = if divider_response.hovered() || divider_response.dragged() {
        ACCENT
    } else {
        GRID
    };
    let divider_center = logic_rect.bottom() + COMBINED_DIVIDER_HEIGHT * 0.5;
    ui.painter().line_segment(
        [
            Pos2::new(available.left(), divider_center),
            Pos2::new(available.right(), divider_center),
        ],
        Stroke::new(2.0_f32, divider_color),
    );
}

fn waveform_card(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    height: f32,
    paint: impl FnOnce(&egui::Ui, Rect),
) {
    egui::Frame::none()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, GRID))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).strong().color(ACCENT));
                ui.label(RichText::new(subtitle).color(TEXT_MUTED));
            });
            ui.add_space(8.0);
            let size = Vec2::new(ui.available_width(), height);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            paint(ui, rect);
        });
}

fn paint_plot_background(ui: &egui::Ui, rect: Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, BACKGROUND);
}

fn paint_plot_grid(ui: &egui::Ui, rect: Rect) {
    let painter = ui.painter();
    for index in 0..=10 {
        let x = egui::lerp(rect.x_range(), index as f32 / 10.0);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0_f32, GRID.gamma_multiply(0.55)),
        );
    }
    for index in 0..=4 {
        let y = egui::lerp(rect.y_range(), index as f32 / 4.0);
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0_f32, GRID.gamma_multiply(0.55)),
        );
    }
}

fn split_waveform_chrome(rect: Rect) -> (Rect, Rect, Rect) {
    let status_bottom = (rect.top() + WAVEFORM_STATUS_HEIGHT).min(rect.bottom());
    let ruler_bottom = (status_bottom + TIME_RULER_HEIGHT).min(rect.bottom());
    (
        Rect::from_min_max(rect.left_top(), Pos2::new(rect.right(), status_bottom)),
        Rect::from_min_max(
            Pos2::new(rect.left(), status_bottom),
            Pos2::new(rect.right(), ruler_bottom),
        ),
        Rect::from_min_max(Pos2::new(rect.left(), ruler_bottom), rect.right_bottom()),
    )
}

fn time_scale_unit(maximum_seconds: f64) -> (&'static str, f64) {
    if maximum_seconds >= 1.0 {
        ("s", 1.0)
    } else if maximum_seconds >= 1.0e-3 {
        ("ms", 1.0e3)
    } else if maximum_seconds >= 1.0e-6 {
        ("µs", 1.0e6)
    } else {
        ("ns", 1.0e9)
    }
}

fn time_scale_precision(label_step: f64) -> usize {
    if !label_step.is_finite() || label_step <= 0.0 {
        return 3;
    }
    if label_step >= 10.0 {
        0
    } else if label_step >= 1.0 {
        1
    } else {
        ((-label_step.log10().floor()) as usize + 1).min(9)
    }
}

fn time_scale_sample_window(
    sample_count: usize,
    sample_offset: u64,
    viewport: WaveformViewport,
) -> Option<(f64, f64)> {
    if sample_count == 0 {
        return None;
    }
    let sample_extent = sample_count.saturating_sub(1).max(1) as f64;
    let first_sample = sample_offset as f64 + viewport.start as f64 * sample_extent;
    let last_sample =
        sample_offset as f64 + (viewport.start + viewport.span) as f64 * sample_extent;
    Some((first_sample, last_sample))
}

fn paint_time_scale(
    painter: &egui::Painter,
    rect: Rect,
    first_sample: f64,
    last_sample: f64,
    sample_rate_hz: u64,
) {
    painter.rect_filled(rect, 4.0, PANEL.gamma_multiply(0.72));
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0_f32, GRID),
    );
    if sample_rate_hz == 0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "TIME",
            FontId::monospace(9.0),
            TEXT_MUTED,
        );
        return;
    }

    const MAJOR_INTERVALS: usize = 10;
    const SUBDIVISIONS_PER_INTERVAL: usize = 5;
    let maximum_seconds = first_sample.abs().max(last_sample.abs()) / sample_rate_hz as f64;
    let (unit, multiplier) = time_scale_unit(maximum_seconds);
    let label_stride = if rect.width() >= 650.0 { 2 } else { 5 };
    let label_step = (last_sample - first_sample) / MAJOR_INTERVALS as f64 * label_stride as f64
        / sample_rate_hz as f64
        * multiplier;
    let precision = time_scale_precision(label_step);
    let total_subdivisions = MAJOR_INTERVALS * SUBDIVISIONS_PER_INTERVAL;

    for index in 0..=total_subdivisions {
        let fraction = index as f32 / total_subdivisions as f32;
        let x = egui::lerp(rect.x_range(), fraction);
        let is_major = index % SUBDIVISIONS_PER_INTERVAL == 0;
        let major_index = index / SUBDIVISIONS_PER_INTERVAL;
        let is_labelled = is_major && major_index % label_stride == 0;
        let tick_height = if is_labelled {
            7.0
        } else if is_major {
            5.0
        } else {
            2.5
        };
        painter.line_segment(
            [
                Pos2::new(x, rect.bottom() - tick_height),
                Pos2::new(x, rect.bottom()),
            ],
            Stroke::new(
                if is_major { 1.0_f32 } else { 0.7_f32 },
                if is_major {
                    TEXT_MUTED
                } else {
                    TEXT_MUTED.gamma_multiply(0.55)
                },
            ),
        );

        if is_labelled {
            let sample = first_sample + (last_sample - first_sample) * fraction as f64;
            let seconds = sample / sample_rate_hz as f64;
            let alignment = if index == 0 {
                egui::Align2::LEFT_TOP
            } else if index == total_subdivisions {
                egui::Align2::RIGHT_TOP
            } else {
                egui::Align2::CENTER_TOP
            };
            painter.text(
                Pos2::new(x, rect.top() + 2.0),
                alignment,
                format!("{:.*} {unit}", precision, seconds * multiplier),
                FontId::monospace(9.0),
                TEXT_MUTED,
            );
        }
    }
}

fn paint_waveform_time_scale(
    painter: &egui::Painter,
    rect: Rect,
    sample_count: usize,
    sample_rate_hz: u64,
    sample_offset: u64,
    viewport: WaveformViewport,
) {
    let Some((first_sample, last_sample)) =
        time_scale_sample_window(sample_count, sample_offset, viewport)
    else {
        paint_time_scale(painter, rect, 0.0, 0.0, 0);
        return;
    };
    paint_time_scale(painter, rect, first_sample, last_sample, sample_rate_hz);
}

fn update_waveform_viewport(
    ui: &egui::Ui,
    rect: Rect,
    id_salt: &'static str,
    viewport: &mut WaveformViewport,
    sample_count: usize,
    sample_rate_hz: u64,
    zoom_anchor: Option<(f32, f32)>,
) {
    let response = ui
        .interact(rect, ui.make_persistent_id(id_salt), Sense::click())
        .on_hover_cursor(egui::CursorIcon::ZoomIn)
        .on_hover_text("Scroll to step timebase · double-click to reset");
    if response.double_clicked() {
        viewport.reset();
    }
    if response.hovered() {
        let scroll_delta = ui.input(|input| input.raw_scroll_delta.y);
        if scroll_delta != 0.0 {
            if let Some((sample_fraction, screen_fraction)) = zoom_anchor {
                viewport.step_timebase_at_sample(
                    scroll_delta,
                    sample_count,
                    sample_rate_hz,
                    sample_fraction,
                    screen_fraction,
                );
            } else {
                let pointer_fraction = response
                    .hover_pos()
                    .map(|pointer| (pointer.x - rect.left()) / rect.width().max(1.0))
                    .unwrap_or(0.5);
                viewport.step_timebase(
                    scroll_delta,
                    sample_count,
                    sample_rate_hz,
                    pointer_fraction,
                );
            }
            ui.ctx().request_repaint();
        }
    }
}

fn paint_waveform_status(
    painter: &egui::Painter,
    rect: Rect,
    total_sample_count: usize,
    sample_rate_hz: u64,
    viewport: WaveformViewport,
) {
    painter.rect_filled(rect, 4.0, PANEL.gamma_multiply(0.9));
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0_f32, GRID),
    );
    if let Some(time_per_division) = viewport.time_per_division(total_sample_count, sample_rate_hz)
    {
        let per_division = Duration::from_secs_f64(time_per_division);
        let span = Duration::from_secs_f64(time_per_division * SCOPE_HORIZONTAL_DIVISIONS as f64);
        painter.text(
            rect.left_center() + Vec2::new(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            format!(
                "SPAN {}  ·  {}/div",
                format_duration(span),
                format_duration(per_division)
            ),
            FontId::monospace(10.0),
            TEXT_MUTED,
        );
    }
    let zoom_factor = viewport.zoom_factor();
    let zoom_label = if zoom_factor >= 1.0 {
        format!("ZOOM {zoom_factor:.1}×")
    } else {
        format!("ZOOM {zoom_factor:.2}×")
    };
    painter.text(
        rect.right_center() + Vec2::new(-6.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        zoom_label,
        FontId::monospace(10.0),
        ACCENT,
    );
}

fn paint_logic_zero_reference(painter: &egui::Painter, plot: Rect, y: f32, channel_color: Color32) {
    const DOT_SPACING: f32 = 7.0;
    let mut x = plot.left();
    while x <= plot.right() {
        painter.circle_filled(Pos2::new(x, y), 0.7, channel_color.gamma_multiply(0.3));
        x += DOT_SPACING;
    }
}

fn paint_logic(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    channel_colors: &mut [Color32; LOGIC_CHANNEL_COUNT],
    viewport: &mut WaveformViewport,
) {
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    let label_width = 42.0;
    let waveform_area = Rect::from_min_max(
        Pos2::new(rect.left() + label_width, rect.top()),
        rect.right_bottom(),
    );
    let (status_bar, time_ruler, plot) = split_waveform_chrome(waveform_area);
    paint_plot_grid(ui, plot);
    let sample_count = waveforms.logic_samples.len();
    let display_sample_count = if sample_count == 0 {
        SAMPLE_COUNT as usize
    } else {
        sample_count
    };
    let display_sample_rate_hz = if waveforms.logic_rate_hz == 0 {
        SAMPLE_RATE_HZ
    } else {
        waveforms.logic_rate_hz
    };
    update_waveform_viewport(
        ui,
        waveform_area,
        "logic_waveform_zoom",
        viewport,
        display_sample_count,
        display_sample_rate_hz,
        None,
    );
    let lane_height = plot.height() / LOGIC_CHANNEL_COUNT as f32;
    let visible_range = viewport.sample_range(sample_count);
    let visible_count = visible_range.len();
    let step = (visible_count as f32 / plot.width().max(1.0))
        .floor()
        .max(1.0) as usize;
    for (channel, channel_color) in channel_colors.iter_mut().enumerate() {
        let lane_top = plot.top() + channel as f32 * lane_height;
        let high = lane_top + lane_height * 0.25;
        let low = lane_top + lane_height * 0.75;
        painter.line_segment(
            [
                Pos2::new(plot.left(), lane_top),
                Pos2::new(plot.right(), lane_top),
            ],
            Stroke::new(0.6_f32, GRID.gamma_multiply(0.45)),
        );
        paint_logic_zero_reference(painter, plot, low, *channel_color);
        let label_rect = Rect::from_min_max(
            Pos2::new(rect.left(), lane_top),
            Pos2::new(plot.left(), lane_top + lane_height),
        );
        let label_response = ui
            .interact(
                label_rect,
                ui.make_persistent_id(("logic_channel_label", channel)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!("Click to change D{channel} color"));
        if label_response.hovered() {
            painter.rect_filled(
                label_rect.shrink(1.0),
                3.0,
                channel_color.gamma_multiply(0.18),
            );
        }
        painter.text(
            Pos2::new(rect.left() + 4.0, lane_top + lane_height * 0.5),
            egui::Align2::LEFT_CENTER,
            format!("D{channel}"),
            FontId::monospace(10.0),
            *channel_color,
        );

        let popup_id = ui.make_persistent_id(("logic_channel_color_popup", channel));
        if label_response.clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &label_response,
            egui::popup::PopupCloseBehavior::CloseOnClickOutside,
            |popup_ui| {
                popup_ui.set_min_width(280.0);
                popup_ui.label(RichText::new(format!("D{channel} color")).strong());
                egui::color_picker::color_picker_color32(
                    popup_ui,
                    channel_color,
                    egui::color_picker::Alpha::Opaque,
                );
                if popup_ui.button("Reset color").clicked() {
                    *channel_color = default_logic_channel_colors()[channel];
                }
            },
        );

        if visible_count == 0 {
            continue;
        }
        let mut points = Vec::new();
        let mut previous_y = None;
        for index in visible_range.clone().step_by(step) {
            let sample = waveforms.logic_samples[index];
            let y = if sample & (1_u32 << channel) != 0 {
                high
            } else {
                low
            };
            let x = waveform_sample_x(plot, *viewport, index as f32, sample_count);
            if let Some(previous_y) = previous_y {
                points.push(Pos2::new(x, previous_y));
            }
            points.push(Pos2::new(x, y));
            previous_y = Some(y);
        }
        if points.len() > 1 {
            painter.add(egui::Shape::line(
                points,
                Stroke::new(1.2_f32, *channel_color),
            ));
        }
    }
    if visible_count == 0 {
        paint_empty(painter, plot, "Waiting for logic samples");
    }
    paint_waveform_time_scale(
        painter,
        time_ruler,
        display_sample_count,
        display_sample_rate_hz,
        waveforms.logic_sample_offset,
        *viewport,
    );
    paint_waveform_status(
        painter,
        status_bar,
        display_sample_count,
        display_sample_rate_hz,
        *viewport,
    );
}

fn format_voltage(value: f32) -> String {
    if value.abs() >= 1.0 {
        format!("{value:.2} V")
    } else if value.abs() >= 0.001 {
        format!("{:.0} mV", value * 1_000.0)
    } else {
        format!("{:.0} µV", value * 1_000_000.0)
    }
}

fn format_trigger_voltage(value: f32) -> String {
    if value.abs() >= 1.0 {
        format!("{value:.2} V")
    } else if value.abs() >= 0.1 {
        format!("{:.0} mV", value * 1_000.0)
    } else {
        format!("{:.2} mV", value * 1_000.0)
    }
}

fn format_sample_rate(rate_hz: u64) -> String {
    if rate_hz >= 1_000_000_000 {
        format!("{:.2} GSa/s", rate_hz as f64 / 1_000_000_000.0)
    } else if rate_hz >= 1_000_000 {
        format!("{:.2} MSa/s", rate_hz as f64 / 1_000_000.0)
    } else if rate_hz >= 1_000 {
        format!("{:.2} kSa/s", rate_hz as f64 / 1_000.0)
    } else {
        format!("{rate_hz} Sa/s")
    }
}

fn paint_scope_graticule(painter: &egui::Painter, rect: Rect) {
    for index in 0..=SCOPE_HORIZONTAL_DIVISIONS {
        let x = egui::lerp(
            rect.x_range(),
            index as f32 / SCOPE_HORIZONTAL_DIVISIONS as f32,
        );
        let center = index == SCOPE_HORIZONTAL_DIVISIONS / 2;
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(
                if center { 1.2_f32 } else { 0.8_f32 },
                if center {
                    TEXT_MUTED.gamma_multiply(0.45)
                } else {
                    GRID.gamma_multiply(0.8)
                },
            ),
        );
    }
    for index in 0..=SCOPE_VERTICAL_DIVISIONS {
        let y = egui::lerp(
            rect.y_range(),
            index as f32 / SCOPE_VERTICAL_DIVISIONS as f32,
        );
        let center = index == SCOPE_VERTICAL_DIVISIONS / 2;
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(
                if center { 1.2_f32 } else { 0.8_f32 },
                if center {
                    TEXT_MUTED.gamma_multiply(0.45)
                } else {
                    GRID.gamma_multiply(0.8)
                },
            ),
        );
    }

    let center = rect.center();
    for index in 0..=SCOPE_HORIZONTAL_DIVISIONS * 5 {
        let x = egui::lerp(
            rect.x_range(),
            index as f32 / (SCOPE_HORIZONTAL_DIVISIONS * 5) as f32,
        );
        painter.circle_filled(Pos2::new(x, center.y), 0.8, TEXT_MUTED.gamma_multiply(0.65));
    }
    for index in 0..=SCOPE_VERTICAL_DIVISIONS * 5 {
        let y = egui::lerp(
            rect.y_range(),
            index as f32 / (SCOPE_VERTICAL_DIVISIONS * 5) as f32,
        );
        painter.circle_filled(Pos2::new(center.x, y), 0.8, TEXT_MUTED.gamma_multiply(0.65));
    }
}

fn scope_vertical_offset_ratio(vertical_offset: u16) -> f32 {
    let span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    f32::from(
        vertical_offset.clamp(DSCOPE_VERTICAL_OFFSET_MIN, DSCOPE_VERTICAL_OFFSET_MAX)
            - DSCOPE_VERTICAL_OFFSET_MIN,
    ) / span
}

fn scope_vertical_offset_y(rect: Rect, vertical_offset: u16) -> f32 {
    egui::lerp(rect.y_range(), scope_vertical_offset_ratio(vertical_offset))
}

fn scope_vertical_offset_from_y(rect: Rect, y: f32) -> u16 {
    let ratio = ((y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
    let span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    (f32::from(DSCOPE_VERTICAL_OFFSET_MIN) + ratio * span).round() as u16
}

fn scope_sample_y(plot: Rect, channel: ScopeChannelSettings, voltage: f32) -> f32 {
    let volts_per_div =
        f32::from(channel.volts_per_div_mv) / 1_000.0 * f32::from(channel.probe_attenuation);
    let voltage_span = volts_per_div * SCOPE_VERTICAL_DIVISIONS as f32;
    scope_vertical_offset_y(plot, channel.vertical_offset) - voltage / voltage_span * plot.height()
}

fn scope_vertical_offset_divisions(rect: Rect, vertical_offset: u16) -> f32 {
    let center_y = scope_vertical_offset_y(rect, DSCOPE_VERTICAL_OFFSET_CENTER);
    (center_y - scope_vertical_offset_y(rect, vertical_offset)) / rect.height().max(1.0)
        * SCOPE_VERTICAL_DIVISIONS as f32
}

fn scope_offset_arrow_at_pointer(
    plot: Rect,
    pointer_y: f32,
    settings: ScopeCaptureSettings,
    vertical_offsets: &[u16; 2],
) -> Option<usize> {
    const HIT_RADIUS: f32 = 18.0;
    let mut closest: Option<(usize, f32)> = None;
    for (channel, vertical_offset) in vertical_offsets.iter().copied().enumerate() {
        if !settings.channels[channel].enabled {
            continue;
        }
        let distance = (pointer_y - scope_vertical_offset_y(plot, vertical_offset)).abs();
        let is_closer = match closest {
            Some((_, closest_distance)) => distance < closest_distance,
            None => true,
        };
        if is_closer {
            closest = Some((channel, distance));
        }
    }
    closest
        .filter(|(_, distance)| *distance <= HIT_RADIUS)
        .map(|(channel, _)| channel)
}

fn paint_scope_offset_arrows(
    ui: &egui::Ui,
    plot: Rect,
    settings: ScopeCaptureSettings,
    channel_colors: [Color32; 2],
    vertical_offsets: &mut [u16; 2],
    trigger_settings: &mut ScopeTriggerSettings,
    controls_enabled: bool,
) {
    const ARROW_BODY_WIDTH: f32 = 15.0;
    const ARROW_TIP_WIDTH: f32 = 8.0;
    const ARROW_HALF_HEIGHT: f32 = 9.0;
    const DOT_SPACING: f32 = 7.0;
    let painter = ui.painter().with_clip_rect(plot);
    let arrow_x = plot.left() + 2.0;
    let arrow_width = ARROW_BODY_WIDTH + ARROW_TIP_WIDTH;
    let interaction_rect = Rect::from_min_max(
        plot.left_top(),
        Pos2::new(
            (plot.left() + arrow_width + 4.0).min(plot.right()),
            plot.bottom(),
        ),
    );
    let interaction_id = ui.make_persistent_id("scope_vertical_offset_arrows");
    let response = ui.interact(
        interaction_rect,
        interaction_id,
        if controls_enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let hovered_channel = response.hover_pos().and_then(|pointer| {
        scope_offset_arrow_at_pointer(plot, pointer.y, settings, vertical_offsets)
    });
    let response = if let Some(channel) = hovered_channel {
        response
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_text(format!(
                "CH{channel} vertical offset: {:+.2} div\nDrag to move · double-click to center",
                scope_vertical_offset_divisions(plot, vertical_offsets[channel])
            ))
    } else {
        response
    };

    if controls_enabled {
        let previous_offsets = *vertical_offsets;
        if response.drag_started() {
            let channel = response.interact_pointer_pos().and_then(|pointer| {
                scope_offset_arrow_at_pointer(plot, pointer.y, settings, vertical_offsets)
            });
            ui.data_mut(|data| {
                data.remove_temp::<usize>(interaction_id);
                if let Some(channel) = channel {
                    data.insert_temp(interaction_id, channel);
                }
            });
        }
        if response.double_clicked() {
            if let Some(channel) = response.interact_pointer_pos().and_then(|pointer| {
                scope_offset_arrow_at_pointer(plot, pointer.y, settings, vertical_offsets)
            }) {
                vertical_offsets[channel] = DSCOPE_VERTICAL_OFFSET_CENTER;
            }
        } else if response.dragged() {
            let channel = ui.data(|data| data.get_temp::<usize>(interaction_id));
            if let (Some(channel), Some(pointer)) = (channel, response.interact_pointer_pos()) {
                vertical_offsets[channel] = scope_vertical_offset_from_y(plot, pointer.y);
            }
        }
        if response.drag_stopped() {
            ui.data_mut(|data| data.remove_temp::<usize>(interaction_id));
        }
        for channel in 0..vertical_offsets.len() {
            if vertical_offsets[channel] != previous_offsets[channel] {
                trigger_settings.levels[channel] = shift_scope_trigger_level(
                    trigger_settings.levels[channel],
                    previous_offsets[channel],
                    vertical_offsets[channel],
                );
            }
        }
    }

    for (channel, color) in channel_colors.iter().enumerate() {
        if !settings.channels[channel].enabled {
            continue;
        }
        let arrow_y = scope_vertical_offset_y(plot, vertical_offsets[channel]);
        let mut x = plot.left();
        while x <= plot.right() {
            painter.circle_filled(Pos2::new(x, arrow_y), 0.8, color.gamma_multiply(0.35));
            x += DOT_SPACING;
        }
    }

    for channel in (0..channel_colors.len()).rev() {
        if !settings.channels[channel].enabled {
            continue;
        }
        let display_color = channel_colors[channel];
        let arrow_y = scope_vertical_offset_y(plot, vertical_offsets[channel]);
        let body_right = arrow_x + ARROW_BODY_WIDTH;
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(arrow_x, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_right, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_right + ARROW_TIP_WIDTH, arrow_y),
                Pos2::new(body_right, arrow_y + ARROW_HALF_HEIGHT),
                Pos2::new(arrow_x, arrow_y + ARROW_HALF_HEIGHT),
            ],
            display_color,
            Stroke::new(1.0_f32, BACKGROUND),
        ));
        painter.text(
            Pos2::new(arrow_x + ARROW_BODY_WIDTH * 0.5, arrow_y),
            egui::Align2::CENTER_CENTER,
            channel.to_string(),
            FontId::monospace(10.0),
            marker_text_color(display_color),
        );
    }
}

fn marker_text_color(marker_color: Color32) -> Color32 {
    let luminance = (299 * u32::from(marker_color.r())
        + 587 * u32::from(marker_color.g())
        + 114 * u32::from(marker_color.b()))
        / 1_000;
    if luminance >= 140 {
        BACKGROUND
    } else {
        Color32::WHITE
    }
}

fn scope_trigger_level_y(plot: Rect, level: u8) -> f32 {
    let reference_span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    let ratio = f32::from(u16::from(level) - DSCOPE_VERTICAL_OFFSET_MIN) / reference_span;
    egui::lerp(plot.y_range(), ratio)
}

fn shift_scope_trigger_level(level: u8, old_offset: u16, new_offset: u16) -> u8 {
    (i32::from(level) + i32::from(new_offset) - i32::from(old_offset)).clamp(
        i32::from(DSCOPE_TRIGGER_LEVEL_MIN),
        i32::from(DSCOPE_TRIGGER_LEVEL_MAX),
    ) as u8
}

fn scope_trigger_level_from_y(plot: Rect, y: f32) -> u8 {
    let ratio = ((y - plot.top()) / plot.height().max(1.0)).clamp(0.0, 1.0);
    let reference_span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    let level = (f32::from(DSCOPE_VERTICAL_OFFSET_MIN) + ratio * reference_span).round() as u8;
    level.clamp(DSCOPE_TRIGGER_LEVEL_MIN, DSCOPE_TRIGGER_LEVEL_MAX)
}

fn scope_trigger_channels(settings: ScopeTriggerSettings) -> &'static [usize] {
    match settings.source {
        DsCopeTriggerSource::Auto if settings.auto_channel == 0 => &[0],
        DsCopeTriggerSource::Auto => &[1],
        DsCopeTriggerSource::ChannelZero => &[0],
        DsCopeTriggerSource::ChannelOne => &[1],
        DsCopeTriggerSource::ChannelZeroAndOne | DsCopeTriggerSource::ChannelZeroOrOne => &[0, 1],
    }
}

fn scope_trigger_threshold_voltage(channel: ScopeChannelSettings, trigger_level: u8) -> f32 {
    let trigger_ratio = f32::from(u16::from(trigger_level) - DSCOPE_VERTICAL_OFFSET_MIN)
        / f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    let zero_ratio = scope_vertical_offset_ratio(channel.vertical_offset);
    let voltage_span = f32::from(channel.volts_per_div_mv) / 1_000.0
        * SCOPE_VERTICAL_DIVISIONS as f32
        * f32::from(channel.probe_attenuation);
    (zero_ratio - trigger_ratio) * voltage_span
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeTriggerCrossing {
    sample: f32,
    channel_index: usize,
}

fn scope_trigger_crossing_sample(
    waveforms: &WaveformStore,
    settings: ScopeCaptureSettings,
    trigger: ScopeTriggerSettings,
) -> Option<ScopeTriggerCrossing> {
    const TRIGGER_SEARCH_SAMPLES: usize = 2;
    let trigger_sample = waveforms.scope_trigger_sample_index?;
    let local_trigger =
        usize::try_from(trigger_sample.checked_sub(waveforms.scope_sample_offset)?).ok()?;
    let mut closest_crossing = None::<ScopeTriggerCrossing>;

    for &channel_index in scope_trigger_channels(trigger) {
        let channel_settings = settings.channels[channel_index];
        if !channel_settings.enabled {
            continue;
        }
        let samples = waveforms.scope_channels.get(channel_index)?;
        let threshold =
            scope_trigger_threshold_voltage(channel_settings, trigger.levels[channel_index]);
        for distance in 0..TRIGGER_SEARCH_SAMPLES {
            let right = local_trigger.checked_sub(distance)?;
            let left = right.checked_sub(1)?;
            let (&left_value, &right_value) = (samples.get(left)?, samples.get(right)?);
            let crosses = match trigger.slope {
                DsCopeTriggerSlope::Rising => left_value <= threshold && right_value >= threshold,
                DsCopeTriggerSlope::Falling => left_value >= threshold && right_value <= threshold,
            };
            if !crosses {
                continue;
            }
            let delta = right_value - left_value;
            let interpolation = if delta.abs() <= f32::EPSILON {
                0.0
            } else {
                ((threshold - left_value) / delta).clamp(0.0, 1.0)
            };
            let crossing = waveforms.scope_sample_offset as f32 + left as f32 + interpolation;
            let crossing = ScopeTriggerCrossing {
                sample: crossing,
                channel_index,
            };
            if closest_crossing.map_or(true, |current| crossing.sample > current.sample) {
                closest_crossing = Some(crossing);
            }
            break;
        }
    }
    closest_crossing
}

fn scope_sample_fraction(sample: f32, sample_offset: u64, sample_count: usize) -> f32 {
    let local_sample = sample - sample_offset as f32;
    local_sample / sample_count.saturating_sub(1).max(1) as f32
}

fn waveform_sample_x(
    plot: Rect,
    viewport: WaveformViewport,
    sample_index: f32,
    sample_count: usize,
) -> f32 {
    let sample_fraction = sample_index / sample_count.saturating_sub(1).max(1) as f32;
    let screen_fraction = (sample_fraction - viewport.start) / viewport.span.max(f32::EPSILON);
    plot.left() + screen_fraction * plot.width()
}

fn scope_plot_sample_indices(
    visible_range: Range<usize>,
    step: usize,
    channel_index: usize,
    trigger_crossing: Option<ScopeTriggerCrossing>,
    sample_offset: u64,
) -> Vec<usize> {
    let mut indices = visible_range
        .clone()
        .step_by(step.max(1))
        .collect::<Vec<_>>();
    if let Some(last) = visible_range.end.checked_sub(1) {
        indices.push(last);
    }
    if let Some(crossing) = trigger_crossing.filter(|crossing| {
        crossing.channel_index == channel_index && crossing.sample >= sample_offset as f32
    }) {
        let local_crossing = crossing.sample - sample_offset as f32;
        for index in [
            local_crossing.floor() as usize,
            local_crossing.ceil() as usize,
        ] {
            if visible_range.contains(&index) {
                indices.push(index);
            }
        }
    }
    indices.sort_unstable();
    indices.dedup();
    indices
}

fn scope_trigger_position_x(plot: Rect, position_percent: u8) -> f32 {
    egui::lerp(plot.x_range(), f32::from(position_percent) / 100.0)
}

fn scope_trigger_position_from_x(plot: Rect, x: f32) -> u8 {
    let ratio = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    (ratio * 100.0).round().clamp(
        f32::from(DSCOPE_TRIGGER_POSITION_MIN_PERCENT),
        f32::from(DSCOPE_TRIGGER_POSITION_MAX_PERCENT),
    ) as u8
}

fn paint_scope_trigger_position(
    ui: &egui::Ui,
    time_ruler: Rect,
    plot: Rect,
    settings: &mut ScopeTriggerSettings,
    controls_enabled: bool,
    viewport: &mut WaveformViewport,
) {
    const COLOR: Color32 = Color32::from_rgb(235, 72, 72);
    const ARROW_HALF_WIDTH: f32 = 10.0;
    const ARROW_HEIGHT: f32 = 19.0;
    const ARROW_SHOULDER_HEIGHT: f32 = 9.0;
    const DOT_SPACING: f32 = 7.0;
    let x = scope_trigger_position_x(plot, settings.position_percent);
    let arrow_top = plot.top() - ARROW_HEIGHT;
    let marker_rect = Rect::from_min_max(
        Pos2::new(x - ARROW_HALF_WIDTH - 4.0, arrow_top - 3.0),
        Pos2::new(x + ARROW_HALF_WIDTH + 4.0, plot.top() + 8.0),
    );
    let response = ui.interact(
        marker_rect,
        ui.make_persistent_id("scope_trigger_position"),
        if controls_enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let response = if controls_enabled {
        response.on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
    } else {
        response
    }
    .on_hover_text(format!(
        "Horizontal trigger position: {}%\nDrag to move · double-click to center",
        settings.position_percent
    ));

    if controls_enabled {
        let old_position = settings.position_percent;
        if response.double_clicked() {
            settings.position_percent = 50;
        } else if response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                settings.position_percent = scope_trigger_position_from_x(plot, pointer.x);
            }
        }
        if settings.position_percent != old_position {
            viewport.move_screen_anchor(
                f32::from(old_position) / 100.0,
                f32::from(settings.position_percent) / 100.0,
            );
            ui.ctx().request_repaint();
        }
    }

    let x = scope_trigger_position_x(plot, settings.position_percent);
    let clip_rect = Rect::from_min_max(
        Pos2::new(plot.left(), time_ruler.top()),
        plot.right_bottom(),
    );
    let painter = ui.painter().with_clip_rect(clip_rect);
    let mut y = plot.top();
    while y <= plot.bottom() {
        painter.circle_filled(Pos2::new(x, y), 0.85, COLOR.gamma_multiply(0.62));
        y += DOT_SPACING;
    }
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(x - ARROW_HALF_WIDTH, plot.top() - ARROW_HEIGHT),
            Pos2::new(x + ARROW_HALF_WIDTH, plot.top() - ARROW_HEIGHT),
            Pos2::new(x + ARROW_HALF_WIDTH, plot.top() - ARROW_SHOULDER_HEIGHT),
            Pos2::new(x, plot.top()),
            Pos2::new(x - ARROW_HALF_WIDTH, plot.top() - ARROW_SHOULDER_HEIGHT),
        ],
        COLOR,
        Stroke::new(1.0_f32, BACKGROUND),
    ));
    painter.text(
        Pos2::new(x, plot.top() - ARROW_HEIGHT + 5.5),
        egui::Align2::CENTER_CENTER,
        "T",
        FontId::monospace(9.0),
        marker_text_color(COLOR),
    );
}

fn paint_scope_trigger_level(
    ui: &egui::Ui,
    plot: Rect,
    scope_settings: ScopeCaptureSettings,
    settings: &mut ScopeTriggerSettings,
    channel_colors: [Color32; 2],
    vertical_offsets: &[u16; 2],
    controls_enabled: bool,
) {
    const ARROW_BODY_WIDTH: f32 = 19.0;
    const ARROW_TIP_WIDTH: f32 = 8.0;
    const ARROW_HALF_HEIGHT: f32 = 9.0;
    const DOT_SPACING: f32 = 7.0;
    let painter = ui.painter().with_clip_rect(plot);
    let active_channels = scope_trigger_channels(*settings)
        .iter()
        .copied()
        .filter(|channel| scope_settings.channels[*channel].enabled)
        .collect::<Vec<_>>();

    for channel in active_channels.iter().copied() {
        let color = channel_colors[channel];
        let mut channel_settings = scope_settings.channels[channel];
        channel_settings.vertical_offset = vertical_offsets[channel];
        let trigger_voltage =
            scope_trigger_threshold_voltage(channel_settings, settings.levels[channel]);
        let arrow_x = plot.right() - 2.0;
        let arrow_width = ARROW_BODY_WIDTH + ARROW_TIP_WIDTH;
        let arrow_y = scope_trigger_level_y(plot, settings.levels[channel]);
        let hit_rect = Rect::from_center_size(
            Pos2::new(arrow_x - arrow_width * 0.5, arrow_y),
            Vec2::new(arrow_width + 4.0, ARROW_HALF_HEIGHT * 2.0 + 10.0),
        );
        let response = ui.interact(
            hit_rect,
            ui.make_persistent_id(("scope_trigger_level", channel)),
            if controls_enabled {
                Sense::click_and_drag()
            } else {
                Sense::hover()
            },
        );
        let response = if controls_enabled {
            response.on_hover_cursor(egui::CursorIcon::ResizeVertical)
        } else {
            response
        }
        .on_hover_text(format!(
            "Trigger level: {}",
            format_trigger_voltage(trigger_voltage)
        ));

        if controls_enabled {
            if response.double_clicked() {
                settings.levels[channel] = vertical_offsets[channel].clamp(
                    u16::from(DSCOPE_TRIGGER_LEVEL_MIN),
                    u16::from(DSCOPE_TRIGGER_LEVEL_MAX),
                ) as u8;
            } else if response.dragged() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    settings.levels[channel] = scope_trigger_level_from_y(plot, pointer.y);
                }
            }
        }

        let arrow_y = scope_trigger_level_y(plot, settings.levels[channel]);
        let mut x = plot.left();
        while x <= plot.right() {
            painter.circle_filled(Pos2::new(x, arrow_y), 0.8, color.gamma_multiply(0.45));
            x += DOT_SPACING;
        }
        let body_left = arrow_x - ARROW_BODY_WIDTH;
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(arrow_x, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_left, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_left - ARROW_TIP_WIDTH, arrow_y),
                Pos2::new(body_left, arrow_y + ARROW_HALF_HEIGHT),
                Pos2::new(arrow_x, arrow_y + ARROW_HALF_HEIGHT),
            ],
            color,
            Stroke::new(1.0_f32, BACKGROUND),
        ));
        painter.text(
            Pos2::new(arrow_x - ARROW_BODY_WIDTH * 0.5, arrow_y),
            egui::Align2::CENTER_CENTER,
            format!("T{channel}"),
            FontId::monospace(9.0),
            marker_text_color(color),
        );
    }
}

fn paint_scope_readouts(
    painter: &egui::Painter,
    rect: Rect,
    visible_samples: usize,
    sample_rate_hz: u64,
    settings: ScopeCaptureSettings,
    channel_colors: [Color32; 2],
) {
    let mut visible_row = 0;
    for (channel, color) in channel_colors.iter().enumerate() {
        let channel_settings = settings.channels[channel];
        if !channel_settings.enabled {
            continue;
        }
        let scale = f32::from(channel_settings.volts_per_div_mv) / 1_000.0
            * f32::from(channel_settings.probe_attenuation);
        painter.text(
            rect.left_top() + Vec2::new(26.0, 7.0 + visible_row as f32 * 14.0),
            egui::Align2::LEFT_TOP,
            format!(
                "CH{}  {}/div  {}  {}×",
                channel,
                format_voltage(scale),
                coupling_label(channel_settings.coupling),
                channel_settings.probe_attenuation,
            ),
            FontId::monospace(10.0),
            *color,
        );
        visible_row += 1;
    }

    if sample_rate_hz > 0 {
        painter.text(
            rect.center_bottom() + Vec2::new(0.0, -6.0),
            egui::Align2::CENTER_BOTTOM,
            format!(
                "RUN  ·  {}  ·  {visible_samples} pts",
                format_sample_rate(sample_rate_hz)
            ),
            FontId::monospace(10.0),
            TEXT_MUTED,
        );
    }
}

fn paint_scope(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    viewport: &mut WaveformViewport,
    scope_options: ScopeWaveformOptions<'_>,
) {
    let ScopeWaveformOptions {
        mut settings,
        channel_colors,
        vertical_offsets,
        trigger_settings,
        offset_controls_enabled,
    } = scope_options;
    for (channel, vertical_offset) in settings.channels.iter_mut().zip(*vertical_offsets) {
        channel.vertical_offset = vertical_offset;
    }
    let sample_count = waveforms
        .scope_channels
        .iter()
        .enumerate()
        .filter(|(index, _)| settings.channels[*index].enabled)
        .map(|(_, samples)| samples)
        .map(VecDeque::len)
        .min()
        .unwrap_or(0);
    let display_sample_count = if sample_count == 0 {
        settings.sample_count as usize
    } else {
        sample_count
    };
    let display_sample_rate_hz = if waveforms.scope_rate_hz == 0 {
        settings.sample_rate_hz
    } else {
        waveforms.scope_rate_hz
    };
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    let (status_bar, time_ruler, plot) = split_waveform_chrome(rect);
    let scope_painter = painter.with_clip_rect(plot);
    paint_scope_graticule(&scope_painter, plot);
    let trigger_crossing = scope_trigger_crossing_sample(waveforms, settings, *trigger_settings);
    let trigger_sample_fraction = trigger_crossing
        .map(|crossing| crossing.sample)
        .or_else(|| {
            waveforms
                .scope_trigger_sample_index
                .map(|sample| sample as f32)
        })
        .map(|sample| scope_sample_fraction(sample, waveforms.scope_sample_offset, sample_count))
        .unwrap_or_else(|| f32::from(settings.trigger.position_percent) / 100.0);
    let trigger_screen_fraction = f32::from(trigger_settings.position_percent) / 100.0;
    if (viewport.span - 1.0).abs() > f32::EPSILON {
        viewport.align_sample_to_screen(trigger_sample_fraction, trigger_screen_fraction);
    }
    update_waveform_viewport(
        ui,
        plot,
        "scope_waveform_zoom",
        viewport,
        display_sample_count,
        display_sample_rate_hz,
        Some((trigger_sample_fraction, trigger_screen_fraction)),
    );
    paint_scope_offset_arrows(
        ui,
        plot,
        settings,
        channel_colors,
        vertical_offsets,
        trigger_settings,
        offset_controls_enabled,
    );
    paint_scope_trigger_level(
        ui,
        plot,
        settings,
        trigger_settings,
        channel_colors,
        vertical_offsets,
        offset_controls_enabled,
    );
    for (channel, vertical_offset) in settings.channels.iter_mut().zip(*vertical_offsets) {
        channel.vertical_offset = vertical_offset;
    }
    settings.trigger = *trigger_settings;

    let visible_range = viewport.sample_range(sample_count);
    let visible_count = visible_range.len();
    if visible_count == 0 {
        let idle_range = viewport.sample_range(display_sample_count);
        paint_scope_readouts(
            &scope_painter,
            plot,
            idle_range.len(),
            settings.sample_rate_hz,
            settings,
            channel_colors,
        );
        paint_empty(&scope_painter, plot, "Waiting for oscilloscope samples");
        paint_waveform_time_scale(
            painter,
            time_ruler,
            display_sample_count,
            display_sample_rate_hz,
            waveforms.scope_sample_offset,
            *viewport,
        );
        paint_waveform_status(
            painter,
            status_bar,
            display_sample_count,
            display_sample_rate_hz,
            *viewport,
        );
        paint_scope_trigger_position(
            ui,
            time_ruler,
            plot,
            trigger_settings,
            offset_controls_enabled,
            viewport,
        );
        return;
    }

    let step = (visible_count as f32 / plot.width().max(1.0))
        .floor()
        .max(1.0) as usize;

    for (channel_index, channel) in waveforms.scope_channels.iter().enumerate() {
        let Some(channel_settings) = settings.channels.get(channel_index).copied() else {
            continue;
        };
        if !channel_settings.enabled {
            continue;
        }
        let points = scope_plot_sample_indices(
            visible_range.clone(),
            step,
            channel_index,
            trigger_crossing,
            waveforms.scope_sample_offset,
        )
        .into_iter()
        .map(|index| {
            let x = waveform_sample_x(plot, *viewport, index as f32, sample_count);
            let y = scope_sample_y(plot, channel_settings, channel[index]);
            Pos2::new(x, y)
        })
        .collect::<Vec<_>>();
        if points.len() > 1 {
            let color = channel_colors[channel_index.min(channel_colors.len() - 1)];
            scope_painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(4.0_f32, color.gamma_multiply(0.16)),
            ));
            scope_painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, color)));
        }
    }

    paint_scope_readouts(
        &scope_painter,
        plot,
        visible_count,
        waveforms.scope_rate_hz,
        settings,
        channel_colors,
    );
    paint_waveform_time_scale(
        painter,
        time_ruler,
        display_sample_count,
        display_sample_rate_hz,
        waveforms.scope_sample_offset,
        *viewport,
    );
    paint_waveform_status(
        painter,
        status_bar,
        display_sample_count,
        display_sample_rate_hz,
        *viewport,
    );
    paint_scope_trigger_position(
        ui,
        time_ruler,
        plot,
        trigger_settings,
        offset_controls_enabled,
        viewport,
    );
}

fn paint_empty(painter: &egui::Painter, rect: Rect, text: &str) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        FontId::proportional(14.0),
        TEXT_MUTED,
    );
}

fn format_duration(duration: Duration) -> String {
    if duration >= Duration::from_secs(1) {
        format!("{:.2} s", duration.as_secs_f64())
    } else if duration >= Duration::from_millis(1) {
        format!("{:.2} ms", duration.as_secs_f64() * 1_000.0)
    } else if duration >= Duration::from_micros(1) {
        format!("{:.1} µs", duration.as_secs_f64() * 1_000_000.0)
    } else {
        format!("{:.1} ns", duration.as_secs_f64() * 1_000_000_000.0)
    }
}

fn capture_start_skew(stats: &HashMap<DeviceId, DeviceStats>) -> Option<Duration> {
    let mut timestamps = stats
        .values()
        .filter_map(|stats| stats.first_timestamp)
        .collect::<Vec<_>>();
    if timestamps.len() < 2 {
        return None;
    }
    timestamps.sort_unstable();
    Some(timestamps[timestamps.len() - 1] - timestamps[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsview_core::{LogicBatch, ScopeBatch};

    #[test]
    fn waveform_store_keeps_newest_samples_within_bound() {
        let mut store = WaveformStore::default();
        store.ingest(
            &CaptureBatch::Logic(LogicBatch {
                sample_rate_hz: 1_000,
                channel_count: 2,
                samples: (0..MAX_WAVEFORM_SAMPLES as u32 + 10).collect(),
            }),
            [true, true],
        );

        assert_eq!(store.logic_samples.len(), MAX_WAVEFORM_SAMPLES);
        assert_eq!(store.logic_samples.front(), Some(&10));
        assert_eq!(store.logic_sample_offset, 10);
    }

    #[test]
    fn waveform_store_tracks_scope_channels() {
        let mut store = WaveformStore::default();
        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000,
                trigger_sample_index: Some(1),
                channels: vec![vec![0.1, 0.2], vec![-0.1, -0.2]],
            }),
            [true, true],
        );

        assert_eq!(store.scope_rate_hz, 1_000_000);
        assert_eq!(store.scope_trigger_sample_index, Some(1));
        assert_eq!(store.scope_channels[0].back(), Some(&0.2));
        assert_eq!(store.scope_channels[1].back(), Some(&-0.2));
    }

    #[test]
    fn waveform_store_preserves_single_scope_channel_identity() {
        let mut store = WaveformStore::default();
        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000_000,
                trigger_sample_index: None,
                channels: vec![vec![0.1, 0.2]],
            }),
            [false, true],
        );

        assert!(store.scope_channels[0].is_empty());
        assert_eq!(store.scope_channels[1].back(), Some(&0.2));
    }

    #[test]
    fn waveform_tabs_require_their_connected_devices() {
        assert!(waveform_tabs(false, false).is_empty());
        assert_eq!(waveform_tabs(true, false), vec![WaveformTab::LogicAnalyzer]);
        assert_eq!(waveform_tabs(false, true), vec![WaveformTab::Oscilloscope]);
        assert_eq!(
            waveform_tabs(true, true),
            vec![
                WaveformTab::LogicAnalyzer,
                WaveformTab::Oscilloscope,
                WaveformTab::Combined,
            ]
        );
    }

    #[test]
    fn capture_setup_tabs_require_their_connected_devices() {
        assert!(capture_settings_tabs(false, false).is_empty());
        assert_eq!(
            capture_settings_tabs(true, false),
            vec![CaptureSettingsTab::DsLogic]
        );
        assert_eq!(
            capture_settings_tabs(false, true),
            vec![CaptureSettingsTab::DsCope]
        );
        let tabs = capture_settings_tabs(true, true);
        assert_eq!(
            tabs,
            vec![CaptureSettingsTab::DsLogic, CaptureSettingsTab::DsCope]
        );
        assert_eq!(
            tabs.iter()
                .copied()
                .map(CaptureSettingsTab::label)
                .collect::<Vec<_>>(),
            ["DSLogic", "DSCope"]
        );
    }

    #[test]
    fn combined_panes_scale_with_window_and_respect_ratio() {
        let (logic, scope) = combined_pane_heights(610.0, 0.6);
        assert!((logic - 360.0).abs() < f32::EPSILON);
        assert!((scope - 240.0).abs() < f32::EPSILON);

        let (logic, scope) = combined_pane_heights(1_010.0, 0.6);
        assert!((logic - 600.0).abs() < f32::EPSILON);
        assert!((scope - 400.0).abs() < f32::EPSILON);

        let (logic, scope) = combined_pane_heights(310.0, 0.99);
        let minimum_scale = 300.0 / (MIN_LOGIC_PANE_HEIGHT + MIN_SCOPE_PANE_HEIGHT);
        assert!((logic - MIN_LOGIC_PANE_HEIGHT * minimum_scale).abs() < 0.001);
        assert!((scope - MIN_SCOPE_PANE_HEIGHT * minimum_scale).abs() < 0.001);
    }

    #[test]
    fn waveform_status_is_positioned_above_the_time_ruler() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 300.0));
        let (status, ruler, plot) = split_waveform_chrome(rect);

        assert_eq!(status.top(), rect.top());
        assert_eq!(status.bottom(), ruler.top());
        assert_eq!(ruler.bottom(), plot.top());
        assert_eq!(plot.bottom(), rect.bottom());
    }

    #[test]
    fn default_resource_directory_contains_both_bitstreams() {
        let directory = PathBuf::from(default_resource_directory());
        assert!(has_required_fpga_resources(&directory));
    }

    #[test]
    fn every_logic_channel_has_a_distinct_default_color() {
        let colors = default_logic_channel_colors();
        for (index, color) in colors.iter().enumerate() {
            assert!(!colors[..index].contains(color));
        }
    }

    #[test]
    fn waveform_zoom_preserves_the_sample_under_the_pointer() {
        let mut viewport = WaveformViewport::default();

        viewport.step_timebase(1.0, 1_000_000, 1_000_000, 0.25);

        assert!((viewport.span - 0.5).abs() < f32::EPSILON);
        assert!((viewport.start + viewport.span * 0.25 - 0.25).abs() < 0.000_001);
        assert!(viewport.sample_range(1_000).len() < 1_000);
        assert_eq!(viewport.time_per_division(1_000_000, 1_000_000), Some(0.05));
    }

    #[test]
    fn waveform_zoom_out_is_clamped_to_the_full_capture() {
        let mut viewport = WaveformViewport::default();
        viewport.step_timebase(1.0, 1_000_000, 1_000_000, 0.75);

        viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.75);

        assert_eq!(viewport, WaveformViewport::default());
        assert_eq!(viewport.sample_range(1_000), 0..1_000);
    }

    #[test]
    fn time_scale_selects_readable_units_for_the_visible_window() {
        assert_eq!(time_scale_unit(2.0), ("s", 1.0));
        assert_eq!(time_scale_unit(0.002), ("ms", 1.0e3));
        assert_eq!(time_scale_unit(0.000_002), ("µs", 1.0e6));
        assert_eq!(time_scale_unit(0.000_000_002), ("ns", 1.0e9));
        assert_eq!(time_scale_precision(12.0), 0);
        assert_eq!(time_scale_precision(1.2), 1);
        assert_eq!(time_scale_precision(0.008), 4);
        assert_eq!(format_duration(Duration::from_nanos(2)), "2.0 ns");
    }

    #[test]
    fn timebase_steps_follow_the_vendor_one_two_five_sequence() {
        assert_eq!(TIMEBASE_STEPS_SECONDS.first(), Some(&2.0e-9));
        assert_eq!(TIMEBASE_STEPS_SECONDS.last(), Some(&10.0));
        assert_eq!(adjacent_timebase(10.0e-9, true), Some(5.0e-9));
        assert_eq!(adjacent_timebase(10.0e-9, false), Some(20.0e-9));
        assert_eq!(adjacent_timebase(2.0e-9, true), None);
        assert_eq!(adjacent_timebase(10.0, false), None);

        let mut viewport = WaveformViewport::default();
        for _ in 0..6 {
            viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5);
        }
        assert!((viewport.time_per_division(1_000_000, 1_000_000).unwrap() - 10.0).abs() < 0.001);
        assert!((viewport.span - 100.0).abs() < 0.001);
        assert_eq!(viewport.sample_range(1_000_000), 0..1_000_000);
    }

    #[test]
    fn idle_time_scale_uses_the_configured_display_window() {
        let viewport = WaveformViewport::default();
        let (first, last) = time_scale_sample_window(SAMPLE_COUNT as usize, 0, viewport).unwrap();

        assert_eq!(first, 0.0);
        assert_eq!(last, SAMPLE_COUNT as f64 - 1.0);

        let mut zoomed = viewport;
        zoomed.step_timebase(1.0, SAMPLE_COUNT as usize, SAMPLE_RATE_HZ, 0.5);
        let (zoomed_first, zoomed_last) =
            time_scale_sample_window(SAMPLE_COUNT as usize, 0, zoomed).unwrap();
        assert!(zoomed_last - zoomed_first < last - first);
    }

    #[test]
    fn scope_settings_map_common_and_per_channel_configuration() {
        let settings = ScopeCaptureSettings {
            sample_rate_hz: 2_000_000,
            sample_count: 250_000,
            channels: [
                ScopeChannelSettings {
                    enabled: true,
                    volts_per_div_mv: 100,
                    coupling: DsCopeCoupling::Ac,
                    probe_attenuation: 10,
                    vertical_offset: 64,
                },
                ScopeChannelSettings {
                    enabled: false,
                    volts_per_div_mv: 2_000,
                    coupling: DsCopeCoupling::Dc,
                    probe_attenuation: 1,
                    vertical_offset: 192,
                },
            ],
            trigger: ScopeTriggerSettings {
                position_percent: 75,
                source: DsCopeTriggerSource::ChannelZero,
                auto_channel: 0,
                slope: DsCopeTriggerSlope::Falling,
                holdoff_ns: 250_000,
                holdoff_unit: TriggerHoldoffUnit::Microseconds,
                noise_sensitivity: 12,
                levels: [64, 192],
            },
            operation_mode: DsCopeOperationMode::InternalTest,
            bandwidth_limit: DsCopeBandwidthLimit::Mhz20,
        };

        let config = settings.capture_config().unwrap();
        assert_eq!(config.sample_rate_hz(), 2_000_000);
        assert_eq!(config.sample_count(), 250_000);
        assert_eq!(config.channels()[0].volts_per_div_mv(), 100);
        assert_eq!(config.channels()[0].coupling(), DsCopeCoupling::Ac);
        assert_eq!(config.channels()[0].probe_attenuation(), 10);
        assert_eq!(config.channels()[0].vertical_offset(), 64);
        assert_eq!(config.channels()[1].volts_per_div_mv(), 2_000);
        assert_eq!(config.channels()[1].coupling(), DsCopeCoupling::Dc);
        assert_eq!(config.channels()[1].probe_attenuation(), 1);
        assert_eq!(config.channels()[1].vertical_offset(), 192);
        assert!(!config.channels()[1].enabled());
        assert_eq!(config.trigger().position_percent(), 75);
        assert_eq!(config.trigger().source(), DsCopeTriggerSource::ChannelZero);
        assert_eq!(config.trigger().slope(), DsCopeTriggerSlope::Falling);
        assert_eq!(config.trigger().holdoff_ns(), 250_000);
        assert_eq!(config.trigger().noise_sensitivity(), 12);
        assert_eq!(config.trigger().levels(), [64, 192]);
        assert_eq!(config.operation_mode(), DsCopeOperationMode::InternalTest);
        assert_eq!(config.bandwidth_limit(), DsCopeBandwidthLimit::Mhz20);
        assert_eq!(ScopeCaptureSettings::from_capture_config(config), settings);
        assert_eq!(format_voltage(0.1), "100 mV");
        assert_eq!(format_sample_rate(1_000_000), "1.00 MSa/s");
        assert_eq!(format_sample_rate(1_000_000_000), "1.00 GSa/s");
    }

    #[test]
    fn scope_limits_follow_the_enabled_channel_count() {
        let mut settings = ScopeCaptureSettings::default();
        assert_eq!(settings.maximum_sample_rate_hz(), 500_000_000);
        assert_eq!(settings.maximum_sample_count(), 1_000_000);

        settings.channels[1].enabled = false;
        settings.sample_rate_hz = 1_000_000_000;
        settings.sample_count = 2_000_000;

        assert_eq!(settings.maximum_sample_rate_hz(), 1_000_000_000);
        assert_eq!(settings.maximum_sample_count(), 2_000_000);
        assert!(settings.capture_config().is_ok());
    }

    #[test]
    fn scope_frontend_change_detection_ignores_display_conversion_and_buffer() {
        let original = ScopeCaptureSettings::default();
        let mut changed = original;
        changed.sample_count = 250_000;
        changed.channels[0].probe_attenuation = 10;
        assert!(!changed.frontend_differs_from(original));

        changed.channels[0].volts_per_div_mv = 100;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.channels[0].vertical_offset = 64;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.trigger.holdoff_unit = TriggerHoldoffUnit::Seconds;
        assert!(!changed.frontend_differs_from(original));

        changed.trigger.slope = DsCopeTriggerSlope::Falling;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.trigger.levels[0] = 64;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.operation_mode = DsCopeOperationMode::InternalTest;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.bandwidth_limit = DsCopeBandwidthLimit::Mhz20;
        assert!(changed.frontend_differs_from(original));
    }

    #[test]
    fn scope_trigger_source_tracks_enabled_channels() {
        let mut trigger = ScopeTriggerSettings {
            source: DsCopeTriggerSource::ChannelOne,
            auto_channel: 1,
            ..ScopeTriggerSettings::default()
        };

        trigger.clamp_to_channels([true, false]);

        assert_eq!(trigger.source, DsCopeTriggerSource::Auto);
        assert_eq!(trigger.auto_channel, 0);
        assert!(trigger.capture_config().is_ok());
    }

    #[test]
    fn scope_trigger_levels_use_vendor_limits_and_follow_channel_offsets() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 20.0), Pos2::new(100.0, 220.0));

        assert_eq!(
            scope_trigger_level_from_y(plot, plot.top()),
            DSCOPE_TRIGGER_LEVEL_MIN
        );
        assert_eq!(
            scope_trigger_level_from_y(plot, plot.bottom()),
            DSCOPE_TRIGGER_LEVEL_MAX
        );
        assert_eq!(shift_scope_trigger_level(128, 128, 160), 160);
        assert_eq!(shift_scope_trigger_level(220, 128, 160), 232);
        assert_eq!(shift_scope_trigger_level(32, 128, 96), 24);
        assert_eq!(format_trigger_voltage(0.0), "0.00 mV");
        assert_eq!(format_trigger_voltage(0.125), "125 mV");
        assert_eq!(format_trigger_voltage(-1.25), "-1.25 V");

        let trigger = ScopeTriggerSettings {
            source: DsCopeTriggerSource::ChannelZeroAndOne,
            ..ScopeTriggerSettings::default()
        };
        assert_eq!(scope_trigger_channels(trigger), &[0, 1]);
    }

    #[test]
    fn scope_trigger_anchor_interpolates_the_threshold_crossing() {
        let mut waveforms = WaveformStore {
            scope_channels: vec![
                VecDeque::from(vec![-1.0, -1.0, 1.0, 1.0]),
                VecDeque::from(vec![0.0; 4]),
            ],
            scope_trigger_sample_index: Some(3),
            ..WaveformStore::default()
        };
        let settings = ScopeCaptureSettings::default();
        let mut trigger = ScopeTriggerSettings {
            source: DsCopeTriggerSource::ChannelZero,
            slope: DsCopeTriggerSlope::Rising,
            ..ScopeTriggerSettings::default()
        };

        assert_eq!(
            scope_trigger_crossing_sample(&waveforms, settings, trigger),
            Some(ScopeTriggerCrossing {
                sample: 1.5,
                channel_index: 0,
            })
        );

        waveforms.scope_channels[0] = VecDeque::from(vec![1.0, 1.0, -1.0, -1.0]);
        trigger.slope = DsCopeTriggerSlope::Falling;
        assert_eq!(
            scope_trigger_crossing_sample(&waveforms, settings, trigger),
            Some(ScopeTriggerCrossing {
                sample: 1.5,
                channel_index: 0,
            })
        );
    }

    #[test]
    fn scope_trace_and_trigger_use_the_same_sample_coordinates() {
        let plot = Rect::from_min_max(Pos2::new(100.0, 20.0), Pos2::new(900.0, 420.0));
        let viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
        };

        assert!((scope_sample_fraction(1_050.0, 1_000, 101) - 0.5).abs() < f32::EPSILON);
        assert!((waveform_sample_x(plot, viewport, 50.0, 101) - 500.0).abs() < f32::EPSILON);
    }

    #[test]
    fn scope_downsampling_preserves_trigger_crossing_samples() {
        let indices = scope_plot_sample_indices(
            0..1_000,
            100,
            0,
            Some(ScopeTriggerCrossing {
                sample: 512.25,
                channel_index: 0,
            }),
            0,
        );

        assert!(indices.contains(&512));
        assert!(indices.contains(&513));
        assert_eq!(indices.last(), Some(&999));
    }

    #[test]
    fn scope_trigger_position_maps_between_percent_and_plot_x() {
        let plot = Rect::from_min_max(Pos2::new(100.0, 20.0), Pos2::new(900.0, 420.0));

        assert!((scope_trigger_position_x(plot, 50) - 500.0).abs() < f32::EPSILON);
        assert_eq!(scope_trigger_position_from_x(plot, 500.0), 50);
        assert_eq!(
            scope_trigger_position_from_x(plot, plot.left()),
            DSCOPE_TRIGGER_POSITION_MIN_PERCENT
        );
        assert_eq!(
            scope_trigger_position_from_x(plot, plot.right()),
            DSCOPE_TRIGGER_POSITION_MAX_PERCENT
        );

        let mut viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
        };
        viewport.move_screen_anchor(0.5, 0.25);
        assert!((viewport.start - 0.375).abs() < f32::EPSILON);
        assert!(((0.5 - viewport.start) / viewport.span - 0.25).abs() < f32::EPSILON);

        let mut hardware_anchored = WaveformViewport::default();
        hardware_anchored.step_timebase_at_sample(1.0, 1_000_000, 1_000_000, 0.503, 0.5);
        assert!(hardware_anchored.span < 1.0);
        assert!(
            ((0.503 - hardware_anchored.start) / hardware_anchored.span - 0.5).abs() < f32::EPSILON
        );
    }

    #[test]
    fn scope_vertical_offset_maps_hardware_limits_to_the_plot() {
        let rect = Rect::from_min_max(Pos2::new(0.0, 20.0), Pos2::new(100.0, 220.0));

        assert_eq!(
            scope_vertical_offset_y(rect, DSCOPE_VERTICAL_OFFSET_MIN),
            rect.top()
        );
        assert_eq!(
            scope_vertical_offset_y(rect, DSCOPE_VERTICAL_OFFSET_MAX),
            rect.bottom()
        );
        assert_eq!(
            scope_vertical_offset_from_y(rect, rect.top()),
            DSCOPE_VERTICAL_OFFSET_MIN
        );
        assert_eq!(
            scope_vertical_offset_from_y(rect, rect.bottom()),
            DSCOPE_VERTICAL_OFFSET_MAX
        );
    }

    #[test]
    fn scope_input_range_rescales_captured_volts() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1_000.0, 800.0));
        let one_volt_per_div = ScopeChannelSettings {
            volts_per_div_mv: 1_000,
            ..ScopeChannelSettings::default()
        };
        let half_volt_per_div = ScopeChannelSettings {
            volts_per_div_mv: 500,
            ..one_volt_per_div
        };
        let zero_y = scope_sample_y(plot, one_volt_per_div, 0.0);
        let one_volt_y = scope_sample_y(plot, one_volt_per_div, 1.0);
        let half_volt_range_y = scope_sample_y(plot, half_volt_per_div, 1.0);

        let pixels_per_division = plot.height() / SCOPE_VERTICAL_DIVISIONS as f32;
        assert!((zero_y - one_volt_y - pixels_per_division).abs() < 0.001);
        assert!((zero_y - half_volt_range_y - pixels_per_division * 2.0).abs() < 0.001);
    }

    #[test]
    fn overlapping_scope_offset_arrows_remain_independently_selectable() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 200.0));
        let mut settings = ScopeCaptureSettings::default();
        let mut offsets = [DSCOPE_VERTICAL_OFFSET_CENTER; 2];
        let center_y = scope_vertical_offset_y(plot, DSCOPE_VERTICAL_OFFSET_CENTER);

        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            Some(0)
        );

        offsets[0] = DSCOPE_VERTICAL_OFFSET_MIN;
        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            Some(1)
        );

        settings.channels[0].enabled = false;
        offsets[0] = DSCOPE_VERTICAL_OFFSET_CENTER;
        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            Some(1)
        );

        settings.channels[1].enabled = false;
        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            None
        );
    }

    #[test]
    fn scope_input_range_encoder_steps_and_clamps_supported_ranges() {
        assert_eq!(step_scope_input_range(100, 1), 200);
        assert_eq!(step_scope_input_range(100, -1), 50);
        assert_eq!(step_scope_input_range(10, -10), 10);
        assert_eq!(step_scope_input_range(2_000, 10), 2_000);
    }
}
