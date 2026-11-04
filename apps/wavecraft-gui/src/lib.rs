mod devices;
mod measurements;
mod protocol_analyzer;
mod recording;
mod sessions;
mod snapshot;
mod theme;
pub mod waveform;

use devices::{
    capture_devices_for_target, first_device_id_for_role, first_radio_id, selected_device_for_role,
    selected_radio, usb_speed_warning, DeviceEntry, DeviceRole, UsbSpeedWarning,
};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use measurements::{
    calculate_scope_measurements, ScopeMeasurementResults, ScopeMeasurementSettings,
    SCOPE_MEASUREMENT_KINDS,
};
use protocol_analyzer::{
    analyzer_channels, analyzer_decoder_chain, analyzer_long_name, analyzer_name,
    analyzer_transport_name, decode_protocol_samples, decode_transport_samples, matching_protocols,
    protocol_stack, ProtocolAnalyzerInstance, ProtocolAnalyzerSettings, ProtocolCaptureDecoder,
    ProtocolDataFormat, ProtocolDecodeOutcome, ProtocolDecodeSnapshot, ProtocolDecodedEvent,
    ProtocolDecodedKind, PROTOCOLS,
};
use recording::{
    choose_recording_save_path, choose_recording_to_open, choose_saleae_export_path,
    create_saleae_export_file,
};
use sessions::{SessionTabs, MAX_SESSION_TABS, SESSION_NAME_MAX_CHARS};
use snapshot::{crop_snapshot_image, save_snapshot_image, PendingSnapshot};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek};
use std::ops::{Range, RangeInclusive};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use theme::{
    configure_style, toolbar_action_button, ACCENT, BACKGROUND, GRID, MATH_COLOR, PANEL,
    SCOPE_COLORS, TEXT_MUTED,
};
use wavecraft_core::{
    CaptureBatch, CaptureCoordinator, CaptureEvent, CaptureFrame, CaptureSession, CaptureSource,
    DeviceDescriptor, DeviceId, InstrumentKind, ScopeBatch,
};
use wavecraft_dsp::{
    BinaryFskProfile, BinaryGfskProfile, ChannelizerConfig, DemodulationProfile,
    DemodulatorRegistry, FskPolarity, FskTiming, SignalAnalyzer, SignalAnalyzerProfile,
    SpectrumProcessor, SpectrumWindow,
};
use wavecraft_hackrf::{HackRfAdapter, HackRfRuntimeControl, HackRfRuntimeSettings};
use wavecraft_recording::{
    read_saleae, write_saleae, CompletionStatus, RecordingReader, RecordingWriter,
    SaleaeAnalogChannel, SaleaeCapture, SaleaeChannel, SaleaeDigitalCapture,
};
use wavecraft_sdr::{
    DiscoveredRadio, HackRfSettings, RadioCapabilities, RadioCaptureSettings, RadioDiscoveryReport,
    RadioProvider, RadioProviderFailure, RadioProviderSettings, RadioRegistry, ValueDomain,
};
use wavecraft_usb::{
    apply_dscope_configuration, auto_calibrate_dscope_with_progress, discover_supported_devices,
    prepare_device, read_dscope_calibration, write_dscope_calibration, DeviceModel,
    DiscoveredDevice, DsCopeBandwidthLimit, DsCopeCalibrationFrame, DsCopeCalibrationValues,
    DsCopeCaptureConfig, DsCopeCaptureSource, DsCopeChannelConfig, DsCopeCoupling,
    DsCopeOperationMode, DsCopeRuntimeCalibrationControl, DsCopeRuntimeTriggerControl,
    DsCopeTriggerConfig, DsCopeTriggerSlope, DsCopeTriggerSource, DsLogicCaptureConfig,
    DsLogicCaptureSource, DsLogicInputFilter, DsLogicOperationMode, DsLogicSimpleTrigger,
    DsLogicSmartTrigger, DsLogicStopOption, DsLogicTriggerCondition, DsLogicTriggerConfig,
    DsLogicTriggerStage, LinkStatus, UsbSpeedClass, DSCOPE_FRAME_SEQUENCE_STRIDE,
    DSCOPE_SAMPLE_COUNTS, DSCOPE_SAMPLE_RATES_HZ, DSCOPE_TRIGGER_HOLDOFF_MAX_NS,
    DSCOPE_TRIGGER_LEVEL_CENTER, DSCOPE_TRIGGER_LEVEL_MAX, DSCOPE_TRIGGER_LEVEL_MIN,
    DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX, DSCOPE_TRIGGER_POSITION_MAX_PERCENT,
    DSCOPE_TRIGGER_POSITION_MIN_PERCENT, DSCOPE_VERTICAL_OFFSET_CENTER, DSCOPE_VERTICAL_OFFSET_MAX,
    DSCOPE_VERTICAL_OFFSET_MIN, DSCOPE_VOLTS_PER_DIV_MV, DSLOGIC_SAMPLE_ALIGNMENT,
    DSLOGIC_SAMPLE_RATES_HZ, DSLOGIC_TRIGGER_STAGE_COUNT, REQUIRED_RESOURCE_FILES,
};
use waveform::radio::{RadioAnalysisStore, RadioDisplayStore, RadioViewState};

const SAMPLE_RATE_HZ: u64 = 1_000_000;
const SAMPLE_COUNT: u64 = 1_000_000;
const BUILD_VERSION_LABEL: &str = concat!("Version: ", env!("WAVECRAFT_BUILD_VERSION"));
const LOGIC_CHANNEL_COUNT: usize = 16;
const RADIO_SPECTRUM_FFT_LENGTH: usize = 2_048;
// Bound logic display storage by activity-preserving buckets. The bucket stride
// expands to cover the configured finite or Loop duration without limiting the
// decoder, recorder, or zoom-out domain to this in-memory bucket count.
const MAX_WAVEFORM_SAMPLES: usize = 2_000_000;
// Continuous Auto acquisition uses a software ring large enough for the
// selected OSC span. Bound its total sample storage across enabled channels.
const SCOPE_ROLLING_MEMORY_BUDGET_BYTES: usize = 128 * 1024 * 1024;
const WAVEFORM_STATUS_HEIGHT: f32 = 20.0;
const TIME_RULER_HEIGHT: f32 = 24.0;
const SCOPE_HORIZONTAL_DIVISIONS: usize = 10;
const SCOPE_VERTICAL_DIVISIONS: usize = 8;
const SCOPE_ADC_VERTICAL_DIVISIONS: f32 = 10.0;
const SCOPE_DOWNSAMPLE_MIN_SAMPLES_PER_PIXEL: usize = 4;
const SCOPE_CONFIGURATION_DEBOUNCE: Duration = Duration::from_millis(350);
const DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS: f64 = 1.0e-3;
const TIMEBASE_STEPS_SECONDS: [f64; 30] = [
    2.0e-9, 5.0e-9, 10.0e-9, 20.0e-9, 50.0e-9, 100.0e-9, 200.0e-9, 500.0e-9, 1.0e-6, 2.0e-6,
    5.0e-6, 10.0e-6, 20.0e-6, 50.0e-6, 100.0e-6, 200.0e-6, 500.0e-6, 1.0e-3, 2.0e-3, 5.0e-3,
    10.0e-3, 20.0e-3, 50.0e-3, 100.0e-3, 200.0e-3, 500.0e-3, 1.0, 2.0, 5.0, 10.0,
];
const FFT_LENGTHS: [usize; 5] = [1_024, 2_048, 4_096, 8_192, 16_384];
const SCOPE_MARKER_LIMIT: usize = 10;
const LOGIC_MARKER_LIMIT: usize = 20;
const LOGIC_MARKER_NAME_MAX_CHARS: usize = 32;
const CHANNEL_NAME_MAX_CHARS: usize = 32;
const LOGIC_CHANNEL_NAME_MAX_CHARS: usize = 8;
const DEFAULT_LOGIC_CHANNEL_MARKER_OPACITY: u8 = 128;
const LOGIC_MARKER_EDGE_SNAP_DISTANCE: f32 = 8.0;
const CAPTURE_FILE_BUTTON_WIDTH: f32 = 82.0;
const DECODED_PANEL_ACTION_BUTTON_SIZE: [f32; 2] = [26.0, 20.0];
static SCOPE_WAVEFORM_REVISION: AtomicU64 = AtomicU64::new(1);
static LOGIC_WAVEFORM_REVISION: AtomicU64 = AtomicU64::new(1);

fn empty_logic_channel_names() -> [String; LOGIC_CHANNEL_COUNT] {
    std::array::from_fn(|_| String::new())
}

fn empty_scope_channel_names() -> [String; 2] {
    std::array::from_fn(|_| String::new())
}

fn normalized_channel_name(name: &str) -> String {
    name.trim().chars().take(CHANNEL_NAME_MAX_CHARS).collect()
}

fn channel_label(prefix: &str, index: usize, name: &str) -> String {
    let identifier = format!("{prefix}{index}");
    let name = name.trim();
    if name.is_empty() {
        identifier
    } else {
        format!("{identifier} · {name}")
    }
}

fn scope_channel_label(index: usize, names: &[String; 2]) -> String {
    channel_label("CH", index, &names[index])
}

fn scope_channels_subtitle(names: &[String; 2]) -> String {
    format!(
        "DSCope · {} / {}",
        scope_channel_label(0, names),
        scope_channel_label(1, names)
    )
}

fn choose_custom_protocol_to_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Load YAML protocol")
        .add_filter("WaveCraft protocol", &["yaml", "yml"])
        .pick_file()
}

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([920.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "WaveCraft",
        options,
        Box::new(|creation_context| Ok(Box::new(WaveCraftApp::new(creation_context)))),
    )
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
enum ScopeMathOperation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl ScopeMathOperation {
    fn label(self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Multiply => "Multiply",
            Self::Divide => "Divide",
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "−",
            Self::Multiply => "×",
            Self::Divide => "÷",
        }
    }

    fn apply(self, left: f32, right: f32) -> Option<f32> {
        let value = match self {
            Self::Add => left + right,
            Self::Subtract => left - right,
            Self::Multiply => left * right,
            Self::Divide if right.abs() > f32::EPSILON => left / right,
            Self::Divide => return None,
        };
        value.is_finite().then_some(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeMathSettings {
    enabled: bool,
    operation: ScopeMathOperation,
    left_channel: usize,
    right_channel: usize,
}

impl Default for ScopeMathSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            operation: ScopeMathOperation::Add,
            left_channel: 0,
            right_channel: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScopeFftWindow {
    Rectangular,
    Hann,
    Hamming,
    Blackman,
    FlatTop,
}

impl ScopeFftWindow {
    fn label(self) -> &'static str {
        match self {
            Self::Rectangular => "Rectangular",
            Self::Hann => "Hann",
            Self::Hamming => "Hamming",
            Self::Blackman => "Blackman",
            Self::FlatTop => "Flat Top",
        }
    }

    fn coefficient(self, index: usize, length: usize) -> f64 {
        if self == Self::Rectangular || length <= 1 {
            return 1.0;
        }
        let phase = std::f64::consts::TAU * index as f64 / (length - 1) as f64;
        match self {
            Self::Rectangular => 1.0,
            Self::Hann => 0.5 * (1.0 - phase.cos()),
            Self::Hamming => 0.54 - 0.46 * phase.cos(),
            Self::Blackman => 0.426_59 - 0.496_56 * phase.cos() + 0.076_849 * (2.0 * phase).cos(),
            Self::FlatTop => {
                1.0 - 1.93 * phase.cos() + 1.29 * (2.0 * phase).cos() - 0.388 * (3.0 * phase).cos()
                    + 0.028 * (4.0 * phase).cos()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeFftSettings {
    enabled: bool,
    channel: usize,
    length: usize,
    window: ScopeFftWindow,
}

impl Default for ScopeFftSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: 0,
            length: 16_384,
            window: ScopeFftWindow::Hann,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeLissajousSettings {
    enabled: bool,
    x_channel: usize,
    y_channel: usize,
}

impl Default for ScopeLissajousSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            x_channel: 0,
            y_channel: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ScopeFunctionSettings {
    math: ScopeMathSettings,
    fft: ScopeFftSettings,
    lissajous: ScopeLissajousSettings,
    measurements: ScopeMeasurementSettings,
}

impl ScopeFunctionSettings {
    fn clamp_to_channels(&mut self, channels: [bool; 2]) {
        let fallback = if channels[0] { 0 } else { 1 };
        if !channels[self.math.left_channel] {
            self.math.left_channel = fallback;
        }
        if !channels[self.math.right_channel] {
            self.math.right_channel = fallback;
        }
        if !channels[self.fft.channel] {
            self.fft.channel = fallback;
        }
        if !channels[self.lissajous.x_channel] {
            self.lissajous.x_channel = fallback;
        }
        if !channels[self.lissajous.y_channel] {
            self.lissajous.y_channel = fallback;
        }
        if channels.into_iter().filter(|enabled| *enabled).count() < 2 {
            self.lissajous.enabled = false;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeChannelMarkerSettings {
    horizontal_count: usize,
    horizontal_positions: [f32; SCOPE_MARKER_LIMIT],
    horizontal_comparisons: [Option<usize>; SCOPE_MARKER_LIMIT],
    horizontal_colors: [Color32; SCOPE_MARKER_LIMIT],
    vertical_count: usize,
    vertical_positions: [f32; SCOPE_MARKER_LIMIT],
    vertical_comparisons: [Option<usize>; SCOPE_MARKER_LIMIT],
    vertical_colors: [Color32; SCOPE_MARKER_LIMIT],
}

impl ScopeChannelMarkerSettings {
    fn new(
        channel: usize,
        horizontal_positions: [f32; SCOPE_MARKER_LIMIT],
        vertical_positions: [f32; SCOPE_MARKER_LIMIT],
    ) -> Self {
        let colors = std::array::from_fn(|index| scope_marker_color(channel, index));
        Self {
            horizontal_count: 0,
            horizontal_positions,
            horizontal_comparisons: [None; SCOPE_MARKER_LIMIT],
            horizontal_colors: colors,
            vertical_count: 0,
            vertical_positions,
            vertical_comparisons: [None; SCOPE_MARKER_LIMIT],
            vertical_colors: colors,
        }
    }

    fn count(self, kind: ScopeMarkerKind) -> usize {
        match kind {
            ScopeMarkerKind::Time => self.horizontal_count,
            ScopeMarkerKind::Voltage => self.vertical_count,
        }
        .min(SCOPE_MARKER_LIMIT)
    }

    fn total_count(self) -> usize {
        self.count(ScopeMarkerKind::Time) + self.count(ScopeMarkerKind::Voltage)
    }

    fn marker(self, kind: ScopeMarkerKind, index: usize) -> Option<(f32, Option<usize>, Color32)> {
        (index < self.count(kind)).then(|| match kind {
            ScopeMarkerKind::Time => (
                self.horizontal_positions[index],
                self.horizontal_comparisons[index],
                self.horizontal_colors[index],
            ),
            ScopeMarkerKind::Voltage => (
                self.vertical_positions[index],
                self.vertical_comparisons[index],
                self.vertical_colors[index],
            ),
        })
    }

    fn set_marker(
        &mut self,
        kind: ScopeMarkerKind,
        index: usize,
        position: f32,
        comparison: Option<usize>,
        color: Color32,
    ) -> bool {
        if index >= SCOPE_MARKER_LIMIT {
            return false;
        }
        match kind {
            ScopeMarkerKind::Time => {
                self.horizontal_positions[index] = position.clamp(0.0, 1.0);
                self.horizontal_comparisons[index] = comparison;
                self.horizontal_colors[index] = color;
            }
            ScopeMarkerKind::Voltage => {
                self.vertical_positions[index] = position.clamp(0.0, 1.0);
                self.vertical_comparisons[index] = comparison;
                self.vertical_colors[index] = color;
            }
        }
        true
    }

    fn add_marker(
        &mut self,
        kind: ScopeMarkerKind,
        position: f32,
        comparison: Option<usize>,
        color: Color32,
    ) -> bool {
        let index = self.count(kind);
        if self.total_count() >= SCOPE_MARKER_LIMIT
            || index >= SCOPE_MARKER_LIMIT
            || !self.set_marker(kind, index, position, comparison, color)
        {
            return false;
        }
        match kind {
            ScopeMarkerKind::Time => self.horizontal_count += 1,
            ScopeMarkerKind::Voltage => self.vertical_count += 1,
        }
        true
    }

    fn remove_marker(&mut self, kind: ScopeMarkerKind, index: usize) -> bool {
        let count = self.count(kind);
        if index >= count {
            return false;
        }
        match kind {
            ScopeMarkerKind::Time => {
                self.horizontal_positions
                    .copy_within(index + 1..count, index);
                self.horizontal_comparisons
                    .copy_within(index + 1..count, index);
                self.horizontal_colors.copy_within(index + 1..count, index);
                self.horizontal_count -= 1;
                remap_marker_comparisons(
                    &mut self.horizontal_comparisons[..self.horizontal_count],
                    index,
                );
            }
            ScopeMarkerKind::Voltage => {
                self.vertical_positions.copy_within(index + 1..count, index);
                self.vertical_comparisons
                    .copy_within(index + 1..count, index);
                self.vertical_colors.copy_within(index + 1..count, index);
                self.vertical_count -= 1;
                remap_marker_comparisons(
                    &mut self.vertical_comparisons[..self.vertical_count],
                    index,
                );
            }
        }
        true
    }
}

fn remap_marker_comparisons(comparisons: &mut [Option<usize>], removed: usize) {
    for comparison in comparisons {
        *comparison = match *comparison {
            Some(index) if index == removed => None,
            Some(index) if index > removed => Some(index - 1),
            comparison => comparison,
        };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScopeMarkerKind {
    Time,
    Voltage,
}

impl ScopeMarkerKind {
    fn label(self) -> &'static str {
        match self {
            Self::Time => "Time (Δt)",
            Self::Voltage => "Amplitude (ΔV)",
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            Self::Time => "T",
            Self::Voltage => "V",
        }
    }
}

fn scope_marker_default_position(kind: ScopeMarkerKind, viewport: WaveformViewport) -> f32 {
    match kind {
        ScopeMarkerKind::Time => viewport.domain_fraction_at_screen(0.5).clamp(0.0, 1.0),
        ScopeMarkerKind::Voltage => 0.5,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeMarkerKey {
    channel: usize,
    kind: ScopeMarkerKind,
    index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeMarkerEditor {
    original: Option<ScopeMarkerKey>,
    channel: usize,
    kind: ScopeMarkerKind,
    position: f32,
    comparison: Option<usize>,
    color: Color32,
}

impl ScopeMarkerEditor {
    fn for_new(
        settings: ScopeMarkerSettings,
        viewport: WaveformViewport,
        channel: usize,
        kind: ScopeMarkerKind,
    ) -> Self {
        let channel_settings = settings.channels[channel];
        let index = channel_settings.count(kind);
        let color = match kind {
            ScopeMarkerKind::Time => {
                channel_settings.horizontal_colors[index.min(SCOPE_MARKER_LIMIT - 1)]
            }
            ScopeMarkerKind::Voltage => {
                channel_settings.vertical_colors[index.min(SCOPE_MARKER_LIMIT - 1)]
            }
        };
        Self {
            original: None,
            channel,
            kind,
            position: scope_marker_default_position(kind, viewport),
            comparison: None,
            color,
        }
    }

    fn for_existing(settings: ScopeMarkerSettings, key: ScopeMarkerKey) -> Option<Self> {
        let (position, comparison, color) =
            settings.channels[key.channel].marker(key.kind, key.index)?;
        Some(Self {
            original: Some(key),
            channel: key.channel,
            kind: key.kind,
            position,
            comparison,
            color,
        })
    }

    fn save(self, settings: &mut ScopeMarkerSettings) -> bool {
        let comparison = self
            .comparison
            .filter(|index| *index < settings.channels[self.channel].count(self.kind));
        match self.original {
            Some(key) if key.channel == self.channel && key.kind == self.kind => {
                settings.channels[self.channel].set_marker(
                    self.kind,
                    key.index,
                    self.position,
                    comparison.filter(|index| *index != key.index),
                    self.color,
                )
            }
            Some(key) => {
                let target_has_room = key.channel == self.channel
                    || settings.channels[self.channel].total_count() < SCOPE_MARKER_LIMIT;
                if !target_has_room
                    || settings.channels[self.channel].count(self.kind) >= SCOPE_MARKER_LIMIT
                {
                    return false;
                }
                if !settings.channels[key.channel].remove_marker(key.kind, key.index) {
                    return false;
                }
                settings.channels[self.channel].add_marker(
                    self.kind,
                    self.position,
                    comparison,
                    self.color,
                )
            }
            None => settings.channels[self.channel].add_marker(
                self.kind,
                self.position,
                comparison,
                self.color,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeMarkerSettings {
    channels: [ScopeChannelMarkerSettings; 2],
}

impl Default for ScopeMarkerSettings {
    fn default() -> Self {
        Self {
            channels: [
                ScopeChannelMarkerSettings::new(
                    0,
                    [0.05, 0.15, 0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85, 0.95],
                    [0.05, 0.15, 0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85, 0.95],
                ),
                ScopeChannelMarkerSettings::new(
                    1,
                    [0.10, 0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80, 0.90, 1.00],
                    [0.10, 0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80, 0.90, 1.00],
                ),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum ScopeQuickTimeMeasurement {
    #[default]
    Idle,
    AwaitingSecond {
        first_position: f32,
    },
    Complete {
        first_position: f32,
        second_position: f32,
    },
}

impl ScopeQuickTimeMeasurement {
    fn start(&mut self, position: f32) {
        *self = Self::AwaitingSecond {
            first_position: position.clamp(0.0, 1.0),
        };
    }

    fn finish(&mut self, position: f32) {
        if let Self::AwaitingSecond { first_position } = *self {
            *self = Self::Complete {
                first_position,
                second_position: position.clamp(0.0, 1.0),
            };
        }
    }

    fn clear(&mut self) {
        *self = Self::Idle;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogicTriggerMode {
    Simple,
    Smart,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LogicCaptureSettings {
    sample_rate_hz: u64,
    sample_count: u64,
    enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
    maximum_heights: [u8; LOGIC_CHANNEL_COUNT],
    operation_mode: DsLogicOperationMode,
    stop_option: DsLogicStopOption,
    threshold_voltage: f32,
    input_filter: DsLogicInputFilter,
    rle_enabled: bool,
    external_clock: bool,
    falling_clock_edge: bool,
    trigger_enabled: bool,
    trigger_mode: LogicTriggerMode,
    trigger_conditions: [Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT],
    smart_trigger_stage_count: u8,
    smart_trigger_conditions:
        [[Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT]; DSLOGIC_TRIGGER_STAGE_COUNT],
}

impl Default for LogicCaptureSettings {
    fn default() -> Self {
        let mut trigger_conditions = [None; LOGIC_CHANNEL_COUNT];
        trigger_conditions[0] = Some(DsLogicTriggerCondition::RisingEdge);
        let mut smart_trigger_conditions =
            [[None; LOGIC_CHANNEL_COUNT]; DSLOGIC_TRIGGER_STAGE_COUNT];
        smart_trigger_conditions[0][0] = Some(DsLogicTriggerCondition::RisingEdge);
        smart_trigger_conditions[1][0] = Some(DsLogicTriggerCondition::FallingEdge);
        Self {
            sample_rate_hz: SAMPLE_RATE_HZ,
            sample_count: SAMPLE_COUNT,
            enabled_channels: [true; LOGIC_CHANNEL_COUNT],
            maximum_heights: [1; LOGIC_CHANNEL_COUNT],
            operation_mode: DsLogicOperationMode::Stream,
            stop_option: DsLogicStopOption::UploadCapturedData,
            threshold_voltage: 1.0,
            input_filter: DsLogicInputFilter::None,
            rle_enabled: false,
            external_clock: false,
            falling_clock_edge: false,
            trigger_enabled: false,
            trigger_mode: LogicTriggerMode::Simple,
            trigger_conditions,
            smart_trigger_stage_count: 2,
            smart_trigger_conditions,
        }
    }
}

impl LogicCaptureSettings {
    fn capture_config(self) -> Result<DsLogicCaptureConfig, String> {
        let stream_mode = self.operation_mode == DsLogicOperationMode::Stream;
        let internal_test = self.operation_mode == DsLogicOperationMode::InternalTest;
        DsLogicCaptureConfig::new(
            if internal_test {
                100_000_000
            } else {
                self.sample_rate_hz
            },
            self.sample_count,
        )
        .map_err(|error| error.to_string())?
        .with_threshold_voltage(self.threshold_voltage)
        .map_err(|error| error.to_string())
        .map(|config| {
            config
                .with_operation_mode(self.operation_mode)
                .with_stop_option(if stream_mode {
                    DsLogicStopOption::StopImmediately
                } else {
                    self.stop_option
                })
                .with_input_filter(self.input_filter)
                .with_rle_enabled(self.rle_enabled && !stream_mode && !internal_test)
                .with_external_clock(self.external_clock)
                .with_falling_clock_edge(self.falling_clock_edge)
        })
        .and_then(|config| {
            let config = config
                .with_enabled_channels(if internal_test {
                    [true; LOGIC_CHANNEL_COUNT]
                } else {
                    self.enabled_channels
                })
                .map_err(|error| error.to_string())?;
            if !self.trigger_enabled {
                return Ok(config);
            }
            if internal_test {
                return Err("Logic triggering is unavailable in internal-test mode".to_owned());
            }
            let trigger = match self.trigger_mode {
                LogicTriggerMode::Simple => DsLogicTriggerConfig::Simple(simple_logic_trigger(
                    self.trigger_conditions,
                    self.enabled_channels,
                )?),
                LogicTriggerMode::Smart => {
                    if self.operation_mode != DsLogicOperationMode::Buffer {
                        return Err(
                            "Smart logic triggering requires buffered capture mode".to_owned()
                        );
                    }
                    let stage_count = usize::from(self.smart_trigger_stage_count)
                        .clamp(2, DSLOGIC_TRIGGER_STAGE_COUNT);
                    let mut trigger = DsLogicSmartTrigger::new(logic_trigger_stage(
                        self.smart_trigger_conditions[0],
                        self.enabled_channels,
                        0,
                    )?);
                    for stage_index in 1..stage_count {
                        trigger = trigger
                            .with_stage(logic_trigger_stage(
                                self.smart_trigger_conditions[stage_index],
                                self.enabled_channels,
                                stage_index,
                            )?)
                            .map_err(|error| error.to_string())?;
                    }
                    DsLogicTriggerConfig::Smart(trigger)
                }
            };
            Ok(config.with_trigger(Some(trigger)))
        })
    }

    fn maximum_sample_rate_hz(self, speed: UsbSpeedClass) -> u64 {
        let mut enabled_channels = self.enabled_channels;
        if !enabled_channels.iter().any(|enabled| *enabled) {
            enabled_channels[0] = true;
        }
        DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)
            .expect("default logic capture configuration is valid")
            .with_operation_mode(self.operation_mode)
            .with_enabled_channels(enabled_channels)
            .expect("at least one logic channel is enabled")
            .maximum_sample_rate_hz(speed)
    }

    fn maximum_sample_count(self) -> u64 {
        DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)
            .expect("default logic capture configuration is valid")
            .with_operation_mode(self.operation_mode)
            .maximum_sample_count()
    }
}

fn configured_logic_trigger_conditions(
    conditions: [Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT],
    enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
    stage_index: Option<usize>,
) -> Result<Vec<(usize, DsLogicTriggerCondition)>, String> {
    let configured = conditions
        .into_iter()
        .enumerate()
        .filter_map(|(channel, condition)| condition.map(|condition| (channel, condition)))
        .collect::<Vec<_>>();
    if configured.is_empty() {
        return Err(match stage_index {
            Some(stage) => format!(
                "Configure at least one condition in smart trigger stage {}",
                stage + 1
            ),
            None => "Configure at least one logic trigger condition".to_owned(),
        });
    }
    if let Some((channel, _)) = configured
        .iter()
        .find(|(channel, _)| !enabled_channels[*channel])
    {
        return Err(format!("Logic trigger channel {channel} must be enabled"));
    }
    Ok(configured)
}

fn simple_logic_trigger(
    conditions: [Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT],
    enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
) -> Result<DsLogicSimpleTrigger, String> {
    let mut configured =
        configured_logic_trigger_conditions(conditions, enabled_channels, None)?.into_iter();
    let (first_channel, first_condition) = configured
        .next()
        .expect("configured trigger conditions are non-empty");
    let mut trigger = DsLogicSimpleTrigger::new(first_channel as u8, first_condition)
        .map_err(|error| error.to_string())?;
    for (channel, condition) in configured {
        trigger = trigger
            .with_channel_condition(channel as u8, condition)
            .map_err(|error| error.to_string())?;
    }
    Ok(trigger)
}

fn logic_trigger_stage(
    conditions: [Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT],
    enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
    stage_index: usize,
) -> Result<DsLogicTriggerStage, String> {
    let mut configured =
        configured_logic_trigger_conditions(conditions, enabled_channels, Some(stage_index))?
            .into_iter();
    let (first_channel, first_condition) = configured
        .next()
        .expect("configured trigger stage is non-empty");
    let mut stage = DsLogicTriggerStage::new(first_channel as u8, first_condition)
        .map_err(|error| error.to_string())?;
    for (channel, condition) in configured {
        stage = stage
            .with_channel_condition(channel as u8, condition)
            .map_err(|error| error.to_string())?;
    }
    Ok(stage)
}

fn logic_trigger_condition_label(condition: DsLogicTriggerCondition) -> &'static str {
    match condition {
        DsLogicTriggerCondition::Low => "Low",
        DsLogicTriggerCondition::High => "High",
        DsLogicTriggerCondition::RisingEdge => "Rising edge",
        DsLogicTriggerCondition::FallingEdge => "Falling edge",
        DsLogicTriggerCondition::EitherEdge => "Either edge",
    }
}

fn logic_trigger_condition_key(condition: DsLogicTriggerCondition) -> &'static str {
    match condition {
        DsLogicTriggerCondition::Low => "low",
        DsLogicTriggerCondition::High => "high",
        DsLogicTriggerCondition::RisingEdge => "rising_edge",
        DsLogicTriggerCondition::FallingEdge => "falling_edge",
        DsLogicTriggerCondition::EitherEdge => "either_edge",
    }
}

fn parse_logic_trigger_condition(value: &str) -> Option<DsLogicTriggerCondition> {
    match value {
        "low" => Some(DsLogicTriggerCondition::Low),
        "high" => Some(DsLogicTriggerCondition::High),
        "rising_edge" => Some(DsLogicTriggerCondition::RisingEdge),
        "falling_edge" => Some(DsLogicTriggerCondition::FallingEdge),
        "either_edge" => Some(DsLogicTriggerCondition::EitherEdge),
        _ => None,
    }
}

fn aligned_logic_sample_count(
    duration_seconds: f64,
    sample_rate_hz: u64,
    maximum_sample_count: u64,
) -> Option<u64> {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 || sample_rate_hz == 0 {
        return None;
    }
    let requested = (duration_seconds * sample_rate_hz as f64).ceil();
    if requested < DSLOGIC_SAMPLE_ALIGNMENT as f64 || requested > u64::MAX as f64 {
        return None;
    }
    let aligned = (requested as u64).div_ceil(DSLOGIC_SAMPLE_ALIGNMENT) * DSLOGIC_SAMPLE_ALIGNMENT;
    (aligned <= maximum_sample_count).then_some(aligned)
}

fn logic_capture_duration_options(settings: LogicCaptureSettings) -> Vec<(f64, u64)> {
    let mut options = Vec::new();
    let maximum_sample_count = settings.maximum_sample_count();
    for duration_seconds in TIMEBASE_STEPS_SECONDS {
        let Some(sample_count) = aligned_logic_sample_count(
            duration_seconds,
            settings.sample_rate_hz,
            maximum_sample_count,
        ) else {
            continue;
        };
        if options
            .last()
            .is_some_and(|(_, previous_count)| *previous_count == sample_count)
        {
            continue;
        }
        options.push((duration_seconds, sample_count));
    }
    options
}

fn logic_capture_duration_seconds(settings: LogicCaptureSettings) -> f64 {
    logic_capture_duration_options(settings)
        .into_iter()
        .find_map(|(duration, sample_count)| {
            (sample_count == settings.sample_count).then_some(duration)
        })
        .unwrap_or(settings.sample_count as f64 / settings.sample_rate_hz as f64)
}

fn logic_capture_duration_label(settings: LogicCaptureSettings) -> String {
    format_duration(Duration::from_secs_f64(logic_capture_duration_seconds(
        settings,
    )))
}

fn set_logic_sample_rate_preserving_duration(
    settings: &mut LogicCaptureSettings,
    sample_rate_hz: u64,
) {
    if settings.sample_rate_hz == sample_rate_hz {
        return;
    }
    let duration_seconds = logic_capture_duration_seconds(*settings);
    settings.sample_rate_hz = sample_rate_hz;
    set_logic_capture_duration(settings, duration_seconds);
}

fn set_logic_operation_mode_preserving_duration(
    settings: &mut LogicCaptureSettings,
    operation_mode: DsLogicOperationMode,
) {
    if settings.operation_mode == operation_mode {
        return;
    }
    let duration_seconds = logic_capture_duration_seconds(*settings);
    settings.operation_mode = operation_mode;
    if operation_mode == DsLogicOperationMode::InternalTest {
        settings.sample_rate_hz = 100_000_000;
    }
    set_logic_capture_duration(settings, duration_seconds);
}

fn set_logic_capture_duration(settings: &mut LogicCaptureSettings, duration_seconds: f64) {
    let maximum_sample_count = settings.maximum_sample_count();
    settings.sample_count = aligned_logic_sample_count(
        duration_seconds,
        settings.sample_rate_hz,
        maximum_sample_count,
    )
    .unwrap_or_else(|| {
        if duration_seconds * (settings.sample_rate_hz as f64) < DSLOGIC_SAMPLE_ALIGNMENT as f64 {
            DSLOGIC_SAMPLE_ALIGNMENT
        } else {
            maximum_sample_count / DSLOGIC_SAMPLE_ALIGNMENT * DSLOGIC_SAMPLE_ALIGNMENT
        }
    });
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
            // Keep the analog frontend centered. `ScopeChannelSettings` keeps
            // the independently movable display offset used by the plot.
            *destination = DsCopeChannelConfig::new(
                source.volts_per_div_mv,
                source.coupling,
                source.probe_attenuation,
            )
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

    fn sample_rate_for_timebase(self, time_per_division_seconds: f64) -> u64 {
        if !time_per_division_seconds.is_finite() || time_per_division_seconds <= 0.0 {
            return self.sample_rate_hz;
        }
        let requested_rate = self.sample_count as f64
            / (time_per_division_seconds * SCOPE_HORIZONTAL_DIVISIONS as f64);
        let maximum_rate = self.maximum_sample_rate_hz();
        DSCOPE_SAMPLE_RATES_HZ
            .iter()
            .rev()
            .copied()
            .find(|rate| *rate <= maximum_rate && *rate as f64 <= requested_rate * (1.0 + 1.0e-9))
            .unwrap_or(DSCOPE_SAMPLE_RATES_HZ[0])
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
                })
            || self.trigger.hardware_differs_from(other.trigger)
    }

    fn runtime_frontend_differs_from(self, other: Self) -> bool {
        self.channels
            .iter()
            .zip(other.channels)
            .any(|(left, right)| {
                left.enabled != right.enabled || left.volts_per_div_mv != right.volts_per_div_mv
            })
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

fn capture_settings_tabs_for_view(
    active_waveform_tab: Option<WaveformTab>,
    has_logic_device: bool,
    has_scope_device: bool,
    has_logic_capture: bool,
    has_scope_capture: bool,
) -> Vec<CaptureSettingsTab> {
    let logic_available = has_logic_device || has_logic_capture;
    let scope_available = has_scope_device || has_scope_capture;
    match active_waveform_tab {
        Some(WaveformTab::LogicAnalyzer) => capture_settings_tabs(logic_available, false),
        Some(WaveformTab::Oscilloscope | WaveformTab::Lissajous) => {
            capture_settings_tabs(false, scope_available)
        }
        Some(WaveformTab::RadioReceiver) => Vec::new(),
        Some(WaveformTab::Combined) | None => {
            capture_settings_tabs(logic_available, scope_available)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureTarget {
    LogicAnalyzer,
    Oscilloscope,
    RadioReceiver,
    Combined,
}

impl CaptureTarget {
    fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "Logic Analyzer",
            Self::Oscilloscope => "Oscilloscope",
            Self::RadioReceiver => "Radio Receiver",
            Self::Combined => "Combined",
        }
    }

    fn is_available(
        self,
        has_logic_analyzer: bool,
        has_oscilloscope: bool,
        has_radio_receiver: bool,
    ) -> bool {
        match self {
            Self::LogicAnalyzer => has_logic_analyzer,
            Self::Oscilloscope => has_oscilloscope,
            Self::RadioReceiver => has_radio_receiver,
            Self::Combined => has_logic_analyzer && has_oscilloscope,
        }
    }

    fn includes_oscilloscope(self) -> bool {
        matches!(self, Self::Oscilloscope | Self::Combined)
    }

    fn includes_logic_analyzer(self) -> bool {
        matches!(self, Self::LogicAnalyzer | Self::Combined)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaveformTab {
    LogicAnalyzer,
    Oscilloscope,
    Lissajous,
    RadioReceiver,
    Combined,
}

impl WaveformTab {
    fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "Logic Analyzer",
            Self::Oscilloscope => "Oscilloscope",
            Self::Lissajous => "Lissajous",
            Self::RadioReceiver => "RF Spectrum",
            Self::Combined => "Combined",
        }
    }

    fn capture_target(self) -> CaptureTarget {
        match self {
            Self::LogicAnalyzer => CaptureTarget::LogicAnalyzer,
            Self::Oscilloscope | Self::Lissajous => CaptureTarget::Oscilloscope,
            Self::RadioReceiver => CaptureTarget::RadioReceiver,
            Self::Combined => CaptureTarget::Combined,
        }
    }

    fn capture_settings_tab(self) -> Option<CaptureSettingsTab> {
        match self {
            Self::LogicAnalyzer => Some(CaptureSettingsTab::DsLogic),
            Self::Oscilloscope | Self::Lissajous => Some(CaptureSettingsTab::DsCope),
            Self::RadioReceiver | Self::Combined => None,
        }
    }
}

fn waveform_tabs(
    has_logic_analyzer: bool,
    has_oscilloscope: bool,
    has_radio_receiver: bool,
    show_lissajous: bool,
) -> Vec<WaveformTab> {
    let mut tabs = Vec::with_capacity(5);
    if has_logic_analyzer {
        tabs.push(WaveformTab::LogicAnalyzer);
    }
    if has_oscilloscope {
        tabs.push(WaveformTab::Oscilloscope);
    }
    if has_oscilloscope && show_lissajous {
        tabs.push(WaveformTab::Lissajous);
    }
    if has_radio_receiver {
        tabs.push(WaveformTab::RadioReceiver);
    }
    if has_logic_analyzer && has_oscilloscope {
        tabs.push(WaveformTab::Combined);
    }
    tabs
}

fn scope_tools_panel_is_visible(active_tab: Option<WaveformTab>) -> bool {
    matches!(
        active_tab,
        Some(WaveformTab::Oscilloscope) | Some(WaveformTab::Lissajous)
    )
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

fn default_logic_channel_marker_opacities() -> [u8; LOGIC_CHANNEL_COUNT] {
    [DEFAULT_LOGIC_CHANNEL_MARKER_OPACITY; LOGIC_CHANNEL_COUNT]
}

#[derive(Clone, Debug, PartialEq)]
struct LogicChannelSettingsDraft {
    enabled: bool,
    maximum_height: u8,
    name: String,
    color: Color32,
    marker_opacity: u8,
}

fn default_logic_channel_settings_drafts() -> [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT] {
    let colors = default_logic_channel_colors();
    std::array::from_fn(|channel| LogicChannelSettingsDraft {
        enabled: true,
        maximum_height: 1,
        name: String::new(),
        color: colors[channel],
        marker_opacity: DEFAULT_LOGIC_CHANNEL_MARKER_OPACITY,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LogicMarker {
    name: String,
    color: Color32,
    start_sample: u64,
    end_sample: u64,
    sample_rate_hz: u64,
}

impl LogicMarker {
    fn new(
        first_sample: u64,
        second_sample: u64,
        sample_rate_hz: u64,
        color: Color32,
    ) -> Option<Self> {
        let (start_sample, end_sample) = if first_sample <= second_sample {
            (first_sample, second_sample)
        } else {
            (second_sample, first_sample)
        };
        (start_sample != end_sample && sample_rate_hz != 0).then_some(Self {
            name: String::new(),
            color,
            start_sample,
            end_sample,
            sample_rate_hz,
        })
    }

    fn duration(&self) -> Duration {
        Duration::from_secs_f64(
            self.end_sample.saturating_sub(self.start_sample) as f64 / self.sample_rate_hz as f64,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LogicTimingMeasurement {
    channel: usize,
    period_start_sample: u64,
    period_end_sample: u64,
    period_samples: u64,
    high_samples: u64,
}

impl LogicTimingMeasurement {
    fn period(self, sample_rate_hz: u64) -> Duration {
        Duration::from_secs_f64(self.period_samples as f64 / sample_rate_hz as f64)
    }

    fn frequency_hz(self, sample_rate_hz: u64) -> f64 {
        sample_rate_hz as f64 / self.period_samples as f64
    }

    fn duty_cycle_percent(self) -> f64 {
        self.high_samples as f64 / self.period_samples as f64 * 100.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LogicMarkerDraft {
    start_sample: u64,
    end_sample: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LogicMarkerSettings {
    markers: Vec<LogicMarker>,
    adding: bool,
    draft: Option<LogicMarkerDraft>,
}

impl LogicMarkerSettings {
    fn arm(&mut self) -> bool {
        if self.markers.len() >= LOGIC_MARKER_LIMIT {
            return false;
        }
        self.adding = true;
        self.draft = None;
        true
    }

    fn cancel(&mut self) {
        self.adding = false;
        self.draft = None;
    }

    fn start(&mut self, sample: u64) {
        if self.adding && self.markers.len() < LOGIC_MARKER_LIMIT {
            self.draft = Some(LogicMarkerDraft {
                start_sample: sample,
                end_sample: sample,
            });
        }
    }

    fn update(&mut self, sample: u64) {
        if let Some(draft) = &mut self.draft {
            draft.end_sample = sample;
        }
    }

    fn finish(&mut self, sample_rate_hz: u64) -> bool {
        let Some(draft) = self.draft.take() else {
            return false;
        };
        let Some(marker) = LogicMarker::new(
            draft.start_sample,
            draft.end_sample,
            sample_rate_hz,
            logic_marker_color(self.markers.len()),
        ) else {
            return false;
        };
        if self.markers.len() >= LOGIC_MARKER_LIMIT {
            self.cancel();
            return false;
        }
        self.markers.push(marker);
        self.adding = false;
        true
    }

    fn clear(&mut self) {
        self.markers.clear();
        self.cancel();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LogicSampleBucket {
    first: u32,
    last: u32,
    activity: u32,
    multiple_transitions: u32,
    sample_count: u32,
}

impl LogicSampleBucket {
    fn new(sample: u32) -> Self {
        Self {
            first: sample,
            last: sample,
            activity: 0,
            multiple_transitions: 0,
            sample_count: 1,
        }
    }

    fn from_samples(samples: &[u32]) -> Option<Self> {
        let first = *samples.first()?;
        let mut bucket = Self::new(first);
        bucket.push_samples(&samples[1..]);
        Some(bucket)
    }

    fn push_samples(&mut self, samples: &[u32]) {
        for sample in samples {
            let transitions = self.last ^ sample;
            self.multiple_transitions |= self.activity & transitions;
            self.activity |= self.first ^ sample;
            self.last = *sample;
        }
        self.sample_count += samples.len() as u32;
    }
}

struct WaveformStore {
    logic_samples: VecDeque<LogicSampleBucket>,
    logic_channels: u8,
    logic_rate_hz: u64,
    logic_revision: u64,
    logic_captured_at: Option<Duration>,
    logic_sample_offset: u64,
    logic_sample_count: usize,
    logic_sample_stride: usize,
    logic_capacity: usize,
    scope_channels: Vec<VecDeque<f32>>,
    scope_rate_hz: u64,
    scope_captured_at: Option<Duration>,
    scope_sample_offset: u64,
    scope_trigger_sample_index: Option<u64>,
    scope_capture_boundaries: VecDeque<u64>,
    scope_capacity: usize,
    scope_rolling: bool,
    scope_revision: u64,
    radio: RadioDisplayStore,
    radio_analysis: RadioAnalysisStore,
}

impl Default for WaveformStore {
    fn default() -> Self {
        Self {
            logic_samples: VecDeque::new(),
            logic_channels: 0,
            logic_rate_hz: 0,
            logic_revision: 0,
            logic_captured_at: None,
            logic_sample_offset: 0,
            logic_sample_count: 0,
            logic_sample_stride: 1,
            logic_capacity: MAX_WAVEFORM_SAMPLES,
            scope_channels: Vec::new(),
            scope_rate_hz: 0,
            scope_captured_at: None,
            scope_sample_offset: 0,
            scope_trigger_sample_index: None,
            scope_capture_boundaries: VecDeque::new(),
            scope_capacity: MAX_WAVEFORM_SAMPLES,
            scope_rolling: false,
            scope_revision: 0,
            radio: RadioDisplayStore::default(),
            radio_analysis: RadioAnalysisStore::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeMeasurementCacheKey {
    waveform_revision: u64,
    channel: usize,
    range_start: usize,
    range_end: usize,
    sample_rate_hz: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ScopeMeasurementCache {
    key: Option<ScopeMeasurementCacheKey>,
    results: ScopeMeasurementResults,
}

impl ScopeMeasurementCache {
    fn results(
        &mut self,
        waveforms: &WaveformStore,
        channel: usize,
        visible_range: Range<usize>,
        sample_rate_hz: u64,
    ) -> ScopeMeasurementResults {
        let key = ScopeMeasurementCacheKey {
            waveform_revision: waveforms.scope_revision,
            channel,
            range_start: visible_range.start,
            range_end: visible_range.end,
            sample_rate_hz,
        };
        if self.key != Some(key) {
            self.results = waveforms
                .scope_channels
                .get(channel)
                .map(|samples| calculate_scope_measurements(samples, visible_range, sample_rate_hz))
                .unwrap_or_default();
            self.key = Some(key);
        }
        self.results
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProtocolDecodeCacheKey {
    waveform_revision: u64,
    sample_offset: u64,
    sample_count: usize,
    analyzers: Vec<ProtocolAnalyzerInstance>,
}

#[derive(Default)]
struct ProtocolDecodeCache {
    key: Option<ProtocolDecodeCacheKey>,
    outcomes: Vec<ProtocolDecodeOutcome>,
    waveform_outcomes: Vec<ProtocolDecodeOutcome>,
}

impl ProtocolDecodeCache {
    fn outcomes<'a>(
        &'a mut self,
        waveforms: &WaveformStore,
        analyzers: &[ProtocolAnalyzerInstance],
        streamed: Option<&'a ProtocolDecodeSnapshot>,
    ) -> &'a [ProtocolDecodeOutcome] {
        if let Some(streamed) = streamed.filter(|decoder| decoder.matches_analyzers(analyzers)) {
            return streamed.outcomes();
        }
        self.refresh(waveforms, analyzers);
        &self.outcomes
    }

    fn waveform_outcomes<'a>(
        &'a mut self,
        waveforms: &WaveformStore,
        analyzers: &[ProtocolAnalyzerInstance],
        streamed: Option<&'a ProtocolDecodeSnapshot>,
    ) -> &'a [ProtocolDecodeOutcome] {
        if let Some(streamed) = streamed.filter(|decoder| decoder.matches_analyzers(analyzers)) {
            return streamed.waveform_outcomes();
        }
        self.refresh(waveforms, analyzers);
        &self.waveform_outcomes
    }

    fn refresh(&mut self, waveforms: &WaveformStore, analyzers: &[ProtocolAnalyzerInstance]) {
        let key = ProtocolDecodeCacheKey {
            waveform_revision: waveforms.logic_revision,
            sample_offset: waveforms.logic_sample_offset,
            sample_count: waveforms.logic_sample_count,
            analyzers: analyzers.to_vec(),
        };
        if self.key.as_ref() != Some(&key) {
            if waveforms.logic_sample_stride != 1
                || waveforms
                    .logic_samples
                    .iter()
                    .any(|bucket| bucket.sample_count != 1)
            {
                self.outcomes = analyzers
                    .iter()
                    .map(|_| {
                        ProtocolDecodeOutcome::Unavailable(
                            "Raw samples are unavailable for this display capture".to_owned(),
                        )
                    })
                    .collect();
                self.waveform_outcomes = self.outcomes.clone();
            } else {
                let samples = waveforms
                    .logic_samples
                    .iter()
                    .map(|bucket| bucket.first)
                    .collect::<Vec<_>>();
                self.outcomes = analyzers
                    .iter()
                    .map(|analyzer| {
                        decode_protocol_samples(
                            analyzer,
                            &samples,
                            waveforms.logic_sample_offset,
                            waveforms.logic_rate_hz,
                        )
                    })
                    .collect();
                self.waveform_outcomes = analyzers
                    .iter()
                    .map(|analyzer| {
                        decode_transport_samples(
                            analyzer,
                            &samples,
                            waveforms.logic_sample_offset,
                            waveforms.logic_rate_hz,
                        )
                    })
                    .collect();
            }
            self.key = Some(key);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedPanelEntry {
    start_sample: u64,
    end_sample: u64,
    title: String,
    detail: String,
    warning: bool,
}

struct I2cPanelTransaction {
    start_sample: u64,
    end_sample: u64,
    address: u8,
    read: bool,
    data: Vec<u8>,
    acknowledgements: Vec<bool>,
}

impl I2cPanelTransaction {
    fn entry(self) -> DecodedPanelEntry {
        let operation = if self.read { "READ" } else { "WRITE" };
        let response = |index| {
            self.acknowledgements
                .get(index)
                .map_or(
                    "no response",
                    |acknowledged| {
                        if *acknowledged {
                            "ACK"
                        } else {
                            "NACK"
                        }
                    },
                )
        };
        let mut details = vec![format!(
            "Address 0x{:02X} ({}) → {}",
            self.address,
            if self.read { "R" } else { "W" },
            response(0)
        )];
        if self.data.is_empty() {
            details.push("No data bytes".to_owned());
        } else {
            details.extend(
                self.data
                    .iter()
                    .enumerate()
                    .map(|(index, byte)| format!("Data 0x{byte:02X} → {}", response(index + 1))),
            );
        }
        DecodedPanelEntry {
            start_sample: self.start_sample,
            end_sample: self.end_sample,
            title: format!("I²C {operation} · address 0x{:02X}", self.address),
            detail: details.join("\n"),
            warning: self
                .acknowledgements
                .iter()
                .any(|acknowledged| !acknowledged),
        }
    }
}

fn decoded_panel_entries(events: &[ProtocolDecodedEvent]) -> Vec<DecodedPanelEntry> {
    let mut entries = Vec::new();
    let mut i2c_transaction: Option<I2cPanelTransaction> = None;
    for event in events {
        match event.kind {
            ProtocolDecodedKind::I2cAddress { address, read } => {
                if let Some(transaction) = i2c_transaction.take() {
                    entries.push(transaction.entry());
                }
                i2c_transaction = Some(I2cPanelTransaction {
                    start_sample: event.start_sample,
                    end_sample: event.end_sample,
                    address,
                    read,
                    data: Vec::new(),
                    acknowledgements: Vec::new(),
                });
            }
            ProtocolDecodedKind::Data { value } => {
                if let Some(transaction) = &mut i2c_transaction {
                    transaction.end_sample = event.end_sample;
                    transaction.data.push(value);
                } else {
                    entries.push(DecodedPanelEntry {
                        start_sample: event.start_sample,
                        end_sample: event.end_sample,
                        title: event.text.clone(),
                        detail: event.detail.clone(),
                        warning: event.warning,
                    });
                }
            }
            ProtocolDecodedKind::Acknowledge { acknowledged } => {
                if let Some(transaction) = &mut i2c_transaction {
                    transaction.end_sample = event.end_sample;
                    transaction.acknowledgements.push(acknowledged);
                }
            }
            ProtocolDecodedKind::Spi { .. }
            | ProtocolDecodedKind::Uart { .. }
            | ProtocolDecodedKind::Custom => {
                if let Some(transaction) = i2c_transaction.take() {
                    entries.push(transaction.entry());
                }
                entries.push(DecodedPanelEntry {
                    start_sample: event.start_sample,
                    end_sample: event.end_sample,
                    title: event.text.clone(),
                    detail: event.detail.clone(),
                    warning: event.warning,
                });
            }
        }
    }
    if let Some(transaction) = i2c_transaction {
        entries.push(transaction.entry());
    }
    entries.reverse();
    entries
}

fn uart_ascii_text(events: &[ProtocolDecodedEvent]) -> Option<String> {
    let bytes = events
        .iter()
        .filter_map(|event| match event.kind {
            ProtocolDecodedKind::Uart { value } => Some(value),
            ProtocolDecodedKind::I2cAddress { .. }
            | ProtocolDecodedKind::Data { .. }
            | ProtocolDecodedKind::Acknowledge { .. }
            | ProtocolDecodedKind::Spi { .. }
            | ProtocolDecodedKind::Custom => None,
        })
        .collect::<Vec<_>>();
    (!bytes.is_empty()).then(|| {
        bytes
            .into_iter()
            .map(|byte| match byte {
                b'\n' => "\n".to_owned(),
                b'\r' => "\r".to_owned(),
                b'\t' => "\t".to_owned(),
                byte if byte.is_ascii_graphic() || byte == b' ' => char::from(byte).to_string(),
                byte => format!("\\x{byte:02X}"),
            })
            .collect::<String>()
    })
}

fn focus_logic_sample_range(
    viewport: &mut WaveformViewport,
    sample_offset: u64,
    sample_count: usize,
    start_sample: u64,
    end_sample: u64,
) {
    if sample_count == 0 {
        return;
    }
    let local_start = start_sample.saturating_sub(sample_offset) as f64;
    let local_end = end_sample.saturating_sub(sample_offset) as f64;
    let center = ((local_start + local_end) * 0.5 / sample_count as f64).clamp(0.0, 1.0);
    let event_span = ((local_end - local_start).max(1.0) / sample_count as f64) as f32;
    viewport.span = (event_span * 4.0).clamp(1.0 / sample_count as f32, 1.0);
    viewport.start = viewport.clamp_start(center as f32 - viewport.span * 0.5);
    viewport.anchor_locked = true;
}

fn protocol_event_elapsed(start_sample: u64, sample_rate_hz: u64) -> Option<Duration> {
    (sample_rate_hz != 0)
        .then(|| Duration::from_secs_f64(start_sample as f64 / sample_rate_hz as f64))
}

fn decoded_panel_entry_id(
    ui: &egui::Ui,
    analyzer: &ProtocolAnalyzerInstance,
    entry: &DecodedPanelEntry,
) -> egui::Id {
    ui.make_persistent_id((
        "decoded_panel_entry",
        analyzer.instance_id,
        entry.start_sample,
        entry.end_sample,
        entry.title.as_str(),
    ))
}

fn paint_decoded_analyzer_messages(
    ui: &mut egui::Ui,
    analyzer: &ProtocolAnalyzerInstance,
    events: &[ProtocolDecodedEvent],
    entries: &[DecodedPanelEntry],
    open_all: Option<bool>,
    sample_rate_hz: u64,
) -> Option<(u64, u64)> {
    let mut selected_range = None;
    if analyzer.data_format == ProtocolDataFormat::Ascii {
        if let Some(text) = uart_ascii_text(events) {
            ui.add_space(5.0);
            ui.label(RichText::new("TEXT").small().strong().color(TEXT_MUTED));
            egui::Frame::none()
                .fill(BACKGROUND)
                .rounding(egui::Rounding::same(5.0))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(text).monospace()).wrap());
                });
        }
    }
    if entries.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new("No decoded operations").color(TEXT_MUTED));
        return None;
    }
    ui.add_space(6.0);
    for entry in entries {
        let card_width = ui.available_width();
        let entry_id = decoded_panel_entry_id(ui, analyzer, entry);
        let mut open = open_all
            .unwrap_or_else(|| ui.data(|data| data.get_temp::<bool>(entry_id).unwrap_or(false)));
        if open_all.is_some() {
            ui.data_mut(|data| data.insert_temp(entry_id, open));
        }
        let time = protocol_event_elapsed(entry.start_sample, sample_rate_hz)
            .map(format_duration)
            .unwrap_or_default();
        let color = if entry.warning {
            Color32::LIGHT_RED
        } else {
            Color32::WHITE
        };
        let card = egui::Frame::none()
            .fill(BACKGROUND)
            .rounding(egui::Rounding::same(5.0))
            .inner_margin(egui::Margin::same(8.0))
            .show(ui, |ui| {
                let content_width = (card_width - 16.0).max(0.0);
                ui.set_width(content_width);
                ui.set_max_width(content_width);
                let header = ui.horizontal(|ui| {
                    ui.label(RichText::new(if open { "▼" } else { "▶" }).color(TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        ui.label(RichText::new(&time).small().monospace().color(TEXT_MUTED));
                        ui.add(
                            egui::Label::new(RichText::new(&entry.title).strong().color(color))
                                .wrap()
                                .halign(egui::Align::Min),
                        );
                    });
                });
                let header_response = ui
                    .interact(
                        header.response.rect,
                        entry_id.with("header"),
                        Sense::click(),
                    )
                    .on_hover_text("Expand or collapse message data");
                if header_response.clicked() {
                    open = !open;
                    ui.data_mut(|data| data.insert_temp(entry_id, open));
                }
                if open {
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(RichText::new(&entry.detail).small().monospace())
                            .wrap()
                            .sense(Sense::click()),
                    )
                    .on_hover_text("Click to focus this message in the waveform")
                    .clicked()
                } else {
                    false
                }
            });
        if card.inner {
            selected_range = Some((entry.start_sample, entry.end_sample));
        }
        ui.add_space(5.0);
    }
    selected_range
}

fn paint_decoded_data_panel(
    ui: &mut egui::Ui,
    analyzers: &[ProtocolAnalyzerInstance],
    outcomes: &[ProtocolDecodeOutcome],
    sample_rate_hz: u64,
) -> Option<(u64, u64)> {
    let mut selected_range = None;
    if analyzers.is_empty() {
        ui.label(RichText::new("No protocol analyzer configured").color(TEXT_MUTED));
        ui.add_space(4.0);
        ui.small("Add an analyzer in Capture Setup to decode transactions.");
        return None;
    }
    for (analyzer_index, (analyzer, outcome)) in analyzers.iter().zip(outcomes).enumerate() {
        if analyzer_index != 0 {
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);
        }
        let name = analyzer_name(analyzer);
        match outcome {
            ProtocolDecodeOutcome::Unavailable(message) => {
                ui.label(
                    RichText::new(name)
                        .strong()
                        .color(logic_marker_color(analyzer_index)),
                );
                ui.add_space(4.0);
                ui.label(RichText::new(message).color(TEXT_MUTED));
            }
            ProtocolDecodeOutcome::Events(events) => {
                let entries = decoded_panel_entries(events);
                let all_open = !entries.is_empty()
                    && entries.iter().all(|entry| {
                        let entry_id = decoded_panel_entry_id(ui, analyzer, entry);
                        ui.data(|data| data.get_temp::<bool>(entry_id).unwrap_or(false))
                    });
                let mut open_all = None;
                let remaining_analyzers = analyzers.len() - analyzer_index;
                let remaining_separator_height =
                    17.0 * remaining_analyzers.saturating_sub(1) as f32;
                let analyzer_height = ((ui.available_height() - remaining_separator_height)
                    / remaining_analyzers as f32)
                    .max(DECODED_PANEL_ACTION_BUTTON_SIZE[1] + 32.0);
                let header = ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(name)
                            .strong()
                            .color(logic_marker_color(analyzer_index)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let response = ui
                            .add_enabled_ui(!entries.is_empty(), |ui| {
                                ui.add_sized(
                                    DECODED_PANEL_ACTION_BUTTON_SIZE,
                                    toolbar_action_button(if all_open { "⊟" } else { "⊞" }, ACCENT),
                                )
                            })
                            .inner
                            .on_hover_text(if all_open {
                                "Collapse all messages for this analyzer"
                            } else {
                                "Expand all messages for this analyzer"
                            });
                        if response.clicked() {
                            open_all = Some(!all_open);
                        }
                    });
                });
                let message_area_height =
                    (analyzer_height - header.response.rect.height()).max(24.0);
                let clicked_range = egui::ScrollArea::vertical()
                    .id_salt(("decoded_analyzer_messages", analyzer.instance_id))
                    .max_height(message_area_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        paint_decoded_analyzer_messages(
                            ui,
                            analyzer,
                            events,
                            &entries,
                            open_all,
                            sample_rate_hz,
                        )
                    })
                    .inner;
                if clicked_range.is_some() {
                    selected_range = clicked_range;
                }
            }
        }
    }
    selected_range
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcquisitionMode {
    Single,
    Repetitive,
    Loop,
    Run,
}

impl AcquisitionMode {
    fn runs_continuously(self) -> bool {
        matches!(self, Self::Repetitive | Self::Loop | Self::Run)
    }

    fn logic_label(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::Repetitive => "Repetitive",
            Self::Loop => "Loop",
            Self::Run => "Run",
        }
    }

    fn rearms_on_completion(self) -> bool {
        matches!(self, Self::Repetitive | Self::Run)
    }
}

fn repetitive_interval(operation_mode: DsLogicOperationMode, configured_seconds: f32) -> Duration {
    if operation_mode == DsLogicOperationMode::Stream {
        return Duration::from_millis(100);
    }
    let tenths = (configured_seconds.clamp(0.1, 10.0) * 10.0).round() as u64;
    Duration::from_millis(tenths * 100)
}

fn should_rearm_acquisition(
    mode: AcquisitionMode,
    run_requested: bool,
    completion: CompletionStatus,
    failed: bool,
) -> bool {
    mode.rearms_on_completion()
        && run_requested
        && completion == CompletionStatus::Complete
        && !failed
}

fn uses_rolling_scope_buffer(
    _mode: AcquisitionMode,
    _has_scope: bool,
    _trigger_source: DsCopeTriggerSource,
) -> bool {
    // Original DSView enters Roll only when the FPGA measurement footer sets
    // its stream-mode bit. Auto is a trigger source, not a request to append
    // independent acquisitions into one waveform. ScopeBatch does not expose
    // hardware stream mode yet, so keep every run acquisition frame-based.
    false
}

fn is_scope_frame_start(sequence: u64) -> bool {
    sequence % DSCOPE_FRAME_SEQUENCE_STRIDE == 0
}

fn scope_rolling_sample_capacity(
    config: DsCopeCaptureConfig,
    time_per_division_seconds: Option<f64>,
) -> usize {
    let fallback = config.sample_count() as usize;
    let Some(time_per_division_seconds) =
        time_per_division_seconds.filter(|seconds| seconds.is_finite() && *seconds > 0.0)
    else {
        return fallback;
    };
    let maximum = SCOPE_ROLLING_MEMORY_BUDGET_BYTES
        / config.enabled_channel_count().max(1)
        / std::mem::size_of::<f32>();
    let visible_samples = (time_per_division_seconds
        * SCOPE_HORIZONTAL_DIVISIONS as f64
        * config.sample_rate_hz() as f64)
        .ceil();
    visible_samples.clamp(1.0, maximum as f64) as usize
}

fn scope_trigger_controls_are_enabled(
    background_operation_pending: bool,
    capture_run_requested: bool,
    selected_scope_device: bool,
    active_capture_supports_runtime_trigger: Option<bool>,
) -> bool {
    if capture_run_requested && selected_scope_device {
        return true;
    }
    !background_operation_pending && active_capture_supports_runtime_trigger.unwrap_or(true)
}

fn scope_frontend_controls_are_enabled(
    background_operation_pending: bool,
    capture_run_requested: bool,
    selected_scope_device: bool,
) -> bool {
    !background_operation_pending || (capture_run_requested && selected_scope_device)
}

fn manual_calibration_control_is_enabled(
    background_operation_pending: bool,
    selected_scope_device: bool,
    editor_open: bool,
    active_mode: Option<AcquisitionMode>,
) -> bool {
    !background_operation_pending
        && selected_scope_device
        && !editor_open
        && active_mode.map_or(true, AcquisitionMode::runs_continuously)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WaveformViewport {
    start: f32,
    span: f32,
    anchor_locked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CaptureTimeRange {
    start_seconds: f64,
    end_seconds: f64,
}

impl CaptureTimeRange {
    fn duration_seconds(self) -> f64 {
        (self.end_seconds - self.start_seconds).max(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CombinedTimeline {
    shared: CaptureTimeRange,
    logic: CaptureTimeRange,
    scope: CaptureTimeRange,
}

impl CombinedTimeline {
    fn source_viewport(
        self,
        shared_viewport: WaveformViewport,
        source: CaptureTimeRange,
    ) -> WaveformViewport {
        let source_duration = source.duration_seconds();
        let shared_duration = self.shared.duration_seconds();
        if source_duration <= 0.0 || shared_duration <= 0.0 {
            return WaveformViewport::default();
        }
        let visible_start =
            self.shared.start_seconds + f64::from(shared_viewport.start) * shared_duration;
        WaveformViewport {
            start: ((visible_start - source.start_seconds) / source_duration) as f32,
            span: (f64::from(shared_viewport.span) * shared_duration / source_duration) as f32,
            anchor_locked: shared_viewport.anchor_locked,
        }
    }

    fn shared_viewport(
        self,
        source_viewport: WaveformViewport,
        source: CaptureTimeRange,
    ) -> WaveformViewport {
        let source_duration = source.duration_seconds();
        let shared_duration = self.shared.duration_seconds();
        if source_duration <= 0.0 || shared_duration <= 0.0 {
            return WaveformViewport::default();
        }
        let visible_start =
            source.start_seconds + f64::from(source_viewport.start) * source_duration;
        let mut viewport = WaveformViewport {
            start: ((visible_start - self.shared.start_seconds) / shared_duration) as f32,
            span: (f64::from(source_viewport.span) * source_duration / shared_duration) as f32,
            anchor_locked: source_viewport.anchor_locked,
        };
        viewport.span = viewport.span.clamp(f32::EPSILON, 1.0);
        viewport.start = viewport.clamp_start(viewport.start);
        viewport
    }
}

impl Default for WaveformViewport {
    fn default() -> Self {
        Self {
            start: 0.0,
            span: 1.0,
            anchor_locked: true,
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
    ) -> bool {
        let cursor_fraction = cursor_fraction.clamp(0.0, 1.0);
        let sample_fraction = self.start + cursor_fraction * self.span;
        let changed = self.step_timebase_at_sample(
            scroll_delta,
            sample_count,
            sample_rate_hz,
            sample_fraction,
            cursor_fraction,
        );
        if changed {
            self.anchor_locked = false;
        }
        changed
    }

    fn sample_range(self, sample_count: usize) -> Range<usize> {
        if sample_count == 0 {
            return 0..0;
        }
        let visible_start = f64::from(self.start).max(0.0);
        let visible_end = (f64::from(self.start) + f64::from(self.span)).min(1.0);
        if visible_start >= visible_end {
            return 0..0;
        }
        let start = ((visible_start * sample_count as f64).floor() as usize).min(sample_count - 1);
        let mut end = (visible_end * sample_count as f64).ceil() as usize;
        end = end.clamp(start + 1, sample_count);
        start..end
    }

    fn sample_range_in_domain(
        self,
        available_sample_count: usize,
        domain_sample_count: usize,
    ) -> Range<usize> {
        if available_sample_count == 0 || domain_sample_count == 0 {
            return 0..0;
        }
        let visible_start = f64::from(self.start).max(0.0);
        let visible_end = (f64::from(self.start) + f64::from(self.span)).min(1.0);
        if visible_start >= visible_end {
            return 0..0;
        }
        let start = ((visible_start * domain_sample_count as f64).floor() as usize)
            .min(available_sample_count);
        let end = ((visible_end * domain_sample_count as f64).ceil() as usize)
            .min(available_sample_count);
        start..end.max(start)
    }

    fn zoom_factor(self) -> f32 {
        1.0 / self.span
    }

    fn domain_fraction_at_screen(self, screen_fraction: f32) -> f32 {
        self.start + screen_fraction * self.span
    }

    fn screen_fraction_at_domain(self, domain_fraction: f32) -> f32 {
        (domain_fraction - self.start) / self.span.max(f32::EPSILON)
    }

    fn move_screen_anchor(&mut self, old_fraction: f32, new_fraction: f32) {
        let sample_anchor = self.start + old_fraction.clamp(0.0, 1.0) * self.span;
        self.start = self.clamp_start(sample_anchor - new_fraction.clamp(0.0, 1.0) * self.span);
        self.anchor_locked = true;
    }

    fn align_sample_to_screen(&mut self, sample_fraction: f32, screen_fraction: f32) {
        self.start =
            self.clamp_start(sample_fraction - screen_fraction.clamp(0.0, 1.0) * self.span);
    }

    fn align_trigger_to_screen(&mut self, sample_fraction: f32, screen_fraction: f32) {
        self.start =
            self.clamp_start(sample_fraction - screen_fraction.clamp(0.0, 1.0) * self.span);
    }

    fn clamp_start(self, start: f32) -> f32 {
        let first = 0.0_f32.min(1.0 - self.span);
        let last = 0.0_f32.max(1.0 - self.span);
        start.clamp(first, last)
    }

    fn pan_by_screen_delta(&mut self, delta_x: f32, plot_width: f32) -> bool {
        if delta_x == 0.0 || plot_width <= 0.0 {
            return false;
        }
        let previous_start = self.start;
        self.start = self.clamp_start(self.start - delta_x / plot_width * self.span);
        if (self.start - previous_start).abs() <= f32::EPSILON {
            return false;
        }
        self.anchor_locked = false;
        true
    }

    fn step_timebase_at_sample(
        &mut self,
        scroll_delta: f32,
        sample_count: usize,
        sample_rate_hz: u64,
        sample_fraction: f32,
        screen_fraction: f32,
    ) -> bool {
        if scroll_delta == 0.0 {
            return false;
        }
        let Some(current_timebase) = self.time_per_division(sample_count, sample_rate_hz) else {
            return false;
        };
        let Some(timebase) = adjacent_timebase(current_timebase, scroll_delta > 0.0) else {
            return false;
        };
        let full_duration = sample_count as f64 / sample_rate_hz as f64;
        let minimum_span = (1.0 / sample_count as f64).min(1.0);
        let new_span = (timebase * SCOPE_HORIZONTAL_DIVISIONS as f64 / full_duration)
            .clamp(minimum_span, 1.0) as f32;
        if (new_span - self.span).abs() <= f32::EPSILON {
            return false;
        }
        self.span = new_span;
        self.align_sample_to_screen(sample_fraction, screen_fraction);
        self.anchor_locked = true;
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeFftViewport {
    start: f32,
    span: f32,
}

impl Default for ScopeFftViewport {
    fn default() -> Self {
        Self {
            start: 0.0,
            span: 1.0,
        }
    }
}

impl ScopeFftViewport {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn zoom(&mut self, scroll_delta: f32, pointer_fraction: f32, minimum_span: f32) {
        if scroll_delta == 0.0 {
            return;
        }
        let pointer_fraction = pointer_fraction.clamp(0.0, 1.0);
        let anchor = self.start + pointer_fraction * self.span;
        let factor = if scroll_delta > 0.0 { 0.8 } else { 1.25 };
        self.span = (self.span * factor).clamp(minimum_span, 1.0);
        self.start = (anchor - pointer_fraction * self.span).clamp(0.0, 1.0 - self.span);
    }

    fn pan(&mut self, delta_x: f32, plot_width: f32) {
        if plot_width <= 0.0 {
            return;
        }
        self.start = (self.start - delta_x / plot_width * self.span).clamp(0.0, 1.0 - self.span);
    }
}

fn apply_time_per_division(
    viewport: &mut WaveformViewport,
    sample_count: usize,
    sample_rate_hz: u64,
    time_per_division_seconds: f64,
) {
    if sample_count == 0 || sample_rate_hz == 0 || !time_per_division_seconds.is_finite() {
        return;
    }
    let new_span =
        (time_per_division_seconds * SCOPE_HORIZONTAL_DIVISIONS as f64 * sample_rate_hz as f64
            / sample_count as f64)
            .clamp(1.0 / sample_count as f64, 1.0) as f32;
    if (new_span - viewport.span).abs() <= f32::EPSILON {
        return;
    }
    let center_sample_fraction = viewport.start + viewport.span * 0.5;
    viewport.span = new_span;
    viewport.start = viewport.clamp_start(center_sample_fraction - new_span * 0.5);
}

fn resolve_scope_timebase(
    selected_timebase: Option<f64>,
    displayed_timebase: Option<f64>,
    requested_timebase: Option<f64>,
    viewport_span: f32,
    auto_sample_rate: bool,
) -> Option<f64> {
    requested_timebase.or_else(|| {
        if auto_sample_rate
            && viewport_span >= 1.0 - f32::EPSILON
            && selected_timebase
                .zip(displayed_timebase)
                .is_some_and(|(selected, displayed)| selected > displayed * (1.0 + 1.0e-9))
        {
            selected_timebase
        } else {
            displayed_timebase
        }
    })
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
    fn logic_capture_layout(expected_sample_count: u64) -> (usize, usize) {
        let expected_sample_count = usize::try_from(expected_sample_count).unwrap_or(usize::MAX);
        let stride = expected_sample_count
            .div_ceil(MAX_WAVEFORM_SAMPLES)
            .clamp(1, u32::MAX as usize);
        let capacity = expected_sample_count
            .div_ceil(stride)
            .clamp(1, MAX_WAVEFORM_SAMPLES);
        (capacity, stride)
    }

    fn logic_time_range(&self) -> Option<CaptureTimeRange> {
        let captured_at = self.logic_captured_at?.as_secs_f64();
        if self.logic_rate_hz == 0 || self.logic_sample_count == 0 {
            return None;
        }
        let rate = self.logic_rate_hz as f64;
        let start_seconds = captured_at + self.logic_sample_offset as f64 / rate;
        Some(CaptureTimeRange {
            start_seconds,
            end_seconds: start_seconds + self.logic_sample_count as f64 / rate,
        })
    }

    fn scope_time_range(&self) -> Option<CaptureTimeRange> {
        let captured_at = self.scope_captured_at?.as_secs_f64();
        if self.scope_rate_hz == 0 {
            return None;
        }
        let sample_count = self
            .scope_channels
            .iter()
            .filter(|samples| !samples.is_empty())
            .map(VecDeque::len)
            .min()?;
        let rate = self.scope_rate_hz as f64;
        let start_seconds = captured_at + self.scope_sample_offset as f64 / rate;
        Some(CaptureTimeRange {
            start_seconds,
            end_seconds: start_seconds + sample_count as f64 / rate,
        })
    }

    fn combined_timeline(&self) -> Option<CombinedTimeline> {
        let logic = self.logic_time_range()?;
        let scope = self.scope_time_range()?;
        let shared = CaptureTimeRange {
            start_seconds: logic.start_seconds.max(scope.start_seconds),
            end_seconds: logic.end_seconds.min(scope.end_seconds),
        };
        (shared.duration_seconds() > 0.0).then_some(CombinedTimeline {
            shared,
            logic,
            scope,
        })
    }

    fn configure_logic_capture(&mut self, expected_sample_count: u64) {
        (self.logic_capacity, self.logic_sample_stride) =
            Self::logic_capture_layout(expected_sample_count);
        self.logic_samples.clear();
        self.logic_revision = LOGIC_WAVEFORM_REVISION.fetch_add(1, Ordering::Relaxed);
        self.logic_captured_at = None;
        self.logic_sample_offset = 0;
        self.logic_sample_count = 0;
    }

    fn can_continue_logic_loop(&self, config: DsLogicCaptureConfig) -> bool {
        let (capacity, stride) = Self::logic_capture_layout(config.sample_count());
        self.logic_rate_hz == config.sample_rate_hz()
            && self.logic_capacity == capacity
            && self.logic_sample_stride == stride
    }

    fn ingest_logic_samples(&mut self, samples: &[u32]) {
        self.logic_revision = LOGIC_WAVEFORM_REVISION.fetch_add(1, Ordering::Relaxed);
        let mut remaining = samples;
        if let Some(bucket) = self.logic_samples.back_mut() {
            let available = self
                .logic_sample_stride
                .saturating_sub(bucket.sample_count as usize);
            let append_count = available.min(remaining.len());
            bucket.push_samples(&remaining[..append_count]);
            remaining = &remaining[append_count..];
        }
        for chunk in remaining.chunks(self.logic_sample_stride) {
            if let Some(bucket) = LogicSampleBucket::from_samples(chunk) {
                self.logic_samples.push_back(bucket);
            }
        }
        self.logic_sample_count = self.logic_sample_count.saturating_add(samples.len());

        while self.logic_samples.len() > self.logic_capacity {
            if let Some(removed) = self.logic_samples.pop_front() {
                let removed_count = removed.sample_count as usize;
                self.logic_sample_count = self.logic_sample_count.saturating_sub(removed_count);
                self.logic_sample_offset = self
                    .logic_sample_offset
                    .saturating_add(removed_count as u64);
            }
        }
    }

    fn can_continue_scope_roll(&self, config: DsCopeCaptureConfig, scope_capacity: usize) -> bool {
        self.scope_rolling
            && self.scope_rate_hz == config.sample_rate_hz()
            && self.scope_capacity == scope_capacity
            && self
                .scope_channels
                .iter()
                .map(|samples| !samples.is_empty())
                .eq(config.channels().iter().map(|channel| channel.enabled()))
    }

    fn mark_scope_capture_start(&mut self, scope_channels: [bool; 2]) {
        let sample_count = self
            .scope_channels
            .iter()
            .zip(scope_channels)
            .filter_map(|(samples, enabled)| enabled.then_some(samples.len() as u64))
            .min()
            .unwrap_or(0);
        if sample_count > 0 {
            self.scope_capture_boundaries
                .push_back(self.scope_sample_offset + sample_count);
        }
    }

    fn ingest_frame(&mut self, frame: &CaptureFrame, scope_channels: [bool; 2]) {
        match &frame.payload {
            CaptureBatch::Logic(_) => {
                self.logic_captured_at.get_or_insert(frame.captured_at);
            }
            CaptureBatch::Scope(_) => {
                self.scope_captured_at.get_or_insert(frame.captured_at);
            }
            CaptureBatch::Iq(_) => {}
        }
        self.ingest(&frame.payload, scope_channels);
    }

    fn ingest(&mut self, batch: &CaptureBatch, scope_channels: [bool; 2]) {
        match batch {
            CaptureBatch::Logic(logic) => {
                self.logic_channels = logic.channel_count;
                self.logic_rate_hz = logic.sample_rate_hz;
                self.ingest_logic_samples(&logic.samples);
            }
            CaptureBatch::Scope(scope) => {
                self.scope_revision = SCOPE_WAVEFORM_REVISION.fetch_add(1, Ordering::Relaxed);
                self.scope_rate_hz = scope.sample_rate_hz;
                if self.scope_channels.len() != scope_channels.len() {
                    self.scope_channels = vec![VecDeque::new(); scope_channels.len()];
                    self.scope_sample_offset = 0;
                    self.scope_capture_boundaries.clear();
                }
                let capture_start = self.scope_sample_offset
                    + self
                        .scope_channels
                        .iter()
                        .zip(scope_channels)
                        .filter_map(|(samples, enabled)| enabled.then_some(samples.len() as u64))
                        .min()
                        .unwrap_or(0);
                if let Some(trigger_sample_index) = scope.trigger_sample_index {
                    self.scope_trigger_sample_index =
                        Some(capture_start.saturating_add(trigger_sample_index));
                }
                let mut removed_samples = 0;
                for (source, physical_channel) in scope.channels.iter().zip(
                    scope_channels
                        .into_iter()
                        .enumerate()
                        .filter_map(|(index, enabled)| enabled.then_some(index)),
                ) {
                    let destination = &mut self.scope_channels[physical_channel];
                    removed_samples = removed_samples.max(extend_bounded_to(
                        destination,
                        source.iter().copied(),
                        self.scope_capacity,
                    ));
                }
                self.scope_sample_offset += removed_samples as u64;
                while self
                    .scope_capture_boundaries
                    .front()
                    .is_some_and(|boundary| *boundary <= self.scope_sample_offset)
                {
                    self.scope_capture_boundaries.pop_front();
                }
            }
            CaptureBatch::Iq(_) => {}
        }
    }
}

fn extend_bounded_to<T>(
    destination: &mut VecDeque<T>,
    values: impl IntoIterator<Item = T>,
    capacity: usize,
) -> usize {
    destination.extend(values);
    let excess = destination.len().saturating_sub(capacity);
    destination.drain(..excess);
    excess
}

fn ingest_radio_iq(
    waveforms: &mut WaveformStore,
    spectrum: &mut SpectrumProcessor,
    analyzer: &mut Option<SignalAnalyzer>,
    iq: &wavecraft_core::IqBatch,
) {
    for frame in spectrum.push(iq) {
        waveforms.radio.ingest(frame);
    }
    let analysis = analyzer.as_mut().map(|analyzer| analyzer.push(iq));
    if let Some(analysis) = analysis {
        let analysis = analysis
            .map_err(|error| error.to_string())
            .and_then(|output| waveforms.radio_analysis.ingest(output));
        if let Err(error) = analysis {
            waveforms.radio_analysis.set_error(error);
            *analyzer = None;
        }
    }
}

struct ActiveCapture {
    session: CaptureSession,
    mode: AcquisitionMode,
    target: CaptureTarget,
    preserve_display_until_complete: bool,
    scope_config: DsCopeCaptureConfig,
    scope_trigger_controls: Vec<DsCopeRuntimeTriggerControl>,
    scope_calibration_controls: Vec<DsCopeRuntimeCalibrationControl>,
    radio_runtime_control: Option<HackRfRuntimeControl>,
    stopped: HashSet<DeviceId>,
    waveforms: WaveformStore,
    protocol_decode: ProtocolCaptureDecoder,
    radio_spectrum: SpectrumProcessor,
    radio_analyzer: Option<SignalAnalyzer>,
    recorder: Option<FileRecorder>,
    recording_path: Option<PathBuf>,
    has_logic: bool,
    has_scope: bool,
    has_radio: bool,
}

#[derive(Clone, Copy, Debug)]
struct ScheduledRepetitiveCapture {
    due: Instant,
    target: CaptureTarget,
}

struct CompletedCapture {
    waveforms: WaveformStore,
    protocol_decode: ProtocolCaptureDecoder,
    has_logic: bool,
    has_scope: bool,
    has_radio: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionTabAction {
    Select(usize),
    Add,
    Close(usize),
    BeginRename(usize),
}

struct SessionNameEditor {
    target: CaptureTarget,
    index: usize,
    draft: String,
    request_focus: bool,
}

struct CalibrationDisplay {
    waveforms: WaveformStore,
    scope_config: DsCopeCaptureConfig,
}

struct CalibrationProgressModal {
    progress: f32,
    phase: String,
    completed: bool,
    values: Option<DsCopeCalibrationValues>,
}

enum CalibrationModalAction {
    Close,
    Save(DsCopeCalibrationValues),
}

struct LoadedRecording {
    capture: CompletedCapture,
    logic_settings: Option<LogicCaptureSettings>,
    scope_settings: Option<ScopeCaptureSettings>,
    radio_settings: Option<RadioSettings>,
    logic_channel_names: [String; LOGIC_CHANNEL_COUNT],
    scope_channel_names: [String; 2],
    completion: CompletionStatus,
}

struct RadioCaptureRequest {
    device: DiscoveredRadio,
    settings: RadioCaptureSettings,
    provider_settings: RadioProviderSettings,
    iq_correction_enabled: bool,
    analyzer_profile: Option<SignalAnalyzerProfile>,
}

#[derive(Clone)]
struct LogicSessionSettings {
    device_id: Option<String>,
    capture: LogicCaptureSettings,
    acquisition_mode: AcquisitionMode,
    repetitive_interval_seconds: f32,
    channel_colors: [Color32; LOGIC_CHANNEL_COUNT],
    channel_marker_opacities: [u8; LOGIC_CHANNEL_COUNT],
    channel_settings_drafts: [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT],
    smart_timing_enabled: bool,
    marker_settings: LogicMarkerSettings,
    protocol_analyzer: ProtocolAnalyzerSettings,
    channel_names: [String; LOGIC_CHANNEL_COUNT],
    viewport: WaveformViewport,
}

impl Default for LogicSessionSettings {
    fn default() -> Self {
        Self {
            device_id: None,
            capture: LogicCaptureSettings::default(),
            acquisition_mode: AcquisitionMode::Single,
            repetitive_interval_seconds: 1.0,
            channel_colors: default_logic_channel_colors(),
            channel_marker_opacities: default_logic_channel_marker_opacities(),
            channel_settings_drafts: default_logic_channel_settings_drafts(),
            smart_timing_enabled: true,
            marker_settings: LogicMarkerSettings::default(),
            protocol_analyzer: ProtocolAnalyzerSettings::default(),
            channel_names: empty_logic_channel_names(),
            viewport: WaveformViewport::default(),
        }
    }
}

#[derive(Clone)]
struct ScopeSessionSettings {
    device_id: Option<String>,
    capture: ScopeCaptureSettings,
    auto_sample_rate: bool,
    smart_timing_enabled: bool,
    function_settings: ScopeFunctionSettings,
    marker_settings: ScopeMarkerSettings,
    quick_time_measurement: ScopeQuickTimeMeasurement,
    channel_colors: [Color32; 2],
    channel_names: [String; 2],
    viewport: WaveformViewport,
    fft_viewport: ScopeFftViewport,
    measurement_caches: [ScopeMeasurementCache; 2],
    measurement_window_positions: [Vec2; 2],
    time_per_division_seconds: Option<f64>,
}

impl Default for ScopeSessionSettings {
    fn default() -> Self {
        Self {
            device_id: None,
            capture: ScopeCaptureSettings::default(),
            auto_sample_rate: true,
            smart_timing_enabled: false,
            function_settings: ScopeFunctionSettings::default(),
            marker_settings: ScopeMarkerSettings::default(),
            quick_time_measurement: ScopeQuickTimeMeasurement::default(),
            channel_colors: SCOPE_COLORS,
            channel_names: empty_scope_channel_names(),
            viewport: WaveformViewport::default(),
            fft_viewport: ScopeFftViewport::default(),
            measurement_caches: [ScopeMeasurementCache::default(); 2],
            measurement_window_positions: default_scope_measurement_window_positions(),
            time_per_division_seconds: Some(DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS),
        }
    }
}

#[derive(Clone, Default)]
struct CombinedSessionSettings {
    logic: LogicSessionSettings,
    scope: ScopeSessionSettings,
    logic_ratio: f32,
    viewport: WaveformViewport,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RadioSettings {
    capture: RadioCaptureSettings,
    gains_db: BTreeMap<String, i32>,
    iq_correction_enabled: bool,
    amplifier_enabled: bool,
    antenna_power_enabled: bool,
    analyzer: RadioAnalyzerSettings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RadioDemodulation {
    BinaryFsk,
    BinaryGfsk,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RadioAnalyzerSettings {
    enabled: bool,
    demodulation: RadioDemodulation,
    channel_offset_hz: i64,
    channel_bandwidth_hz: u64,
    output_sample_rate_hz: u64,
    filter_tap_count: usize,
    symbol_rate_hz: u64,
    deviation_hz: u64,
    gfsk_bt_milli: u32,
    inverted: bool,
    adaptive_timing: bool,
    timing_loop_bandwidth_milli: u32,
    max_clock_error_ppm: u32,
}

impl Default for RadioAnalyzerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            demodulation: RadioDemodulation::BinaryFsk,
            channel_offset_hz: 0,
            channel_bandwidth_hz: 200_000,
            output_sample_rate_hz: 1_000_000,
            filter_tap_count: 129,
            symbol_rate_hz: 100_000,
            deviation_hz: 50_000,
            gfsk_bt_milli: 500,
            inverted: false,
            adaptive_timing: true,
            timing_loop_bandwidth_milli: 100,
            max_clock_error_ppm: 10_000,
        }
    }
}

impl RadioAnalyzerSettings {
    fn signal_profile(&self) -> Result<Option<SignalAnalyzerProfile>, String> {
        if !self.enabled {
            return Ok(None);
        }
        let timing = if self.adaptive_timing {
            FskTiming::Adaptive {
                loop_bandwidth: f64::from(self.timing_loop_bandwidth_milli) / 1_000.0,
                max_clock_error_ppm: self.max_clock_error_ppm,
            }
        } else {
            FskTiming::Fixed
        };
        let polarity = if self.inverted {
            FskPolarity::Inverted
        } else {
            FskPolarity::Normal
        };
        let demodulation = match self.demodulation {
            RadioDemodulation::BinaryFsk => DemodulationProfile::BinaryFsk(BinaryFskProfile {
                symbol_rate_hz: self.symbol_rate_hz,
                deviation_hz: self.deviation_hz,
                polarity,
                timing,
            }),
            RadioDemodulation::BinaryGfsk => DemodulationProfile::BinaryGfsk(BinaryGfskProfile {
                symbol_rate_hz: self.symbol_rate_hz,
                deviation_hz: self.deviation_hz,
                bt: f64::from(self.gfsk_bt_milli) / 1_000.0,
                polarity,
                timing,
            }),
        };
        let profile = SignalAnalyzerProfile {
            channel: ChannelizerConfig {
                frequency_offset_hz: self.channel_offset_hz,
                bandwidth_hz: self.channel_bandwidth_hz,
                output_sample_rate_hz: self.output_sample_rate_hz,
                filter_tap_count: self.filter_tap_count,
            },
            demodulation,
        };
        profile.validate().map_err(|error| error.to_string())?;
        Ok(Some(profile))
    }
}

impl Default for RadioSettings {
    fn default() -> Self {
        Self {
            capture: RadioCaptureSettings {
                center_frequency_hz: 100_000_000,
                sample_rate_hz: 10_000_000,
                bandwidth_hz: 5_000_000,
                receive_channel: 0,
                sample_limit: None,
            },
            gains_db: BTreeMap::new(),
            iq_correction_enabled: true,
            amplifier_enabled: false,
            antenna_power_enabled: false,
            analyzer: RadioAnalyzerSettings::default(),
        }
    }
}

type FileRecorder = RecordingWriter<BufWriter<File>>;

enum WorkerSuccess {
    Devices {
        usb: Vec<DiscoveredDevice>,
        radio: RadioDiscoveryReport,
    },
    Capture {
        session: CaptureSession,
        mode: AcquisitionMode,
        target: CaptureTarget,
        retain_protocol_history: bool,
        logic_config: Box<DsLogicCaptureConfig>,
        scope_config: DsCopeCaptureConfig,
        scope_trigger_controls: Vec<DsCopeRuntimeTriggerControl>,
        scope_calibration_controls: Vec<DsCopeRuntimeCalibrationControl>,
        radio_runtime_control: Option<HackRfRuntimeControl>,
        radio_spectrum: Box<SpectrumProcessor>,
        radio_analyzer: Option<Box<SignalAnalyzer>>,
    },
    ScopeConfigured,
    ScopeAutoCalibrated(DsCopeCalibrationValues),
    ScopeAutoCalibrationSaved,
    ScopeCalibrationLoaded(DsCopeCalibrationValues),
    ScopeCalibrationSaved,
    RecordingLoaded {
        path: PathBuf,
        recording: Box<LoadedRecording>,
    },
    SaleaeExported(PathBuf),
}

type WorkerResult = Result<WorkerSuccess, String>;

struct WaveCraftApp {
    resource_directory: String,
    settings_open: bool,
    auto_calibration_confirmation_open: bool,
    manual_calibration: Option<ManualCalibrationEditor>,
    manual_calibration_load_requested: bool,
    manual_calibration_save_requested: Option<DsCopeCalibrationValues>,
    record_capture: bool,
    recording_path: String,
    capture_run_requested: bool,
    requested_capture_target: Option<CaptureTarget>,
    logic_acquisition_mode: AcquisitionMode,
    repetitive_interval_seconds: f32,
    scheduled_repetitive_capture: Option<ScheduledRepetitiveCapture>,
    devices: Vec<DeviceEntry>,
    selected_logic_device_id: Option<String>,
    selected_scope_device_id: Option<String>,
    radios: Vec<DiscoveredRadio>,
    selected_radio_id: Option<String>,
    radio_discovery_failures: Vec<RadioProviderFailure>,
    radio_settings: RadioSettings,
    radio_view: RefCell<RadioViewState>,
    active_capture_settings_tab: CaptureSettingsTab,
    logic_settings: LogicCaptureSettings,
    scope_settings: ScopeCaptureSettings,
    scope_auto_sample_rate: bool,
    scope_smart_timing_enabled: bool,
    scope_function_settings: ScopeFunctionSettings,
    scope_marker_settings: ScopeMarkerSettings,
    scope_quick_time_measurement: ScopeQuickTimeMeasurement,
    scope_marker_editor: Option<ScopeMarkerEditor>,
    active_waveform_tab: Option<WaveformTab>,
    combined_logic_ratio: f32,
    logic_channel_colors: [Color32; LOGIC_CHANNEL_COUNT],
    logic_channel_marker_opacities: [u8; LOGIC_CHANNEL_COUNT],
    logic_channel_settings_drafts: [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT],
    logic_smart_timing_enabled: bool,
    logic_marker_settings: LogicMarkerSettings,
    protocol_analyzer: ProtocolAnalyzerSettings,
    protocol_decode_cache: ProtocolDecodeCache,
    decoded_data_panel_open: bool,
    decoded_data_panel_width: f32,
    scope_channel_colors: [Color32; 2],
    logic_channel_names: [String; LOGIC_CHANNEL_COUNT],
    scope_channel_names: [String; 2],
    logic_viewport: WaveformViewport,
    scope_viewport: WaveformViewport,
    combined_viewport: WaveformViewport,
    scope_fft_viewport: ScopeFftViewport,
    scope_measurement_caches: [ScopeMeasurementCache; 2],
    scope_measurement_window_positions: [Vec2; 2],
    snapshot_plot_rect: Option<Rect>,
    pending_snapshot: Option<PendingSnapshot>,
    scope_time_per_division_seconds: Option<f64>,
    scope_capture_restart_requested: bool,
    scope_configuration_due: Option<Instant>,
    pending: Option<Receiver<WorkerResult>>,
    calibration_progress: Option<Receiver<DsCopeCalibrationFrame>>,
    calibration_display: Option<CalibrationDisplay>,
    calibration_active: bool,
    calibration_modal: Option<CalibrationProgressModal>,
    usb_speed_warning: Option<UsbSpeedWarning>,
    capture: Option<ActiveCapture>,
    logic_sessions: SessionTabs<CompletedCapture, LogicSessionSettings>,
    scope_sessions: SessionTabs<CompletedCapture, ScopeSessionSettings>,
    session_name_editor: Option<SessionNameEditor>,
    combined_capture: Option<CompletedCapture>,
    radio_capture: Option<CompletedCapture>,
    combined_settings: CombinedSessionSettings,
    status: String,
    last_error: Option<String>,
}

fn default_scope_measurement_window_positions() -> [Vec2; 2] {
    [Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0)]
}

impl WaveCraftApp {
    fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        configure_style(&creation_context.egui_ctx);
        let mut app = Self {
            resource_directory: default_resource_directory(),
            settings_open: false,
            auto_calibration_confirmation_open: false,
            manual_calibration: None,
            manual_calibration_load_requested: false,
            manual_calibration_save_requested: None,
            record_capture: false,
            recording_path: "capture.dsv".to_owned(),
            capture_run_requested: false,
            requested_capture_target: None,
            logic_acquisition_mode: AcquisitionMode::Single,
            repetitive_interval_seconds: 1.0,
            scheduled_repetitive_capture: None,
            devices: Vec::new(),
            selected_logic_device_id: None,
            selected_scope_device_id: None,
            radios: Vec::new(),
            selected_radio_id: None,
            radio_discovery_failures: Vec::new(),
            radio_settings: RadioSettings::default(),
            radio_view: RefCell::new(RadioViewState::default()),
            active_capture_settings_tab: CaptureSettingsTab::DsLogic,
            logic_settings: LogicCaptureSettings::default(),
            scope_settings: ScopeCaptureSettings::default(),
            scope_auto_sample_rate: true,
            scope_smart_timing_enabled: false,
            scope_function_settings: ScopeFunctionSettings::default(),
            scope_marker_settings: ScopeMarkerSettings::default(),
            scope_quick_time_measurement: ScopeQuickTimeMeasurement::default(),
            scope_marker_editor: None,
            active_waveform_tab: None,
            combined_logic_ratio: 0.55,
            logic_channel_colors: default_logic_channel_colors(),
            logic_channel_marker_opacities: default_logic_channel_marker_opacities(),
            logic_channel_settings_drafts: default_logic_channel_settings_drafts(),
            logic_smart_timing_enabled: true,
            logic_marker_settings: LogicMarkerSettings::default(),
            protocol_analyzer: ProtocolAnalyzerSettings::default(),
            protocol_decode_cache: ProtocolDecodeCache::default(),
            decoded_data_panel_open: false,
            decoded_data_panel_width: 360.0,
            scope_channel_colors: SCOPE_COLORS,
            logic_channel_names: empty_logic_channel_names(),
            scope_channel_names: empty_scope_channel_names(),
            logic_viewport: WaveformViewport::default(),
            scope_viewport: WaveformViewport::default(),
            combined_viewport: WaveformViewport::default(),
            scope_fft_viewport: ScopeFftViewport::default(),
            scope_measurement_caches: [ScopeMeasurementCache::default(); 2],
            scope_measurement_window_positions: default_scope_measurement_window_positions(),
            snapshot_plot_rect: None,
            pending_snapshot: None,
            scope_time_per_division_seconds: Some(DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS),
            scope_capture_restart_requested: false,
            scope_configuration_due: None,
            pending: None,
            calibration_progress: None,
            calibration_display: None,
            calibration_active: false,
            calibration_modal: None,
            usb_speed_warning: None,
            capture: None,
            logic_sessions: SessionTabs::new(LogicSessionSettings::default()),
            scope_sessions: SessionTabs::new(ScopeSessionSettings::default()),
            session_name_editor: None,
            combined_capture: None,
            radio_capture: None,
            combined_settings: CombinedSessionSettings {
                logic_ratio: 0.55,
                ..CombinedSessionSettings::default()
            },
            status: "Ready".to_owned(),
            last_error: None,
        };
        app.begin_refresh();
        app
    }

    fn finish_pending_snapshot(&mut self, context: &egui::Context) {
        if self.pending_snapshot.is_none() {
            return;
        }
        let screenshot = context.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(screenshot) = screenshot else {
            return;
        };
        let pending = self
            .pending_snapshot
            .take()
            .expect("pending snapshot was checked above");
        let snapshot = match crop_snapshot_image(
            screenshot.as_ref(),
            pending.plot_rect,
            pending.pixels_per_point,
        ) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.status = "Snapshot failed".to_owned();
                self.last_error = Some(format!("Snapshot failed: {error}"));
                return;
            }
        };

        let Some(directory) = rfd::FileDialog::new()
            .set_title("Select snapshot directory")
            .pick_folder()
        else {
            self.status = "Snapshot canceled".to_owned();
            return;
        };
        match save_snapshot_image(&directory, &snapshot) {
            Ok(path) => {
                self.status = format!("Snapshot saved to {}", path.display());
                self.last_error = None;
            }
            Err(error) => {
                self.status = "Snapshot failed".to_owned();
                self.last_error = Some(format!("Snapshot failed: {error}"));
            }
        }
    }

    fn begin_open_recording(&mut self, path: PathBuf) {
        if self.is_busy() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.capture_run_requested = false;
        self.requested_capture_target = None;
        self.scheduled_repetitive_capture = None;
        self.scope_configuration_due = None;
        self.status = format!("Opening capture {}…", path.display());
        self.last_error = None;
        let protocol_analyzers = self.protocol_analyzer.analyzers.clone();
        thread::spawn(move || {
            let result = load_gui_recording(&path, &protocol_analyzers).map(|recording| {
                WorkerSuccess::RecordingLoaded {
                    path,
                    recording: Box::new(recording),
                }
            });
            let _ = sender.send(result);
        });
    }

    fn begin_export_saleae(&mut self, path: PathBuf) {
        if self.is_busy() {
            return;
        }
        let Some(completed) = self.displayed_completed_capture() else {
            self.set_error("There is no completed capture to export".to_owned());
            return;
        };
        let capture = match saleae_capture_from_display(
            completed,
            &self.logic_settings,
            &self.logic_channel_names,
            &self.scope_channel_names,
        ) {
            Ok(capture) => capture,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.status = format!("Exporting Saleae capture {}…", path.display());
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let file = create_saleae_export_file(&path)?;
                write_saleae(BufWriter::new(file), &capture).map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::SaleaeExported(path))
            })();
            let _ = sender.send(result);
        });
    }

    fn begin_refresh(&mut self) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.capture_run_requested = false;
        self.requested_capture_target = None;
        self.scheduled_repetitive_capture = None;
        self.scope_capture_restart_requested = false;
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.reset_session_captures();
        self.logic_marker_settings.clear();
        self.status = "Discovering instruments…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let radio = RadioRegistry::new()
                .with_adapter(HackRfAdapter::new())
                .discover();
            let result = discover_supported_devices()
                .map(|report| WorkerSuccess::Devices {
                    usb: report.devices,
                    radio,
                })
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn reconcile_device_selections(&mut self) {
        if self.selected_logic_device_id.is_none() {
            self.selected_logic_device_id =
                first_device_id_for_role(&self.devices, DeviceRole::LogicAnalyzer);
        }
        if self.selected_scope_device_id.is_none() {
            self.selected_scope_device_id =
                first_device_id_for_role(&self.devices, DeviceRole::Oscilloscope);
        }
        if self.selected_radio_id.is_none() {
            self.selected_radio_id = first_radio_id(&self.radios);
        }
    }

    fn selected_logic_device(&self) -> Option<&DiscoveredDevice> {
        selected_device_for_role(
            &self.devices,
            self.selected_logic_device_id.as_deref(),
            DeviceRole::LogicAnalyzer,
        )
    }

    fn selected_scope_device_ref(&self) -> Option<&DiscoveredDevice> {
        selected_device_for_role(
            &self.devices,
            self.selected_scope_device_id.as_deref(),
            DeviceRole::Oscilloscope,
        )
    }

    fn reconcile_radio_settings(&mut self) {
        let Some(radio) = selected_radio(&self.radios, self.selected_radio_id.as_deref()) else {
            return;
        };
        normalize_radio_settings(&mut self.radio_settings, &radio.capabilities);
    }

    fn radio_capture_request(&self, mode: AcquisitionMode) -> Result<RadioCaptureRequest, String> {
        let device = selected_radio(&self.radios, self.selected_radio_id.as_deref())
            .cloned()
            .ok_or_else(|| "Select a connected radio receiver in Capture Setup".to_owned())?;
        let mut settings = self.radio_settings.capture.clone();
        settings.sample_limit =
            (mode == AcquisitionMode::Single).then_some(settings.sample_rate_hz);
        settings
            .validate(&device.capabilities)
            .map_err(|error| error.to_string())?;
        let provider_settings = match device.provider {
            RadioProvider::HackRf => RadioProviderSettings::HackRf(HackRfSettings {
                lna_gain_db: radio_gain_u32(&self.radio_settings, "lna")?,
                vga_gain_db: radio_gain_u32(&self.radio_settings, "vga")?,
                amplifier_enabled: self.radio_settings.amplifier_enabled,
                antenna_power_enabled: self.radio_settings.antenna_power_enabled,
            }),
            provider => {
                return Err(format!(
                    "capture is not implemented for the {provider:?} radio provider"
                ));
            }
        };
        let analyzer_profile = self.radio_settings.analyzer.signal_profile()?;
        Ok(RadioCaptureRequest {
            device,
            settings,
            provider_settings,
            iq_correction_enabled: self.radio_settings.iq_correction_enabled,
            analyzer_profile,
        })
    }

    fn begin_capture(&mut self, mode: AcquisitionMode, target: CaptureTarget) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        if !self.capture_run_requested && target.includes_logic_analyzer() {
            self.logic_marker_settings.clear();
        }
        self.scheduled_repetitive_capture = None;
        if self.record_capture && mode.runs_continuously() {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            self.record_capture = false;
            self.last_error =
                Some("Complete-capture recording is available in Single mode only".to_owned());
            return;
        }
        let devices = match capture_devices_for_target(
            &self.devices,
            self.selected_logic_device_id.as_deref(),
            self.selected_scope_device_id.as_deref(),
            target,
        ) {
            Ok(devices) => devices,
            Err(error) => {
                self.capture_run_requested = false;
                self.requested_capture_target = None;
                self.record_capture = false;
                self.last_error = Some(error);
                return;
            }
        };
        let radio_request = if target == CaptureTarget::RadioReceiver {
            match self.radio_capture_request(mode) {
                Ok(request) => Some(request),
                Err(error) => {
                    self.capture_run_requested = false;
                    self.requested_capture_target = None;
                    self.record_capture = false;
                    self.last_error = Some(error);
                    return;
                }
            }
        } else {
            None
        };
        if self.record_capture && self.recording_path.trim().is_empty() {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            self.record_capture = false;
            self.last_error = Some("Enter a recording file path".to_owned());
            return;
        }
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.capture_run_requested = false;
                self.requested_capture_target = None;
                self.record_capture = false;
                self.last_error = Some(error);
                return;
            }
        };
        let logic_config = match self.logic_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.capture_run_requested = false;
                self.requested_capture_target = None;
                self.record_capture = false;
                self.last_error = Some(error);
                return;
            }
        }
        .with_loop_mode(mode == AcquisitionMode::Loop);
        if matches!(mode, AcquisitionMode::Repetitive | AcquisitionMode::Loop)
            && target != CaptureTarget::LogicAnalyzer
        {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            self.record_capture = false;
            self.last_error = Some(
                "Repetitive and Loop modes are available for Logic Analyzer capture only"
                    .to_owned(),
            );
            return;
        }
        if mode == AcquisitionMode::Loop
            && logic_config.operation_mode() != DsLogicOperationMode::Stream
        {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            self.record_capture = false;
            self.last_error = Some("Loop mode requires DSLogic Stream mode".to_owned());
            return;
        }
        let calibration_values = self.manual_calibration.as_ref().map(|editor| editor.values);

        let retain_protocol_history =
            mode == AcquisitionMode::Repetitive && self.capture_run_requested;
        let continuing_loop = mode == AcquisitionMode::Loop && self.capture_run_requested;
        self.capture_run_requested = true;
        self.requested_capture_target = Some(target);
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        if mode == AcquisitionMode::Single || (mode == AcquisitionMode::Loop && !continuing_loop) {
            self.clear_completed_capture_for_target(target);
        }
        self.status = match mode {
            AcquisitionMode::Single => {
                format!("Preparing {} single acquisition…", target.label())
            }
            AcquisitionMode::Repetitive => {
                format!("Preparing {} repetitive acquisition…", target.label())
            }
            AcquisitionMode::Loop => {
                format!("Preparing {} loop acquisition…", target.label())
            }
            AcquisitionMode::Run => {
                format!("Preparing {} continuous acquisition…", target.label())
            }
        };
        self.last_error = None;
        thread::spawn(move || {
            let result = start_capture(
                devices,
                radio_request,
                &resource_directory,
                logic_config,
                scope_config,
                calibration_values,
            )
            .map(|started| WorkerSuccess::Capture {
                session: started.session,
                mode,
                target,
                retain_protocol_history,
                logic_config: Box::new(logic_config),
                scope_config,
                scope_trigger_controls: started.scope_trigger_controls,
                scope_calibration_controls: started.scope_calibration_controls,
                radio_runtime_control: started.radio_runtime_control,
                radio_spectrum: Box::new(started.radio_spectrum),
                radio_analyzer: started.radio_analyzer.map(Box::new),
            });
            let _ = sender.send(result);
        });
    }

    fn begin_scope_configuration(&mut self) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        self.scope_configuration_due = None;
        let Some(device) = self.selected_scope_device_ref().cloned() else {
            self.status = "DSCope settings will be applied when capture starts".to_owned();
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
        self.pending = Some(receiver);
        self.status = format!(
            "Applying DSCope configuration to {}…",
            device.location.stable_id()
        );
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeConfigured)
            })();
            let _ = sender.send(result);
        });
    }

    fn selected_scope_device_for_operation(&mut self) -> Option<DiscoveredDevice> {
        let device = self.selected_scope_device_ref().cloned();
        if device.is_none() {
            self.last_error = Some("Select a connected DSCope U3P100 in Capture Setup".to_owned());
        }
        device
    }

    fn begin_auto_calibration(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device_for_operation() else {
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
        let (progress_sender, progress_receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.calibration_progress = Some(progress_receiver);
        self.calibration_display = None;
        self.calibration_active = true;
        self.calibration_modal = Some(CalibrationProgressModal {
            progress: 0.0,
            phase: "Preparing oscilloscope…".to_owned(),
            completed: false,
            values: None,
        });
        self.select_waveform_tab(WaveformTab::Oscilloscope);
        self.status = "Auto-calibrating DSCope offsets and ADC branches…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                let values = auto_calibrate_dscope_with_progress(&device, |frame| {
                    let _ = progress_sender.send(frame);
                })
                .map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeAutoCalibrated(values))
            })();
            let _ = sender.send(result);
        });
    }

    fn begin_save_auto_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device_for_operation() else {
            return;
        };
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        if let Some(modal) = self.calibration_modal.as_mut() {
            modal.progress = 0.0;
            modal.phase = "Saving calibration to device NVM…".to_owned();
            modal.completed = false;
            modal.values = None;
        }
        self.status = "Saving DSCope calibration to device NVM…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                write_dscope_calibration(&device, values).map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeAutoCalibrationSaved)
            })();
            let _ = sender.send(result);
        });
    }

    fn begin_manual_calibration(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device_for_operation() else {
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

    fn request_manual_calibration(&mut self) {
        if self.pending.is_some() || self.manual_calibration.is_some() {
            return;
        }
        if let Some(active) = self.capture.as_ref() {
            if active.mode != AcquisitionMode::Run {
                return;
            }
            self.manual_calibration_load_requested = true;
            self.scope_capture_restart_requested = false;
            active.session.request_stop();
            self.status = "Loading calibration while continuous acquisition pauses…".to_owned();
            self.last_error = None;
        } else {
            self.begin_manual_calibration();
        }
    }

    fn begin_save_manual_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device_for_operation() else {
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

    fn request_save_manual_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.pending.is_some() {
            return;
        }
        if let Some(active) = self.capture.as_ref() {
            self.manual_calibration_save_requested = Some(values);
            self.scope_capture_restart_requested = false;
            active.session.request_stop();
            self.status = "Pausing acquisition to save calibration…".to_owned();
            self.last_error = None;
        } else {
            self.begin_save_manual_calibration(values);
        }
    }

    fn stop_capture(&mut self) {
        self.capture_run_requested = false;
        self.requested_capture_target = None;
        self.scheduled_repetitive_capture = None;
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
        let recording_path = active.recording_path.take();
        if recording_path.is_some() {
            self.record_capture = false;
        }
        let has_logic = active.has_logic;
        let has_scope = active.has_scope;
        let has_radio = active.has_radio;
        active.protocol_decode.finish();
        if !active.preserve_display_until_complete {
            self.set_completed_capture_for_target(
                active.target,
                CompletedCapture {
                    waveforms: active.waveforms,
                    protocol_decode: active.protocol_decode,
                    has_logic,
                    has_scope,
                    has_radio,
                },
            );
        }
        match (stop_result, recording_result) {
            (Ok(()), Ok(())) => {
                self.status = recording_path.map_or_else(
                    || "Capture stopped".to_owned(),
                    |path| format!("Capture stopped · recording saved to {}", path.display()),
                );
            }
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
            Ok(WorkerSuccess::Devices { usb, radio }) => {
                self.usb_speed_warning = usb_speed_warning(&usb);
                self.devices = usb
                    .into_iter()
                    .map(|device| DeviceEntry { device })
                    .collect();
                self.radios = radio.devices;
                self.radio_discovery_failures = radio.failures;
                self.reconcile_device_selections();
                let detected_count = self.devices.len() + self.radios.len();
                self.status = if detected_count == 0 {
                    "No supported instruments detected".to_owned()
                } else {
                    format!("Detected {detected_count} supported instrument(s)")
                };
            }
            Ok(WorkerSuccess::Capture {
                session,
                mode,
                target,
                retain_protocol_history,
                logic_config,
                mut scope_config,
                scope_trigger_controls,
                scope_calibration_controls,
                radio_runtime_control,
                radio_spectrum,
                radio_analyzer,
            }) => {
                let logic_config = *logic_config;
                if !self.capture_run_requested {
                    self.requested_capture_target = None;
                    match session.stop() {
                        Ok(()) => self.status = "Acquisition stopped".to_owned(),
                        Err(error) => self.set_error(error.to_string()),
                    }
                    return;
                }
                if self.scope_capture_restart_requested {
                    self.scope_capture_restart_requested = false;
                    match session.stop() {
                        Ok(()) => {
                            self.status = "Reconfiguring oscilloscope frontend…".to_owned();
                            self.begin_capture(mode, target);
                        }
                        Err(error) => self.set_error(error.to_string()),
                    }
                    return;
                }
                if !scope_trigger_controls.is_empty() {
                    let desired_trigger = match self.scope_settings.trigger.capture_config() {
                        Ok(trigger) => trigger,
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
                    };
                    if desired_trigger != scope_config.trigger() {
                        let update_result = scope_config
                            .with_trigger_config(desired_trigger)
                            .map_err(|error| error.to_string())
                            .and_then(|updated_config| {
                                for control in &scope_trigger_controls {
                                    control
                                        .update(desired_trigger)
                                        .map_err(|error| error.to_string())?;
                                }
                                Ok(updated_config)
                            });
                        match update_result {
                            Ok(updated_config) => scope_config = updated_config,
                            Err(error) => {
                                let cleanup = session.stop().err();
                                self.set_error(match cleanup {
                                    Some(cleanup) => format!(
                                        "failed to apply the latest trigger settings: {error}; additionally failed to stop: {cleanup}"
                                    ),
                                    None => format!(
                                        "failed to apply the latest trigger settings: {error}"
                                    ),
                                });
                                return;
                            }
                        }
                    }
                }
                let count = session.descriptors().len();
                let recording_path = self
                    .record_capture
                    .then(|| PathBuf::from(self.recording_path.trim()));
                let recorder = match recording_path.as_deref() {
                    Some(path) => {
                        match start_gui_recording(
                            path,
                            session.descriptors(),
                            GuiRecordingSettings {
                                logic_config,
                                logic_settings: self.logic_settings,
                                scope_config,
                                scope_settings: self.scope_settings,
                                radio_settings: &self.radio_settings,
                                logic_channel_names: &self.logic_channel_names,
                                scope_channel_names: &self.scope_channel_names,
                            },
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
                        }
                    }
                    None => None,
                };
                let has_logic = session
                    .descriptors()
                    .iter()
                    .any(|device| device.kind == InstrumentKind::LogicAnalyzer);
                let has_scope = session
                    .descriptors()
                    .iter()
                    .any(|device| device.kind == InstrumentKind::Oscilloscope);
                let has_radio = session
                    .descriptors()
                    .iter()
                    .any(|device| device.kind == InstrumentKind::RadioReceiver);
                let has_scope_trigger = !scope_trigger_controls.is_empty();
                let has_logic_trigger = has_logic && logic_config.trigger().is_some();
                let rolling =
                    uses_rolling_scope_buffer(mode, has_scope, scope_config.trigger().source());
                let scope_capacity = if rolling {
                    scope_rolling_sample_capacity(
                        scope_config,
                        self.scope_time_per_division_seconds,
                    )
                } else {
                    scope_config.sample_count() as usize
                };
                let continue_roll = rolling
                    && self
                        .completed_capture_for_target(target)
                        .is_some_and(|capture| {
                            capture
                                .waveforms
                                .can_continue_scope_roll(scope_config, scope_capacity)
                        });
                let continue_logic_loop = mode == AcquisitionMode::Loop
                    && self
                        .completed_capture_for_target(target)
                        .is_some_and(|capture| {
                            capture.has_logic
                                && capture.waveforms.can_continue_logic_loop(logic_config)
                        });
                let preserve_display_until_complete =
                    mode == AcquisitionMode::Run || mode == AcquisitionMode::Repetitive;
                let preserve_display_until_complete = preserve_display_until_complete
                    && self.completed_capture_for_target(target).is_some()
                    && !continue_roll
                    && target != CaptureTarget::RadioReceiver;
                let prior_protocol_history = retain_protocol_history
                    .then(|| self.completed_capture_for_target(target))
                    .flatten()
                    .map(|capture| capture.protocol_decode.snapshot());
                let continued_capture = if continue_roll || continue_logic_loop {
                    self.take_completed_capture_for_target(target)
                } else {
                    None
                };
                let (mut waveforms, mut protocol_decode) = continued_capture.map_or_else(
                    || {
                        (
                            WaveformStore::default(),
                            ProtocolCaptureDecoder::new(&self.protocol_analyzer.analyzers),
                        )
                    },
                    |capture| (capture.waveforms, capture.protocol_decode),
                );
                if let Some(snapshot) = &prior_protocol_history {
                    protocol_decode.retain_history_from(snapshot);
                }
                protocol_decode.reconfigure(&self.protocol_analyzer.analyzers);
                if !continue_logic_loop {
                    waveforms.configure_logic_capture(logic_config.sample_count());
                }
                waveforms.scope_capacity = scope_capacity;
                waveforms.scope_rolling = rolling;
                self.capture = Some(ActiveCapture {
                    session,
                    mode,
                    target,
                    preserve_display_until_complete,
                    scope_config,
                    scope_trigger_controls,
                    scope_calibration_controls,
                    radio_runtime_control,
                    stopped: HashSet::new(),
                    waveforms,
                    protocol_decode,
                    radio_spectrum: *radio_spectrum,
                    radio_analyzer: radio_analyzer.map(|analyzer| *analyzer),
                    recorder,
                    recording_path: recording_path.clone(),
                    has_logic,
                    has_scope,
                    has_radio,
                });
                self.status = match (mode, recording_path) {
                    (AcquisitionMode::Single, Some(path)) if has_logic_trigger => format!(
                        "Single · waiting for logic trigger; recording to {}",
                        path.display()
                    ),
                    (AcquisitionMode::Single, Some(path)) => {
                        format!(
                            "{} Single acquisition from {count} instrument(s) to {}",
                            target.label(),
                            path.display()
                        )
                    }
                    (AcquisitionMode::Single, None) => {
                        if has_logic_trigger {
                            "Single · waiting for logic trigger".to_owned()
                        } else if has_scope_trigger
                            && scope_config.trigger().source() != DsCopeTriggerSource::Auto
                        {
                            "Single · waiting for trigger".to_owned()
                        } else {
                            format!(
                                "{} Single acquisition from {count} instrument(s)",
                                target.label()
                            )
                        }
                    }
                    (AcquisitionMode::Repetitive, _) => format!(
                        "{} Repetitive acquisition from {count} instrument(s)",
                        target.label()
                    ),
                    (AcquisitionMode::Loop, _) => format!(
                        "{} Loop acquisition from {count} instrument(s)",
                        target.label()
                    ),
                    (AcquisitionMode::Run, _) => {
                        if has_scope_trigger
                            && scope_config.trigger().source() == DsCopeTriggerSource::Auto
                        {
                            "Run · Auto".to_owned()
                        } else if has_scope_trigger {
                            "Run · waiting for trigger".to_owned()
                        } else {
                            format!(
                                "{} Run acquisition from {count} instrument(s)",
                                target.label()
                            )
                        }
                    }
                };
            }
            Ok(WorkerSuccess::RecordingLoaded { path, recording }) => {
                if let Some(active) = self.active_waveform_tab {
                    self.save_waveform_view_settings(active);
                }
                let LoadedRecording {
                    capture,
                    logic_settings,
                    scope_settings,
                    radio_settings,
                    logic_channel_names,
                    scope_channel_names,
                    completion,
                } = *recording;
                if let Some(logic_settings) = logic_settings {
                    self.logic_settings = logic_settings;
                }
                if let Some(scope_settings) = scope_settings {
                    self.scope_settings = scope_settings;
                    self.scope_auto_sample_rate = false;
                }
                if let Some(radio_settings) = radio_settings {
                    self.radio_settings = radio_settings;
                }
                let target = match (capture.has_logic, capture.has_scope, capture.has_radio) {
                    (true, true, false) => CaptureTarget::Combined,
                    (true, false, false) => CaptureTarget::LogicAnalyzer,
                    (false, true, false) => CaptureTarget::Oscilloscope,
                    (false, false, true) => CaptureTarget::RadioReceiver,
                    _ => unreachable!("recordings contain one supported capture target"),
                };
                self.set_completed_capture_for_target(target, capture);
                self.logic_marker_settings.clear();
                self.logic_channel_names = logic_channel_names;
                self.scope_channel_names = scope_channel_names;
                self.record_capture = false;
                self.logic_viewport.reset();
                self.scope_viewport.reset();
                self.combined_viewport.reset();
                self.scope_fft_viewport.reset();
                self.scope_measurement_caches = [ScopeMeasurementCache::default(); 2];
                self.scope_time_per_division_seconds = None;
                match target {
                    CaptureTarget::LogicAnalyzer => {
                        self.save_active_logic_session_settings();
                        self.active_waveform_tab = Some(WaveformTab::LogicAnalyzer);
                        self.active_capture_settings_tab = CaptureSettingsTab::DsLogic;
                    }
                    CaptureTarget::Oscilloscope => {
                        self.save_active_scope_session_settings();
                        self.active_waveform_tab = Some(WaveformTab::Oscilloscope);
                        self.active_capture_settings_tab = CaptureSettingsTab::DsCope;
                    }
                    CaptureTarget::RadioReceiver => {
                        self.active_waveform_tab = Some(WaveformTab::RadioReceiver);
                    }
                    CaptureTarget::Combined => {
                        self.combined_settings = self.current_combined_session_settings();
                        self.active_waveform_tab = Some(WaveformTab::Combined);
                    }
                }
                self.status = format!(
                    "Opened {} · {} recording",
                    path.display(),
                    recording_completion_label(completion)
                );
                self.last_error = None;
            }
            Ok(WorkerSuccess::SaleaeExported(path)) => {
                self.status = format!("Exported Saleae capture to {}", path.display());
                self.last_error = None;
            }
            Ok(WorkerSuccess::ScopeConfigured) => {
                self.status = "Applied DSCope configuration to the selected instrument".to_owned();
            }
            Ok(WorkerSuccess::ScopeAutoCalibrated(values)) => {
                self.calibration_active = false;
                self.calibration_progress = None;
                self.calibration_display = None;
                if let Some(modal) = self.calibration_modal.as_mut() {
                    modal.progress = 1.0;
                    modal.phase = "Calibration is done. Save the result to device NVM?".to_owned();
                    modal.completed = true;
                    modal.values = Some(values);
                }
                self.status = "DSCope auto calibration finished · result not saved".to_owned();
            }
            Ok(WorkerSuccess::ScopeAutoCalibrationSaved) => {
                if let Some(modal) = self.calibration_modal.as_mut() {
                    modal.progress = 1.0;
                    modal.phase = "Calibration was saved to device NVM.".to_owned();
                    modal.completed = true;
                    modal.values = None;
                }
                self.status = "DSCope auto calibration saved and verified".to_owned();
            }
            Ok(WorkerSuccess::ScopeCalibrationLoaded(values)) => {
                let range = DSCOPE_VOLTS_PER_DIV_MV
                    .iter()
                    .position(|value| *value == self.scope_settings.channels[0].volts_per_div_mv)
                    .unwrap_or(0);
                for channel in &mut self.scope_settings.channels {
                    channel.volts_per_div_mv = DSCOPE_VOLTS_PER_DIV_MV[range];
                }
                self.manual_calibration = Some(ManualCalibrationEditor { values, range });
                self.status = "Loaded DSCope calibration values".to_owned();
                if self.capture_run_requested {
                    if let Some(target) = self.requested_capture_target {
                        self.begin_capture(AcquisitionMode::Run, target);
                    }
                }
            }
            Ok(WorkerSuccess::ScopeCalibrationSaved) => {
                self.manual_calibration = None;
                self.status = "DSCope manual calibration saved and verified".to_owned();
                if self.capture_run_requested {
                    if let Some(target) = self.requested_capture_target {
                        self.begin_capture(AcquisitionMode::Run, target);
                    }
                }
            }
            Err(error) => self.set_error(error),
        }
    }

    fn poll_calibration_progress(&mut self) {
        let mut latest = None;
        let mut disconnected = false;
        if let Some(receiver) = self.calibration_progress.as_ref() {
            loop {
                match receiver.try_recv() {
                    Ok(frame) => latest = Some(frame),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.calibration_progress = None;
        }
        let Some(frame) = latest else {
            return;
        };

        let config = frame.config();
        if let Some(modal) = self.calibration_modal.as_mut() {
            modal.progress = modal.progress.max(frame.progress());
            modal.phase = frame.phase().to_owned();
        }
        let enabled_channels = config.channels().map(|channel| channel.enabled());
        let mut waveforms = WaveformStore {
            scope_capacity: config.sample_count() as usize,
            ..WaveformStore::default()
        };
        waveforms.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: config.sample_rate_hz(),
                trigger_sample_index: None,
                channels: frame.channels().to_vec(),
            }),
            enabled_channels,
        );
        self.calibration_display = Some(CalibrationDisplay {
            waveforms,
            scope_config: config,
        });
        self.scope_viewport.reset();
        self.scope_time_per_division_seconds = Some(
            config.sample_count() as f64
                / config.sample_rate_hz() as f64
                / SCOPE_HORIZONTAL_DIVISIONS as f64,
        );
        let volts_per_div_mv = config.channels()[0].volts_per_div_mv();
        self.status = format!(
            "Auto-calibrating DSCope · {volts_per_div_mv} mV/div · {}",
            if config.enabled_channel_count() == 1 {
                "interleaved ADC"
            } else {
                "dual-channel ADC"
            }
        );
    }

    fn poll_capture(&mut self) {
        let mut finish_status = None::<(Result<String, String>, CompletionStatus)>;
        let protocol_analyzers = self.protocol_analyzer.analyzers.clone();
        if let Some(active) = self.capture.as_mut() {
            active.protocol_decode.reconfigure(&protocol_analyzers);
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
                                if let CaptureBatch::Logic(logic) = &frame.payload {
                                    active.protocol_decode.ingest_logic_frame(
                                        frame.device_id.as_str(),
                                        frame.sequence,
                                        &logic.samples,
                                        logic.sample_rate_hz,
                                    );
                                }
                                if let CaptureBatch::Iq(iq) = &frame.payload {
                                    ingest_radio_iq(
                                        &mut active.waveforms,
                                        &mut active.radio_spectrum,
                                        &mut active.radio_analyzer,
                                        iq,
                                    );
                                }
                                let enabled_scope_channels = active
                                    .scope_config
                                    .channels()
                                    .map(|channel| channel.enabled());
                                if active.waveforms.scope_rolling
                                    && is_scope_frame_start(frame.sequence)
                                    && matches!(&frame.payload, CaptureBatch::Scope(_))
                                {
                                    active
                                        .waveforms
                                        .mark_scope_capture_start(enabled_scope_channels);
                                }
                                active
                                    .waveforms
                                    .ingest_frame(&frame, enabled_scope_channels);
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
                let mode = active.mode;
                let target = active.target;
                let preserve_display_until_complete = active.preserve_display_until_complete;
                let recording_path = active.recording_path.take();
                let has_logic = active.has_logic;
                let has_scope = active.has_scope;
                let has_radio = active.has_radio;
                let restart_for_frontend =
                    self.scope_capture_restart_requested && self.capture_run_requested;
                self.scope_capture_restart_requested = false;
                let stop_result = active.session.stop();
                let recording_completion = if stop_result.is_ok() {
                    completion
                } else {
                    CompletionStatus::Failed
                };
                let recording_result =
                    finish_gui_recording(&mut active.recorder, recording_completion);
                active.protocol_decode.finish();
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
                let update_completed_capture = if restart_for_frontend {
                    !preserve_display_until_complete
                } else {
                    completion == CompletionStatus::Complete || !preserve_display_until_complete
                };
                if update_completed_capture {
                    self.set_completed_capture_for_target(
                        target,
                        CompletedCapture {
                            waveforms: active.waveforms,
                            protocol_decode: active.protocol_decode,
                            has_logic,
                            has_scope,
                            has_radio,
                        },
                    );
                }
                if error.is_none() && self.manual_calibration_load_requested {
                    self.manual_calibration_load_requested = false;
                    self.begin_manual_calibration();
                    return;
                }
                if error.is_none() {
                    if let Some(values) = self.manual_calibration_save_requested.take() {
                        self.begin_save_manual_calibration(values);
                        return;
                    }
                }
                let repeat = error.is_none()
                    && (restart_for_frontend
                        || should_rearm_acquisition(
                            mode,
                            self.capture_run_requested,
                            completion,
                            false,
                        ));
                if !repeat {
                    self.capture_run_requested = false;
                    self.requested_capture_target = None;
                }
                if recording_path.is_some() {
                    self.record_capture = false;
                }
                match error {
                    Some(error) => self.set_error(error),
                    None if restart_for_frontend => {
                        self.status = "Reconfiguring oscilloscope frontend…".to_owned();
                    }
                    None => {
                        let capture_status =
                            result.ok().unwrap_or_else(|| "Capture complete".to_owned());
                        self.status = recording_path.map_or(capture_status.clone(), |path| {
                            format!("{capture_status} · recording saved to {}", path.display())
                        });
                    }
                }
                if repeat {
                    if mode == AcquisitionMode::Repetitive && !restart_for_frontend {
                        let interval = repetitive_interval(
                            self.logic_settings.operation_mode,
                            self.repetitive_interval_seconds,
                        );
                        self.scheduled_repetitive_capture = Some(ScheduledRepetitiveCapture {
                            due: Instant::now() + interval,
                            target,
                        });
                        self.status = format!(
                            "Repetitive · next acquisition in {:.1} s",
                            interval.as_secs_f32()
                        );
                    } else {
                        self.begin_capture(mode, target);
                    }
                }
            }
        }
    }

    fn poll_repetitive_capture(&mut self) {
        let Some(scheduled) = self.scheduled_repetitive_capture else {
            return;
        };
        if !self.capture_run_requested {
            self.scheduled_repetitive_capture = None;
            return;
        }
        if Instant::now() < scheduled.due {
            return;
        }
        self.scheduled_repetitive_capture = None;
        self.begin_capture(AcquisitionMode::Repetitive, scheduled.target);
    }

    fn completed_capture_for_target(&self, target: CaptureTarget) -> Option<&CompletedCapture> {
        match target {
            CaptureTarget::LogicAnalyzer => self.logic_sessions.active_capture(),
            CaptureTarget::Oscilloscope => self.scope_sessions.active_capture(),
            CaptureTarget::RadioReceiver => self.radio_capture.as_ref(),
            CaptureTarget::Combined => self.combined_capture.as_ref(),
        }
    }

    fn displayed_completed_capture(&self) -> Option<&CompletedCapture> {
        match self.active_waveform_tab {
            Some(WaveformTab::LogicAnalyzer) => self.logic_sessions.active_capture(),
            Some(WaveformTab::Oscilloscope | WaveformTab::Lissajous) => {
                self.scope_sessions.active_capture()
            }
            Some(WaveformTab::RadioReceiver) => self.radio_capture.as_ref(),
            Some(WaveformTab::Combined) => self.combined_capture.as_ref(),
            None => None,
        }
    }

    fn has_displayed_completed_capture(&self) -> bool {
        self.displayed_completed_capture()
            .is_some_and(|capture| capture.has_logic || capture.has_scope)
    }

    fn clear_completed_capture_for_target(&mut self, target: CaptureTarget) {
        match target {
            CaptureTarget::LogicAnalyzer => self.logic_sessions.clear_active(),
            CaptureTarget::Oscilloscope => self.scope_sessions.clear_active(),
            CaptureTarget::RadioReceiver => self.radio_capture = None,
            CaptureTarget::Combined => self.combined_capture = None,
        }
    }

    fn take_completed_capture_for_target(
        &mut self,
        target: CaptureTarget,
    ) -> Option<CompletedCapture> {
        match target {
            CaptureTarget::LogicAnalyzer => self.logic_sessions.take_active(),
            CaptureTarget::Oscilloscope => self.scope_sessions.take_active(),
            CaptureTarget::RadioReceiver => self.radio_capture.take(),
            CaptureTarget::Combined => self.combined_capture.take(),
        }
    }

    fn set_completed_capture_for_target(
        &mut self,
        target: CaptureTarget,
        capture: CompletedCapture,
    ) {
        match target {
            CaptureTarget::LogicAnalyzer => self.logic_sessions.replace_active(capture),
            CaptureTarget::Oscilloscope => self.scope_sessions.replace_active(capture),
            CaptureTarget::RadioReceiver => self.radio_capture = Some(capture),
            CaptureTarget::Combined => self.combined_capture = Some(capture),
        }
    }

    fn current_logic_session_settings(&self) -> LogicSessionSettings {
        LogicSessionSettings {
            device_id: self.selected_logic_device_id.clone(),
            capture: self.logic_settings,
            acquisition_mode: self.logic_acquisition_mode,
            repetitive_interval_seconds: self.repetitive_interval_seconds,
            channel_colors: self.logic_channel_colors,
            channel_marker_opacities: self.logic_channel_marker_opacities,
            channel_settings_drafts: self.logic_channel_settings_drafts.clone(),
            smart_timing_enabled: self.logic_smart_timing_enabled,
            marker_settings: self.logic_marker_settings.clone(),
            protocol_analyzer: self.protocol_analyzer.clone(),
            channel_names: self.logic_channel_names.clone(),
            viewport: self.logic_viewport,
        }
    }

    fn apply_logic_session_settings(&mut self, settings: LogicSessionSettings) {
        self.selected_logic_device_id = settings.device_id;
        self.logic_settings = settings.capture;
        self.logic_acquisition_mode = settings.acquisition_mode;
        self.repetitive_interval_seconds = settings.repetitive_interval_seconds;
        self.logic_channel_colors = settings.channel_colors;
        self.logic_channel_marker_opacities = settings.channel_marker_opacities;
        self.logic_channel_settings_drafts = settings.channel_settings_drafts;
        self.logic_smart_timing_enabled = settings.smart_timing_enabled;
        self.logic_marker_settings = settings.marker_settings;
        self.protocol_analyzer = settings.protocol_analyzer;
        self.logic_channel_names = settings.channel_names;
        self.logic_viewport = settings.viewport;
    }

    fn save_active_logic_session_settings(&mut self) {
        let settings = self.current_logic_session_settings();
        self.logic_sessions.replace_active_settings(settings);
    }

    fn current_scope_session_settings(&self) -> ScopeSessionSettings {
        ScopeSessionSettings {
            device_id: self.selected_scope_device_id.clone(),
            capture: self.scope_settings,
            auto_sample_rate: self.scope_auto_sample_rate,
            smart_timing_enabled: self.scope_smart_timing_enabled,
            function_settings: self.scope_function_settings,
            marker_settings: self.scope_marker_settings,
            quick_time_measurement: self.scope_quick_time_measurement,
            channel_colors: self.scope_channel_colors,
            channel_names: self.scope_channel_names.clone(),
            viewport: self.scope_viewport,
            fft_viewport: self.scope_fft_viewport,
            measurement_caches: self.scope_measurement_caches,
            measurement_window_positions: self.scope_measurement_window_positions,
            time_per_division_seconds: self.scope_time_per_division_seconds,
        }
    }

    fn apply_scope_session_settings(&mut self, settings: ScopeSessionSettings) {
        self.selected_scope_device_id = settings.device_id;
        self.scope_settings = settings.capture;
        self.scope_auto_sample_rate = settings.auto_sample_rate;
        self.scope_smart_timing_enabled = settings.smart_timing_enabled;
        self.scope_function_settings = settings.function_settings;
        self.scope_marker_settings = settings.marker_settings;
        self.scope_quick_time_measurement = settings.quick_time_measurement;
        self.scope_marker_editor = None;
        self.scope_channel_colors = settings.channel_colors;
        self.scope_channel_names = settings.channel_names;
        self.scope_viewport = settings.viewport;
        self.scope_fft_viewport = settings.fft_viewport;
        self.scope_measurement_caches = settings.measurement_caches;
        self.scope_measurement_window_positions = settings.measurement_window_positions;
        self.scope_time_per_division_seconds = settings.time_per_division_seconds;
    }

    fn save_active_scope_session_settings(&mut self) {
        let settings = self.current_scope_session_settings();
        self.scope_sessions.replace_active_settings(settings);
    }

    fn current_combined_session_settings(&self) -> CombinedSessionSettings {
        CombinedSessionSettings {
            logic: self.current_logic_session_settings(),
            scope: self.current_scope_session_settings(),
            logic_ratio: self.combined_logic_ratio,
            viewport: self.combined_viewport,
        }
    }

    fn apply_combined_session_settings(&mut self, settings: CombinedSessionSettings) {
        self.apply_logic_session_settings(settings.logic);
        self.apply_scope_session_settings(settings.scope);
        self.combined_logic_ratio = settings.logic_ratio;
        self.combined_viewport = settings.viewport;
    }

    fn save_waveform_view_settings(&mut self, tab: WaveformTab) {
        match tab {
            WaveformTab::LogicAnalyzer => self.save_active_logic_session_settings(),
            WaveformTab::Oscilloscope | WaveformTab::Lissajous => {
                self.save_active_scope_session_settings();
            }
            WaveformTab::RadioReceiver => {}
            WaveformTab::Combined => {
                self.combined_settings = self.current_combined_session_settings();
            }
        }
    }

    fn apply_waveform_view_settings(&mut self, tab: WaveformTab) {
        match tab {
            WaveformTab::LogicAnalyzer => {
                let settings = self.logic_sessions.active_settings().clone();
                self.apply_logic_session_settings(settings);
            }
            WaveformTab::Oscilloscope | WaveformTab::Lissajous => {
                let settings = self.scope_sessions.active_settings().clone();
                self.apply_scope_session_settings(settings);
            }
            WaveformTab::RadioReceiver => {}
            WaveformTab::Combined => {
                self.apply_combined_session_settings(self.combined_settings.clone());
            }
        }
    }

    fn reset_session_captures(&mut self) {
        let logic_settings = self.current_logic_session_settings();
        let scope_settings = self.current_scope_session_settings();
        self.logic_sessions.reset(logic_settings);
        self.scope_sessions.reset(scope_settings);
        self.session_name_editor = None;
        self.combined_capture = None;
        self.radio_capture = None;
    }

    fn set_error(&mut self, error: String) {
        self.capture_run_requested = false;
        self.requested_capture_target = None;
        self.scheduled_repetitive_capture = None;
        self.scope_capture_restart_requested = false;
        self.manual_calibration_load_requested = false;
        self.manual_calibration_save_requested = None;
        self.calibration_active = false;
        self.calibration_progress = None;
        self.calibration_display = None;
        self.calibration_modal = None;
        self.status = "Operation failed".to_owned();
        self.last_error = Some(error);
    }

    fn available_waveform_tabs(&self) -> Vec<WaveformTab> {
        let displayed_logic = self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.has_logic)
            || self.logic_sessions.any(|capture| capture.has_logic)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_logic);
        let displayed_scope = self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.has_scope)
            || self.scope_sessions.any(|capture| capture.has_scope)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_scope);
        let displayed_radio = self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.has_radio)
            || self
                .radio_capture
                .as_ref()
                .is_some_and(|capture| capture.has_radio);
        let has_logic_analyzer = displayed_logic
            || self
                .devices
                .iter()
                .any(|entry| entry.device.profile.model.is_logic_analyzer());
        let has_oscilloscope = displayed_scope
            || self
                .devices
                .iter()
                .any(|entry| entry.device.profile.model == DeviceModel::DsCopeU3P100);
        let has_radio_receiver = displayed_radio || !self.radios.is_empty();
        waveform_tabs(
            has_logic_analyzer,
            has_oscilloscope,
            has_radio_receiver,
            self.scope_function_settings.lissajous.enabled,
        )
    }

    fn select_waveform_tab(&mut self, tab: WaveformTab) {
        if self.active_waveform_tab != Some(tab) {
            if let Some(active) = self.active_waveform_tab {
                self.save_waveform_view_settings(active);
            }
            self.apply_waveform_view_settings(tab);
        }
        self.active_waveform_tab = Some(tab);
        if let Some(settings_tab) = tab.capture_settings_tab() {
            self.active_capture_settings_tab = settings_tab;
        }
    }

    fn available_capture_settings_tabs(&self) -> Vec<CaptureSettingsTab> {
        let has_logic_device = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model.is_logic_analyzer());
        let has_scope_device = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model == DeviceModel::DsCopeU3P100);
        let has_logic_capture = self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.has_logic)
            || self.logic_sessions.any(|capture| capture.has_logic)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_logic);
        let has_scope_capture = self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.has_scope)
            || self.scope_sessions.any(|capture| capture.has_scope)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_scope);
        capture_settings_tabs_for_view(
            self.active_waveform_tab,
            has_logic_device,
            has_scope_device,
            has_logic_capture,
            has_scope_capture,
        )
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
        let selected = if available.contains(&WaveformTab::Combined) {
            Some(WaveformTab::Combined)
        } else {
            available.first().copied()
        };
        if let Some(tab) = selected {
            self.select_waveform_tab(tab);
        } else {
            self.active_waveform_tab = None;
        }
    }

    fn is_busy(&self) -> bool {
        self.pending.is_some()
            || self.capture.is_some()
            || self.scheduled_repetitive_capture.is_some()
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

fn trigger_source_label(source: DsCopeTriggerSource, channel_names: &[String; 2]) -> String {
    match source {
        DsCopeTriggerSource::Auto => "Auto".to_owned(),
        DsCopeTriggerSource::ChannelZero => scope_channel_label(0, channel_names),
        DsCopeTriggerSource::ChannelOne => scope_channel_label(1, channel_names),
        DsCopeTriggerSource::ChannelZeroAndOne => format!(
            "{} AND {}",
            scope_channel_label(0, channel_names),
            scope_channel_label(1, channel_names)
        ),
        DsCopeTriggerSource::ChannelZeroOrOne => format!(
            "{} OR {}",
            scope_channel_label(0, channel_names),
            scope_channel_label(1, channel_names)
        ),
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
    channel_names: &[String; 2],
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
                            .selected_text(trigger_source_label(settings.source, channel_names))
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
                                                trigger_source_label(source, channel_names),
                                            );
                                        },
                                    );
                                }
                            });
                        ui.end_row();

                        if settings.source == DsCopeTriggerSource::Auto {
                            ui.label("Auto channel");
                            egui::ComboBox::from_id_salt("scope_trigger_auto_channel")
                                .selected_text(scope_channel_label(
                                    usize::from(settings.auto_channel),
                                    channel_names,
                                ))
                                .show_ui(ui, |ui| {
                                    for (channel, channel_enabled) in
                                        channels.into_iter().enumerate()
                                    {
                                        ui.add_enabled_ui(channel_enabled, |ui| {
                                            ui.selectable_value(
                                                &mut settings.auto_channel,
                                                channel as u8,
                                                scope_channel_label(channel, channel_names),
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

fn paint_scope_channel_picker(
    ui: &mut egui::Ui,
    id: &'static str,
    channel: &mut usize,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(scope_channel_label(*channel, channel_names))
        .show_ui(ui, |ui| {
            for (index, enabled) in channels.into_iter().enumerate() {
                ui.add_enabled_ui(enabled, |ui| {
                    ui.selectable_value(channel, index, scope_channel_label(index, channel_names));
                });
            }
        });
}

fn paint_scope_function_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeFunctionSettings,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::CollapsingHeader::new(RichText::new("Functions").strong())
        .default_open(false)
        .show(ui, |ui| {
            ui.checkbox(&mut settings.math.enabled, "Math");
            ui.add_enabled_ui(settings.math.enabled, |ui| {
                egui::Grid::new("dscope_math_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Operation");
                        egui::ComboBox::from_id_salt("scope_math_operation")
                            .selected_text(settings.math.operation.label())
                            .show_ui(ui, |ui| {
                                for operation in [
                                    ScopeMathOperation::Add,
                                    ScopeMathOperation::Subtract,
                                    ScopeMathOperation::Multiply,
                                    ScopeMathOperation::Divide,
                                ] {
                                    ui.selectable_value(
                                        &mut settings.math.operation,
                                        operation,
                                        operation.label(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("First source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_math_left_channel",
                            &mut settings.math.left_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();

                        ui.label("Second source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_math_right_channel",
                            &mut settings.math.right_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();
                    });
            });

            ui.add_space(6.0);
            ui.checkbox(&mut settings.fft.enabled, "FFT");
            ui.add_enabled_ui(settings.fft.enabled, |ui| {
                egui::Grid::new("dscope_fft_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_fft_channel",
                            &mut settings.fft.channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();

                        ui.label("Length");
                        egui::ComboBox::from_id_salt("scope_fft_length")
                            .selected_text(settings.fft.length.to_string())
                            .show_ui(ui, |ui| {
                                for length in FFT_LENGTHS {
                                    ui.selectable_value(
                                        &mut settings.fft.length,
                                        length,
                                        length.to_string(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Window");
                        egui::ComboBox::from_id_salt("scope_fft_window")
                            .selected_text(settings.fft.window.label())
                            .show_ui(ui, |ui| {
                                for window in [
                                    ScopeFftWindow::Rectangular,
                                    ScopeFftWindow::Hann,
                                    ScopeFftWindow::Hamming,
                                    ScopeFftWindow::Blackman,
                                    ScopeFftWindow::FlatTop,
                                ] {
                                    ui.selectable_value(
                                        &mut settings.fft.window,
                                        window,
                                        window.label(),
                                    );
                                }
                            });
                        ui.end_row();
                    });
                ui.small(RichText::new("Magnitude in dBV").color(TEXT_MUTED));
            });

            ui.add_space(6.0);
            let lissajous_available = channels.into_iter().filter(|enabled| *enabled).count() >= 2;
            ui.add_enabled(
                lissajous_available,
                egui::Checkbox::new(&mut settings.lissajous.enabled, "Lissajous tab (XY)"),
            );
            ui.add_enabled_ui(settings.lissajous.enabled && lissajous_available, |ui| {
                egui::Grid::new("dscope_lissajous_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("X source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_lissajous_x_channel",
                            &mut settings.lissajous.x_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();

                        ui.label("Y source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_lissajous_y_channel",
                            &mut settings.lissajous.y_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();
                    });
                ui.small(
                    RichText::new("Plots Y-channel voltage against X-channel voltage")
                        .color(TEXT_MUTED),
                );
            });
            if !lissajous_available {
                ui.small(RichText::new("Requires two enabled channels").color(TEXT_MUTED));
            }
        });
}

fn paint_scope_measurement_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeMeasurementSettings,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::CollapsingHeader::new(RichText::new("Measurements").strong())
        .default_open(false)
        .show(ui, |ui| {
            for (channel, channel_enabled) in channels.into_iter().enumerate() {
                let channel_settings = &mut settings.channels[channel];
                ui.add_enabled_ui(channel_enabled, |ui| {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut channel_settings.enabled,
                                RichText::new(format!(
                                    "{} measurements",
                                    scope_channel_label(channel, channel_names)
                                ))
                                .strong()
                                .color(SCOPE_COLORS[channel]),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Clear").clicked() {
                                        channel_settings.selected.fill(false);
                                    }
                                    if ui.small_button("All").clicked() {
                                        channel_settings.selected.fill(true);
                                    }
                                },
                            );
                        });
                        ui.add_enabled_ui(channel_settings.enabled, |ui| {
                            egui::Grid::new(("dscope_measurement_settings", channel))
                                .num_columns(2)
                                .spacing([12.0, 4.0])
                                .show(ui, |ui| {
                                    for (index, kind) in
                                        SCOPE_MEASUREMENT_KINDS.into_iter().enumerate()
                                    {
                                        ui.checkbox(
                                            &mut channel_settings.selected[kind.index()],
                                            kind.label(),
                                        );
                                        if index % 2 == 1 {
                                            ui.end_row();
                                        }
                                    }
                                });
                        });
                    });
                });
                if !channel_enabled {
                    ui.small(
                        RichText::new(format!(
                            "{} is disabled",
                            scope_channel_label(channel, channel_names)
                        ))
                        .color(TEXT_MUTED),
                    );
                }
                if channel + 1 < channels.len() {
                    ui.add_space(6.0);
                }
            }
            ui.small(RichText::new("Calculated over the visible window").color(TEXT_MUTED));
        });
}

fn paint_scope_marker_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeMarkerSettings,
    editor: &mut Option<ScopeMarkerEditor>,
    viewport: WaveformViewport,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::CollapsingHeader::new(RichText::new("Markers").strong())
        .default_open(false)
        .show(ui, |ui| {
            for (channel, channel_enabled) in channels.into_iter().enumerate() {
                ui.add_enabled_ui(channel_enabled, |ui| {
                    ui.group(|ui| {
                        let total = settings.channels[channel].total_count();
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} markers",
                                    scope_channel_label(channel, channel_names)
                                ))
                                    .strong()
                                    .color(SCOPE_COLORS[channel]),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            total < SCOPE_MARKER_LIMIT,
                                            egui::Button::new("+ Add marker"),
                                        )
                                        .clicked()
                                    {
                                        *editor = Some(ScopeMarkerEditor::for_new(
                                            *settings,
                                            viewport,
                                            channel,
                                            ScopeMarkerKind::Time,
                                        ));
                                    }
                                    ui.monospace(format!("{total} / {SCOPE_MARKER_LIMIT}"));
                                },
                            );
                        });

                        let mut remove = None;
                        for kind in [ScopeMarkerKind::Time, ScopeMarkerKind::Voltage] {
                            let count = settings.channels[channel].count(kind);
                            for index in 0..count {
                                let (_, comparison, color) = settings.channels[channel]
                                    .marker(kind, index)
                                    .expect("marker index is active");
                                ui.horizontal(|ui| {
                                    ui.colored_label(
                                        color,
                                        RichText::new(format!(
                                            "{}{}{}",
                                            kind.short_label(),
                                            channel,
                                            index + 1
                                        ))
                                        .strong(),
                                    );
                                    ui.small(kind.label());
                                    ui.small(match comparison {
                                        Some(other) => format!(
                                            "vs {}{}{}",
                                            kind.short_label(),
                                            channel,
                                            other + 1
                                        ),
                                        None => "Standalone".to_owned(),
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .small_button("×")
                                                .on_hover_text("Remove marker")
                                                .clicked()
                                            {
                                                remove = Some((kind, index));
                                            }
                                            if ui.small_button("Edit…").clicked() {
                                                *editor = ScopeMarkerEditor::for_existing(
                                                    *settings,
                                                    ScopeMarkerKey {
                                                        channel,
                                                        kind,
                                                        index,
                                                    },
                                                );
                                            }
                                        },
                                    );
                                });
                            }
                        }
                        if let Some((kind, index)) = remove {
                            settings.channels[channel].remove_marker(kind, index);
                        }
                        if total > 0 {
                            ui.small(
                                RichText::new(
                                    "Drag time markers left/right and amplitude markers up/down in the OSC plot",
                                )
                                .color(TEXT_MUTED),
                            );
                        }
                    });
                });
                if !channel_enabled {
                    ui.small(
                        RichText::new(format!(
                            "{} is disabled",
                            scope_channel_label(channel, channel_names)
                        ))
                        .color(TEXT_MUTED),
                    );
                }
                if channel + 1 < channels.len() {
                    ui.add_space(6.0);
                }
            }
        });
}

fn paint_scope_marker_editor(
    context: &egui::Context,
    editor_state: &mut Option<ScopeMarkerEditor>,
    settings: &mut ScopeMarkerSettings,
    viewport: WaveformViewport,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    let Some(mut editor) = *editor_state else {
        return;
    };
    let mut open = true;
    let mut cancel = false;
    let mut save = false;
    let screen_rect = context.input(|input| input.screen_rect());
    egui::Area::new(egui::Id::new("scope_marker_modal_backdrop"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen_rect.min)
        .show(context, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, Color32::from_black_alpha(96));
        });
    let title = if editor.original.is_some() {
        "Marker settings"
    } else {
        "Add marker"
    };
    egui::Window::new(title)
        .open(&mut open)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(360.0)
        .show(context, |ui| {
            ui.label(
                RichText::new(format!(
                    "{} marker",
                    scope_channel_label(editor.channel, channel_names)
                ))
                .strong()
                .color(SCOPE_COLORS[editor.channel]),
            );
            let kind_before = editor.kind;
            ui.horizontal(|ui| {
                ui.label("Type");
                egui::ComboBox::from_id_salt("scope_marker_kind")
                    .selected_text(editor.kind.label())
                    .show_ui(ui, |ui| {
                        for kind in [ScopeMarkerKind::Time, ScopeMarkerKind::Voltage] {
                            ui.selectable_value(&mut editor.kind, kind, kind.label());
                        }
                    });
            });
            if kind_before != editor.kind {
                editor.comparison = None;
                if editor.original.is_none() {
                    editor.position = scope_marker_default_position(editor.kind, viewport);
                }
            }

            ui.add_space(8.0);
            let position_label = match editor.kind {
                ScopeMarkerKind::Time => "Horizontal position",
                ScopeMarkerKind::Voltage => "Vertical position",
            };
            ui.label(position_label);
            let mut position_percent = editor.position * 100.0;
            if ui
                .add(
                    egui::Slider::new(&mut position_percent, 0.0..=100.0)
                        .suffix(" %")
                        .max_decimals(1),
                )
                .changed()
            {
                editor.position = position_percent / 100.0;
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Color");
                ui.color_edit_button_srgba(&mut editor.color);
            });

            ui.add_space(8.0);
            ui.label("Math");
            let marker_count = settings.channels[editor.channel].count(editor.kind);
            let original_index = editor
                .original
                .filter(|key| key.channel == editor.channel && key.kind == editor.kind)
                .map(|key| key.index);
            if editor
                .comparison
                .is_some_and(|index| index >= marker_count || Some(index) == original_index)
            {
                editor.comparison = None;
            }
            egui::ComboBox::from_id_salt("scope_marker_comparison")
                .selected_text(match editor.comparison {
                    Some(index) => format!(
                        "Compare with {}{}{}",
                        editor.kind.short_label(),
                        editor.channel,
                        index + 1
                    ),
                    None => "Standalone (no calculation)".to_owned(),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut editor.comparison,
                        None,
                        "Standalone (no calculation)",
                    );
                    for index in 0..marker_count {
                        if Some(index) == original_index {
                            continue;
                        }
                        ui.selectable_value(
                            &mut editor.comparison,
                            Some(index),
                            format!(
                                "{}{}{}",
                                editor.kind.short_label(),
                                editor.channel,
                                index + 1
                            ),
                        );
                    }
                });
            ui.small(
                RichText::new(match (editor.kind, editor.comparison) {
                    (ScopeMarkerKind::Time, Some(_)) => {
                        "The OSC plot displays Δt and the reciprocal frequency."
                    }
                    (ScopeMarkerKind::Voltage, Some(_)) => {
                        "The OSC plot displays ΔV using this channel's scale."
                    }
                    (_, None) => "Standalone markers do not display a delta calculation.",
                })
                .color(TEXT_MUTED),
            );

            let stays_on_channel = editor
                .original
                .is_some_and(|key| key.channel == editor.channel);
            let target_total = settings.channels[editor.channel].total_count();
            let can_save =
                channels[editor.channel] && (stays_on_channel || target_total < SCOPE_MARKER_LIMIT);
            if !can_save {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "This channel already has the maximum of {SCOPE_MARKER_LIMIT} markers."
                    ))
                    .color(Color32::LIGHT_RED),
                );
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui
                    .add_enabled(can_save, egui::Button::new("Save marker"))
                    .clicked()
                {
                    save = true;
                }
            });
        });

    if context.input(|input| input.key_pressed(egui::Key::Escape)) {
        cancel = true;
    }
    if save {
        editor.save(settings);
        *editor_state = None;
    } else if cancel || !open {
        *editor_state = None;
    } else {
        *editor_state = Some(editor);
    }
}

fn paint_scope_channel_settings(
    ui: &mut egui::Ui,
    channel_index: usize,
    settings: &mut ScopeChannelSettings,
    channel_color: &mut Color32,
    channel_name: &mut String,
    other_channel_enabled: bool,
    availability: CaptureSettingsAvailability,
) {
    egui::Frame::none()
        .fill(PANEL.gamma_multiply(0.72))
        .rounding(egui::Rounding::same(5.0))
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let can_toggle_enable = !settings.enabled || other_channel_enabled;
            let enable_label = format!(
                "{} enabled",
                channel_label("CH", channel_index, channel_name)
            );
            let enable_control = egui::Checkbox::new(&mut settings.enabled, enable_label);
            ui.add_enabled(
                availability.channel_enable && can_toggle_enable,
                enable_control,
            )
            .on_hover_text("At least one oscilloscope channel must remain enabled");
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add_enabled(
                    availability.capture,
                    egui::TextEdit::singleline(channel_name)
                        .char_limit(CHANNEL_NAME_MAX_CHARS)
                        .hint_text(format!("CH{channel_index}")),
                );
            });
            ui.add_enabled_ui(availability.input_range && settings.enabled, |ui| {
                ui.label("Input range");
                paint_scope_input_range_encoder(
                    ui,
                    channel_index,
                    &mut settings.volts_per_div_mv,
                    channel_color,
                );
            });
            ui.add_space(12.0);
            ui.add_enabled_ui(availability.capture && settings.enabled, |ui| {
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
                                for attenuation in [1, 10, 100] {
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

#[derive(Clone, Copy)]
struct CaptureSettingsAvailability {
    capture: bool,
    sample_rate: bool,
    channel_enable: bool,
    input_range: bool,
    manual_calibration: bool,
}

struct ScopeCaptureSettingsEditor<'a> {
    settings: &'a mut ScopeCaptureSettings,
    auto_sample_rate: &'a mut bool,
    smart_timing_enabled: &'a mut bool,
}

struct LogicCaptureSettingsEditor<'a> {
    settings: &'a mut LogicCaptureSettings,
    speed: UsbSpeedClass,
    acquisition_mode: &'a mut AcquisitionMode,
    repetitive_interval_seconds: &'a mut f32,
    smart_timing_enabled: &'a mut bool,
    marker_settings: &'a mut LogicMarkerSettings,
    protocol_analyzer: &'a mut ProtocolAnalyzerSettings,
    displayed_sample_count: usize,
    displayed_sample_rate_hz: u64,
}

struct ScopeVisualizationSettingsEditor<'a> {
    channel_colors: &'a mut [Color32; 2],
    scope_channel_names: &'a mut [String; 2],
}

fn paint_logic_trigger_conditions(
    ui: &mut egui::Ui,
    enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
    conditions: &mut [Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT],
    id_source: impl std::hash::Hash,
) {
    ui.vertical(|ui| {
        ui.small(RichText::new("All selected conditions must match together").color(TEXT_MUTED));
        for channel in 0..LOGIC_CHANNEL_COUNT {
            if !enabled_channels[channel] {
                continue;
            }
            ui.horizontal(|ui| {
                ui.monospace(format!("CH {channel}"));
                egui::ComboBox::from_id_salt((&id_source, channel))
                    .width(92.0)
                    .selected_text(
                        conditions[channel]
                            .map(logic_trigger_condition_label)
                            .unwrap_or("Don't care"),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut conditions[channel], None, "Don't care");
                        for condition in [
                            DsLogicTriggerCondition::RisingEdge,
                            DsLogicTriggerCondition::FallingEdge,
                            DsLogicTriggerCondition::EitherEdge,
                            DsLogicTriggerCondition::High,
                            DsLogicTriggerCondition::Low,
                        ] {
                            ui.selectable_value(
                                &mut conditions[channel],
                                Some(condition),
                                logic_trigger_condition_label(condition),
                            );
                        }
                    });
            });
        }
        if !conditions.iter().any(Option::is_some) {
            ui.small(RichText::new("Choose at least one condition").color(Color32::LIGHT_RED));
        }
    });
}

fn paint_capture_settings(
    ui: &mut egui::Ui,
    tab: CaptureSettingsTab,
    logic_editor: LogicCaptureSettingsEditor<'_>,
    scope_editor: ScopeCaptureSettingsEditor<'_>,
    visualization_editor: ScopeVisualizationSettingsEditor<'_>,
    availability: CaptureSettingsAvailability,
    calibration_action: &mut Option<ScopeCalibrationAction>,
) {
    let LogicCaptureSettingsEditor {
        settings: logic_settings,
        speed: logic_speed,
        acquisition_mode: logic_acquisition_mode,
        repetitive_interval_seconds,
        smart_timing_enabled: logic_smart_timing_enabled,
        marker_settings: logic_marker_settings,
        protocol_analyzer,
        displayed_sample_count: displayed_logic_sample_count,
        displayed_sample_rate_hz: displayed_logic_sample_rate_hz,
    } = logic_editor;
    let ScopeCaptureSettingsEditor {
        settings: scope_settings,
        auto_sample_rate: scope_auto_sample_rate,
        smart_timing_enabled: scope_smart_timing_enabled,
    } = scope_editor;
    let ScopeVisualizationSettingsEditor {
        channel_colors: scope_channel_colors,
        scope_channel_names,
    } = visualization_editor;
    let card_width = ui.available_width();
    egui::Frame::none()
        .fill(BACKGROUND)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            let content_width = (card_width - 20.0).max(0.0);
            ui.set_width(content_width);
            ui.set_max_width(content_width);
            ui.label(
                RichText::new(match tab {
                    CaptureSettingsTab::DsLogic => "DSLogic 16-channel analyzer",
                    CaptureSettingsTab::DsCope => "DSCope U3P100",
                })
                .strong(),
            );
            ui.add_space(6.0);
            match tab {
                CaptureSettingsTab::DsLogic => {
                    if logic_settings.operation_mode == DsLogicOperationMode::InternalTest {
                        logic_settings.enabled_channels = [true; LOGIC_CHANNEL_COUNT];
                        logic_settings.trigger_enabled = false;
                        set_logic_sample_rate_preserving_duration(
                            logic_settings,
                            100_000_000,
                        );
                    }
                    if *logic_acquisition_mode == AcquisitionMode::Loop {
                        logic_settings.trigger_enabled = false;
                    }
                    if logic_settings.operation_mode != DsLogicOperationMode::Buffer
                        && logic_settings.trigger_mode == LogicTriggerMode::Smart
                    {
                        logic_settings.trigger_mode = LogicTriggerMode::Simple;
                    }
                    for channel in 0..LOGIC_CHANNEL_COUNT {
                        if !logic_settings.enabled_channels[channel] {
                            logic_settings.trigger_conditions[channel] = None;
                            for stage in &mut logic_settings.smart_trigger_conditions {
                                stage[channel] = None;
                            }
                        }
                    }
                    if logic_settings.trigger_enabled
                        && logic_settings.trigger_mode == LogicTriggerMode::Simple
                        && !logic_settings.trigger_conditions.iter().any(Option::is_some)
                    {
                        if let Some(channel) = logic_settings
                            .enabled_channels
                            .iter()
                            .position(|enabled| *enabled)
                        {
                            logic_settings.trigger_conditions[channel] =
                                Some(DsLogicTriggerCondition::RisingEdge);
                        }
                    }
                    if logic_settings.trigger_enabled
                        && logic_settings.trigger_mode == LogicTriggerMode::Smart
                    {
                        let default_channel = logic_settings
                            .enabled_channels
                            .iter()
                            .position(|enabled| *enabled);
                        for stage_index in
                            0..usize::from(logic_settings.smart_trigger_stage_count)
                        {
                            if !logic_settings.smart_trigger_conditions[stage_index]
                                .iter()
                                .any(Option::is_some)
                            {
                                if let Some(channel) = default_channel {
                                    logic_settings.smart_trigger_conditions[stage_index][channel] =
                                        Some(if stage_index % 2 == 0 {
                                            DsLogicTriggerCondition::RisingEdge
                                        } else {
                                            DsLogicTriggerCondition::FallingEdge
                                        });
                                }
                            }
                        }
                    }
                    let maximum_sample_rate_hz =
                        logic_settings.maximum_sample_rate_hz(logic_speed);
                    if logic_settings.sample_rate_hz > maximum_sample_rate_hz {
                        let sample_rate_hz = DSLOGIC_SAMPLE_RATES_HZ
                            .iter()
                            .rev()
                            .copied()
                            .find(|rate| *rate <= maximum_sample_rate_hz)
                            .unwrap_or(SAMPLE_RATE_HZ);
                        set_logic_sample_rate_preserving_duration(
                            logic_settings,
                            sample_rate_hz,
                        );
                    }
                    let enabled_channel_count = logic_settings
                        .enabled_channels
                        .iter()
                        .filter(|enabled| **enabled)
                        .count();
                    egui::Grid::new("dslogic_capture_settings")
                        .num_columns(2)
                        .min_col_width(90.0)
                        .max_col_width(((content_width - 12.0) * 0.5).max(90.0))
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Samples");
                            ui.monospace(format_sample_count(logic_settings.sample_count));
                            ui.end_row();
                            ui.label("Maximum");
                            ui.monospace(format_sample_rate(maximum_sample_rate_hz));
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    ui.checkbox(
                        logic_smart_timing_enabled,
                        "Smart timing on waveform hover",
                    )
                    .on_hover_text(
                        "Show edge width, rising-edge period, frequency, and duty cycle for the digital waveform under the mouse",
                    );
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(RichText::new("Options").strong())
                        .default_open(false)
                        .show(ui, |ui| {
                            const CONTROL_WIDTH: f32 = 100.0;
                            ui.set_max_width(ui.available_width());
                            ui.add_enabled_ui(availability.capture, |ui| {
                                egui::Grid::new("dslogic_options")
                                    .num_columns(2)
                                    .max_col_width(126.0)
                                    .spacing([8.0, 6.0])
                                    .show(ui, |ui| {
                                        ui.label("Operation mode");
                                        let previous_operation_mode =
                                            logic_settings.operation_mode;
                                        egui::ComboBox::from_id_salt("logic_operation_mode")
                                            .width(CONTROL_WIDTH)
                                            .selected_text(match logic_settings.operation_mode {
                                                DsLogicOperationMode::Buffer => "Buffer mode",
                                                DsLogicOperationMode::Stream => "Stream mode",
                                                DsLogicOperationMode::InternalTest => {
                                                    "Internal test"
                                                }
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut logic_settings.operation_mode,
                                                    DsLogicOperationMode::Buffer,
                                                    "Buffer mode",
                                                );
                                                ui.selectable_value(
                                                    &mut logic_settings.operation_mode,
                                                    DsLogicOperationMode::Stream,
                                                    "Stream mode",
                                                );
                                                ui.selectable_value(
                                                    &mut logic_settings.operation_mode,
                                                    DsLogicOperationMode::InternalTest,
                                                    "Internal test",
                                                );
                                            });
                                        if logic_settings.operation_mode
                                            != previous_operation_mode
                                        {
                                            let selected_operation_mode =
                                                logic_settings.operation_mode;
                                            logic_settings.operation_mode =
                                                previous_operation_mode;
                                            set_logic_operation_mode_preserving_duration(
                                                logic_settings,
                                                selected_operation_mode,
                                            );
                                        }
                                        ui.end_row();

                                        ui.label("Stop options");
                                        ui.add_enabled_ui(
                                            logic_settings.operation_mode
                                                != DsLogicOperationMode::Stream,
                                            |ui| {
                                                egui::ComboBox::from_id_salt("logic_stop_option")
                                                    .width(CONTROL_WIDTH)
                                                    .selected_text(match logic_settings.stop_option {
                                                        DsLogicStopOption::StopImmediately => {
                                                            "Stop immediately"
                                                        }
                                                        DsLogicStopOption::UploadCapturedData => {
                                                            "Upload data"
                                                        }
                                                    })
                                                    .show_ui(ui, |ui| {
                                                        ui.selectable_value(
                                                            &mut logic_settings.stop_option,
                                                            DsLogicStopOption::StopImmediately,
                                                            "Stop immediately",
                                                        );
                                                        ui.selectable_value(
                                                            &mut logic_settings.stop_option,
                                                            DsLogicStopOption::UploadCapturedData,
                                                            "Upload data",
                                                        );
                                                    });
                                            },
                                        );
                                        ui.end_row();

                                        ui.label("Threshold level");
                                        ui.add(
                                            egui::DragValue::new(
                                                &mut logic_settings.threshold_voltage,
                                            )
                                            .range(0.0..=5.0)
                                            .speed(0.1)
                                            .fixed_decimals(1)
                                            .suffix(" V"),
                                        );
                                        ui.end_row();

                                        ui.label("Filter targets");
                                        egui::ComboBox::from_id_salt("logic_input_filter")
                                            .width(CONTROL_WIDTH)
                                            .selected_text(match logic_settings.input_filter {
                                                DsLogicInputFilter::None => "None",
                                                DsLogicInputFilter::OneSampleClock => {
                                                    "1 sample clock"
                                                }
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut logic_settings.input_filter,
                                                    DsLogicInputFilter::None,
                                                    "None",
                                                );
                                                ui.selectable_value(
                                                    &mut logic_settings.input_filter,
                                                    DsLogicInputFilter::OneSampleClock,
                                                    "1 sample clock",
                                                );
                                            });
                                        ui.end_row();

                                        ui.label("Enable RLE compress");
                                        ui.add_enabled(
                                            logic_settings.operation_mode
                                                != DsLogicOperationMode::Stream,
                                            egui::Checkbox::without_text(
                                                &mut logic_settings.rle_enabled,
                                            ),
                                        );
                                        ui.end_row();

                                        ui.label("Using external clock");
                                        ui.checkbox(&mut logic_settings.external_clock, "");
                                        ui.end_row();

                                        ui.label("Using clock negedge");
                                        ui.checkbox(&mut logic_settings.falling_clock_edge, "");
                                        ui.end_row();
                                    });
                            });
                        });
                    ui.add_space(8.0);
                    let capture_summary = format!(
                        "Capture · {} @ {}",
                        logic_capture_duration_label(*logic_settings),
                        format_sample_rate(logic_settings.sample_rate_hz)
                    );
                    egui::CollapsingHeader::new(RichText::new(capture_summary).strong())
                        .id_salt("dslogic_capture_section")
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width());
                            egui::Grid::new("dslogic_capture_menu")
                                .num_columns(2)
                                .max_col_width(126.0)
                                .spacing([8.0, 6.0])
                                .show(ui, |ui| {
                                    ui.label("Mode");
                                    ui.add_enabled_ui(availability.capture, |ui| {
                                        let loop_available = logic_settings.operation_mode
                                            == DsLogicOperationMode::Stream;
                                        egui::ComboBox::from_id_salt("logic_acquisition_mode")
                                            .width(100.0)
                                            .selected_text(logic_acquisition_mode.logic_label())
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    logic_acquisition_mode,
                                                    AcquisitionMode::Single,
                                                    "Single",
                                                );
                                                ui.selectable_value(
                                                    logic_acquisition_mode,
                                                    AcquisitionMode::Repetitive,
                                                    "Repetitive",
                                                );
                                                ui.add_enabled_ui(loop_available, |ui| {
                                                    ui.selectable_value(
                                                        logic_acquisition_mode,
                                                        AcquisitionMode::Loop,
                                                        "Loop",
                                                    )
                                                    .on_disabled_hover_text(
                                                        "Loop requires DSLogic Stream mode",
                                                    );
                                                });
                                            });
                                    });
                                    ui.end_row();

                                    ui.label("Duration");
                                    ui.add_enabled_ui(availability.capture, |ui| {
                                        egui::ComboBox::from_id_salt("logic_capture_duration")
                                            .width(100.0)
                                            .selected_text(logic_capture_duration_label(
                                                *logic_settings,
                                            ))
                                            .show_ui(ui, |ui| {
                                                for (duration_seconds, sample_count) in
                                                    logic_capture_duration_options(*logic_settings)
                                                {
                                                    ui.selectable_value(
                                                        &mut logic_settings.sample_count,
                                                        sample_count,
                                                        format_duration(Duration::from_secs_f64(
                                                            duration_seconds,
                                                        )),
                                                    );
                                                }
                                            });
                                    });
                                    ui.end_row();

                                    ui.label("Sample rate");
                                    ui.add_enabled_ui(
                                        availability.capture
                                            && logic_settings.operation_mode
                                                != DsLogicOperationMode::InternalTest,
                                        |ui| {
                                            let previous_rate = logic_settings.sample_rate_hz;
                                            egui::ComboBox::from_id_salt("logic_sample_rate")
                                                .width(100.0)
                                                .selected_text(format_sample_rate(
                                                    logic_settings.sample_rate_hz,
                                                ))
                                                .show_ui(ui, |ui| {
                                                    for rate in DSLOGIC_SAMPLE_RATES_HZ
                                                        .iter()
                                                        .copied()
                                                        .filter(|rate| {
                                                            *rate <= maximum_sample_rate_hz
                                                        })
                                                    {
                                                        ui.selectable_value(
                                                            &mut logic_settings.sample_rate_hz,
                                                            rate,
                                                            format_sample_rate(rate),
                                                        );
                                                    }
                                                });
                                            if logic_settings.sample_rate_hz != previous_rate {
                                                let selected_rate = logic_settings.sample_rate_hz;
                                                logic_settings.sample_rate_hz = previous_rate;
                                                set_logic_sample_rate_preserving_duration(
                                                    logic_settings,
                                                    selected_rate,
                                                );
                                            }
                                        },
                                    );
                                    ui.end_row();

                                    ui.label("Hardware trigger");
                                    let trigger_response = ui.add_enabled(
                                        availability.capture
                                            && logic_settings.operation_mode
                                                != DsLogicOperationMode::InternalTest
                                            && *logic_acquisition_mode != AcquisitionMode::Loop,
                                        egui::Checkbox::without_text(
                                            &mut logic_settings.trigger_enabled,
                                        ),
                                    )
                                    .on_hover_text(
                                        "Arm the analyzer and wait for the configured hardware trigger",
                                    );
                                    if trigger_response.changed()
                                        && logic_settings.trigger_enabled
                                        && logic_settings.trigger_mode == LogicTriggerMode::Simple
                                        && !logic_settings
                                            .trigger_conditions
                                            .iter()
                                            .any(Option::is_some)
                                    {
                                        if let Some(channel) = logic_settings
                                            .enabled_channels
                                            .iter()
                                            .position(|enabled| *enabled)
                                        {
                                            logic_settings.trigger_conditions[channel] =
                                                Some(DsLogicTriggerCondition::RisingEdge);
                                        }
                                    }
                                    ui.end_row();

                                    if logic_settings.trigger_enabled {
                                        ui.label("Trigger mode");
                                        ui.add_enabled_ui(availability.capture, |ui| {
                                            egui::ComboBox::from_id_salt("logic_trigger_mode")
                                                .width(92.0)
                                                .selected_text(match logic_settings.trigger_mode {
                                                    LogicTriggerMode::Simple => "Simple",
                                                    LogicTriggerMode::Smart => "Smart",
                                                })
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut logic_settings.trigger_mode,
                                                        LogicTriggerMode::Simple,
                                                        "Simple",
                                                    )
                                                    .on_hover_text(
                                                        "All channel conditions match at one sample",
                                                    );
                                                    let smart_available =
                                                        logic_settings.operation_mode
                                                            == DsLogicOperationMode::Buffer;
                                                    ui.add_enabled_ui(smart_available, |ui| {
                                                        ui.selectable_value(
                                                            &mut logic_settings.trigger_mode,
                                                            LogicTriggerMode::Smart,
                                                            if smart_available {
                                                                "Smart"
                                                            } else {
                                                                "Smart (Buffer mode required)"
                                                            },
                                                        )
                                                        .on_hover_text(
                                                            "Match two or more channel-condition stages in order",
                                                        )
                                                        .on_disabled_hover_text(
                                                            "Smart trigger requires Buffer mode. Select Buffer under Capture mode to enable it.",
                                                        );
                                                    });
                                                });
                                        });
                                        ui.end_row();

                                        match logic_settings.trigger_mode {
                                            LogicTriggerMode::Simple => {
                                                ui.label("Conditions");
                                                ui.add_enabled_ui(availability.capture, |ui| {
                                                    paint_logic_trigger_conditions(
                                                        ui,
                                                        logic_settings.enabled_channels,
                                                        &mut logic_settings.trigger_conditions,
                                                        "logic_simple_trigger",
                                                    );
                                                });
                                                ui.end_row();
                                            }
                                            LogicTriggerMode::Smart => {
                                                ui.label("Smart stages");
                                                ui.add_enabled_ui(availability.capture, |ui| {
                                                    ui.vertical(|ui| {
                                                        ui.horizontal(|ui| {
                                                            ui.label("Stages");
                                                            ui.add(
                                                                egui::DragValue::new(
                                                                    &mut logic_settings
                                                                        .smart_trigger_stage_count,
                                                                )
                                                                .range(
                                                                    2..=DSLOGIC_TRIGGER_STAGE_COUNT
                                                                        as u8,
                                                                ),
                                                            );
                                                        });
                                                        ui.small(
                                                            RichText::new(
                                                                "Each stage matches its channels together; stages match in order",
                                                            )
                                                            .color(TEXT_MUTED),
                                                        );
                                                        for stage_index in 0..usize::from(
                                                            logic_settings.smart_trigger_stage_count,
                                                        ) {
                                                            egui::CollapsingHeader::new(format!(
                                                                "Stage {}",
                                                                stage_index + 1
                                                            ))
                                                            .default_open(stage_index < 2)
                                                            .show(ui, |ui| {
                                                                paint_logic_trigger_conditions(
                                                                    ui,
                                                                    logic_settings.enabled_channels,
                                                                    &mut logic_settings
                                                                        .smart_trigger_conditions
                                                                        [stage_index],
                                                                    (
                                                                        "logic_smart_trigger",
                                                                        stage_index,
                                                                    ),
                                                                );
                                                            });
                                                        }
                                                    });
                                                });
                                                ui.end_row();
                                            }
                                        }
                                    }

                                    if *logic_acquisition_mode == AcquisitionMode::Repetitive {
                                        ui.label("Repeat interval");
                                        if logic_settings.operation_mode
                                            == DsLogicOperationMode::Stream
                                        {
                                            ui.label(RichText::new("0.1 s").color(TEXT_MUTED))
                                                .on_hover_text(
                                                    "Stream-mode Repetitive uses the original fixed 0.1 s interval",
                                                );
                                        } else {
                                            ui.add_enabled(
                                                availability.capture,
                                                egui::DragValue::new(
                                                    repetitive_interval_seconds,
                                                )
                                                .range(0.1..=10.0)
                                                .speed(0.1)
                                                .fixed_decimals(1)
                                                .suffix(" s"),
                                            )
                                            .on_hover_text("Delay between complete captures");
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(
                        RichText::new(format!(
                            "Channels ({enabled_channel_count}/{LOGIC_CHANNEL_COUNT} enabled)"
                        ))
                        .strong(),
                    )
                    .id_salt("dslogic_channels_section")
                    .default_open(true)
                    .show(ui, |ui| {
                            let channel_controls_enabled = availability.capture
                                && logic_settings.operation_mode
                                    != DsLogicOperationMode::InternalTest;
                            ui.add_enabled_ui(channel_controls_enabled, |ui| {
                                egui::Grid::new("dslogic_channels")
                                    .num_columns(8)
                                    .min_col_width(20.0)
                                    .max_col_width(24.0)
                                    .spacing([5.0, 4.0])
                                    .show(ui, |ui| {
                                        for (channel, enabled) in logic_settings
                                            .enabled_channels
                                            .iter_mut()
                                            .enumerate()
                                        {
                                            ui.vertical_centered(|ui| {
                                                ui.small(channel.to_string());
                                                ui.add(egui::Checkbox::without_text(enabled));
                                            });
                                            if channel % 8 == 7 {
                                                ui.end_row();
                                            }
                                        }
                                    });
                                ui.horizontal(|ui| {
                                    if ui.small_button("Enable all").clicked() {
                                        logic_settings.enabled_channels =
                                            [true; LOGIC_CHANNEL_COUNT];
                                    }
                                    if ui.small_button("Disable all").clicked() {
                                        logic_settings.enabled_channels =
                                            [false; LOGIC_CHANNEL_COUNT];
                                    }
                                });
                            });
                            if !logic_settings
                                .enabled_channels
                                .iter()
                                .any(|enabled| *enabled)
                            {
                                ui.small(
                                    RichText::new("Enable at least one channel")
                                        .color(Color32::LIGHT_RED),
                                );
                            }
                        });
                    ui.add_space(8.0);
                    paint_logic_marker_settings(
                        ui,
                        logic_marker_settings,
                        displayed_logic_sample_count,
                        displayed_logic_sample_rate_hz,
                    );
                    ui.add_space(8.0);
                    paint_protocol_analyzer_settings(
                        ui,
                        protocol_analyzer,
                        &logic_settings.enabled_channels,
                    );
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
                    egui::Grid::new("dscope_common_capture_settings")
                        .num_columns(2)
                        .spacing([8.0, 5.0])
                        .show(ui, |ui| {
                            ui.label("Sample rate");
                            ui.horizontal(|ui| {
                                ui.add_enabled_ui(
                                    availability.sample_rate && !*scope_auto_sample_rate,
                                    |ui| {
                                        egui::ComboBox::from_id_salt("scope_sample_rate")
                                            .width(100.0)
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
                                    },
                                );
                                ui.add_enabled(
                                    availability.sample_rate,
                                    egui::Checkbox::new(scope_auto_sample_rate, "Auto"),
                                );
                            });
                            ui.end_row();

                            ui.label("Buffer");
                            ui.add_enabled_ui(availability.capture, |ui| {
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
                            });
                            ui.end_row();

                            ui.label("Maximum");
                            ui.monospace(format_sample_rate(maximum_sample_rate_hz));
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    ui.checkbox(
                        scope_smart_timing_enabled,
                        "Smart timing on waveform hover",
                    )
                    .on_hover_text(
                        "Show elapsed time from the trigger crossing and the voltage at the waveform point under the mouse",
                    );
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(RichText::new("Options").strong())
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.add_enabled_ui(availability.capture, |ui| {
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
                            });
                            ui.add_space(6.0);
                            ui.label(RichText::new("Calibration").color(TEXT_MUTED));
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(availability.capture, egui::Button::new("Auto…"))
                                    .clicked()
                                {
                                    *calibration_action = Some(ScopeCalibrationAction::Auto);
                                }
                                if ui
                                    .add_enabled(
                                        availability.manual_calibration,
                                        egui::Button::new("Manual…"),
                                    )
                                    .clicked()
                                {
                                    *calibration_action = Some(ScopeCalibrationAction::Manual);
                                }
                            });
                        });
                    ui.add_space(8.0);
                    let channel_count = scope_settings.channels.len();
                    let channel_enabled = scope_settings.channels.map(|channel| channel.enabled);
                    for (index, channel) in scope_settings.channels.iter_mut().enumerate() {
                        paint_scope_channel_settings(
                            ui,
                            index,
                            channel,
                            &mut scope_channel_colors[index],
                            &mut scope_channel_names[index],
                            channel_enabled[1 - index],
                            availability,
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

fn paint_logic_marker_settings(
    ui: &mut egui::Ui,
    settings: &mut LogicMarkerSettings,
    logic_sample_count: usize,
    logic_sample_rate_hz: u64,
) {
    let has_waveform = logic_sample_count > 1 && logic_sample_rate_hz != 0;
    egui::CollapsingHeader::new(RichText::new("Markers").strong())
        .id_salt("logic_markers_section")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if settings.adding {
                    if ui.button("Cancel").clicked() {
                        settings.cancel();
                    }
                } else if ui
                    .add_enabled(
                        has_waveform && settings.markers.len() < LOGIC_MARKER_LIMIT,
                        egui::Button::new("+ Add"),
                    )
                    .on_disabled_hover_text(if has_waveform {
                        "The maximum of 20 markers has been reached"
                    } else {
                        "Capture or open logic samples before adding a marker"
                    })
                    .clicked()
                {
                    settings.arm();
                }
                ui.label(
                    RichText::new(format!("{}/{}", settings.markers.len(), LOGIC_MARKER_LIMIT))
                        .small()
                        .color(TEXT_MUTED),
                );
                if !settings.markers.is_empty()
                    && ui
                        .small_button("Clear")
                        .on_hover_text("Remove all markers")
                        .clicked()
                {
                    settings.clear();
                }
            });

            if settings.adding {
                ui.add_space(4.0);
                ui.small(
                    RichText::new(
                        "Drag with the left mouse button. Endpoints snap to nearby edges on the channel under the pointer.",
                    )
                    .color(ACCENT),
                );
            } else if settings.markers.is_empty() {
                ui.add_space(4.0);
                ui.small(RichText::new("No markers").color(TEXT_MUTED));
            }

            let mut remove = None;
            for (index, marker) in settings.markers.iter_mut().enumerate() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    paint_logic_marker_color_picker(ui, index, &mut marker.color);
                    if marker.name.trim().is_empty() {
                        ui.colored_label(marker.color, format!("M{}", index + 1));
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut marker.name)
                            .char_limit(LOGIC_MARKER_NAME_MAX_CHARS)
                            .desired_width(105.0)
                            .hint_text("Name"),
                    )
                    .on_hover_text("Optional marker name");
                });
                ui.horizontal(|ui| {
                    ui.add_space(26.0);
                    ui.monospace(format_duration(marker.duration()))
                        .on_hover_text(format!(
                            "Samples {}–{} at {}",
                            marker.start_sample,
                            marker.end_sample,
                            format_sample_rate(marker.sample_rate_hz)
                        ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("×")
                            .on_hover_text("Remove marker")
                            .clicked()
                        {
                            remove = Some(index);
                        }
                    });
                });
            }
            if let Some(index) = remove {
                settings.markers.remove(index);
            }
        });
}

fn paint_protocol_analyzer_settings(
    ui: &mut egui::Ui,
    settings: &mut ProtocolAnalyzerSettings,
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
) {
    let heading = format!("Protocol analyzer ({})", settings.analyzers.len());
    egui::CollapsingHeader::new(RichText::new(heading).strong())
        .id_salt("logic_protocol_analyzer_section")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let add_label = if settings.picker_open {
                    "Close list"
                } else {
                    "+ Add protocol"
                };
                if ui.button(add_label).clicked() {
                    settings.picker_open = !settings.picker_open;
                }
            });

            if let Some(error) = &settings.custom_load_error {
                ui.add_space(4.0);
                ui.label(RichText::new(error).color(Color32::LIGHT_RED));
            }

            if settings.picker_open {
                ui.add_space(5.0);
                ui.add(
                    egui::TextEdit::singleline(&mut settings.search)
                        .desired_width(ui.available_width())
                        .hint_text(format!("Search {} protocols…", PROTOCOLS.len())),
                );
                let protocols = matching_protocols(&settings.search);
                ui.small(RichText::new(format!("{} matches", protocols.len())).color(TEXT_MUTED));
                let mut selected_protocol = None;
                egui::Frame::none()
                    .fill(PANEL)
                    .rounding(egui::Rounding::same(4.0))
                    .inner_margin(egui::Margin::same(4.0))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("protocol_catalog_scroll")
                            .max_height(210.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                for protocol in protocols {
                                    let response = ui
                                        .selectable_label(false, protocol.name)
                                        .on_hover_text(format!(
                                            "{}\nDecoder ID: {}",
                                            protocol.long_name, protocol.id
                                        ));
                                    if response.clicked() {
                                        selected_protocol = Some(protocol.id);
                                    }
                                }
                            });
                    });
                if let Some(protocol_id) = selected_protocol {
                    settings.begin_add(protocol_id);
                }
            }

            if settings.analyzers.is_empty() {
                ui.add_space(4.0);
                ui.small(RichText::new("No protocols configured").color(TEXT_MUTED));
                return;
            }

            let mut remove = None;
            let mut edit = None;
            let mut load_yaml = None;
            let mut clear_yaml = None;
            for (analyzer_index, analyzer) in settings.analyzers.iter().enumerate() {
                let channels = analyzer_channels(analyzer);
                let assigned_channels = analyzer
                    .channel_mappings
                    .iter()
                    .filter(|mapping| mapping.is_some())
                    .count();
                let missing_required = channels.iter().enumerate().any(|(index, channel)| {
                    channel.required
                        && analyzer
                            .channel_mappings
                            .get(index)
                            .copied()
                            .flatten()
                            .is_none()
                });
                let disabled_mapping = analyzer.channel_mappings.iter().any(|mapping| {
                    mapping
                        .map(|channel| !enabled_channels[channel])
                        .unwrap_or(false)
                });
                ui.add_space(6.0);
                egui::Frame::none()
                    .fill(PANEL)
                    .rounding(egui::Rounding::same(4.0))
                    .inner_margin(egui::Margin::same(6.0))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.small(RichText::new(analyzer_long_name(analyzer)).color(TEXT_MUTED));
                            ui.label(RichText::new(analyzer_name(analyzer)).strong());
                            ui.small(
                                RichText::new(format!(
                                    "{} · {assigned_channels}/{} inputs",
                                    analyzer.data_format.label(),
                                    channels.len()
                                ))
                                .color(TEXT_MUTED),
                            );
                            if missing_required {
                                ui.small(
                                    RichText::new("Assign every required input")
                                        .color(Color32::LIGHT_RED),
                                );
                            } else if disabled_mapping {
                                ui.small(
                                    RichText::new(
                                        "One or more mapped capture channels are disabled",
                                    )
                                    .color(Color32::YELLOW),
                                );
                            }
                            ui.add_space(3.0);
                            ui.horizontal(|ui| {
                                if ui.small_button("Edit…").clicked() {
                                    edit = Some(analyzer_index);
                                }
                                if ui
                                    .small_button("×")
                                    .on_hover_text("Remove analyzer")
                                    .clicked()
                                {
                                    remove = Some(analyzer_index);
                                }
                                ui.separator();
                                if ui.small_button("Load YAML").clicked() {
                                    load_yaml = Some(analyzer_index);
                                }
                                if ui
                                    .add_enabled(
                                        analyzer.custom_protocol.is_some(),
                                        egui::Button::new("Clear").small(),
                                    )
                                    .on_hover_text("Clear the attached YAML protocol")
                                    .clicked()
                                {
                                    clear_yaml = Some(analyzer_index);
                                }
                            });
                        })
                        .response
                        .on_hover_text(format!(
                            "Decoder chain: {}",
                            analyzer_decoder_chain(analyzer).join(" → ")
                        ));
                    });
            }
            if let Some(index) = remove {
                settings.analyzers.remove(index);
            } else if let Some(index) = edit {
                settings.begin_edit(index);
            } else if let Some(index) = load_yaml {
                settings.custom_load_error = None;
                if let Some(path) = choose_custom_protocol_to_open() {
                    match std::fs::read_to_string(&path) {
                        Ok(source) => {
                            if let Err(error) =
                                settings.begin_attach_custom_yaml(index, path.clone(), source)
                            {
                                settings.custom_load_error =
                                    Some(format!("Failed to load {}: {error}", path.display()));
                            }
                        }
                        Err(error) => {
                            settings.custom_load_error =
                                Some(format!("Failed to read {}: {error}", path.display()));
                        }
                    }
                }
            } else if let Some(index) = clear_yaml {
                settings.clear_custom_protocol(index);
            }
        });
}

fn paint_protocol_analyzer_modal(
    context: &egui::Context,
    settings: &mut ProtocolAnalyzerSettings,
    logic_channel_names: &[String; LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
) {
    let Some(mut editor) = settings.editor.clone() else {
        return;
    };
    let channels = analyzer_channels(&editor.analyzer);
    editor
        .analyzer
        .channel_mappings
        .resize(channels.len(), None);

    let screen_rect = context.input(|input| input.screen_rect());
    egui::Area::new(egui::Id::new("protocol_settings_modal_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen_rect.min)
        .show(context, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, Color32::from_black_alpha(112));
        });

    let mut open = true;
    let mut cancel = false;
    let mut save = false;
    let title = if editor.target_index.is_some() {
        "Protocol settings"
    } else {
        "Add protocol"
    };
    egui::Window::new(title)
        .id(egui::Id::new("protocol_settings_modal"))
        .open(&mut open)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(440.0)
        .min_width(380.0)
        .show(context, |ui| {
            ui.label(
                RichText::new(analyzer_name(&editor.analyzer))
                    .strong()
                    .color(ACCENT),
            );
            ui.label(RichText::new(analyzer_long_name(&editor.analyzer)).color(TEXT_MUTED));
            if let Some(custom) = &editor.analyzer.custom_protocol {
                ui.small(
                    RichText::new(format!("Schema: {}", custom.source_path.display()))
                        .color(TEXT_MUTED),
                );
            }
            let decoder_chain = analyzer_decoder_chain(&editor.analyzer);
            if decoder_chain.len() > 1 {
                ui.small(
                    RichText::new(format!("Decoder chain: {}", decoder_chain.join(" → ")))
                        .color(TEXT_MUTED),
                );
            }
            ui.add_space(10.0);

            let maximum_height = (screen_rect.height() * 0.62).max(220.0);
            egui::ScrollArea::vertical()
                .id_salt("protocol_settings_modal_scroll")
                .max_height(maximum_height)
                .show(ui, |ui| {
                    egui::Grid::new("protocol_settings_modal_grid")
                        .num_columns(2)
                        .min_col_width(120.0)
                        .spacing([16.0, 7.0])
                        .show(ui, |ui| {
                            ui.label("Output format");
                            egui::ComboBox::from_id_salt("protocol_modal_format")
                                .width(210.0)
                                .selected_text(editor.analyzer.data_format.label())
                                .show_ui(ui, |ui| {
                                    for format in ProtocolDataFormat::ALL {
                                        ui.selectable_value(
                                            &mut editor.analyzer.data_format,
                                            format,
                                            format.label(),
                                        );
                                    }
                                });
                            ui.end_row();

                            for (channel_index, channel) in channels.iter().enumerate() {
                                let label = if channel.required {
                                    format!("{} *", channel.name)
                                } else {
                                    channel.name.to_owned()
                                };
                                ui.label(label).on_hover_text(if channel.required {
                                    "Required decoder input"
                                } else {
                                    "Optional decoder input"
                                });
                                let selected_text = editor.analyzer.channel_mappings[channel_index]
                                    .map(|logic_channel| {
                                        channel_label(
                                            "D",
                                            logic_channel,
                                            &logic_channel_names[logic_channel],
                                        )
                                    })
                                    .unwrap_or_else(|| "Not assigned".to_owned());
                                egui::ComboBox::from_id_salt((
                                    "protocol_modal_channel",
                                    channel_index,
                                ))
                                .width(210.0)
                                .selected_text(selected_text)
                                .show_ui(ui, |ui| {
                                    if !channel.required {
                                        ui.selectable_value(
                                            &mut editor.analyzer.channel_mappings[channel_index],
                                            None,
                                            "Not assigned",
                                        );
                                    }
                                    for (logic_channel, channel_name) in
                                        logic_channel_names.iter().enumerate()
                                    {
                                        ui.selectable_value(
                                            &mut editor.analyzer.channel_mappings[channel_index],
                                            Some(logic_channel),
                                            channel_label("D", logic_channel, channel_name),
                                        );
                                    }
                                });
                                ui.end_row();
                            }
                        });
                });

            let missing_required = channels.iter().enumerate().any(|(index, channel)| {
                channel.required && editor.analyzer.channel_mappings[index].is_none()
            });
            let disabled_mapping = editor.analyzer.channel_mappings.iter().any(|mapping| {
                mapping
                    .map(|channel| !enabled_channels[channel])
                    .unwrap_or(false)
            });
            ui.add_space(8.0);
            if missing_required {
                ui.label(
                    RichText::new("Assign every required input before saving")
                        .color(Color32::LIGHT_RED),
                );
            } else if disabled_mapping {
                ui.label(
                    RichText::new("One or more mapped capture channels are disabled")
                        .color(Color32::YELLOW),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui
                    .add_enabled(!missing_required, egui::Button::new("Save"))
                    .clicked()
                {
                    save = true;
                }
            });
        });

    if save {
        settings.save_editor(editor);
    } else if cancel || !open {
        settings.cancel_editor();
    } else {
        settings.editor = Some(editor);
    }
}

fn paint_logic_marker_color_picker(ui: &mut egui::Ui, index: usize, color: &mut Color32) {
    let popup_id = ui.make_persistent_id(("logic_marker_color_popup", index));
    let button = ui
        .add(
            egui::Button::new(RichText::new("◆").color(marker_text_color(*color)))
                .fill(*color)
                .min_size(Vec2::splat(18.0)),
        )
        .on_hover_text("Change marker color");
    if button.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(popup_id));
    }
    egui::popup::popup_below_widget(
        ui,
        popup_id,
        &button,
        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
        |popup_ui| {
            popup_ui.set_min_width(150.0);
            popup_ui.set_max_width(150.0);
            popup_ui.label(RichText::new("Marker color").strong());
            egui::Grid::new(("logic_marker_color_palette", index))
                .num_columns(5)
                .spacing([4.0, 4.0])
                .show(popup_ui, |ui| {
                    for palette_index in 0..LOGIC_MARKER_LIMIT {
                        let palette_color = logic_marker_color(palette_index);
                        let selected = *color == palette_color;
                        let response = ui
                            .add(
                                egui::Button::new("")
                                    .fill(palette_color)
                                    .stroke(Stroke::new(
                                        if selected { 2.0_f32 } else { 1.0_f32 },
                                        if selected { Color32::WHITE } else { GRID },
                                    ))
                                    .min_size(Vec2::splat(20.0)),
                            )
                            .on_hover_text(format!("Color {}", palette_index + 1));
                        if response.clicked() {
                            *color = palette_color;
                            ui.memory_mut(egui::Memory::close_popup);
                        }
                        if palette_index % 5 == 4 {
                            ui.end_row();
                        }
                    }
                });
            popup_ui.separator();
            let [mut red, mut green, mut blue, _] = color.to_array();
            let mut changed = false;
            egui::Grid::new(("logic_marker_color_rgb", index))
                .num_columns(2)
                .spacing([6.0, 3.0])
                .show(popup_ui, |ui| {
                    for (label, component) in [("R", &mut red), ("G", &mut green), ("B", &mut blue)]
                    {
                        ui.label(label);
                        changed |= ui
                            .add(egui::DragValue::new(component).range(0..=255).speed(1))
                            .changed();
                        ui.end_row();
                    }
                });
            if changed {
                *color = Color32::from_rgb(red, green, blue);
            }
        },
    );
}

struct ScopeToolsOptions {
    viewport: WaveformViewport,
    trigger_controls_enabled: bool,
}

fn paint_scope_tools(
    ui: &mut egui::Ui,
    scope_settings: &mut ScopeCaptureSettings,
    function_settings: &mut ScopeFunctionSettings,
    marker_settings: &mut ScopeMarkerSettings,
    marker_editor: &mut Option<ScopeMarkerEditor>,
    channel_names: &[String; 2],
    options: ScopeToolsOptions,
) {
    let ScopeToolsOptions {
        viewport,
        trigger_controls_enabled,
    } = options;
    let channel_enabled = scope_settings.channels.map(|channel| channel.enabled);
    scope_settings.trigger.clamp_to_channels(channel_enabled);
    function_settings.clamp_to_channels(channel_enabled);

    paint_scope_trigger_settings(
        ui,
        &mut scope_settings.trigger,
        channel_enabled,
        channel_names,
        trigger_controls_enabled,
    );
    ui.add_space(8.0);
    paint_scope_function_settings(ui, function_settings, channel_enabled, channel_names);
    ui.add_space(8.0);
    paint_scope_measurement_settings(
        ui,
        &mut function_settings.measurements,
        channel_enabled,
        channel_names,
    );
    ui.add_space(8.0);
    paint_scope_marker_settings(
        ui,
        marker_settings,
        marker_editor,
        viewport,
        channel_enabled,
        channel_names,
    );
}

fn paint_calibration_progress_modal(
    context: &egui::Context,
    state: &CalibrationProgressModal,
) -> Option<CalibrationModalAction> {
    let screen_rect = context.input(|input| input.screen_rect());
    egui::Area::new(egui::Id::new("calibration_progress_modal_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen_rect.min)
        .show(context, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, Color32::from_black_alpha(112));
        });

    let mut action = None;
    egui::Window::new("DSCope Auto Calibration")
        .id(egui::Id::new("calibration_progress_modal"))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(430.0)
        .show(context, |ui| {
            if state.completed {
                ui.label(RichText::new(&state.phase).strong().color(ACCENT));
                ui.add_space(8.0);
                ui.add(egui::ProgressBar::new(1.0).show_percentage().fill(ACCENT));
                ui.add_space(12.0);
                if let Some(values) = state.values {
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            action = Some(CalibrationModalAction::Close);
                        }
                        if ui.button("Save").clicked() {
                            action = Some(CalibrationModalAction::Save(values));
                        }
                    });
                } else if ui.button("Close").clicked() {
                    action = Some(CalibrationModalAction::Close);
                }
            } else {
                ui.label(RichText::new(&state.phase).strong());
                ui.add_space(8.0);
                ui.add(
                    egui::ProgressBar::new(state.progress)
                        .show_percentage()
                        .animate(true)
                        .fill(ACCENT),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Keep all probes disconnected. Do not disconnect the oscilloscope.",
                    )
                    .color(Color32::LIGHT_RED),
                );
            }
        });
    action
}

fn paint_instrument_settings(ui: &mut egui::Ui, devices: &[DeviceEntry]) {
    if devices.is_empty() {
        ui.label(RichText::new("No supported USB instruments").color(TEXT_MUTED));
        return;
    }

    for entry in devices {
        ui.add_space(4.0);
        egui::Frame::none()
            .fill(BACKGROUND)
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(10.0))
            .show(ui, |ui| {
                ui.label(RichText::new(entry.device.profile.display_name).strong());
                ui.small(RichText::new(entry.device.location.stable_id()).color(TEXT_MUTED));
                let (link_text, link_color) = match entry.device.link_status() {
                    LinkStatus::Optimal => (
                        format!("{} · optimal", entry.device.speed.description()),
                        ACCENT,
                    ),
                    LinkStatus::ReducedPerformance => (
                        format!("{} · reduced performance", entry.device.speed.description()),
                        SCOPE_COLORS[0],
                    ),
                    LinkStatus::Unsupported => {
                        ("Unsupported link speed".to_owned(), Color32::LIGHT_RED)
                    }
                };
                ui.small(RichText::new(link_text).color(link_color));
                ui.small(
                    RichText::new(if entry.device.profile.model.supports_capture() {
                        "Acquisition driver available"
                    } else {
                        "Discovery, probe, and FPGA preparation available"
                    })
                    .color(TEXT_MUTED),
                );
            });
    }
}

fn selected_radio_text(radios: &[DiscoveredRadio], selected_id: Option<&str>) -> String {
    match selected_id {
        Some(id) => selected_radio(radios, Some(id))
            .map(|radio| radio.descriptor.name.clone())
            .unwrap_or_else(|| "Unavailable radio receiver".to_owned()),
        None => "No radio receiver available".to_owned(),
    }
}

fn displayed_radio_id(radio: &DiscoveredRadio) -> String {
    let id = radio.descriptor.id.as_str();
    if radio.provider != RadioProvider::HackRf {
        return id.to_owned();
    }
    let Some(serial) = id.strip_prefix("hackrf:") else {
        return id.to_owned();
    };
    let serial = serial.trim_start_matches('0');
    format!("hackrf:{}", if serial.is_empty() { "0" } else { serial })
}

fn nearest_domain_value(domain: &ValueDomain, requested: u64) -> u64 {
    match domain {
        ValueDomain::Discrete(values) => values
            .iter()
            .copied()
            .min_by_key(|value| value.abs_diff(requested))
            .unwrap_or(requested),
        ValueDomain::Stepped { range, step } => {
            if *step == 0 {
                return requested.clamp(*range.start(), *range.end());
            }
            let maximum = range.start().saturating_add(
                range
                    .end()
                    .saturating_sub(*range.start())
                    .saturating_div(*step)
                    .saturating_mul(*step),
            );
            let clamped = requested.clamp(*range.start(), maximum);
            let offset = clamped.saturating_sub(*range.start());
            let lower = range
                .start()
                .saturating_add((offset / step).saturating_mul(*step));
            let upper = lower.saturating_add(*step).min(maximum);
            if clamped.abs_diff(lower) <= clamped.abs_diff(upper) {
                lower
            } else {
                upper
            }
        }
    }
}

fn normalize_radio_settings(settings: &mut RadioSettings, capabilities: &RadioCapabilities) {
    settings.capture.center_frequency_hz = nearest_domain_value(
        &capabilities.frequency_hz,
        settings.capture.center_frequency_hz,
    );
    settings.capture.sample_rate_hz = nearest_domain_value(
        &capabilities.sample_rates_hz,
        settings.capture.sample_rate_hz,
    );
    settings.capture.bandwidth_hz =
        nearest_domain_value(&capabilities.bandwidths_hz, settings.capture.bandwidth_hz);
    settings.capture.receive_channel = settings
        .capture
        .receive_channel
        .min(capabilities.receive_channels.saturating_sub(1));

    settings.gains_db.retain(|id, value| {
        let Some(control) = capabilities
            .gain_controls
            .iter()
            .find(|control| control.id == *id)
        else {
            return false;
        };
        *value = normalize_gain_value(
            *value,
            *control.range_db.start(),
            *control.range_db.end(),
            control.step_db,
        );
        true
    });
    for control in &capabilities.gain_controls {
        settings
            .gains_db
            .entry(control.id.clone())
            .or_insert(control.default_db);
    }
}

fn radio_gain_u32(settings: &RadioSettings, id: &str) -> Result<Option<u32>, String> {
    settings
        .gains_db
        .get(id)
        .copied()
        .map(|value| {
            u32::try_from(value).map_err(|_| format!("radio gain {id} cannot be negative"))
        })
        .transpose()
}

fn hackrf_runtime_settings(settings: &RadioSettings) -> Result<HackRfRuntimeSettings, String> {
    Ok(HackRfRuntimeSettings {
        center_frequency_hz: settings.capture.center_frequency_hz,
        bandwidth_hz: settings.capture.bandwidth_hz,
        lna_gain_db: radio_gain_u32(settings, "lna")?,
        vga_gain_db: radio_gain_u32(settings, "vga")?,
        amplifier_enabled: settings.amplifier_enabled,
    })
}

fn radio_runtime_settings_differ(current: &RadioSettings, previous: &RadioSettings) -> bool {
    current.capture.center_frequency_hz != previous.capture.center_frequency_hz
        || current.capture.bandwidth_hz != previous.capture.bandwidth_hz
        || current.gains_db != previous.gains_db
        || current.amplifier_enabled != previous.amplifier_enabled
}

fn normalize_gain_value(value: i32, minimum: i32, maximum: i32, step: u32) -> i32 {
    let clamped = value.clamp(minimum, maximum);
    let step = i64::from(step);
    if step == 0 {
        return clamped;
    }
    let offset = i64::from(clamped) - i64::from(minimum);
    let rounded_steps = (offset + step / 2) / step;
    (i64::from(minimum) + rounded_steps * step).min(i64::from(maximum)) as i32
}

fn format_radio_value(value_hz: u64) -> String {
    if value_hz >= 1_000_000_000 && value_hz % 1_000_000_000 == 0 {
        format!("{} GHz", value_hz / 1_000_000_000)
    } else if value_hz >= 1_000_000 && value_hz % 1_000_000 == 0 {
        format!("{} MHz", value_hz / 1_000_000)
    } else if value_hz >= 1_000 && value_hz % 1_000 == 0 {
        format!("{} kHz", value_hz / 1_000)
    } else {
        format!("{value_hz} Hz")
    }
}

fn radio_hz_from_mhz(domain: &ValueDomain, value_mhz: f64) -> u64 {
    if !value_mhz.is_finite() {
        return nearest_domain_value(domain, 0);
    }
    let requested_hz = (value_mhz * 1_000_000.0)
        .round()
        .clamp(0.0, u64::MAX as f64) as u64;
    nearest_domain_value(domain, requested_hz)
}

fn format_radio_mhz_number(value_hz: u64) -> String {
    let whole = value_hz / 1_000_000;
    let remainder = value_hz % 1_000_000;
    if remainder == 0 {
        return whole.to_string();
    }
    let fractional = format!("{remainder:06}");
    format!("{whole}.{}", fractional.trim_end_matches('0'))
}

const RADIO_VALUE_WIDTH: f32 = 80.0;
const RADIO_GAIN_HANDLE_ASPECT_RATIO: f32 = 1.8;

fn add_left_aligned_sized(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    widget: impl egui::Widget,
) -> egui::Response {
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::left_to_right(egui::Align::Center)
            .with_main_align(egui::Align::Min)
            .with_main_justify(true),
        |ui| ui.add(widget),
    )
    .inner
}

fn paint_radio_parameter_label(ui: &mut egui::Ui, label: &str, help: Option<&str>) {
    let response = ui.label(label);
    if let Some(help) = help {
        response.on_hover_text(help);
    }
}

fn paint_radio_gain_slider(
    ui: &mut egui::Ui,
    value: &mut i32,
    range: std::ops::RangeInclusive<i32>,
    step: u32,
) -> egui::Response {
    let minimum = *range.start();
    let maximum = *range.end();
    ui.spacing_mut().slider_width = RADIO_VALUE_WIDTH;
    let response = ui.add(
        egui::Slider::new(value, range)
            .step_by(f64::from(step.max(1)))
            .show_value(false)
            .handle_shape(egui::style::HandleShape::Rect {
                aspect_ratio: RADIO_GAIN_HANDLE_ASPECT_RATIO,
            }),
    );
    let fraction = if maximum > minimum {
        (*value - minimum) as f32 / (maximum - minimum) as f32
    } else {
        0.0
    }
    .clamp(0.0, 1.0);
    let handle_radius = response.rect.height() / 2.5;
    let handle_half_width = handle_radius * RADIO_GAIN_HANDLE_ASPECT_RATIO;
    let handle_x = egui::lerp(
        response.rect.left() + handle_half_width..=response.rect.right() - handle_half_width,
        fraction,
    );
    ui.painter().text(
        Pos2::new(handle_x, response.rect.center().y),
        egui::Align2::CENTER_CENTER,
        value.to_string(),
        FontId::proportional(9.0),
        ui.visuals().strong_text_color(),
    );
    response
}

fn paint_radio_mhz_value(ui: &mut egui::Ui, value_hz: &mut u64, domain: &ValueDomain) {
    let bounds = match domain {
        ValueDomain::Discrete(values) => values
            .iter()
            .copied()
            .min()
            .zip(values.iter().copied().max()),
        ValueDomain::Stepped { range, .. } => Some((*range.start(), *range.end())),
    };
    let mut value_mhz = *value_hz as f64 / 1_000_000.0;
    let mut editor = egui::DragValue::new(&mut value_mhz)
        .speed(0.001)
        .min_decimals(3)
        .max_decimals(6);
    if let Some((minimum_hz, maximum_hz)) = bounds {
        editor = editor.range(minimum_hz as f64 / 1_000_000.0..=maximum_hz as f64 / 1_000_000.0);
    }
    let editor_size = egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y);
    let changed = ui
        .horizontal(|ui| {
            let changed = add_left_aligned_sized(ui, editor_size, editor).changed();
            ui.label("MHz");
            changed
        })
        .inner;
    if changed {
        *value_hz = radio_hz_from_mhz(domain, value_mhz);
    }
}

fn paint_radio_bandwidth_value(ui: &mut egui::Ui, value: &mut u64, domain: &ValueDomain) {
    let editor_size = egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y);
    ui.horizontal(|ui| {
        match domain {
            ValueDomain::Discrete(values) => {
                egui::ComboBox::from_id_salt("radio_bandwidth")
                    .width(RADIO_VALUE_WIDTH)
                    .selected_text(format_radio_mhz_number(*value))
                    .show_ui(ui, |ui| {
                        for choice in values {
                            ui.selectable_value(value, *choice, format_radio_mhz_number(*choice));
                        }
                    });
            }
            ValueDomain::Stepped { range, step } => {
                let mut value_mhz = *value as f64 / 1_000_000.0;
                if add_left_aligned_sized(
                    ui,
                    editor_size,
                    egui::DragValue::new(&mut value_mhz)
                        .range(
                            *range.start() as f64 / 1_000_000.0..=*range.end() as f64 / 1_000_000.0,
                        )
                        .speed(*step as f64 / 1_000_000.0)
                        .min_decimals(3)
                        .max_decimals(6),
                )
                .changed()
                {
                    *value = radio_hz_from_mhz(domain, value_mhz);
                }
            }
        }
        ui.label("MHz");
    });
    *value = nearest_domain_value(domain, *value);
}

fn paint_radio_capture_settings(
    ui: &mut egui::Ui,
    radio: &DiscoveredRadio,
    settings: &mut RadioSettings,
    enabled: bool,
    runtime_enabled: bool,
) {
    ui.add_space(10.0);
    let card_width = ui.available_width();
    egui::Frame::none()
        .fill(BACKGROUND)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            let content_width = (card_width - 20.0).max(0.0);
            ui.set_width(content_width);
            ui.set_max_width(content_width);
            egui::CollapsingHeader::new(RichText::new("Receive configuration").strong())
                .id_salt("radio_receive_configuration_section")
                .default_open(true)
                .show(ui, |ui| {
                    paint_radio_receive_configuration(
                        ui,
                        radio,
                        settings,
                        enabled,
                        runtime_enabled,
                    );
                });
            ui.separator();
            egui::CollapsingHeader::new(RichText::new("Signal analyzer").strong())
                .id_salt("radio_signal_analyzer_section")
                .default_open(false)
                .show(ui, |ui| {
                    paint_radio_analyzer_configuration(
                        ui,
                        &mut settings.analyzer,
                        settings.capture.sample_rate_hz,
                        enabled,
                    );
                });
        });
}

fn paint_scaled_u64_value(
    ui: &mut egui::Ui,
    value: &mut u64,
    scale: f64,
    range: RangeInclusive<f64>,
    unit: &str,
) {
    let mut scaled = *value as f64 / scale;
    let changed = ui
        .horizontal(|ui| {
            let changed = add_left_aligned_sized(
                ui,
                egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                egui::DragValue::new(&mut scaled)
                    .range(range)
                    .speed(0.001)
                    .min_decimals(3)
                    .max_decimals(6),
            )
            .changed();
            ui.label(unit);
            changed
        })
        .inner;
    if changed {
        *value = (scaled * scale).round().max(0.0) as u64;
    }
}

fn paint_scaled_i64_value(
    ui: &mut egui::Ui,
    value: &mut i64,
    scale: f64,
    range: RangeInclusive<f64>,
    unit: &str,
) {
    let mut scaled = *value as f64 / scale;
    let changed = ui
        .horizontal(|ui| {
            let changed = add_left_aligned_sized(
                ui,
                egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                egui::DragValue::new(&mut scaled)
                    .range(range)
                    .speed(0.001)
                    .min_decimals(3)
                    .max_decimals(6),
            )
            .changed();
            ui.label(unit);
            changed
        })
        .inner;
    if changed {
        *value = (scaled * scale).round() as i64;
    }
}

fn paint_radio_analyzer_configuration(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    capture_sample_rate_hz: u64,
    enabled: bool,
) {
    ui.set_max_width(ui.available_width());
    egui::Grid::new("radio_signal_analyzer_configuration")
        .num_columns(2)
        .max_col_width(126.0)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            paint_radio_parameter_label(
                ui,
                "Enable analyzer",
                Some("Channelize captured IQ and run the selected demodulator"),
            );
            ui.add_enabled(
                enabled,
                egui::Checkbox::without_text(&mut settings.enabled),
            );
            ui.end_row();

            let controls_enabled = enabled && settings.enabled;
            paint_radio_parameter_label(
                ui,
                "Modulation",
                Some("Selects a modulation-specific profile; unrelated parameters remain hidden"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                egui::ComboBox::from_id_salt("radio_analyzer_modulation")
                    .width(RADIO_VALUE_WIDTH)
                    .selected_text(match settings.demodulation {
                        RadioDemodulation::BinaryFsk => "2-FSK",
                        RadioDemodulation::BinaryGfsk => "GFSK",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut settings.demodulation,
                            RadioDemodulation::BinaryFsk,
                            "2-FSK",
                        );
                        ui.selectable_value(
                            &mut settings.demodulation,
                            RadioDemodulation::BinaryGfsk,
                            "GFSK",
                        );
                    });
            });
            ui.end_row();

            let half_span_mhz = capture_sample_rate_hz as f64 / 2_000_000.0;
            paint_radio_parameter_label(
                ui,
                "Channel offset",
                Some("Offset from the capture center frequency to the signal center"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                paint_scaled_i64_value(
                    ui,
                    &mut settings.channel_offset_hz,
                    1_000_000.0,
                    -half_span_mhz..=half_span_mhz,
                    "MHz",
                );
            });
            ui.end_row();

            let capture_rate_mhz = capture_sample_rate_hz as f64 / 1_000_000.0;
            paint_radio_parameter_label(
                ui,
                "Channel bandwidth",
                Some("Full complex bandwidth retained around the selected channel"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                paint_scaled_u64_value(
                    ui,
                    &mut settings.channel_bandwidth_hz,
                    1_000_000.0,
                    0.000_001..=capture_rate_mhz.max(0.000_001),
                    "MHz",
                );
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Processing rate",
                Some("IQ sample rate after channel filtering and bounded downsampling"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                paint_scaled_u64_value(
                    ui,
                    &mut settings.output_sample_rate_hz,
                    1_000_000.0,
                    0.000_001..=capture_rate_mhz.max(0.000_001),
                    "MHz",
                );
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Channel filter taps",
                Some("Odd FIR length from 3 to 4095; more taps improve rejection but cost CPU"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                add_left_aligned_sized(
                    ui,
                    egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                    egui::DragValue::new(&mut settings.filter_tap_count)
                        .range(3..=4_095)
                        .speed(2),
                );
            });
            settings.filter_tap_count = settings.filter_tap_count.clamp(3, 4_095) | 1;
            ui.end_row();

            let processing_rate_khz = settings.output_sample_rate_hz as f64 / 1_000.0;
            paint_radio_parameter_label(ui, "Symbol rate", Some("Configured symbol clock"));
            ui.add_enabled_ui(controls_enabled, |ui| {
                paint_scaled_u64_value(
                    ui,
                    &mut settings.symbol_rate_hz,
                    1_000.0,
                    0.001..=(processing_rate_khz / 2.0).max(0.001),
                    "ksym/s",
                );
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Frequency deviation",
                Some("Expected mark or space displacement from the channel center"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                paint_scaled_u64_value(
                    ui,
                    &mut settings.deviation_hz,
                    1_000.0,
                    0.001..=(processing_rate_khz / 2.0).max(0.001),
                    "kHz",
                );
            });
            ui.end_row();

            if settings.demodulation == RadioDemodulation::BinaryGfsk {
                paint_radio_parameter_label(
                    ui,
                    "Gaussian BT",
                    Some("Expected transmitter Gaussian bandwidth-time product"),
                );
                ui.add_enabled_ui(controls_enabled, |ui| {
                    let mut bt = f64::from(settings.gfsk_bt_milli) / 1_000.0;
                    if add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut bt)
                            .range(0.01..=2.0)
                            .speed(0.01)
                            .min_decimals(2)
                            .max_decimals(3),
                    )
                    .changed()
                    {
                        settings.gfsk_bt_milli = (bt * 1_000.0).round() as u32;
                    }
                });
                ui.end_row();
            }

            paint_radio_parameter_label(ui, "Polarity", Some("Swaps mark and space decisions"));
            ui.add_enabled_ui(controls_enabled, |ui| {
                egui::ComboBox::from_id_salt("radio_analyzer_polarity")
                    .width(RADIO_VALUE_WIDTH)
                    .selected_text(if settings.inverted {
                        "Inverted"
                    } else {
                        "Normal"
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.inverted, false, "Normal");
                        ui.selectable_value(&mut settings.inverted, true, "Inverted");
                    });
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Adaptive timing",
                Some("Tracks modest transmitter symbol-clock error; acquisition starts after transitions"),
            );
            ui.add_enabled(
                controls_enabled,
                egui::Checkbox::without_text(&mut settings.adaptive_timing),
            );
            ui.end_row();

            if settings.adaptive_timing {
                paint_radio_parameter_label(
                    ui,
                    "Timing loop bandwidth",
                    Some("Higher values track faster but admit more timing jitter"),
                );
                ui.add_enabled_ui(controls_enabled, |ui| {
                    let mut bandwidth =
                        f64::from(settings.timing_loop_bandwidth_milli) / 1_000.0;
                    if add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut bandwidth)
                            .range(0.001..=1.0)
                            .speed(0.001)
                            .min_decimals(3)
                            .max_decimals(3),
                    )
                    .changed()
                    {
                        settings.timing_loop_bandwidth_milli =
                            (bandwidth * 1_000.0).round() as u32;
                    }
                });
                ui.end_row();

                paint_radio_parameter_label(
                    ui,
                    "Clock error limit",
                    Some("Maximum symbol-clock correction allowed by the timing loop"),
                );
                ui.add_enabled_ui(controls_enabled, |ui| {
                    ui.horizontal(|ui| {
                        add_left_aligned_sized(
                            ui,
                            egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                            egui::DragValue::new(&mut settings.max_clock_error_ppm)
                                .range(0..=250_000)
                                .speed(100),
                        );
                        ui.label("ppm");
                    });
                });
                ui.end_row();
            }
        });

    if settings.enabled {
        if let Err(error) = settings.signal_profile() {
            ui.label(RichText::new(error).small().color(Color32::LIGHT_RED));
        }
    }
}

fn paint_radio_receive_configuration(
    ui: &mut egui::Ui,
    radio: &DiscoveredRadio,
    settings: &mut RadioSettings,
    enabled: bool,
    runtime_enabled: bool,
) {
    ui.set_max_width(ui.available_width());
    let tuning_enabled = enabled || runtime_enabled;
    let center_frequency_help = (radio.provider == RadioProvider::HackRf).then_some(
        "Tunes the frequency at the center of the RF spectrum and waterfall. HackRF One range: 1 MHz–6 GHz. This can be changed while capture is running.",
    );
    let sample_rate_help = (radio.provider == RadioProvider::HackRf).then_some(
        "Sets the I/Q sampling rate and the captured RF frequency span. Supported range: 2–20 MHz. Step: 1 Hz (0.000001 MHz). HackRF supports any whole-Hz value in this range; 8 MHz or higher is recommended.",
    );
    let bandwidth_help = (radio.provider == RadioProvider::HackRf).then_some(
        "Selects the HackRF analog baseband low-pass filter width. A narrower filter rejects more out-of-band noise and interference. Keep it at or below the sample rate to limit aliasing.",
    );
    egui::Grid::new("radio_receive_configuration")
        .num_columns(2)
        .max_col_width(126.0)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            paint_radio_parameter_label(ui, "Center frequency", center_frequency_help);
            ui.add_enabled_ui(tuning_enabled, |ui| {
                paint_radio_mhz_value(
                    ui,
                    &mut settings.capture.center_frequency_hz,
                    &radio.capabilities.frequency_hz,
                );
            });
            ui.end_row();

            paint_radio_parameter_label(ui, "Sample rate", sample_rate_help);
            ui.add_enabled_ui(enabled, |ui| {
                paint_radio_mhz_value(
                    ui,
                    &mut settings.capture.sample_rate_hz,
                    &radio.capabilities.sample_rates_hz,
                );
            });
            ui.end_row();

            paint_radio_parameter_label(ui, "Baseband bandwidth", bandwidth_help);
            ui.add_enabled_ui(tuning_enabled, |ui| {
                paint_radio_bandwidth_value(
                    ui,
                    &mut settings.capture.bandwidth_hz,
                    &radio.capabilities.bandwidths_hz,
                );
            });
            ui.end_row();

            if radio.capabilities.receive_channels > 1 {
                ui.label("Receive channel");
                ui.add_enabled_ui(enabled, |ui| {
                    egui::ComboBox::from_id_salt("radio_receive_channel")
                        .width(RADIO_VALUE_WIDTH)
                        .selected_text(format!("RX {}", settings.capture.receive_channel))
                        .show_ui(ui, |ui| {
                            for channel in 0..radio.capabilities.receive_channels {
                                ui.selectable_value(
                                    &mut settings.capture.receive_channel,
                                    channel,
                                    format!("RX {channel}"),
                                );
                            }
                        });
                });
                ui.end_row();
            }

            for control in &radio.capabilities.gain_controls {
                paint_radio_parameter_label(ui, &control.label, control.help.as_deref());
                let value = settings
                    .gains_db
                    .entry(control.id.clone())
                    .or_insert(control.default_db);
                let slider_response = ui
                    .add_enabled_ui(tuning_enabled, |ui| {
                        ui.horizontal(|ui| {
                            let response = ui
                                .scope(|ui| {
                                    paint_radio_gain_slider(
                                        ui,
                                        value,
                                        control.range_db.clone(),
                                        control.step_db,
                                    )
                                })
                                .inner;
                            ui.label("dB");
                            response
                        })
                        .inner
                    })
                    .inner;
                if let Some(help) = &control.help {
                    slider_response.on_hover_text(help);
                }
                *value = normalize_gain_value(
                    *value,
                    *control.range_db.start(),
                    *control.range_db.end(),
                    control.step_db,
                );
                ui.end_row();
            }

            if radio.provider == RadioProvider::HackRf {
                let iq_help = "Remove the measured I/Q DC offset before spectrum processing";
                paint_radio_parameter_label(ui, "IQ correction", Some(iq_help));
                ui.add_enabled(
                    enabled,
                    egui::Checkbox::without_text(&mut settings.iq_correction_enabled),
                )
                .on_hover_text(iq_help);
                ui.end_row();

                let amplifier_help = "Adds roughly 11 dB of HackRF front-end gain. Leave disabled around strong signals to reduce overload and distortion.";
                paint_radio_parameter_label(ui, "RF amplifier", Some(amplifier_help));
                ui.add_enabled(
                    tuning_enabled,
                    egui::Checkbox::without_text(&mut settings.amplifier_enabled),
                )
                .on_hover_text(amplifier_help);
                ui.end_row();

                let antenna_help =
                    "Only enable for an antenna or accessory designed for bias power";
                paint_radio_parameter_label(ui, "Antenna port power (3.3 V)", Some(antenna_help));
                ui.add_enabled(
                    enabled,
                    egui::Checkbox::without_text(&mut settings.antenna_power_enabled),
                )
                .on_hover_text(antenna_help);
                ui.end_row();
            }
        });
    if settings.capture.bandwidth_hz > settings.capture.sample_rate_hz {
        ui.label(
            RichText::new("Bandwidth above sample rate may cause aliasing")
                .small()
                .color(Color32::from_rgb(255, 190, 80)),
        );
    }
}

fn paint_radio_settings(
    ui: &mut egui::Ui,
    radios: &[DiscoveredRadio],
    selected_id: &mut Option<String>,
    failures: &[RadioProviderFailure],
    enabled: bool,
) -> bool {
    let changed = paint_radio_device_selector(ui, radios, selected_id, enabled);
    for radio in radios {
        ui.add_space(4.0);
        egui::Frame::none()
            .fill(BACKGROUND)
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(10.0))
            .show(ui, |ui| {
                ui.label(RichText::new(&radio.descriptor.name).strong());
                ui.small(RichText::new(displayed_radio_id(radio)).color(TEXT_MUTED));
                ui.small(
                    RichText::new(format!(
                        "{:?} · {:?} · {} RX channel(s)",
                        radio.provider,
                        radio.connection_backend,
                        radio.capabilities.receive_channels
                    ))
                    .color(TEXT_MUTED),
                );
            });
    }

    for failure in failures {
        ui.add_space(4.0);
        ui.small(
            RichText::new(format!(
                "{:?} discovery unavailable: {}",
                failure.provider, failure.error
            ))
            .color(Color32::LIGHT_RED),
        );
    }
    changed
}

fn paint_radio_device_selector(
    ui: &mut egui::Ui,
    radios: &[DiscoveredRadio],
    selected_id: &mut Option<String>,
    enabled: bool,
) -> bool {
    let before = selected_id.clone();
    let choices = radios
        .iter()
        .map(|radio| {
            let id = radio.descriptor.id.as_str().to_owned();
            let displayed_id = displayed_radio_id(radio);
            (id, format!("{}\n{displayed_id}", radio.descriptor.name))
        })
        .collect::<Vec<_>>();
    ui.add_enabled_ui(enabled && !choices.is_empty(), |ui| {
        egui::ComboBox::from_id_salt("radio_device_selector")
            .width(ui.available_width())
            .selected_text(selected_radio_text(radios, selected_id.as_deref()))
            .show_ui(ui, |ui| {
                for (id, text) in choices {
                    ui.selectable_value(selected_id, Some(id), text);
                }
            });
    });
    if let Some(id) = selected_id.as_deref() {
        let displayed_id = selected_radio(radios, Some(id))
            .map(displayed_radio_id)
            .unwrap_or_else(|| id.to_owned());
        ui.add(
            egui::Label::new(
                RichText::new(displayed_id)
                    .small()
                    .monospace()
                    .color(TEXT_MUTED),
            )
            .wrap(),
        );
    }
    before != *selected_id
}

fn selected_device_text(
    devices: &[DeviceEntry],
    selected_id: Option<&str>,
    role: DeviceRole,
) -> String {
    match selected_id {
        Some(id) => selected_device_for_role(devices, Some(id), role)
            .map(|device| format!("{} · {id}", device.profile.display_name))
            .unwrap_or_else(|| format!("Unavailable · {id}")),
        None => role.empty_label().to_owned(),
    }
}

fn paint_device_selector(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    label: &str,
    devices: &[DeviceEntry],
    selected_id: &mut Option<String>,
    role: DeviceRole,
    enabled: bool,
) -> bool {
    let choices = devices
        .iter()
        .map(|entry| &entry.device)
        .filter(|device| role.matches(device))
        .map(|device| {
            let id = device.location.stable_id();
            (
                id.clone(),
                format!("{} · {id}", device.profile.display_name),
            )
        })
        .collect::<Vec<_>>();
    let before = selected_id.clone();
    ui.label(RichText::new(label).small().color(TEXT_MUTED));
    ui.add_enabled_ui(enabled && !choices.is_empty(), |ui| {
        egui::ComboBox::from_id_salt(id_salt)
            .width(ui.available_width())
            .selected_text(selected_device_text(devices, selected_id.as_deref(), role))
            .show_ui(ui, |ui| {
                for (id, text) in choices {
                    ui.selectable_value(selected_id, Some(id), text);
                }
            });
    });
    before != *selected_id
}

fn paint_capture_device_selectors(
    ui: &mut egui::Ui,
    active_tab: Option<WaveformTab>,
    devices: &[DeviceEntry],
    selected_logic_device_id: &mut Option<String>,
    selected_scope_device_id: &mut Option<String>,
    enabled: bool,
) -> (bool, bool) {
    let mut logic_changed = false;
    let mut scope_changed = false;
    match active_tab {
        Some(WaveformTab::LogicAnalyzer) => {
            logic_changed = paint_device_selector(
                ui,
                "logic_device_selector",
                "Device",
                devices,
                selected_logic_device_id,
                DeviceRole::LogicAnalyzer,
                enabled,
            );
        }
        Some(WaveformTab::Oscilloscope | WaveformTab::Lissajous) => {
            scope_changed = paint_device_selector(
                ui,
                "scope_device_selector",
                "Device",
                devices,
                selected_scope_device_id,
                DeviceRole::Oscilloscope,
                enabled,
            );
        }
        Some(WaveformTab::Combined) => {
            logic_changed = paint_device_selector(
                ui,
                "combined_logic_device_selector",
                "Logic analyzer",
                devices,
                selected_logic_device_id,
                DeviceRole::LogicAnalyzer,
                enabled,
            );
            ui.add_space(4.0);
            scope_changed = paint_device_selector(
                ui,
                "combined_scope_device_selector",
                "Oscilloscope",
                devices,
                selected_scope_device_id,
                DeviceRole::Oscilloscope,
                enabled,
            );
        }
        Some(WaveformTab::RadioReceiver) => {}
        None => {}
    }
    (logic_changed, scope_changed)
}

fn paint_usb_speed_warning(context: &egui::Context, warning: &UsbSpeedWarning) -> bool {
    let mut dismissed = false;
    let screen_rect = context.input(|input| input.screen_rect());
    egui::Area::new(egui::Id::new("usb_speed_warning_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen_rect.min)
        .show(context, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, Color32::from_black_alpha(112));
        });

    egui::Area::new(egui::Id::new("usb_speed_warning"))
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .order(egui::Order::Foreground)
        .movable(false)
        .show(context, |ui| {
            egui::Frame::window(&context.style())
                .inner_margin(egui::Margin::same(12.0))
                .show(ui, |ui| {
                    ui.set_max_width(340.0);
                    ui.label(
                        RichText::new("USB 3 connection recommended")
                            .strong()
                            .color(SCOPE_COLORS[0]),
                    );
                    ui.add_space(4.0);
                    for device in &warning.devices {
                        ui.add(
                            egui::Label::new(format!(
                                "{} is using {}.",
                                device.profile.display_name,
                                device.speed.description()
                            ))
                            .wrap(),
                        );
                    }
                    ui.add_space(4.0);
                    ui.label("Use a USB 3 port and cable for full performance.");
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("OK").clicked() {
                            dismissed = true;
                        }
                    });
                });
        });
    dismissed
}

impl eframe::App for WaveCraftApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_calibration_progress();
        self.poll_worker();
        self.reconcile_capture_settings_tab();
        self.reconcile_waveform_tab();
        self.reconcile_device_selections();
        self.reconcile_radio_settings();
        self.poll_capture();
        self.poll_repetitive_capture();
        self.finish_pending_snapshot(context);

        let mut refresh_clicked = false;
        let mut snapshot_clicked = false;
        let mut record_now_clicked = false;
        let mut open_recording_clicked = false;
        let mut export_saleae_clicked = false;
        let mut start_request = None;
        let mut stop_clicked = false;
        let mut calibration_action = None;
        let mut logic_device_selection_changed = false;
        let mut scope_device_selection_changed = false;
        let mut radio_device_selection_changed = false;
        let capture_settings_enabled = !self.is_busy();
        let radio_runtime_controls_enabled = self.capture.as_ref().is_some_and(|active| {
            active.mode == AcquisitionMode::Run
                && active.target == CaptureTarget::RadioReceiver
                && active.radio_runtime_control.is_some()
        });
        let radio_settings_before_ui = self.radio_settings.clone();
        let control_target = self.active_waveform_tab.map(WaveformTab::capture_target);
        let selected_logic_device = self.selected_logic_device().is_some();
        let logic_settings_speed = self
            .selected_logic_device()
            .map(|device| device.speed)
            .or_else(|| {
                self.devices
                    .iter()
                    .filter(|entry| entry.device.profile.model.is_logic_analyzer())
                    .map(|entry| entry.device.speed)
                    .min()
            })
            .unwrap_or(UsbSpeedClass::HighSpeed);
        let selected_scope_device = self.selected_scope_device_ref().is_some();
        let selected_radio_device =
            selected_radio(&self.radios, self.selected_radio_id.as_deref()).is_some();
        let control_target_available = control_target.is_some_and(|target| {
            target.is_available(
                selected_logic_device,
                selected_scope_device,
                selected_radio_device,
            )
        });
        let scope_capture_requested = self.capture_run_requested
            && self
                .requested_capture_target
                .is_some_and(CaptureTarget::includes_oscilloscope);
        let manual_calibration_enabled = manual_calibration_control_is_enabled(
            self.pending.is_some(),
            selected_scope_device,
            self.manual_calibration.is_some(),
            self.capture
                .as_ref()
                .filter(|active| active.target.includes_oscilloscope())
                .map(|active| active.mode),
        );
        let trigger_controls_enabled = scope_trigger_controls_are_enabled(
            self.pending.is_some(),
            scope_capture_requested,
            selected_scope_device,
            self.capture
                .as_ref()
                .map(|active| !active.scope_trigger_controls.is_empty()),
        );
        let frontend_controls_enabled = scope_frontend_controls_are_enabled(
            self.pending.is_some(),
            scope_capture_requested,
            selected_scope_device,
        ) && !(self.record_capture && self.is_busy());
        let scope_settings_before_ui = self.scope_settings;
        let scope_auto_sample_rate_before_ui = self.scope_auto_sample_rate;
        let manual_calibration_before_ui =
            self.manual_calibration.as_ref().map(|editor| editor.values);
        let manual_calibration_range_before_ui =
            self.manual_calibration.as_ref().map(|editor| editor.range);

        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(12.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("WAVECRAFT").strong().size(18.0).color(ACCENT));
                    ui.separator();
                    ui.menu_button("App", |ui| {
                        if ui.button("Settings…").clicked() {
                            self.settings_open = true;
                            ui.close_menu();
                        }
                    });
                    ui.separator();
                    ui.label(RichText::new("USB and SDR capture workbench").color(TEXT_MUTED));
                    ui.separator();
                    refresh_clicked = ui
                        .add_enabled(
                            !self.is_busy(),
                            toolbar_action_button("↻ Refresh", SCOPE_COLORS[1]),
                        )
                        .clicked();
                    ui.separator();
                    if ui
                        .add_enabled(
                            !self.is_busy() && control_target_available,
                            toolbar_action_button("⏺ Record", Color32::LIGHT_RED)
                                .min_size(Vec2::new(CAPTURE_FILE_BUTTON_WIDTH, 0.0)),
                        )
                        .on_hover_text("Record a complete Single capture to a .dsv file")
                        .clicked()
                    {
                        record_now_clicked = true;
                    }
                    if ui
                        .add_enabled(
                            !self.is_busy(),
                            toolbar_action_button("📂 Open", TEXT_MUTED)
                                .min_size(Vec2::new(CAPTURE_FILE_BUTTON_WIDTH, 0.0)),
                        )
                        .on_hover_text("Open a saved .dsv capture")
                        .clicked()
                    {
                        open_recording_clicked = true;
                    }
                    if ui
                        .add_enabled(
                            !self.is_busy() && self.has_displayed_completed_capture(),
                            toolbar_action_button("Export", TEXT_MUTED)
                                .min_size(Vec2::new(CAPTURE_FILE_BUTTON_WIDTH, 0.0)),
                        )
                        .on_hover_text("Export the completed capture as a Saleae Logic 2 .sal file")
                        .clicked()
                    {
                        export_saleae_clicked = true;
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.snapshot_plot_rect.is_some()
                                && self.pending_snapshot.is_none(),
                            toolbar_action_button("📷", TEXT_MUTED),
                        )
                        .on_hover_text("Save the visible waveform plot as a PNG snapshot")
                        .clicked()
                    {
                        snapshot_clicked = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let logic_only = control_target == Some(CaptureTarget::LogicAnalyzer);
                        let acquisition_active = self.capture_run_requested
                            && self.requested_capture_target == control_target
                            && (self.capture.is_some()
                                || self.pending.is_some()
                                || self.scheduled_repetitive_capture.is_some());
                        let start_mode = if logic_only {
                            self.logic_acquisition_mode
                        } else {
                            AcquisitionMode::Run
                        };
                        let run_stop_clicked = ui
                            .add_enabled(
                                acquisition_active
                                    || (!self.is_busy()
                                        && control_target_available
                                        && !self.record_capture),
                                if acquisition_active {
                                    toolbar_action_button("■ Stop", Color32::LIGHT_RED)
                                } else {
                                    toolbar_action_button("▶ Run", ACCENT)
                                },
                            )
                            .on_hover_text(if acquisition_active {
                                "Stop the active acquisition"
                            } else if logic_only {
                                match start_mode {
                                    AcquisitionMode::Single => {
                                        "Wait for one trigger, acquire once, then stop"
                                    }
                                    AcquisitionMode::Repetitive => {
                                        "Repeatedly acquire complete captures"
                                    }
                                    AcquisitionMode::Loop => {
                                        "Continuously append stream captures to the rolling display"
                                    }
                                    AcquisitionMode::Run => "Start the selected acquisition",
                                }
                            } else {
                                "Run continuously; Auto trigger rolls, edge triggers update the display"
                            })
                            .clicked();
                        if run_stop_clicked {
                            if acquisition_active {
                                stop_clicked = true;
                            } else if let Some(target) = control_target {
                                start_request = Some((start_mode, target));
                            }
                        }
                        if ui
                            .add_enabled(
                                !self.is_busy() && control_target_available,
                                toolbar_action_button("▶ Single", SCOPE_COLORS[0]),
                            )
                            .on_hover_text("Wait for one trigger, acquire once, then stop")
                            .clicked()
                        {
                            if let Some(target) = control_target {
                                start_request = Some((AcquisitionMode::Single, target));
                            }
                        }
                    });
                });
            });

        let available_waveform_tabs = self.available_waveform_tabs();
        if !available_waveform_tabs.is_empty() {
            egui::TopBottomPanel::top("waveform_tabs")
                .frame(
                    egui::Frame::none()
                        .fill(PANEL)
                        .inner_margin(egui::Margin::symmetric(14.0, 7.0)),
                )
                .show(context, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("VIEW").strong().color(TEXT_MUTED));
                        ui.separator();
                        for tab in &available_waveform_tabs {
                            if ui
                                .selectable_label(
                                    self.active_waveform_tab == Some(*tab),
                                    tab.label(),
                                )
                                .clicked()
                            {
                                self.select_waveform_tab(*tab);
                            }
                        }
                    });
                });
        }

        let (displayed_logic_sample_count, displayed_logic_sample_rate_hz) = self
            .capture
            .as_ref()
            .filter(|active| !active.preserve_display_until_complete)
            .map(|active| &active.waveforms)
            .or_else(|| {
                self.displayed_completed_capture()
                    .map(|capture| &capture.waveforms)
            })
            .map(|waveforms| (waveforms.logic_sample_count, waveforms.logic_rate_hz))
            .unwrap_or((0, 0));

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
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(content_width);
                        ui.set_max_width(content_width);
                        ui.label(RichText::new("CAPTURE SETUP").strong().color(TEXT_MUTED));
                        ui.add_space(8.0);
                        let (logic_changed, scope_changed) = paint_capture_device_selectors(
                            ui,
                            self.active_waveform_tab,
                            &self.devices,
                            &mut self.selected_logic_device_id,
                            &mut self.selected_scope_device_id,
                            capture_settings_enabled && self.manual_calibration.is_none(),
                        );
                        logic_device_selection_changed |= logic_changed;
                        scope_device_selection_changed |= scope_changed;
                        if self.active_waveform_tab == Some(WaveformTab::RadioReceiver) {
                            radio_device_selection_changed |= paint_radio_device_selector(
                                ui,
                                &self.radios,
                                &mut self.selected_radio_id,
                                capture_settings_enabled,
                            );
                            if radio_device_selection_changed {
                                self.reconcile_radio_settings();
                            }
                            if let Some(radio) =
                                selected_radio(&self.radios, self.selected_radio_id.as_deref())
                                    .cloned()
                            {
                                paint_radio_capture_settings(
                                    ui,
                                    &radio,
                                    &mut self.radio_settings,
                                    capture_settings_enabled,
                                    radio_runtime_controls_enabled,
                                );
                            }
                        }
                        if self.active_waveform_tab.is_some() {
                            ui.add_space(12.0);
                        }
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
                        if available_capture_settings_tabs.is_empty()
                            && self.active_waveform_tab != Some(WaveformTab::RadioReceiver)
                        {
                            ui.label(
                                RichText::new(
                                    "Connect a supported instrument to configure capture.",
                                )
                                .color(TEXT_MUTED),
                            );
                        } else if self.active_waveform_tab != Some(WaveformTab::RadioReceiver) {
                            paint_capture_settings(
                                ui,
                                self.active_capture_settings_tab,
                                LogicCaptureSettingsEditor {
                                    settings: &mut self.logic_settings,
                                    speed: logic_settings_speed,
                                    acquisition_mode: &mut self.logic_acquisition_mode,
                                    repetitive_interval_seconds: &mut self
                                        .repetitive_interval_seconds,
                                    smart_timing_enabled: &mut self.logic_smart_timing_enabled,
                                    marker_settings: &mut self.logic_marker_settings,
                                    protocol_analyzer: &mut self.protocol_analyzer,
                                    displayed_sample_count: displayed_logic_sample_count,
                                    displayed_sample_rate_hz: displayed_logic_sample_rate_hz,
                                },
                                ScopeCaptureSettingsEditor {
                                    settings: &mut self.scope_settings,
                                    auto_sample_rate: &mut self.scope_auto_sample_rate,
                                    smart_timing_enabled: &mut self.scope_smart_timing_enabled,
                                },
                                ScopeVisualizationSettingsEditor {
                                    channel_colors: &mut self.scope_channel_colors,
                                    scope_channel_names: &mut self.scope_channel_names,
                                },
                                CaptureSettingsAvailability {
                                    capture: capture_settings_enabled,
                                    sample_rate: frontend_controls_enabled,
                                    channel_enable: frontend_controls_enabled,
                                    input_range: frontend_controls_enabled,
                                    manual_calibration: manual_calibration_enabled,
                                },
                                &mut calibration_action,
                            );
                        }
                    });
            });

        paint_protocol_analyzer_modal(
            context,
            &mut self.protocol_analyzer,
            &self.logic_channel_names,
            &self.logic_settings.enabled_channels,
        );

        if self.logic_acquisition_mode == AcquisitionMode::Loop
            && self.logic_settings.operation_mode != DsLogicOperationMode::Stream
        {
            self.logic_acquisition_mode = AcquisitionMode::Single;
        }

        if scope_tools_panel_is_visible(self.active_waveform_tab) {
            egui::SidePanel::right("scope_tools_fixed")
                .resizable(false)
                .exact_width(300.0)
                .frame(
                    egui::Frame::none()
                        .fill(PANEL)
                        .inner_margin(egui::Margin::same(14.0)),
                )
                .show(context, |ui| {
                    let content_width = ui.available_width();
                    egui::ScrollArea::vertical()
                        .max_width(content_width)
                        .show(ui, |ui| {
                            ui.set_width(content_width);
                            ui.label(RichText::new("SCOPE TOOLS").strong().color(TEXT_MUTED));
                            ui.add_space(8.0);
                            egui::Frame::none()
                                .fill(BACKGROUND)
                                .rounding(egui::Rounding::same(6.0))
                                .inner_margin(egui::Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.set_width((content_width - 20.0).max(0.0));
                                    paint_scope_tools(
                                        ui,
                                        &mut self.scope_settings,
                                        &mut self.scope_function_settings,
                                        &mut self.scope_marker_settings,
                                        &mut self.scope_marker_editor,
                                        &self.scope_channel_names,
                                        ScopeToolsOptions {
                                            viewport: self.scope_viewport,
                                            trigger_controls_enabled,
                                        },
                                    );
                                });
                        });
                });
        }

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
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(BUILD_VERSION_LABEL)
                                .small()
                                .monospace()
                                .color(TEXT_MUTED),
                        )
                        .on_hover_text("Build version");
                    });
                });
            });

        let session_tab_state = match self.active_waveform_tab {
            Some(WaveformTab::LogicAnalyzer) => Some((
                CaptureTarget::LogicAnalyzer,
                self.logic_sessions.names(),
                self.logic_sessions.active_index(),
            )),
            Some(WaveformTab::Oscilloscope | WaveformTab::Lissajous) => Some((
                CaptureTarget::Oscilloscope,
                self.scope_sessions.names(),
                self.scope_sessions.active_index(),
            )),
            Some(WaveformTab::RadioReceiver | WaveformTab::Combined) | None => None,
        };
        let mut session_tab_action = None;
        let mut session_name_commit = false;
        let mut session_name_cancel = false;
        if let Some((target, tab_names, active_tab)) = session_tab_state {
            let tab_count = tab_names.len();
            let sessions_enabled = !self.is_busy();
            egui::TopBottomPanel::bottom("session_tabs")
                .frame(
                    egui::Frame::none()
                        .fill(PANEL)
                        .inner_margin(egui::Margin::symmetric(14.0, 6.0)),
                )
                .show(context, |ui| {
                    egui::ScrollArea::horizontal()
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for (index, name) in tab_names.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        let editing = self
                                            .session_name_editor
                                            .as_ref()
                                            .is_some_and(|editor| {
                                                editor.target == target && editor.index == index
                                            });
                                        if editing {
                                            if let Some(editor) = self.session_name_editor.as_mut() {
                                                let response = ui.add_enabled(
                                                    sessions_enabled,
                                                    egui::TextEdit::singleline(&mut editor.draft)
                                                        .char_limit(SESSION_NAME_MAX_CHARS)
                                                        .desired_width(140.0),
                                                );
                                                if editor.request_focus {
                                                    response.request_focus();
                                                    editor.request_focus = false;
                                                }
                                                let escape_pressed = ui.input(|input| {
                                                    input.key_pressed(egui::Key::Escape)
                                                });
                                                let enter_pressed = ui.input(|input| {
                                                    input.key_pressed(egui::Key::Enter)
                                                });
                                                if escape_pressed {
                                                    session_name_cancel = true;
                                                } else if enter_pressed || response.lost_focus() {
                                                    session_name_commit = true;
                                                }
                                            }
                                        } else {
                                            let response = ui
                                                .add_enabled(
                                                    sessions_enabled,
                                                    egui::SelectableLabel::new(
                                                        active_tab == index,
                                                        name,
                                                    ),
                                                )
                                                .on_hover_text(if sessions_enabled {
                                                    "Show this session · double-click to rename"
                                                } else {
                                                    "Stop the active acquisition before changing sessions"
                                                });
                                            if response.double_clicked() {
                                                session_tab_action = Some((
                                                    target,
                                                    SessionTabAction::BeginRename(index),
                                                ));
                                            } else if response.clicked() {
                                                session_tab_action = Some((
                                                    target,
                                                    SessionTabAction::Select(index),
                                                ));
                                            }
                                        }
                                        let close_enabled = sessions_enabled && tab_count > 1;
                                        if ui
                                            .add_enabled(
                                                close_enabled,
                                                egui::Button::new("×").small().frame(false),
                                            )
                                            .on_hover_text(if !sessions_enabled {
                                                "Stop the active acquisition before closing a session"
                                            } else if tab_count <= 1 {
                                                "At least one session must remain open"
                                            } else {
                                                "Close and permanently discard this session"
                                            })
                                            .clicked()
                                        {
                                            session_tab_action =
                                                Some((target, SessionTabAction::Close(index)));
                                        }
                                    });
                                }
                                let add_enabled = sessions_enabled && tab_count < MAX_SESSION_TABS;
                                if ui
                                    .add_enabled(add_enabled, egui::Button::new("+").small())
                                    .on_hover_text(if tab_count >= MAX_SESSION_TABS {
                                        "The 10-session limit has been reached"
                                    } else if !sessions_enabled {
                                        "Stop the active acquisition before adding a session"
                                    } else {
                                        "Add a new empty session"
                                    })
                                    .clicked()
                                {
                                    session_tab_action = Some((target, SessionTabAction::Add));
                                }
                            });
                        });
                });
        }
        if session_name_cancel {
            self.session_name_editor = None;
        } else if session_name_commit {
            if let Some(editor) = self.session_name_editor.take() {
                let renamed = match editor.target {
                    CaptureTarget::LogicAnalyzer => {
                        self.logic_sessions.rename(editor.index, &editor.draft)
                    }
                    CaptureTarget::Oscilloscope => {
                        self.scope_sessions.rename(editor.index, &editor.draft)
                    }
                    CaptureTarget::RadioReceiver | CaptureTarget::Combined => false,
                };
                if renamed {
                    self.status = "Session renamed".to_owned();
                    self.last_error = None;
                }
            }
        }
        if let Some((target, action)) = session_tab_action {
            let action_session_name = match action {
                SessionTabAction::Close(index) => match target {
                    CaptureTarget::LogicAnalyzer => self.logic_sessions.name(index),
                    CaptureTarget::Oscilloscope => self.scope_sessions.name(index),
                    CaptureTarget::RadioReceiver | CaptureTarget::Combined => None,
                }
                .map(str::to_owned),
                _ => None,
            };
            if !matches!(action, SessionTabAction::BeginRename(_)) {
                self.session_name_editor = None;
            }
            let changed = match target {
                CaptureTarget::LogicAnalyzer => {
                    self.save_active_logic_session_settings();
                    match action {
                        SessionTabAction::Select(index) => {
                            if !self.logic_sessions.select(index) {
                                false
                            } else {
                                let settings = self.logic_sessions.active_settings().clone();
                                self.apply_logic_session_settings(settings);
                                true
                            }
                        }
                        SessionTabAction::Add => {
                            let settings = self.current_logic_session_settings();
                            self.logic_sessions.add(settings)
                        }
                        SessionTabAction::Close(index) => {
                            if !self.logic_sessions.close(index) {
                                false
                            } else {
                                let settings = self.logic_sessions.active_settings().clone();
                                self.apply_logic_session_settings(settings);
                                true
                            }
                        }
                        SessionTabAction::BeginRename(index) => {
                            if let Some(name) = self.logic_sessions.name(index).map(str::to_owned) {
                                self.session_name_editor = Some(SessionNameEditor {
                                    target,
                                    index,
                                    draft: name,
                                    request_focus: true,
                                });
                                true
                            } else {
                                false
                            }
                        }
                    }
                }
                CaptureTarget::Oscilloscope => {
                    self.save_active_scope_session_settings();
                    match action {
                        SessionTabAction::Select(index) => {
                            if !self.scope_sessions.select(index) {
                                false
                            } else {
                                let settings = self.scope_sessions.active_settings().clone();
                                self.apply_scope_session_settings(settings);
                                true
                            }
                        }
                        SessionTabAction::Add => {
                            let settings = self.current_scope_session_settings();
                            self.scope_sessions.add(settings)
                        }
                        SessionTabAction::Close(index) => {
                            if !self.scope_sessions.close(index) {
                                false
                            } else {
                                let settings = self.scope_sessions.active_settings().clone();
                                self.apply_scope_session_settings(settings);
                                true
                            }
                        }
                        SessionTabAction::BeginRename(index) => {
                            if let Some(name) = self.scope_sessions.name(index).map(str::to_owned) {
                                self.session_name_editor = Some(SessionNameEditor {
                                    target,
                                    index,
                                    draft: name,
                                    request_focus: true,
                                });
                                true
                            } else {
                                false
                            }
                        }
                    }
                }
                CaptureTarget::RadioReceiver | CaptureTarget::Combined => {
                    unreachable!("radio and combined views have no session tabs")
                }
            };
            if changed {
                match action {
                    SessionTabAction::Add => {
                        let active_name = match target {
                            CaptureTarget::LogicAnalyzer => {
                                self.logic_sessions.name(self.logic_sessions.active_index())
                            }
                            CaptureTarget::Oscilloscope => {
                                self.scope_sessions.name(self.scope_sessions.active_index())
                            }
                            CaptureTarget::RadioReceiver | CaptureTarget::Combined => {
                                unreachable!("radio and combined views have no session tabs")
                            }
                        }
                        .unwrap_or("session");
                        self.status = format!("Added {} {active_name}", target.label());
                        self.last_error = None;
                    }
                    SessionTabAction::Close(_) => {
                        self.status = format!(
                            "Closed {} {}; its data was discarded",
                            target.label(),
                            action_session_name.as_deref().unwrap_or("session")
                        );
                        self.last_error = None;
                    }
                    SessionTabAction::Select(_) | SessionTabAction::BeginRename(_) => {}
                }
            }
        }

        let mut snapshot_plot_rect = None;
        let protocol_analyzer_for_waveform = self.protocol_analyzer.clone();
        let mut protocol_decode_cache = std::mem::take(&mut self.protocol_decode_cache);
        let streamed_protocol_decode = self
            .capture
            .as_ref()
            .filter(|active| !active.preserve_display_until_complete)
            .map(|active| active.protocol_decode.snapshot())
            .or_else(|| {
                (!self.calibration_active)
                    .then(|| self.displayed_completed_capture())
                    .flatten()
                    .map(|capture| capture.protocol_decode.snapshot())
            });
        let mut decoded_data_panel_open = self.decoded_data_panel_open;
        let mut decoded_data_panel_width = self.decoded_data_panel_width;
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
                                "Connect a supported instrument, then Refresh",
                            )
                            .color(TEXT_MUTED),
                        );
                    });
                    return;
                }

                let empty_waveforms = WaveformStore::default();
                let waveforms = self
                    .capture
                    .as_ref()
                    .filter(|active| !active.preserve_display_until_complete)
                    .map(|active| &active.waveforms)
                    .or_else(|| {
                        self.calibration_active
                            .then_some(self.calibration_display.as_ref())
                            .flatten()
                            .map(|display| &display.waveforms)
                    })
                    .or_else(|| {
                        self.displayed_completed_capture()
                            .map(|capture| &capture.waveforms)
                    })
                    .unwrap_or(&empty_waveforms);
                let mut combined_logic_ratio = self.combined_logic_ratio;
                let mut logic_channel_colors = self.logic_channel_colors;
                let mut logic_channel_marker_opacities = self.logic_channel_marker_opacities;
                let mut logic_channel_names = self.logic_channel_names.clone();
                let mut logic_channel_settings_drafts = self.logic_channel_settings_drafts.clone();
                let mut logic_marker_settings = self.logic_marker_settings.clone();
                let mut logic_enabled_channels = self.logic_settings.enabled_channels;
                let mut logic_maximum_heights = self.logic_settings.maximum_heights;
                let mut logic_viewport = self.logic_viewport;
                let mut scope_viewport = self.scope_viewport;
                let mut combined_viewport = self.combined_viewport;
                let mut scope_fft_viewport = self.scope_fft_viewport;
                let mut scope_measurement_caches = self.scope_measurement_caches;
                let mut scope_measurement_window_positions =
                    self.scope_measurement_window_positions;
                let mut scope_marker_settings = self.scope_marker_settings;
                let mut scope_quick_time_measurement = self.scope_quick_time_measurement;
                let mut scope_time_per_division_seconds = self.scope_time_per_division_seconds;
                let mut scope_timebase_changed = false;
                let mut scope_vertical_offsets = self
                    .scope_settings
                    .channels
                    .map(|channel| channel.vertical_offset);
                let mut scope_trigger_settings = self.scope_settings.trigger;
                let scope_settings = self
                    .capture
                    .as_ref()
                    .map(|active| ScopeCaptureSettings::from_capture_config(active.scope_config))
                    .or_else(|| {
                        self.calibration_active
                            .then_some(self.calibration_display.as_ref())
                            .flatten()
                            .map(|display| {
                                ScopeCaptureSettings::from_capture_config(display.scope_config)
                            })
                    })
                    .unwrap_or(self.scope_settings);
                match self.active_waveform_tab {
                    Some(WaveformTab::LogicAnalyzer) => {
                        let decoded_outcomes = streamed_protocol_decode
                            .as_ref()
                            .filter(|snapshot| {
                                snapshot
                                    .matches_analyzers(&protocol_analyzer_for_waveform.analyzers)
                            })
                            .map_or_else(
                                || {
                                    Cow::Owned(
                                        protocol_decode_cache
                                            .outcomes(
                                                waveforms,
                                                &protocol_analyzer_for_waveform.analyzers,
                                                None,
                                            )
                                            .to_vec(),
                                    )
                                },
                                |snapshot| Cow::Borrowed(snapshot.outcomes()),
                            );
                        let sample_offset = waveforms.logic_sample_offset;
                        let sample_count = waveforms.logic_sample_count;
                        let sample_rate_hz = waveforms.logic_rate_hz;
                        let available_size = ui.available_size();
                        let spacing = ui.spacing().item_spacing.x;
                        let splitter_width = 6.0;
                        let minimum_waveform_width = 260.0;
                        let mut selected_range = None;
                        let mut paint_waveform = |waveform_ui: &mut egui::Ui| {
                            waveform_card_fill(
                                waveform_ui,
                                "DIGITAL",
                                "DSLogic · channels 0–15",
                                |ui, rect| {
                                    paint_logic(
                                        ui,
                                        rect,
                                        waveforms,
                                        LogicWaveformOptions {
                                            channel_colors: &mut logic_channel_colors,
                                            channel_marker_opacities:
                                                &mut logic_channel_marker_opacities,
                                            channel_names: &mut logic_channel_names,
                                            channel_settings_drafts:
                                                &mut logic_channel_settings_drafts,
                                            smart_timing_enabled: self.logic_smart_timing_enabled,
                                            marker_settings: &mut logic_marker_settings,
                                            maximum_heights: &mut logic_maximum_heights,
                                            enabled_channels: &mut logic_enabled_channels,
                                            protocol_analyzer: &protocol_analyzer_for_waveform,
                                            protocol_decode_cache: &mut protocol_decode_cache,
                                            protocol_stream: streamed_protocol_decode.as_ref(),
                                            time_axis: None,
                                        },
                                        &mut logic_viewport,
                                    )
                                },
                            )
                        };
                        if decoded_data_panel_open {
                            let maximum_panel_width = (available_size.x
                                - minimum_waveform_width
                                - splitter_width
                                - spacing * 2.0)
                                .max(120.0);
                            decoded_data_panel_width = decoded_data_panel_width.clamp(
                                280.0_f32.min(maximum_panel_width),
                                maximum_panel_width.min(560.0),
                            );
                            let waveform_width = (available_size.x
                                - decoded_data_panel_width
                                - splitter_width
                                - spacing * 2.0)
                                .max(1.0);
                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    Vec2::new(waveform_width, available_size.y),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        snapshot_plot_rect = Some(paint_waveform(ui));
                                    },
                                );
                                let (splitter_rect, splitter_response) = ui.allocate_exact_size(
                                    Vec2::new(splitter_width, available_size.y),
                                    Sense::drag(),
                                );
                                let splitter_response = splitter_response
                                    .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                                ui.painter().line_segment(
                                    [splitter_rect.center_top(), splitter_rect.center_bottom()],
                                    Stroke::new(1.0_f32, GRID),
                                );
                                if splitter_response.dragged() {
                                    let delta = ui.input(|input| input.pointer.delta().x);
                                    decoded_data_panel_width = (decoded_data_panel_width - delta)
                                        .clamp(
                                            280.0_f32.min(maximum_panel_width),
                                            maximum_panel_width.min(560.0),
                                        );
                                }
                                ui.allocate_ui_with_layout(
                                    Vec2::new(decoded_data_panel_width, available_size.y),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        egui::Frame::none()
                                            .fill(PANEL)
                                            .rounding(egui::Rounding::same(6.0))
                                            .inner_margin(egui::Margin::same(12.0))
                                            .show(ui, |ui| {
                                                ui.set_min_height(
                                                    (available_size.y - 24.0).max(0.0),
                                                );
                                                ui.horizontal(|ui| {
                                                    ui.label(
                                                        RichText::new("DECODED DATA")
                                                            .strong()
                                                            .color(TEXT_MUTED),
                                                    );
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            if ui
                                                                .add_sized(
                                                                    DECODED_PANEL_ACTION_BUTTON_SIZE,
                                                                    toolbar_action_button(
                                                                        "▶", ACCENT,
                                                                    ),
                                                                )
                                                                .on_hover_text(
                                                                    "Collapse decoded data",
                                                                )
                                                                .clicked()
                                                            {
                                                                decoded_data_panel_open = false;
                                                            }
                                                        },
                                                    );
                                                });
                                                ui.add_space(8.0);
                                                ui.set_width(ui.available_width());
                                                selected_range = paint_decoded_data_panel(
                                                    ui,
                                                    &protocol_analyzer_for_waveform.analyzers,
                                                    decoded_outcomes.as_ref(),
                                                    sample_rate_hz,
                                                );
                                            });
                                    },
                                );
                            });
                        } else {
                            let collapsed_width = 34.0;
                            let waveform_width =
                                (available_size.x - collapsed_width - spacing).max(1.0);
                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    Vec2::new(waveform_width, available_size.y),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        snapshot_plot_rect = Some(paint_waveform(ui));
                                    },
                                );
                                ui.allocate_ui_with_layout(
                                    Vec2::new(collapsed_width, available_size.y),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        egui::Frame::none()
                                            .fill(PANEL)
                                            .rounding(egui::Rounding::same(6.0))
                                            .inner_margin(egui::Margin::same(4.0))
                                            .show(ui, |ui| {
                                                ui.set_min_height(
                                                    (available_size.y - 8.0).max(0.0),
                                                );
                                                if ui
                                                    .add_sized(
                                                        DECODED_PANEL_ACTION_BUTTON_SIZE,
                                                        toolbar_action_button("◀", ACCENT),
                                                    )
                                                    .on_hover_text("Expand decoded data")
                                                    .clicked()
                                                {
                                                    decoded_data_panel_open = true;
                                                }
                                            });
                                    },
                                );
                            });
                        }
                        if let Some((start_sample, end_sample)) = selected_range {
                            focus_logic_sample_range(
                                &mut logic_viewport,
                                sample_offset,
                                sample_count,
                                start_sample,
                                end_sample,
                            );
                        }
                    }
                    Some(WaveformTab::Oscilloscope) => {
                        let subtitle = scope_channels_subtitle(&self.scope_channel_names);
                        snapshot_plot_rect = Some(waveform_card_fill(
                            ui,
                            "OSCILLOSCOPE",
                            &subtitle,
                            |ui, rect| {
                                paint_scope(
                                    ui,
                                    rect,
                                    waveforms,
                                    &mut scope_viewport,
                                    &mut scope_fft_viewport,
                                    true,
                                    ScopeWaveformOptions {
                                        settings: scope_settings,
                                        auto_sample_rate: self.scope_auto_sample_rate,
                                        smart_timing_enabled: self.scope_smart_timing_enabled,
                                        function_settings: self.scope_function_settings,
                                        channel_names: &self.scope_channel_names,
                                        measurement_caches: &mut scope_measurement_caches,
                                        measurement_window_positions:
                                            &mut scope_measurement_window_positions,
                                        marker_settings: &mut scope_marker_settings,
                                        quick_time_measurement: &mut scope_quick_time_measurement,
                                        channel_colors: self.scope_channel_colors,
                                        vertical_offsets: &mut scope_vertical_offsets,
                                        trigger_settings: &mut scope_trigger_settings,
                                        time_per_division_seconds:
                                            &mut scope_time_per_division_seconds,
                                        timebase_changed: &mut scope_timebase_changed,
                                        offset_controls_enabled: frontend_controls_enabled,
                                        trigger_controls_enabled,
                                        time_axis: None,
                                    },
                                )
                            },
                        ));
                    }
                    Some(WaveformTab::Lissajous) => {
                        let lissajous = self.scope_function_settings.lissajous;
                        let subtitle = format!(
                            "DSCope · {} → X / {} → Y",
                            scope_channel_label(lissajous.x_channel, &self.scope_channel_names),
                            scope_channel_label(lissajous.y_channel, &self.scope_channel_names)
                        );
                        let mut lissajous_scope_settings = scope_settings;
                        for (channel, vertical_offset) in lissajous_scope_settings
                            .channels
                            .iter_mut()
                            .zip(scope_vertical_offsets)
                        {
                            channel.vertical_offset = vertical_offset;
                        }
                        snapshot_plot_rect = Some(waveform_card_fill(
                            ui,
                            "LISSAJOUS",
                            &subtitle,
                            |ui, rect| {
                                paint_scope_lissajous(
                                    ui,
                                    rect,
                                    waveforms,
                                    ScopeLissajousPaintOptions {
                                        viewport: scope_viewport,
                                        settings: lissajous_scope_settings,
                                        lissajous,
                                        channel_colors: self.scope_channel_colors,
                                        channel_names: &self.scope_channel_names,
                                    },
                                )
                            },
                        ));
                    }
                    Some(WaveformTab::RadioReceiver) => {
                        let subtitle = waveforms.radio.newest_spectrum().map_or_else(
                            || "Waiting for IQ samples".to_owned(),
                            |spectrum| {
                                format!(
                                    "{} · {} FFT bins",
                                    format_radio_value(spectrum.center_frequency_hz),
                                    spectrum.power_dbfs.len()
                                )
                            },
                        );
                        snapshot_plot_rect = Some(waveform_card_fill(
                            ui,
                            "RF SPECTRUM",
                            &subtitle,
                            |ui, rect| {
                                self.radio_view
                                    .borrow_mut()
                                    .paint(
                                        ui,
                                        rect,
                                        &waveforms.radio,
                                        &waveforms.radio_analysis,
                                        self.radio_settings.analyzer.enabled,
                                    );
                            },
                        ));
                    }
                    Some(WaveformTab::Combined) => {
                        snapshot_plot_rect = Some(combined_waveform_cards(
                            ui,
                            waveforms,
                            &mut combined_logic_ratio,
                            LogicWaveformOptions {
                                channel_colors: &mut logic_channel_colors,
                                channel_marker_opacities: &mut logic_channel_marker_opacities,
                                channel_names: &mut logic_channel_names,
                                channel_settings_drafts: &mut logic_channel_settings_drafts,
                                smart_timing_enabled: self.logic_smart_timing_enabled,
                                marker_settings: &mut logic_marker_settings,
                                maximum_heights: &mut logic_maximum_heights,
                                enabled_channels: &mut logic_enabled_channels,
                                protocol_analyzer: &protocol_analyzer_for_waveform,
                                protocol_decode_cache: &mut protocol_decode_cache,
                                protocol_stream: streamed_protocol_decode.as_ref(),
                                time_axis: None,
                            },
                            CombinedWaveformViewports {
                                logic: &mut logic_viewport,
                                scope: &mut scope_viewport,
                                combined: &mut combined_viewport,
                                fft: &mut scope_fft_viewport,
                            },
                            ScopeWaveformOptions {
                                settings: scope_settings,
                                auto_sample_rate: self.scope_auto_sample_rate,
                                smart_timing_enabled: self.scope_smart_timing_enabled,
                                function_settings: self.scope_function_settings,
                                channel_names: &self.scope_channel_names,
                                measurement_caches: &mut scope_measurement_caches,
                                measurement_window_positions:
                                    &mut scope_measurement_window_positions,
                                marker_settings: &mut scope_marker_settings,
                                quick_time_measurement: &mut scope_quick_time_measurement,
                                channel_colors: self.scope_channel_colors,
                                vertical_offsets: &mut scope_vertical_offsets,
                                trigger_settings: &mut scope_trigger_settings,
                                time_per_division_seconds: &mut scope_time_per_division_seconds,
                                timebase_changed: &mut scope_timebase_changed,
                                offset_controls_enabled: frontend_controls_enabled,
                                trigger_controls_enabled,
                                time_axis: None,
                            },
                        ));
                    }
                    None => {}
                }
                self.combined_logic_ratio = combined_logic_ratio;
                self.logic_channel_colors = logic_channel_colors;
                self.logic_channel_marker_opacities = logic_channel_marker_opacities;
                self.logic_channel_names = logic_channel_names;
                self.logic_channel_settings_drafts = logic_channel_settings_drafts;
                self.logic_marker_settings = logic_marker_settings;
                self.logic_settings.enabled_channels = logic_enabled_channels;
                self.logic_settings.maximum_heights = logic_maximum_heights;
                self.logic_viewport = logic_viewport;
                self.scope_viewport = scope_viewport;
                self.combined_viewport = combined_viewport;
                self.scope_fft_viewport = scope_fft_viewport;
                self.scope_measurement_caches = scope_measurement_caches;
                self.scope_measurement_window_positions = scope_measurement_window_positions;
                self.scope_marker_settings = scope_marker_settings;
                self.scope_quick_time_measurement = scope_quick_time_measurement;
                self.scope_time_per_division_seconds = scope_time_per_division_seconds;
                let automatic_sample_rate_inputs_changed = scope_timebase_changed
                    || (!scope_auto_sample_rate_before_ui && self.scope_auto_sample_rate)
                    || self.scope_settings.sample_count != scope_settings_before_ui.sample_count
                    || self.scope_settings.enabled_channel_count()
                        != scope_settings_before_ui.enabled_channel_count();
                if self.scope_auto_sample_rate && automatic_sample_rate_inputs_changed {
                    if let Some(time_per_division) = scope_time_per_division_seconds {
                        self.scope_settings.sample_rate_hz = self
                            .scope_settings
                            .sample_rate_for_timebase(time_per_division);
                    }
                }
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
        self.protocol_decode_cache = protocol_decode_cache;
        self.decoded_data_panel_open = decoded_data_panel_open;
        self.decoded_data_panel_width = decoded_data_panel_width;
        self.snapshot_plot_rect = snapshot_plot_rect;
        if snapshot_clicked {
            if let Some(plot_rect) = snapshot_plot_rect {
                self.pending_snapshot = Some(PendingSnapshot {
                    plot_rect,
                    pixels_per_point: context.pixels_per_point(),
                });
                self.status = "Preparing snapshot…".to_owned();
                context.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            }
        }

        let mut settings_open = self.settings_open;
        let mut close_settings = false;
        let settings_locked = self.is_busy();
        if settings_open {
            let screen_rect = context.input(|input| input.screen_rect());
            egui::Area::new(egui::Id::new("settings_modal_backdrop"))
                .order(egui::Order::Middle)
                .fixed_pos(screen_rect.min)
                .show(context, |ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
                    ui.painter().rect_filled(
                        rect,
                        egui::Rounding::ZERO,
                        Color32::from_black_alpha(112),
                    );
                });
        }
        egui::Window::new("Settings")
            .open(&mut settings_open)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size([330.0, 420.0])
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
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
                    paint_instrument_settings(ui, &self.devices);

                    ui.add_space(14.0);
                    ui.label(RichText::new("RADIO RECEIVER").strong().color(ACCENT));
                    ui.add_space(6.0);
                    radio_device_selection_changed |= paint_radio_settings(
                        ui,
                        &self.radios,
                        &mut self.selected_radio_id,
                        &self.radio_discovery_failures,
                        !settings_locked,
                    );
                    if radio_device_selection_changed {
                        self.reconcile_radio_settings();
                    }
                    if let Some(radio) =
                        selected_radio(&self.radios, self.selected_radio_id.as_deref()).cloned()
                    {
                        paint_radio_capture_settings(
                            ui,
                            &radio,
                            &mut self.radio_settings,
                            !settings_locked,
                            radio_runtime_controls_enabled,
                        );
                    }

                    if settings_locked {
                        ui.add_space(8.0);
                        ui.small(
                            RichText::new(if radio_runtime_controls_enabled {
                                "Device, sample-rate, IQ-correction, and antenna-power settings remain locked; tuning and gain controls are live"
                            } else {
                                "Settings are locked while an operation is running"
                            })
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

        let dismiss_usb_speed_warning = self
            .usb_speed_warning
            .as_ref()
            .is_some_and(|warning| paint_usb_speed_warning(context, warning));
        if dismiss_usb_speed_warning {
            self.usb_speed_warning = None;
        }

        let mut auto_confirmation_open = self.auto_calibration_confirmation_open;
        let mut auto_confirmed = false;
        let mut auto_cancelled = false;
        egui::Window::new("DSCope Auto Calibration")
            .id(egui::Id::new("auto_calibration_confirmation"))
            .open(&mut auto_confirmation_open)
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label(RichText::new("Disconnect all probes before continuing.").strong());
                ui.add_space(6.0);
                ui.label(
                    "WaveCraft will run the vendor calibration sequence for both channels: offset transfer, all input ranges, and ADC branch balance. It can take a while. Device NVM is changed only if you choose Save when calibration finishes.",
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Keep all probes disconnected during calibration. Do not disconnect the oscilloscope while calibration is running.",
                    )
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
        let manual_calibration_actions_enabled = self.pending.is_none();
        if let Some(editor) = self.manual_calibration.as_mut() {
            let mut manual_open = true;
            egui::Window::new("DSCope Manual Calibration")
                .open(&mut manual_open)
                .collapsible(false)
                .resizable(false)
                .default_width(430.0)
                .show(context, |ui| {
                    ui.label(
                        "Adjust VGAIN and VOFF for the selected input range. VCOMB is shared by all ranges of a channel. Active acquisition previews edits; device NVM changes only after Save.",
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
                            let mut gain = editor
                                .values
                                .vga_gain(channel, editor.range)
                                .expect("manual calibration selection is valid");
                            ui.horizontal(|ui| {
                                ui.label("VGAIN");
                                ui.add(
                                    egui::DragValue::new(&mut gain)
                                        .range(1..=0x1fff)
                                        .speed(1),
                                );
                            });
                            let _ = editor.values.set_vga_gain(channel, editor.range, gain);

                            let mut offset = editor
                                .values
                                .preoffset(channel, editor.range)
                                .expect("manual calibration selection is valid");
                            ui.horizontal(|ui| {
                                ui.label("VOFF");
                                ui.add(egui::Slider::new(&mut offset, 0..=1_023));
                            });
                            let _ = editor.values.set_preoffset(channel, editor.range, offset);

                            let mut comb = i16::from(
                                editor
                                    .values
                                    .comb_comp(channel)
                                    .expect("manual calibration channel is valid"),
                            );
                            ui.horizontal(|ui| {
                                ui.label("VCOMB");
                                ui.add(egui::Slider::new(&mut comb, -127..=127));
                            });
                            let _ = editor.values.set_comb_comp(channel, comb);
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
                        if ui
                            .add_enabled(
                                manual_calibration_actions_enabled,
                                egui::Button::new("Save to device"),
                            )
                            .clicked()
                        {
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

        paint_scope_marker_editor(
            context,
            &mut self.scope_marker_editor,
            &mut self.scope_marker_settings,
            self.scope_viewport,
            self.scope_settings.channels.map(|channel| channel.enabled),
            &self.scope_channel_names,
        );
        let calibration_modal_action = self
            .calibration_modal
            .as_ref()
            .and_then(|state| paint_calibration_progress_modal(context, state));
        match calibration_modal_action {
            Some(CalibrationModalAction::Close) => self.calibration_modal = None,
            Some(CalibrationModalAction::Save(values)) => self.begin_save_auto_calibration(values),
            None => {}
        }

        if record_now_clicked {
            self.record_capture = false;
            if let Some(path) = choose_recording_save_path(&self.recording_path) {
                self.recording_path = path.to_string_lossy().into_owned();
                self.record_capture = true;
                if let Some(target) = control_target {
                    start_request = Some((AcquisitionMode::Single, target));
                }
            }
        }
        if open_recording_clicked {
            if let Some(path) = choose_recording_to_open() {
                self.begin_open_recording(path);
            }
        }
        if export_saleae_clicked {
            if let Some(path) = choose_saleae_export_path() {
                self.begin_export_saleae(path);
            }
        }
        if refresh_clicked {
            self.begin_refresh();
        }
        if !open_recording_clicked {
            if let Some((mode, target)) = start_request {
                self.begin_capture(mode, target);
            }
        }
        if stop_clicked {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            let repetitive_waiting = self.scheduled_repetitive_capture.take().is_some();
            self.scope_capture_restart_requested = false;
            self.scope_configuration_due = None;
            if self.capture.is_some() {
                self.stop_capture();
            } else if self.pending.is_some() {
                self.status = "Stopping acquisition after device preparation…".to_owned();
            } else if repetitive_waiting {
                self.status = "Acquisition stopped".to_owned();
            }
        }
        match calibration_action {
            Some(ScopeCalibrationAction::Auto) => {
                self.auto_calibration_confirmation_open = true;
            }
            Some(ScopeCalibrationAction::Manual) => self.request_manual_calibration(),
            None => {}
        }
        if auto_confirmed {
            self.begin_auto_calibration();
        }
        if let Some(values) = save_manual_values {
            self.request_save_manual_calibration(values);
        }
        if logic_device_selection_changed || scope_device_selection_changed {
            if let Some(active) = self.active_waveform_tab {
                self.save_waveform_view_settings(active);
            }
            self.last_error = None;
            self.status = "Capture device selection updated".to_owned();
        }
        if radio_device_selection_changed {
            self.last_error = None;
            self.status = "Radio device selection updated".to_owned();
        }
        if radio_runtime_settings_differ(&self.radio_settings, &radio_settings_before_ui) {
            let runtime_control = self.capture.as_ref().and_then(|active| {
                (active.mode == AcquisitionMode::Run
                    && active.target == CaptureTarget::RadioReceiver)
                    .then_some(active.radio_runtime_control.as_ref())
                    .flatten()
            });
            if let Some(runtime_control) = runtime_control {
                let update_result =
                    hackrf_runtime_settings(&self.radio_settings).and_then(|settings| {
                        runtime_control.update(settings).map_err(|e| e.to_string())
                    });
                match update_result {
                    Ok(()) => {
                        self.status = "Updating HackRF tuning during capture…".to_owned();
                        self.last_error = None;
                    }
                    Err(error) => self.set_error(error),
                }
            }
        }
        if scope_device_selection_changed
            && self.selected_scope_device_ref().is_some()
            && !self.is_busy()
        {
            self.scope_configuration_due = Some(Instant::now() + SCOPE_CONFIGURATION_DEBOUNCE);
        }
        let manual_calibration_range_after_ui =
            self.manual_calibration.as_ref().map(|editor| editor.range);
        if manual_calibration_range_after_ui != manual_calibration_range_before_ui {
            if let Some(range) = manual_calibration_range_after_ui {
                let volts_per_div_mv = DSCOPE_VOLTS_PER_DIV_MV[range];
                for channel in &mut self.scope_settings.channels {
                    channel.volts_per_div_mv = volts_per_div_mv;
                }
            }
        }
        let manual_calibration_changed =
            self.manual_calibration.as_ref().map(|editor| editor.values)
                != manual_calibration_before_ui;
        let runtime_frontend_changed = self
            .scope_settings
            .runtime_frontend_differs_from(scope_settings_before_ui);
        let trigger_changed = self
            .scope_settings
            .trigger
            .hardware_differs_from(scope_settings_before_ui.trigger);
        let mut runtime_calibration_applied = false;
        if manual_calibration_changed && !runtime_frontend_changed && scope_capture_requested {
            if let (Some(values), Some(active)) = (
                self.manual_calibration.as_ref().map(|editor| editor.values),
                self.capture.as_ref(),
            ) {
                runtime_calibration_applied = active
                    .scope_calibration_controls
                    .iter()
                    .all(|control| control.update(values).is_ok());
            }
        }
        let manual_calibration_requires_restart =
            manual_calibration_changed && !runtime_calibration_applied;
        if (runtime_frontend_changed || manual_calibration_requires_restart)
            && scope_capture_requested
            && selected_scope_device
        {
            self.scope_configuration_due = Some(Instant::now() + SCOPE_CONFIGURATION_DEBOUNCE);
            self.status = "Adjusting oscilloscope frontend…".to_owned();
            self.last_error = None;
        } else if runtime_calibration_applied {
            self.scope_configuration_due = None;
            self.status = "Applying manual calibration during capture…".to_owned();
            self.last_error = None;
        } else if trigger_changed && self.capture.is_some() {
            let update_result = self
                .scope_settings
                .trigger
                .capture_config()
                .and_then(|trigger| {
                    let Some(active) = self.capture.as_mut() else {
                        return Err("capture ended before the trigger update".to_owned());
                    };
                    let updated_config = active
                        .scope_config
                        .with_trigger_config(trigger)
                        .map_err(|error| error.to_string())?;
                    for control in &active.scope_trigger_controls {
                        control.update(trigger).map_err(|error| error.to_string())?;
                    }
                    active.scope_config = updated_config;
                    Ok(())
                });
            match update_result {
                Ok(()) => {
                    self.scope_configuration_due = None;
                    self.status = "Updating trigger during capture…".to_owned();
                    self.last_error = None;
                }
                Err(error) => self.set_error(error),
            }
        } else if self
            .scope_settings
            .frontend_differs_from(scope_settings_before_ui)
        {
            self.scope_configuration_due = Some(Instant::now() + SCOPE_CONFIGURATION_DEBOUNCE);
            self.status = "Adjusting DSCope configuration…".to_owned();
        }
        let scope_configuration_is_due = self
            .scope_configuration_due
            .is_some_and(|due| Instant::now() >= due);
        if !refresh_clicked
            && start_request.is_none()
            && !stop_clicked
            && scope_configuration_is_due
        {
            if scope_capture_requested && selected_scope_device {
                self.scope_configuration_due = None;
                self.scope_capture_restart_requested = true;
                if let Some(active) = self.capture.as_ref() {
                    active.session.request_stop();
                }
                self.status = "Reconfiguring oscilloscope frontend…".to_owned();
            } else if !self.is_busy() {
                self.begin_scope_configuration();
            }
        }
        if self.pending.is_some() || self.capture.is_some() {
            context.request_repaint_after(Duration::from_millis(16));
        } else if let Some(scheduled) = self.scheduled_repetitive_capture {
            context.request_repaint_after(scheduled.due.saturating_duration_since(Instant::now()));
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
    REQUIRED_RESOURCE_FILES
        .iter()
        .all(|name| directory.join(name).is_file())
}

fn insert_channel_name_metadata(
    metadata: &mut BTreeMap<String, String>,
    logic_channel_names: &[String; LOGIC_CHANNEL_COUNT],
    scope_channel_names: &[String; 2],
) {
    for (index, name) in logic_channel_names.iter().enumerate() {
        metadata.insert(
            format!("logic.ch{index}.name"),
            normalized_channel_name(name),
        );
    }
    for (index, name) in scope_channel_names.iter().enumerate() {
        metadata.insert(
            format!("scope.ch{index}.name"),
            normalized_channel_name(name),
        );
    }
}

struct GuiRecordingSettings<'a> {
    logic_config: DsLogicCaptureConfig,
    logic_settings: LogicCaptureSettings,
    scope_config: DsCopeCaptureConfig,
    scope_settings: ScopeCaptureSettings,
    radio_settings: &'a RadioSettings,
    logic_channel_names: &'a [String; LOGIC_CHANNEL_COUNT],
    scope_channel_names: &'a [String; 2],
}

fn start_gui_recording(
    path: &Path,
    devices: &[DeviceDescriptor],
    settings: GuiRecordingSettings<'_>,
) -> Result<FileRecorder, String> {
    let GuiRecordingSettings {
        logic_config,
        logic_settings,
        scope_config,
        scope_settings,
        radio_settings,
        logic_channel_names,
        scope_channel_names,
    } = settings;
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
        "radio.center_frequency_hz".to_owned(),
        radio_settings.capture.center_frequency_hz.to_string(),
    );
    metadata.insert(
        "radio.sample_rate_hz".to_owned(),
        radio_settings.capture.sample_rate_hz.to_string(),
    );
    metadata.insert(
        "radio.bandwidth_hz".to_owned(),
        radio_settings.capture.bandwidth_hz.to_string(),
    );
    metadata.insert(
        "radio.receive_channel".to_owned(),
        radio_settings.capture.receive_channel.to_string(),
    );
    metadata.insert(
        "radio.iq_correction_enabled".to_owned(),
        radio_settings.iq_correction_enabled.to_string(),
    );
    metadata.insert(
        "radio.amplifier_enabled".to_owned(),
        radio_settings.amplifier_enabled.to_string(),
    );
    metadata.insert(
        "radio.antenna_power_enabled".to_owned(),
        radio_settings.antenna_power_enabled.to_string(),
    );
    metadata.insert(
        "radio.analyzer.enabled".to_owned(),
        radio_settings.analyzer.enabled.to_string(),
    );
    metadata.insert(
        "radio.analyzer.demodulation".to_owned(),
        match radio_settings.analyzer.demodulation {
            RadioDemodulation::BinaryFsk => "fsk2",
            RadioDemodulation::BinaryGfsk => "gfsk2",
        }
        .to_owned(),
    );
    for (key, value) in [
        (
            "channel_offset_hz",
            radio_settings.analyzer.channel_offset_hz.to_string(),
        ),
        (
            "channel_bandwidth_hz",
            radio_settings.analyzer.channel_bandwidth_hz.to_string(),
        ),
        (
            "output_sample_rate_hz",
            radio_settings.analyzer.output_sample_rate_hz.to_string(),
        ),
        (
            "filter_tap_count",
            radio_settings.analyzer.filter_tap_count.to_string(),
        ),
        (
            "symbol_rate_hz",
            radio_settings.analyzer.symbol_rate_hz.to_string(),
        ),
        (
            "deviation_hz",
            radio_settings.analyzer.deviation_hz.to_string(),
        ),
        (
            "gfsk_bt_milli",
            radio_settings.analyzer.gfsk_bt_milli.to_string(),
        ),
        ("inverted", radio_settings.analyzer.inverted.to_string()),
        (
            "adaptive_timing",
            radio_settings.analyzer.adaptive_timing.to_string(),
        ),
        (
            "timing_loop_bandwidth_milli",
            radio_settings
                .analyzer
                .timing_loop_bandwidth_milli
                .to_string(),
        ),
        (
            "max_clock_error_ppm",
            radio_settings.analyzer.max_clock_error_ppm.to_string(),
        ),
    ] {
        metadata.insert(format!("radio.analyzer.{key}"), value);
    }
    for (id, gain_db) in &radio_settings.gains_db {
        metadata.insert(format!("radio.gain.{id}_db"), gain_db.to_string());
    }
    metadata.insert(
        "logic.sample_rate_hz".to_owned(),
        logic_config.sample_rate_hz().to_string(),
    );
    metadata.insert(
        "logic.sample_count".to_owned(),
        logic_config.sample_count().to_string(),
    );
    metadata.insert(
        "logic.operation_mode".to_owned(),
        match logic_config.operation_mode() {
            DsLogicOperationMode::Buffer => "buffer",
            DsLogicOperationMode::Stream => "stream",
            DsLogicOperationMode::InternalTest => "internal_test",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.stop_option".to_owned(),
        match logic_config.stop_option() {
            DsLogicStopOption::StopImmediately => "stop_immediately",
            DsLogicStopOption::UploadCapturedData => "upload_captured_data",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.threshold_voltage".to_owned(),
        logic_config.threshold_voltage().to_string(),
    );
    metadata.insert(
        "logic.input_filter".to_owned(),
        match logic_config.input_filter() {
            DsLogicInputFilter::None => "none",
            DsLogicInputFilter::OneSampleClock => "one_sample_clock",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.rle_enabled".to_owned(),
        logic_config.rle_enabled().to_string(),
    );
    metadata.insert(
        "logic.external_clock".to_owned(),
        logic_config.external_clock().to_string(),
    );
    metadata.insert(
        "logic.falling_clock_edge".to_owned(),
        logic_config.falling_clock_edge().to_string(),
    );
    metadata.insert(
        "logic.trigger.enabled".to_owned(),
        logic_config.trigger().is_some().to_string(),
    );
    match logic_config.trigger() {
        Some(DsLogicTriggerConfig::Simple(trigger)) => {
            metadata.insert("logic.trigger.mode".to_owned(), "simple".to_owned());
            let mut first_condition = None;
            for (channel, condition) in trigger.conditions().into_iter().enumerate() {
                let Some(condition) = condition else {
                    continue;
                };
                first_condition.get_or_insert((channel, condition));
                metadata.insert(
                    format!("logic.trigger.ch{channel}.condition"),
                    logic_trigger_condition_key(condition).to_owned(),
                );
            }
            if let Some((channel, condition)) = first_condition {
                // Keep the original one-channel keys readable by recordings made
                // while simple triggers supported only one condition.
                metadata.insert("logic.trigger.channel".to_owned(), channel.to_string());
                metadata.insert(
                    "logic.trigger.condition".to_owned(),
                    logic_trigger_condition_key(condition).to_owned(),
                );
            }
        }
        Some(DsLogicTriggerConfig::Smart(trigger)) => {
            metadata.insert("logic.trigger.mode".to_owned(), "smart".to_owned());
            metadata.insert(
                "logic.trigger.stage_count".to_owned(),
                trigger.stage_count().to_string(),
            );
            for (stage_index, stage) in trigger.stages().into_iter().enumerate() {
                let Some(stage) = stage else {
                    continue;
                };
                for (channel, condition) in stage.conditions().into_iter().enumerate() {
                    if let Some(condition) = condition {
                        metadata.insert(
                            format!("logic.trigger.stage{stage_index}.ch{channel}.condition"),
                            logic_trigger_condition_key(condition).to_owned(),
                        );
                    }
                }
            }
        }
        None => {}
    }
    for (channel, enabled) in logic_config.enabled_channels().into_iter().enumerate() {
        metadata.insert(format!("logic.ch{channel}.enabled"), enabled.to_string());
        metadata.insert(
            format!("logic.ch{channel}.maximum_height"),
            logic_settings.maximum_heights[channel].to_string(),
        );
    }
    insert_channel_name_metadata(&mut metadata, logic_channel_names, scope_channel_names);
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
    for (index, channel) in scope_settings.channels.iter().enumerate() {
        metadata.insert(
            format!("scope.ch{index}.enabled"),
            channel.enabled.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.volts_per_div_mv"),
            channel.volts_per_div_mv.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.coupling"),
            coupling_label(channel.coupling).to_ascii_lowercase(),
        );
        metadata.insert(
            format!("scope.ch{index}.probe_attenuation"),
            channel.probe_attenuation.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.vertical_offset"),
            channel.vertical_offset.to_string(),
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

fn recording_metadata_value<T: std::str::FromStr>(
    metadata: &BTreeMap<String, String>,
    key: &str,
) -> Option<T> {
    metadata.get(key)?.parse().ok()
}

fn channel_names_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> ([String; LOGIC_CHANNEL_COUNT], [String; 2]) {
    let logic_names = std::array::from_fn(|index| {
        metadata
            .get(&format!("logic.ch{index}.name"))
            .map(|name| normalized_channel_name(name))
            .unwrap_or_default()
    });
    let scope_names = std::array::from_fn(|index| {
        metadata
            .get(&format!("scope.ch{index}.name"))
            .map(|name| normalized_channel_name(name))
            .unwrap_or_default()
    });
    (logic_names, scope_names)
}

fn logic_settings_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> LogicCaptureSettings {
    let mut settings = LogicCaptureSettings::default();
    if let Some(sample_rate_hz) = recording_metadata_value::<u64>(metadata, "logic.sample_rate_hz")
    {
        if DSLOGIC_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            settings.sample_rate_hz = sample_rate_hz;
        }
    }
    if let Some(operation_mode) = metadata.get("logic.operation_mode") {
        settings.operation_mode = match operation_mode.as_str() {
            "stream" => DsLogicOperationMode::Stream,
            "internal_test" => DsLogicOperationMode::InternalTest,
            _ => DsLogicOperationMode::Buffer,
        };
    }
    if let Some(sample_count) = recording_metadata_value::<u64>(metadata, "logic.sample_count") {
        if (DSLOGIC_SAMPLE_ALIGNMENT..=settings.maximum_sample_count()).contains(&sample_count) {
            settings.sample_count = sample_count;
        }
    }
    for (channel, enabled) in settings.enabled_channels.iter_mut().enumerate() {
        if let Some(recorded) =
            recording_metadata_value::<bool>(metadata, &format!("logic.ch{channel}.enabled"))
        {
            *enabled = recorded;
        }
    }
    if let Some(stop_option) = metadata.get("logic.stop_option") {
        settings.stop_option = match stop_option.as_str() {
            "stop_immediately" => DsLogicStopOption::StopImmediately,
            _ => DsLogicStopOption::UploadCapturedData,
        };
    }
    if let Some(threshold_voltage) =
        recording_metadata_value::<f32>(metadata, "logic.threshold_voltage")
    {
        if threshold_voltage.is_finite() && (0.0..=5.0).contains(&threshold_voltage) {
            settings.threshold_voltage = threshold_voltage;
        }
    }
    if let Some(input_filter) = metadata.get("logic.input_filter") {
        settings.input_filter = match input_filter.as_str() {
            "one_sample_clock" => DsLogicInputFilter::OneSampleClock,
            _ => DsLogicInputFilter::None,
        };
    }
    if let Some(maximum_height) = recording_metadata_value::<u8>(metadata, "logic.maximum_height") {
        if (1..=5).contains(&maximum_height) {
            settings.maximum_heights = [maximum_height; LOGIC_CHANNEL_COUNT];
        }
    }
    for (channel, maximum_height) in settings.maximum_heights.iter_mut().enumerate() {
        if let Some(recorded) =
            recording_metadata_value::<u8>(metadata, &format!("logic.ch{channel}.maximum_height"))
        {
            if (1..=5).contains(&recorded) {
                *maximum_height = recorded;
            }
        }
    }
    if let Some(rle_enabled) = recording_metadata_value(metadata, "logic.rle_enabled") {
        settings.rle_enabled = rle_enabled;
    }
    if let Some(external_clock) = recording_metadata_value(metadata, "logic.external_clock") {
        settings.external_clock = external_clock;
    }
    if let Some(falling_clock_edge) = recording_metadata_value(metadata, "logic.falling_clock_edge")
    {
        settings.falling_clock_edge = falling_clock_edge;
    }
    if recording_metadata_value::<bool>(metadata, "logic.trigger.enabled") == Some(true) {
        if metadata.get("logic.trigger.mode").map(String::as_str) == Some("smart") {
            let stage_count = recording_metadata_value::<u8>(metadata, "logic.trigger.stage_count")
                .map(|count| usize::from(count).clamp(2, DSLOGIC_TRIGGER_STAGE_COUNT));
            if let Some(stage_count) = stage_count {
                let mut stages = [[None; LOGIC_CHANNEL_COUNT]; DSLOGIC_TRIGGER_STAGE_COUNT];
                for (stage_index, stage) in stages.iter_mut().enumerate().take(stage_count) {
                    for (channel, destination) in stage.iter_mut().enumerate() {
                        if let Some(condition) = metadata
                            .get(&format!(
                                "logic.trigger.stage{stage_index}.ch{channel}.condition"
                            ))
                            .and_then(|value| parse_logic_trigger_condition(value))
                        {
                            *destination = Some(condition);
                        }
                    }
                }
                let stages_are_valid = stages[..stage_count].iter().all(|stage| {
                    stage.iter().enumerate().any(|(channel, condition)| {
                        condition.is_some() && settings.enabled_channels[channel]
                    }) && stage.iter().enumerate().all(|(channel, condition)| {
                        condition.is_none() || settings.enabled_channels[channel]
                    })
                });
                if stages_are_valid && settings.operation_mode == DsLogicOperationMode::Buffer {
                    settings.trigger_mode = LogicTriggerMode::Smart;
                    settings.smart_trigger_stage_count = stage_count as u8;
                    settings.smart_trigger_conditions = stages;
                    settings.trigger_enabled = true;
                }
            }
        } else {
            let mut trigger_conditions = [None; LOGIC_CHANNEL_COUNT];
            if let (Some(channel), Some(condition)) = (
                recording_metadata_value::<u8>(metadata, "logic.trigger.channel"),
                metadata
                    .get("logic.trigger.condition")
                    .and_then(|value| parse_logic_trigger_condition(value)),
            ) {
                if let Some(destination) = trigger_conditions.get_mut(usize::from(channel)) {
                    *destination = Some(condition);
                }
            }
            for (channel, destination) in trigger_conditions.iter_mut().enumerate() {
                if let Some(condition) = metadata
                    .get(&format!("logic.trigger.ch{channel}.condition"))
                    .and_then(|value| parse_logic_trigger_condition(value))
                {
                    *destination = Some(condition);
                }
            }
            let conditions_are_valid =
                trigger_conditions
                    .iter()
                    .enumerate()
                    .any(|(channel, condition)| {
                        condition.is_some() && settings.enabled_channels[channel]
                    })
                    && trigger_conditions
                        .iter()
                        .enumerate()
                        .all(|(channel, condition)| {
                            condition.is_none() || settings.enabled_channels[channel]
                        });
            if conditions_are_valid && settings.operation_mode != DsLogicOperationMode::InternalTest
            {
                settings.trigger_mode = LogicTriggerMode::Simple;
                settings.trigger_conditions = trigger_conditions;
                settings.trigger_enabled = true;
            }
        }
    }
    settings
}

fn scope_settings_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> ScopeCaptureSettings {
    let mut settings = ScopeCaptureSettings::default();
    if let Some(sample_rate_hz) = recording_metadata_value::<u64>(metadata, "scope.sample_rate_hz")
    {
        if DSCOPE_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            settings.sample_rate_hz = sample_rate_hz;
        }
    }
    if let Some(sample_count) = recording_metadata_value::<u64>(metadata, "scope.sample_count") {
        if DSCOPE_SAMPLE_COUNTS.contains(&sample_count) {
            settings.sample_count = sample_count;
        }
    }
    for (index, channel) in settings.channels.iter_mut().enumerate() {
        if let Some(enabled) =
            recording_metadata_value::<bool>(metadata, &format!("scope.ch{index}.enabled"))
        {
            channel.enabled = enabled;
        }
        if let Some(volts_per_div_mv) =
            recording_metadata_value::<u16>(metadata, &format!("scope.ch{index}.volts_per_div_mv"))
        {
            if DSCOPE_VOLTS_PER_DIV_MV.contains(&volts_per_div_mv) {
                channel.volts_per_div_mv = volts_per_div_mv;
            }
        }
        if let Some(coupling) = metadata.get(&format!("scope.ch{index}.coupling")) {
            channel.coupling = if coupling.eq_ignore_ascii_case("ac") {
                DsCopeCoupling::Ac
            } else {
                DsCopeCoupling::Dc
            };
        }
        if let Some(probe_attenuation) =
            recording_metadata_value::<u8>(metadata, &format!("scope.ch{index}.probe_attenuation"))
        {
            if matches!(probe_attenuation, 1 | 10 | 100) {
                channel.probe_attenuation = probe_attenuation;
            }
        }
        if let Some(vertical_offset) =
            recording_metadata_value::<u16>(metadata, &format!("scope.ch{index}.vertical_offset"))
        {
            if (DSCOPE_VERTICAL_OFFSET_MIN..=DSCOPE_VERTICAL_OFFSET_MAX).contains(&vertical_offset)
            {
                channel.vertical_offset = vertical_offset;
            }
        }
    }

    if let Some(position_percent) =
        recording_metadata_value::<u8>(metadata, "scope.trigger.position_percent")
    {
        if (DSCOPE_TRIGGER_POSITION_MIN_PERCENT..=DSCOPE_TRIGGER_POSITION_MAX_PERCENT)
            .contains(&position_percent)
        {
            settings.trigger.position_percent = position_percent;
        }
    }
    if let Some(source) = metadata.get("scope.trigger.source") {
        settings.trigger.source = match source.as_str() {
            "ch0" => DsCopeTriggerSource::ChannelZero,
            "ch1" => DsCopeTriggerSource::ChannelOne,
            "ch0_and_ch1" => DsCopeTriggerSource::ChannelZeroAndOne,
            "ch0_or_ch1" => DsCopeTriggerSource::ChannelZeroOrOne,
            _ => DsCopeTriggerSource::Auto,
        };
    }
    if let Some(auto_channel) =
        recording_metadata_value::<u8>(metadata, "scope.trigger.auto_channel")
    {
        if auto_channel <= 1 {
            settings.trigger.auto_channel = auto_channel;
        }
    }
    if let Some(slope) = metadata.get("scope.trigger.slope") {
        settings.trigger.slope = if slope == "falling" {
            DsCopeTriggerSlope::Falling
        } else {
            DsCopeTriggerSlope::Rising
        };
    }
    if let Some(holdoff_ns) = recording_metadata_value::<u64>(metadata, "scope.trigger.holdoff_ns")
    {
        settings.trigger.holdoff_ns = holdoff_ns.min(DSCOPE_TRIGGER_HOLDOFF_MAX_NS);
    }
    if let Some(noise_sensitivity) =
        recording_metadata_value::<u8>(metadata, "scope.trigger.noise_sensitivity")
    {
        settings.trigger.noise_sensitivity =
            noise_sensitivity.min(DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX);
    }
    for (index, level) in settings.trigger.levels.iter_mut().enumerate() {
        if let Some(recorded_level) =
            recording_metadata_value::<u8>(metadata, &format!("scope.trigger.ch{index}.level"))
        {
            *level = recorded_level.clamp(DSCOPE_TRIGGER_LEVEL_MIN, DSCOPE_TRIGGER_LEVEL_MAX);
        }
    }
    settings
        .trigger
        .clamp_to_channels(settings.channels.map(|channel| channel.enabled));
    settings
}

fn radio_settings_from_recording_metadata(metadata: &BTreeMap<String, String>) -> RadioSettings {
    let mut settings = RadioSettings::default();
    if let Some(value) = recording_metadata_value(metadata, "radio.center_frequency_hz") {
        settings.capture.center_frequency_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.sample_rate_hz") {
        settings.capture.sample_rate_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.bandwidth_hz") {
        settings.capture.bandwidth_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.receive_channel") {
        settings.capture.receive_channel = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.amplifier_enabled") {
        settings.amplifier_enabled = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.iq_correction_enabled") {
        settings.iq_correction_enabled = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.antenna_power_enabled") {
        settings.antenna_power_enabled = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.enabled") {
        settings.analyzer.enabled = value;
    }
    if let Some(value) = metadata.get("radio.analyzer.demodulation") {
        settings.analyzer.demodulation = match value.as_str() {
            "gfsk2" => RadioDemodulation::BinaryGfsk,
            _ => RadioDemodulation::BinaryFsk,
        };
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.channel_offset_hz") {
        settings.analyzer.channel_offset_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.channel_bandwidth_hz") {
        settings.analyzer.channel_bandwidth_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.output_sample_rate_hz")
    {
        settings.analyzer.output_sample_rate_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.filter_tap_count") {
        settings.analyzer.filter_tap_count = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.symbol_rate_hz") {
        settings.analyzer.symbol_rate_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.deviation_hz") {
        settings.analyzer.deviation_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.gfsk_bt_milli") {
        settings.analyzer.gfsk_bt_milli = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.inverted") {
        settings.analyzer.inverted = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.adaptive_timing") {
        settings.analyzer.adaptive_timing = value;
    }
    if let Some(value) =
        recording_metadata_value(metadata, "radio.analyzer.timing_loop_bandwidth_milli")
    {
        settings.analyzer.timing_loop_bandwidth_milli = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.max_clock_error_ppm") {
        settings.analyzer.max_clock_error_ppm = value;
    }
    for (key, value) in metadata {
        let Some(id) = key
            .strip_prefix("radio.gain.")
            .and_then(|key| key.strip_suffix("_db"))
        else {
            continue;
        };
        if let Ok(value) = value.parse::<i32>() {
            settings.gains_db.insert(id.to_owned(), value);
        }
    }
    settings
}

#[cfg(test)]
fn read_gui_recording(input: impl Read) -> Result<LoadedRecording, String> {
    read_gui_recording_with_analyzers(input, &[])
}

fn read_gui_recording_with_analyzers(
    input: impl Read,
    analyzers: &[ProtocolAnalyzerInstance],
) -> Result<LoadedRecording, String> {
    let mut reader = RecordingReader::new(input).map_err(|error| error.to_string())?;
    let header = reader.header().clone();
    let has_scope_channel_metadata = (0..2).any(|index| {
        header
            .metadata
            .contains_key(&format!("scope.ch{index}.enabled"))
    });
    let logic_settings = logic_settings_from_recording_metadata(&header.metadata);
    let mut scope_settings = scope_settings_from_recording_metadata(&header.metadata);
    let radio_settings = radio_settings_from_recording_metadata(&header.metadata);
    let (logic_channel_names, scope_channel_names) =
        channel_names_from_recording_metadata(&header.metadata);
    let mut enabled_scope_channels = scope_settings.channels.map(|channel| channel.enabled);
    if !enabled_scope_channels.into_iter().any(|enabled| enabled) {
        enabled_scope_channels = [true, true];
        scope_settings.channels[0].enabled = true;
        scope_settings.channels[1].enabled = true;
    }
    let mut waveforms = WaveformStore {
        scope_capacity: (scope_settings.sample_count as usize).min(MAX_WAVEFORM_SAMPLES),
        ..WaveformStore::default()
    };
    if header.metadata.contains_key("logic.sample_count") {
        waveforms.configure_logic_capture(logic_settings.sample_count);
    }
    let mut radio_spectrum =
        SpectrumProcessor::new(RADIO_SPECTRUM_FFT_LENGTH, SpectrumWindow::Hann)?
            .with_iq_correction(radio_settings.iq_correction_enabled);
    let mut radio_analyzer = match radio_settings.analyzer.signal_profile() {
        Ok(Some(profile)) => {
            let analyzer = DemodulatorRegistry::with_builtins()
                .map_err(|error| error.to_string())
                .and_then(|registry| {
                    SignalAnalyzer::new(profile, &registry).map_err(|error| error.to_string())
                });
            match analyzer {
                Ok(analyzer) => Some(analyzer),
                Err(error) => {
                    waveforms.radio_analysis.set_error(error);
                    None
                }
            }
        }
        Ok(None) => None,
        Err(error) => {
            waveforms.radio_analysis.set_error(error);
            None
        }
    };
    let mut protocol_decode = ProtocolCaptureDecoder::new(analyzers);
    while let Some(event) = reader.read_event().map_err(|error| error.to_string())? {
        if let CaptureEvent::Frame(frame) = event {
            if let CaptureBatch::Logic(logic) = &frame.payload {
                protocol_decode.ingest_logic_frame(
                    frame.device_id.as_str(),
                    frame.sequence,
                    &logic.samples,
                    logic.sample_rate_hz,
                );
            }
            if let CaptureBatch::Iq(iq) = &frame.payload {
                ingest_radio_iq(&mut waveforms, &mut radio_spectrum, &mut radio_analyzer, iq);
            }
            waveforms.ingest_frame(&frame, enabled_scope_channels);
        }
    }
    protocol_decode.finish();
    let completion = reader
        .completion_status()
        .ok_or_else(|| "recording has no completion status".to_owned())?;
    let has_logic = header
        .devices
        .iter()
        .any(|device| device.kind == InstrumentKind::LogicAnalyzer)
        || waveforms.logic_channels > 0;
    let has_scope = header
        .devices
        .iter()
        .any(|device| device.kind == InstrumentKind::Oscilloscope)
        || waveforms.scope_rate_hz > 0;
    let has_radio = header
        .devices
        .iter()
        .any(|device| device.kind == InstrumentKind::RadioReceiver)
        || waveforms.radio.newest_spectrum().is_some();
    if has_scope && !has_scope_channel_metadata {
        for (channel, samples) in scope_settings
            .channels
            .iter_mut()
            .zip(&waveforms.scope_channels)
        {
            channel.enabled = !samples.is_empty();
        }
        scope_settings
            .trigger
            .clamp_to_channels(scope_settings.channels.map(|channel| channel.enabled));
    }
    if !has_logic && !has_scope && !has_radio {
        return Err("recording does not contain a supported capture".to_owned());
    }
    Ok(LoadedRecording {
        capture: CompletedCapture {
            waveforms,
            protocol_decode,
            has_logic,
            has_scope,
            has_radio,
        },
        logic_settings: has_logic.then_some(logic_settings),
        scope_settings: has_scope.then_some(scope_settings),
        radio_settings: has_radio.then_some(radio_settings),
        logic_channel_names,
        scope_channel_names,
        completion,
    })
}

fn load_gui_recording(
    path: &Path,
    analyzers: &[ProtocolAnalyzerInstance],
) -> Result<LoadedRecording, String> {
    let file = File::open(path)
        .map_err(|error| format!("failed to open recording {}: {error}", path.display()))?;
    let is_saleae = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sal"));
    let result = if is_saleae {
        read_gui_saleae_with_analyzers(BufReader::new(file), analyzers)
    } else {
        read_gui_recording_with_analyzers(BufReader::new(file), analyzers)
    };
    result.map_err(|error| format!("failed to read recording {}: {error}", path.display()))
}

#[cfg(test)]
fn read_gui_saleae(input: impl Read + Seek) -> Result<LoadedRecording, String> {
    read_gui_saleae_with_analyzers(input, &[])
}

fn read_gui_saleae_with_analyzers(
    input: impl Read + Seek,
    analyzers: &[ProtocolAnalyzerInstance],
) -> Result<LoadedRecording, String> {
    let saleae = read_saleae(input).map_err(|error| error.to_string())?;
    let has_logic = saleae.digital.is_some();
    let has_scope = !saleae.analog.is_empty();
    if !has_logic && !has_scope {
        return Err("Saleae capture contains no supported channels".to_owned());
    }

    let mut logic_settings = LogicCaptureSettings {
        enabled_channels: [false; LOGIC_CHANNEL_COUNT],
        ..LogicCaptureSettings::default()
    };
    let mut logic_names = empty_logic_channel_names();
    let mut waveforms = WaveformStore::default();
    let mut protocol_decode = ProtocolCaptureDecoder::new(analyzers);
    if let Some(digital) = &saleae.digital {
        for channel in &digital.channels {
            let index = usize::from(channel.index);
            if index >= LOGIC_CHANNEL_COUNT {
                return Err(format!(
                    "Saleae digital channel {} exceeds the GUI's {} channels",
                    channel.index, LOGIC_CHANNEL_COUNT
                ));
            }
            logic_settings.enabled_channels[index] = true;
            logic_names[index] = normalized_channel_name(&channel.name);
        }
        if DSLOGIC_SAMPLE_RATES_HZ.contains(&digital.sample_rate_hz) {
            logic_settings.sample_rate_hz = digital.sample_rate_hz;
        }
        waveforms.configure_logic_capture(digital.samples.len() as u64);
        protocol_decode.ingest_logic(&digital.samples, digital.sample_rate_hz);
        waveforms.ingest(
            &CaptureBatch::Logic(wavecraft_core::LogicBatch {
                sample_rate_hz: digital.sample_rate_hz,
                channel_count: digital
                    .channels
                    .iter()
                    .map(|channel| channel.index + 1)
                    .max()
                    .unwrap_or(0),
                samples: digital.samples.clone(),
            }),
            [false; 2],
        );
        waveforms.logic_captured_at = Some(Duration::ZERO);
    }
    protocol_decode.finish();

    let mut scope_settings = ScopeCaptureSettings::default();
    for channel in &mut scope_settings.channels {
        channel.enabled = false;
    }
    let mut scope_names = empty_scope_channel_names();
    if has_scope {
        let rate = saleae
            .analog_sample_rate_hz
            .ok_or_else(|| "Saleae analog sample rate is missing".to_owned())?;
        if DSCOPE_SAMPLE_RATES_HZ.contains(&rate) {
            scope_settings.sample_rate_hz = rate;
        }
        let maximum_samples = saleae
            .analog
            .iter()
            .map(|channel| channel.samples.len())
            .max()
            .unwrap_or(0);
        waveforms.scope_capacity = maximum_samples.clamp(1, MAX_WAVEFORM_SAMPLES);
        let mut enabled = [false; 2];
        let mut channels = Vec::with_capacity(saleae.analog.len());
        for channel in &saleae.analog {
            let index = usize::from(channel.index);
            if index >= 2 {
                return Err(format!(
                    "Saleae analog channel {} exceeds the GUI's two scope channels",
                    channel.index
                ));
            }
            enabled[index] = true;
            scope_settings.channels[index].enabled = true;
            scope_names[index] = normalized_channel_name(&channel.name);
            channels.push(channel.samples.clone());
        }
        waveforms.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: rate,
                trigger_sample_index: None,
                channels,
            }),
            enabled,
        );
        waveforms.scope_captured_at = Some(Duration::ZERO);
    }

    Ok(LoadedRecording {
        capture: CompletedCapture {
            waveforms,
            protocol_decode,
            has_logic,
            has_scope,
            has_radio: false,
        },
        logic_settings: has_logic.then_some(logic_settings),
        scope_settings: has_scope.then_some(scope_settings),
        radio_settings: None,
        logic_channel_names: logic_names,
        scope_channel_names: scope_names,
        completion: CompletionStatus::Complete,
    })
}

fn saleae_capture_from_display(
    completed: &CompletedCapture,
    logic_settings: &LogicCaptureSettings,
    logic_names: &[String; LOGIC_CHANNEL_COUNT],
    scope_names: &[String; 2],
) -> Result<SaleaeCapture, String> {
    let waveforms = &completed.waveforms;
    let digital = if completed.has_logic && !waveforms.logic_samples.is_empty() {
        if waveforms.logic_sample_stride != 1 || waveforms.logic_sample_offset != 0 {
            return Err(
                "This logic capture was decimated for display; export its original .dsv file with wavecraft-cli to preserve every sample"
                    .to_owned(),
            );
        }
        if waveforms
            .logic_samples
            .iter()
            .any(|bucket| bucket.sample_count != 1)
        {
            return Err(
                "The displayed logic capture does not contain individual samples".to_owned(),
            );
        }
        let mut channels = (0..usize::from(waveforms.logic_channels).min(LOGIC_CHANNEL_COUNT))
            .filter(|index| logic_settings.enabled_channels[*index])
            .map(|index| SaleaeChannel {
                index: index as u8,
                name: normalized_channel_name(&logic_names[index]),
            })
            .collect::<Vec<_>>();
        if channels.is_empty() {
            channels = (0..usize::from(waveforms.logic_channels).min(LOGIC_CHANNEL_COUNT))
                .map(|index| SaleaeChannel {
                    index: index as u8,
                    name: normalized_channel_name(&logic_names[index]),
                })
                .collect();
        }
        Some(SaleaeDigitalCapture {
            sample_rate_hz: waveforms.logic_rate_hz,
            samples: waveforms
                .logic_samples
                .iter()
                .map(|bucket| bucket.first)
                .collect(),
            channels,
        })
    } else {
        None
    };

    if waveforms.scope_sample_offset != 0 {
        return Err(
            "This scope capture was trimmed for display; export its original .dsv file with wavecraft-cli to preserve every sample"
                .to_owned(),
        );
    }
    let analog = if completed.has_scope {
        waveforms
            .scope_channels
            .iter()
            .enumerate()
            .filter(|(_, samples)| !samples.is_empty())
            .map(|(index, samples)| {
                SaleaeAnalogChannel::with_automatic_range(
                    index as u8,
                    normalized_channel_name(&scope_names[index]),
                    samples.iter().copied().collect(),
                )
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if digital.is_none() && analog.is_empty() {
        return Err("There are no captured samples to export".to_owned());
    }
    Ok(SaleaeCapture {
        digital,
        analog_sample_rate_hz: (!analog.is_empty()).then_some(waveforms.scope_rate_hz),
        analog,
    })
}

fn recording_completion_label(completion: CompletionStatus) -> &'static str {
    match completion {
        CompletionStatus::Complete => "complete",
        CompletionStatus::Cancelled => "cancelled",
        CompletionStatus::Failed => "failed",
    }
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

struct StartedCapture {
    session: CaptureSession,
    scope_trigger_controls: Vec<DsCopeRuntimeTriggerControl>,
    scope_calibration_controls: Vec<DsCopeRuntimeCalibrationControl>,
    radio_runtime_control: Option<HackRfRuntimeControl>,
    radio_spectrum: SpectrumProcessor,
    radio_analyzer: Option<SignalAnalyzer>,
}

fn start_capture(
    devices: Vec<DiscoveredDevice>,
    radio_request: Option<RadioCaptureRequest>,
    resource_directory: &Path,
    logic_config: DsLogicCaptureConfig,
    scope_config: DsCopeCaptureConfig,
    calibration_values: Option<DsCopeCalibrationValues>,
) -> Result<StartedCapture, String> {
    let iq_correction_enabled = radio_request
        .as_ref()
        .is_some_and(|request| request.iq_correction_enabled);
    let radio_analyzer = radio_request
        .as_ref()
        .and_then(|request| request.analyzer_profile.clone())
        .map(|profile| {
            let registry = DemodulatorRegistry::with_builtins()
                .map_err(|error| format!("failed to initialize demodulators: {error}"))?;
            SignalAnalyzer::new(profile, &registry)
                .map_err(|error| format!("invalid radio analyzer profile: {error}"))
        })
        .transpose()?;
    let mut sources = Vec::<Box<dyn CaptureSource>>::with_capacity(
        devices.len() + usize::from(radio_request.is_some()),
    );
    let mut scope_trigger_controls = Vec::new();
    let mut scope_calibration_controls = Vec::new();
    let mut radio_runtime_control = None;
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
            model if model.is_logic_analyzer() && model.supports_capture() => {
                sources.push(Box::new(
                    DsLogicCaptureSource::new(device, logic_config)
                        .map_err(|error| error.to_string())?,
                ))
            }
            DeviceModel::DsCopeU3P100 => {
                let (source, control) =
                    DsCopeCaptureSource::with_runtime_trigger_control(device, scope_config)
                        .map_err(|error| error.to_string())?;
                let source = match calibration_values {
                    Some(values) => source.with_calibration_values(values),
                    None => source,
                };
                let (source, calibration_control) = source.with_runtime_calibration_control();
                sources.push(Box::new(source));
                scope_trigger_controls.push(control);
                scope_calibration_controls.push(calibration_control);
            }
            _ => {
                return Err(format!(
                    "{} supports discovery, probing, and FPGA preparation, but its acquisition driver is not implemented",
                    device.profile.display_name
                ));
            }
        }
    }
    if let Some(request) = radio_request {
        let source = match request.device.provider {
            RadioProvider::HackRf => {
                let (source, control) = HackRfAdapter::new()
                    .open_with_runtime_control(
                        &request.device,
                        request.settings,
                        request.provider_settings,
                    )
                    .map_err(|error| error.to_string())?;
                radio_runtime_control = Some(control);
                source
            }
            provider => {
                return Err(format!(
                    "capture is not implemented for the {provider:?} radio provider"
                ));
            }
        };
        sources.push(source);
    }
    let radio_spectrum = SpectrumProcessor::new(RADIO_SPECTRUM_FFT_LENGTH, SpectrumWindow::Hann)?
        .with_iq_correction(iq_correction_enabled);
    let session = CaptureCoordinator::start(sources).map_err(|error| error.to_string())?;
    Ok(StartedCapture {
        session,
        scope_trigger_controls,
        scope_calibration_controls,
        radio_runtime_control,
        radio_spectrum,
        radio_analyzer,
    })
}

const WAVEFORM_CARD_CHROME_HEIGHT: f32 = 58.0;
const COMBINED_DIVIDER_HEIGHT: f32 = 10.0;
const MIN_LOGIC_PANE_HEIGHT: f32 = 260.0;
const MIN_SCOPE_PANE_HEIGHT: f32 = 165.0;

struct ScopeWaveformOptions<'a> {
    settings: ScopeCaptureSettings,
    auto_sample_rate: bool,
    smart_timing_enabled: bool,
    function_settings: ScopeFunctionSettings,
    channel_names: &'a [String; 2],
    measurement_caches: &'a mut [ScopeMeasurementCache; 2],
    measurement_window_positions: &'a mut [Vec2; 2],
    marker_settings: &'a mut ScopeMarkerSettings,
    quick_time_measurement: &'a mut ScopeQuickTimeMeasurement,
    channel_colors: [Color32; 2],
    vertical_offsets: &'a mut [u16; 2],
    trigger_settings: &'a mut ScopeTriggerSettings,
    time_per_division_seconds: &'a mut Option<f64>,
    timebase_changed: &'a mut bool,
    offset_controls_enabled: bool,
    trigger_controls_enabled: bool,
    time_axis: Option<WaveformTimeAxis>,
}

fn waveform_card_fill(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    paint: impl FnOnce(&egui::Ui, Rect),
) -> Rect {
    let plot_height = (ui.available_height() - WAVEFORM_CARD_CHROME_HEIGHT).max(64.0);
    waveform_card(ui, title, subtitle, plot_height, paint)
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

struct CombinedWaveformViewports<'a> {
    logic: &'a mut WaveformViewport,
    scope: &'a mut WaveformViewport,
    combined: &'a mut WaveformViewport,
    fft: &'a mut ScopeFftViewport,
}

struct LogicWaveformOptions<'a> {
    channel_colors: &'a mut [Color32; LOGIC_CHANNEL_COUNT],
    channel_marker_opacities: &'a mut [u8; LOGIC_CHANNEL_COUNT],
    channel_names: &'a mut [String; LOGIC_CHANNEL_COUNT],
    channel_settings_drafts: &'a mut [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT],
    smart_timing_enabled: bool,
    marker_settings: &'a mut LogicMarkerSettings,
    maximum_heights: &'a mut [u8; LOGIC_CHANNEL_COUNT],
    enabled_channels: &'a mut [bool; LOGIC_CHANNEL_COUNT],
    protocol_analyzer: &'a ProtocolAnalyzerSettings,
    protocol_decode_cache: &'a mut ProtocolDecodeCache,
    protocol_stream: Option<&'a ProtocolDecodeSnapshot>,
    time_axis: Option<WaveformTimeAxis>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WaveformTimeAxis {
    start_seconds: f64,
    duration_seconds: f64,
}

fn combined_waveform_cards(
    ui: &mut egui::Ui,
    waveforms: &WaveformStore,
    logic_ratio: &mut f32,
    mut logic_options: LogicWaveformOptions<'_>,
    viewports: CombinedWaveformViewports<'_>,
    mut scope_options: ScopeWaveformOptions<'_>,
) -> Rect {
    let CombinedWaveformViewports {
        logic: logic_viewport,
        scope: scope_viewport,
        combined: combined_viewport,
        fft: scope_fft_viewport,
    } = viewports;
    let timeline = waveforms.combined_timeline();
    if let Some(timeline) = timeline {
        *logic_viewport = timeline.source_viewport(*combined_viewport, timeline.logic);
        *scope_viewport = timeline.source_viewport(*combined_viewport, timeline.scope);
        logic_options.time_axis = Some(WaveformTimeAxis {
            start_seconds: timeline.logic.start_seconds - timeline.shared.start_seconds,
            duration_seconds: timeline.logic.duration_seconds(),
        });
        scope_options.time_axis = Some(WaveformTimeAxis {
            start_seconds: timeline.scope.start_seconds - timeline.shared.start_seconds,
            duration_seconds: timeline.scope.duration_seconds(),
        });
    }
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
    let logic_plot_rect = waveform_card_fill(
        &mut logic_ui,
        "DIGITAL",
        "DSLogic · channels 0–15",
        |ui, rect| paint_logic(ui, rect, waveforms, logic_options, logic_viewport),
    );

    if let Some(timeline) = timeline {
        *combined_viewport = timeline.shared_viewport(*logic_viewport, timeline.logic);
        *scope_viewport = timeline.source_viewport(*combined_viewport, timeline.scope);
        *scope_options.time_per_division_seconds = Some(
            f64::from(combined_viewport.span) * timeline.shared.duration_seconds()
                / SCOPE_HORIZONTAL_DIVISIONS as f64,
        );
    }

    let mut scope_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("combined_scope_waveform")
            .max_rect(scope_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let scope_subtitle = scope_channels_subtitle(scope_options.channel_names);
    let scope_plot_rect = waveform_card_fill(
        &mut scope_ui,
        "OSCILLOSCOPE",
        &scope_subtitle,
        |ui, rect| {
            paint_scope(
                ui,
                rect,
                waveforms,
                scope_viewport,
                scope_fft_viewport,
                false,
                scope_options,
            )
        },
    );

    if let Some(timeline) = timeline {
        *combined_viewport = timeline.shared_viewport(*scope_viewport, timeline.scope);
        *logic_viewport = timeline.source_viewport(*combined_viewport, timeline.logic);
        *scope_viewport = timeline.source_viewport(*combined_viewport, timeline.scope);
    }

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
    logic_plot_rect.union(scope_plot_rect)
}

fn waveform_card(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    height: f32,
    paint: impl FnOnce(&egui::Ui, Rect),
) -> Rect {
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
            rect
        })
        .inner
}

fn paint_plot_background(ui: &egui::Ui, rect: Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, BACKGROUND);
}

fn fit_rect_to_grid(area: Rect, horizontal_divisions: usize, vertical_divisions: usize) -> Rect {
    if area.width() <= 0.0
        || area.height() <= 0.0
        || horizontal_divisions == 0
        || vertical_divisions == 0
    {
        return area;
    }
    let target_aspect = horizontal_divisions as f32 / vertical_divisions as f32;
    let area_aspect = area.width() / area.height();
    let size = if area_aspect > target_aspect {
        Vec2::new(area.height() * target_aspect, area.height())
    } else {
        Vec2::new(area.width(), area.width() / target_aspect)
    };
    Rect::from_center_size(area.center(), size)
}

fn align_waveform_chrome(status: Rect, ruler: Rect, plot: Rect) -> (Rect, Rect) {
    let ruler = Rect::from_min_max(
        Pos2::new(plot.left(), plot.top() - ruler.height()),
        Pos2::new(plot.right(), plot.top()),
    );
    let status = Rect::from_min_max(
        Pos2::new(plot.left(), ruler.top() - status.height()),
        Pos2::new(plot.right(), ruler.top()),
    );
    (status, ruler)
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

fn time_scale_label_stride(width: f32) -> usize {
    const MAJOR_INTERVALS: usize = 10;
    const MINIMUM_LABEL_SPACING: f32 = 68.0;
    let maximum_labels = (width / MINIMUM_LABEL_SPACING).floor() as usize;
    let maximum_labels = maximum_labels.clamp(2, MAJOR_INTERVALS + 1);
    MAJOR_INTERVALS.div_ceil(maximum_labels - 1)
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
    let last_sample = sample_offset as f64
        + (f64::from(viewport.start) + f64::from(viewport.span)) * sample_extent;
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
    let label_stride = time_scale_label_stride(rect.width());
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
        let is_labelled =
            is_major && (major_index % label_stride == 0 || major_index == MAJOR_INTERVALS);
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
    time_axis: Option<WaveformTimeAxis>,
) {
    if let Some(time_axis) = time_axis {
        let first_seconds =
            time_axis.start_seconds + f64::from(viewport.start) * time_axis.duration_seconds;
        let last_seconds = first_seconds + f64::from(viewport.span) * time_axis.duration_seconds;
        paint_time_scale(painter, rect, first_seconds, last_seconds, 1);
        return;
    }
    let Some((first_sample, last_sample)) =
        time_scale_sample_window(sample_count, sample_offset, viewport)
    else {
        paint_time_scale(painter, rect, 0.0, 0.0, 0);
        return;
    };
    if sample_rate_hz == 0 {
        paint_time_scale(painter, rect, 0.0, 0.0, 0);
        return;
    }
    let rate = sample_rate_hz as f64;
    paint_time_scale(painter, rect, first_sample / rate, last_sample / rate, 1);
}

enum WaveformPlotClick {
    Primary { double_clicked: bool, position: f32 },
    Secondary,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum WaveformZoomAnchor {
    Pointer,
    Sample {
        domain_fraction: f32,
        screen_fraction: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WaveformViewportOptions {
    reset_on_double_click: bool,
    primary_pan_enabled: bool,
    zoom_anchor: WaveformZoomAnchor,
    timebase_basis: Option<f64>,
}

fn update_waveform_viewport(
    ui: &egui::Ui,
    rect: Rect,
    id_salt: &'static str,
    viewport: &mut WaveformViewport,
    sample_count: usize,
    sample_rate_hz: u64,
    options: WaveformViewportOptions,
) -> (bool, bool, Option<WaveformPlotClick>, Option<f64>) {
    let mut panned = false;
    let mut timebase_changed = false;
    let mut requested_timebase = None;
    let response = ui.interact(
        rect,
        ui.make_persistent_id(id_salt),
        Sense::click_and_drag(),
    );
    let cursor = if !options.primary_pan_enabled {
        egui::CursorIcon::Crosshair
    } else if response.dragged_by(egui::PointerButton::Primary) {
        egui::CursorIcon::Grabbing
    } else {
        egui::CursorIcon::Grab
    };
    let response = response.on_hover_cursor(cursor);
    let click = if response.double_clicked() {
        response
            .interact_pointer_pos()
            .map(|pointer| WaveformPlotClick::Primary {
                double_clicked: true,
                position: ((pointer.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0),
            })
    } else if response.clicked() {
        response
            .interact_pointer_pos()
            .map(|pointer| WaveformPlotClick::Primary {
                double_clicked: false,
                position: ((pointer.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0),
            })
    } else if response.secondary_clicked() {
        Some(WaveformPlotClick::Secondary)
    } else {
        None
    };
    if response.double_clicked() && options.reset_on_double_click && options.primary_pan_enabled {
        let previous = *viewport;
        viewport.reset();
        timebase_changed = *viewport != previous;
    }
    if options.primary_pan_enabled && response.dragged_by(egui::PointerButton::Primary) {
        let delta_x = ui.input(|input| input.pointer.delta().x);
        if viewport.pan_by_screen_delta(delta_x, rect.width()) {
            panned = true;
            ui.ctx().request_repaint();
        }
    }
    if response.hovered() {
        let scroll_delta = ui.input(|input| input.raw_scroll_delta.y);
        if scroll_delta != 0.0 {
            requested_timebase = options
                .timebase_basis
                .or_else(|| viewport.time_per_division(sample_count, sample_rate_hz))
                .and_then(|current| adjacent_timebase(current, scroll_delta > 0.0));
            let pointer_fraction = response
                .hover_pos()
                .map(|pointer| (pointer.x - rect.left()) / rect.width().max(1.0))
                .unwrap_or(0.5);
            timebase_changed = match options.zoom_anchor {
                WaveformZoomAnchor::Pointer => viewport.step_timebase(
                    scroll_delta,
                    sample_count,
                    sample_rate_hz,
                    pointer_fraction,
                ),
                WaveformZoomAnchor::Sample {
                    domain_fraction,
                    screen_fraction,
                } => viewport.step_timebase_at_sample(
                    scroll_delta,
                    sample_count,
                    sample_rate_hz,
                    domain_fraction,
                    screen_fraction,
                ),
            };
            ui.ctx().request_repaint();
        }
    }
    (panned, timebase_changed, click, requested_timebase)
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

const LOGIC_CHANNEL_MARKER_MIN_BODY_WIDTH: f32 = 16.0;
const LOGIC_CHANNEL_MARKER_TIP_WIDTH: f32 = 6.0;
const LOGIC_CHANNEL_MARKER_CHARACTER_WIDTH: f32 = 6.0;
const LOGIC_CHANNEL_MARKER_HORIZONTAL_PADDING: f32 = 8.0;
const LOGIC_SIGNAL_DEFAULT_STEPS_PER_LANE: usize = 4;

#[derive(Clone, Copy, Debug)]
struct LogicSignalLayout {
    base_height: f32,
    levels: [(f32, f32); LOGIC_CHANNEL_COUNT],
}

fn logic_signal_layout(
    plot_top: f32,
    plot_height: f32,
    maximum_heights: &[u8; LOGIC_CHANNEL_COUNT],
) -> LogicSignalLayout {
    let requested_height_steps = maximum_heights
        .iter()
        .map(|height| usize::from((*height).clamp(1, 5)))
        .sum::<usize>();
    let minimum_steps = LOGIC_CHANNEL_COUNT * LOGIC_SIGNAL_DEFAULT_STEPS_PER_LANE;
    let required_steps = requested_height_steps + LOGIC_CHANNEL_COUNT + 1;
    let total_steps = required_steps
        .max(minimum_steps)
        .div_ceil(LOGIC_CHANNEL_COUNT)
        * LOGIC_CHANNEL_COUNT;
    let lane_steps = total_steps / LOGIC_CHANNEL_COUNT;
    let base_height = plot_height / total_steps as f32;
    let mut step_levels = [(0_i32, 0_i32); LOGIC_CHANNEL_COUNT];

    for channel in 0..LOGIC_CHANNEL_COUNT {
        let maximum_height = i32::from(maximum_heights[channel].clamp(1, 5));
        let nominal_low = (channel * lane_steps + lane_steps - 1) as i32;
        let mut high = nominal_low - maximum_height;
        let mut low = nominal_low;
        let minimum_high = if channel == 0 {
            1
        } else {
            step_levels[channel - 1].1 + 1
        };
        if high < minimum_high {
            let movement = minimum_high - high;
            high += movement;
            low += movement;
        }
        step_levels[channel] = (high, low);
    }

    for channel in (0..LOGIC_CHANNEL_COUNT).rev() {
        let maximum_low = if channel + 1 == LOGIC_CHANNEL_COUNT {
            total_steps as i32 - 1
        } else {
            step_levels[channel + 1].0 - 1
        };
        let (mut high, mut low) = step_levels[channel];
        if low > maximum_low {
            let movement = low - maximum_low;
            high -= movement;
            low -= movement;
        }
        step_levels[channel] = (high, low);
    }

    debug_assert!(step_levels[0].0 >= 1);
    debug_assert!(step_levels[LOGIC_CHANNEL_COUNT - 1].1 < total_steps as i32);
    for channel in 1..LOGIC_CHANNEL_COUNT {
        debug_assert!(step_levels[channel - 1].1 < step_levels[channel].0);
    }

    LogicSignalLayout {
        base_height,
        levels: step_levels.map(|(high, low)| {
            (
                plot_top + high as f32 * base_height,
                plot_top + low as f32 * base_height,
            )
        }),
    }
}

fn logic_channel_marker_text(channel: usize, channel_name: &str) -> String {
    let channel_name = channel_name.trim();
    if channel_name.is_empty() {
        channel.to_string()
    } else {
        channel_name
            .chars()
            .take(LOGIC_CHANNEL_NAME_MAX_CHARS)
            .collect()
    }
}

fn logic_channel_marker_body_width(marker_text: &str) -> f32 {
    (marker_text.chars().count() as f32 * LOGIC_CHANNEL_MARKER_CHARACTER_WIDTH
        + LOGIC_CHANNEL_MARKER_HORIZONTAL_PADDING)
        .max(LOGIC_CHANNEL_MARKER_MIN_BODY_WIDTH)
}

fn logic_channel_marker_rect(plot: Rect, y: f32, lane_height: f32, marker_text: &str) -> Rect {
    let half_height = (lane_height * 0.22).clamp(3.5, 6.5);
    let body_width = logic_channel_marker_body_width(marker_text);
    Rect::from_min_max(
        Pos2::new(plot.left() + 1.0, y - half_height),
        Pos2::new(
            plot.left() + 1.0 + body_width + LOGIC_CHANNEL_MARKER_TIP_WIDTH,
            y + half_height,
        ),
    )
}

fn paint_logic_channel_marker(
    painter: &egui::Painter,
    plot: Rect,
    y: f32,
    lane_height: f32,
    marker_text: &str,
    color: Color32,
    opacity: u8,
) {
    let marker = logic_channel_marker_rect(plot, y, lane_height, marker_text);
    let body_width = logic_channel_marker_body_width(marker_text);
    let body_right = marker.left() + body_width;
    let marker_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), opacity);
    let text_color = marker_text_color(color);
    let text_color =
        Color32::from_rgba_unmultiplied(text_color.r(), text_color.g(), text_color.b(), opacity);
    let outline_color =
        Color32::from_rgba_unmultiplied(BACKGROUND.r(), BACKGROUND.g(), BACKGROUND.b(), opacity);
    painter
        .with_clip_rect(plot)
        .add(egui::Shape::convex_polygon(
            vec![
                marker.left_top(),
                Pos2::new(body_right, marker.top()),
                marker.right_center(),
                Pos2::new(body_right, marker.bottom()),
                marker.left_bottom(),
            ],
            marker_color,
            Stroke::new(1.0_f32, outline_color),
        ));
    painter.with_clip_rect(plot).text(
        Pos2::new(marker.left() + body_width * 0.5, marker.center().y),
        egui::Align2::CENTER_CENTER,
        marker_text,
        FontId::monospace((marker.height() * 0.65).clamp(7.0, 9.0)),
        text_color,
    );
}

const MAX_LOGIC_TRANSITIONS_PER_PIXEL: usize = 4;

#[derive(Clone, Copy, Debug)]
struct LogicTransitionBucket {
    pixel: i32,
    start_high: bool,
    end_high: bool,
    transition_count: usize,
    transition_x: [f32; MAX_LOGIC_TRANSITIONS_PER_PIXEL],
}

impl Default for LogicTransitionBucket {
    fn default() -> Self {
        Self {
            pixel: -1,
            start_high: false,
            end_high: false,
            transition_count: 0,
            transition_x: [0.0; MAX_LOGIC_TRANSITIONS_PER_PIXEL],
        }
    }
}

impl LogicTransitionBucket {
    fn begin(&mut self, pixel: i32, start_high: bool) {
        self.pixel = pixel;
        self.start_high = start_high;
        self.end_high = start_high;
        self.transition_count = 0;
    }

    fn record(&mut self, x: f32, end_high: bool) {
        if self.transition_count < MAX_LOGIC_TRANSITIONS_PER_PIXEL {
            self.transition_x[self.transition_count] = x;
        }
        self.transition_count += 1;
        self.end_high = end_high;
    }

    fn flush(&mut self, points: &mut Vec<Pos2>, plot: Rect, high: f32, low: f32) {
        if self.transition_count == 0 {
            return;
        }
        let y = |is_high| if is_high { high } else { low };
        if self.transition_count <= MAX_LOGIC_TRANSITIONS_PER_PIXEL {
            let mut is_high = self.start_high;
            for x in self
                .transition_x
                .iter()
                .copied()
                .take(self.transition_count)
            {
                points.push(Pos2::new(x, y(is_high)));
                is_high = !is_high;
                points.push(Pos2::new(x, y(is_high)));
            }
        } else {
            // Once several edges occupy the same screen pixel, their exact
            // spacing is no longer visible. Draw an activity stroke instead
            // of dropping the entire bucket and aliasing the signal.
            let x = (plot.left() + self.pixel as f32 + 0.5).clamp(plot.left(), plot.right());
            points.push(Pos2::new(x, y(self.start_high)));
            points.push(Pos2::new(x, y(!self.start_high)));
            if self.end_high == self.start_high {
                points.push(Pos2::new(x, y(self.end_high)));
            }
        }
        self.transition_count = 0;
        self.pixel = -1;
    }
}

#[derive(Clone, Copy)]
struct LogicSampleView<'a> {
    samples: &'a VecDeque<LogicSampleBucket>,
    sample_count: usize,
    sample_stride: usize,
}

fn logic_waveform_points(
    sample_view: LogicSampleView<'_>,
    visible_range: Range<usize>,
    plot: Rect,
    viewport: WaveformViewport,
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
) -> [Vec<Pos2>; LOGIC_CHANNEL_COUNT] {
    let LogicSampleView {
        samples,
        sample_count,
        sample_stride,
    } = sample_view;
    let mut points: [Vec<Pos2>; LOGIC_CHANNEL_COUNT] = std::array::from_fn(|_| Vec::new());
    let Some(first_index) = visible_range.clone().next() else {
        return points;
    };
    let first_bucket_index = first_index / sample_stride;
    let end_bucket_index = ((visible_range.end - 1) / sample_stride + 1).min(samples.len());
    let Some(first_bucket) = samples.get(first_bucket_index) else {
        return points;
    };
    let mut previous = first_bucket.first;
    let first_x = if viewport.start >= 0.0 {
        plot.left()
    } else {
        waveform_sample_x(plot, viewport, first_index as f64, sample_count)
    };
    for channel in 0..LOGIC_CHANNEL_COUNT {
        if enabled_channels[channel] {
            let (high, low) = levels[channel];
            let y = if previous & (1_u32 << channel) != 0 {
                high
            } else {
                low
            };
            points[channel].push(Pos2::new(first_x, y));
        }
    }

    let enabled_mask = enabled_channels
        .iter()
        .enumerate()
        .fold(0_u32, |mask, (channel, enabled)| {
            mask | (u32::from(*enabled) << channel)
        });
    let mut buckets = [LogicTransitionBucket::default(); LOGIC_CHANNEL_COUNT];
    for (bucket_index, bucket) in samples
        .iter()
        .copied()
        .enumerate()
        .take(end_bucket_index)
        .skip(first_bucket_index)
    {
        let bucket_start = bucket_index.saturating_mul(sample_stride);

        let mut boundary_changes = (previous ^ bucket.first) & enabled_mask;
        if boundary_changes != 0 {
            let x = waveform_sample_x(plot, viewport, bucket_start as f64, sample_count)
                .clamp(plot.left(), plot.right());
            let pixel = (x - plot.left()).floor() as i32;
            while boundary_changes != 0 {
                let channel = boundary_changes.trailing_zeros() as usize;
                boundary_changes &= boundary_changes - 1;
                let transition_bucket = &mut buckets[channel];
                if transition_bucket.pixel != pixel {
                    let (high, low) = levels[channel];
                    transition_bucket.flush(&mut points[channel], plot, high, low);
                    transition_bucket.begin(pixel, previous & (1_u32 << channel) != 0);
                }
                transition_bucket.record(x, bucket.first & (1_u32 << channel) != 0);
            }
        }

        let mut activity = bucket.activity & enabled_mask;
        if activity != 0 {
            let activity_index = bucket_start
                .saturating_add(bucket.sample_count as usize / 2)
                .min(sample_count.saturating_sub(1));
            let x = waveform_sample_x(plot, viewport, activity_index as f64, sample_count)
                .clamp(plot.left(), plot.right());
            let pixel = (x - plot.left()).floor() as i32;
            while activity != 0 {
                let channel = activity.trailing_zeros() as usize;
                activity &= activity - 1;
                let mask = 1_u32 << channel;
                let starts_high = bucket.first & mask != 0;
                let ends_high = bucket.last & mask != 0;
                let transition_bucket = &mut buckets[channel];
                if transition_bucket.pixel != pixel {
                    let (high, low) = levels[channel];
                    transition_bucket.flush(&mut points[channel], plot, high, low);
                    transition_bucket.begin(pixel, starts_high);
                }
                if starts_high == ends_high {
                    transition_bucket.record(x, !starts_high);
                }
                transition_bucket.record(x, ends_high);
            }
        }
        previous = bucket.last;
    }

    let last_index = visible_range.end - 1;
    let last_x = if viewport.start + viewport.span <= 1.0 {
        plot.right()
    } else {
        waveform_sample_x(plot, viewport, last_index as f64, sample_count)
    };
    for channel in 0..LOGIC_CHANNEL_COUNT {
        if !enabled_channels[channel] {
            continue;
        }
        let (high, low) = levels[channel];
        buckets[channel].flush(&mut points[channel], plot, high, low);
        let y = if previous & (1_u32 << channel) != 0 {
            high
        } else {
            low
        };
        points[channel].push(Pos2::new(last_x, y));
    }
    points
}

fn logic_marker_color(index: usize) -> Color32 {
    scope_marker_color(index % 2, index / 2)
}

fn logic_sample_at_x(
    plot: Rect,
    viewport: WaveformViewport,
    sample_offset: u64,
    sample_count: usize,
    x: f32,
) -> Option<u64> {
    if sample_count == 0 {
        return None;
    }
    let screen_fraction = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    let domain_fraction = (f64::from(viewport.start)
        + f64::from(screen_fraction) * f64::from(viewport.span))
    .clamp(0.0, 1.0);
    let local_sample = (domain_fraction * sample_count.saturating_sub(1) as f64).round() as u64;
    Some(sample_offset.saturating_add(local_sample))
}

fn logic_marker_x(
    plot: Rect,
    viewport: WaveformViewport,
    sample_offset: u64,
    sample_count: usize,
    sample: u64,
) -> Option<f32> {
    let last_sample = sample_offset.saturating_add(sample_count.saturating_sub(1) as u64);
    if sample_count == 0 || sample < sample_offset || sample > last_sample {
        return None;
    }
    Some(waveform_sample_x(
        plot,
        viewport,
        sample.saturating_sub(sample_offset) as f64,
        sample_count,
    ))
}

fn logic_channel_at_y(
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
    pointer_y: f32,
) -> Option<usize> {
    levels
        .iter()
        .enumerate()
        .filter(|(channel, _)| enabled_channels[*channel])
        .min_by(|(_, left), (_, right)| {
            let left_distance = (pointer_y - (left.0 + left.1) * 0.5).abs();
            let right_distance = (pointer_y - (right.0 + right.1) * 0.5).abs();
            left_distance.total_cmp(&right_distance)
        })
        .map(|(channel, _)| channel)
}

fn nearest_logic_transition_sample(
    waveforms: &WaveformStore,
    channel: usize,
    target_sample: u64,
    search_radius: u64,
) -> Option<u64> {
    if channel >= LOGIC_CHANNEL_COUNT
        || waveforms.logic_samples.is_empty()
        || waveforms.logic_sample_stride == 0
    {
        return None;
    }
    let local_target = target_sample.saturating_sub(waveforms.logic_sample_offset);
    let stride = waveforms.logic_sample_stride as u64;
    let target_bucket = local_target / stride;
    let bucket_radius = search_radius.div_ceil(stride).saturating_add(2);
    let first_bucket = target_bucket
        .saturating_sub(bucket_radius)
        .min(waveforms.logic_samples.len() as u64) as usize;
    let end_bucket = target_bucket
        .saturating_add(bucket_radius)
        .saturating_add(1)
        .min(waveforms.logic_samples.len() as u64) as usize;
    let channel_mask = 1_u32 << channel;
    let mut closest = None;

    for bucket_index in first_bucket..end_bucket {
        let bucket = waveforms.logic_samples[bucket_index];
        let bucket_start = (bucket_index as u64).saturating_mul(stride);
        if bucket_index > 0 {
            let previous = waveforms.logic_samples[bucket_index - 1];
            if (previous.last ^ bucket.first) & channel_mask != 0 {
                let candidate = waveforms.logic_sample_offset.saturating_add(bucket_start);
                let distance = candidate.abs_diff(target_sample);
                if distance <= search_radius
                    && closest.map_or(true, |(_, closest_distance)| distance < closest_distance)
                {
                    closest = Some((candidate, distance));
                }
            }
        }
        if bucket.activity & channel_mask != 0 {
            let activity_sample = bucket_start
                .saturating_add(u64::from(bucket.sample_count) / 2)
                .min(waveforms.logic_sample_count.saturating_sub(1) as u64);
            let candidate = waveforms
                .logic_sample_offset
                .saturating_add(activity_sample);
            let distance = candidate.abs_diff(target_sample);
            if distance <= search_radius
                && closest.map_or(true, |(_, closest_distance)| distance < closest_distance)
            {
                closest = Some((candidate, distance));
            }
        }
    }
    closest.map(|(sample, _)| sample)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LogicSmartTimingMeasurement {
    channel: usize,
    edge_start_sample: u64,
    edge_end_sample: u64,
    cycle: Option<LogicTimingMeasurement>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LogicTransition {
    sample: u64,
    new_state_high: bool,
}

fn exact_logic_bucket_state(bucket: LogicSampleBucket, mask: u32) -> Option<bool> {
    (bucket.activity & mask == 0).then_some(bucket.first & mask != 0)
}

fn logic_transition_within_bucket(
    waveforms: &WaveformStore,
    mask: u32,
    bucket_index: usize,
) -> Option<LogicTransition> {
    let bucket = waveforms.logic_samples[bucket_index];
    if bucket.activity & mask == 0 || bucket.multiple_transitions & mask != 0 {
        return None;
    }
    let bucket_start = (bucket_index as u64).saturating_mul(waveforms.logic_sample_stride as u64);
    Some(LogicTransition {
        sample: waveforms
            .logic_sample_offset
            .saturating_add(bucket_start)
            .saturating_add(u64::from(bucket.sample_count) / 2),
        new_state_high: bucket.last & mask != 0,
    })
}

fn logic_transition_before_bucket(
    waveforms: &WaveformStore,
    mask: u32,
    before_bucket: usize,
) -> Option<LogicTransition> {
    for right_index in (1..=before_bucket).rev() {
        let left = waveforms.logic_samples[right_index - 1];
        let right = waveforms.logic_samples[right_index];
        if (left.last ^ right.first) & mask != 0 {
            return Some(LogicTransition {
                sample: waveforms.logic_sample_offset.saturating_add(
                    (right_index as u64).saturating_mul(waveforms.logic_sample_stride as u64),
                ),
                new_state_high: right.first & mask != 0,
            });
        }
        if left.activity & mask != 0 {
            return logic_transition_within_bucket(waveforms, mask, right_index - 1);
        }
    }
    None
}

fn logic_transition_after_bucket(
    waveforms: &WaveformStore,
    mask: u32,
    after_bucket: usize,
) -> Option<LogicTransition> {
    for right_index in after_bucket.saturating_add(1)..waveforms.logic_samples.len() {
        let left = waveforms.logic_samples[right_index - 1];
        let right = waveforms.logic_samples[right_index];
        if (left.last ^ right.first) & mask != 0 {
            return Some(LogicTransition {
                sample: waveforms.logic_sample_offset.saturating_add(
                    (right_index as u64).saturating_mul(waveforms.logic_sample_stride as u64),
                ),
                new_state_high: right.first & mask != 0,
            });
        }
        if right.activity & mask != 0 {
            return logic_transition_within_bucket(waveforms, mask, right_index);
        }
    }
    None
}

fn logic_smart_timing_measurement(
    waveforms: &WaveformStore,
    channel: usize,
    anchor_sample: u64,
) -> Option<LogicSmartTimingMeasurement> {
    if channel >= LOGIC_CHANNEL_COUNT
        || waveforms.logic_rate_hz == 0
        || waveforms.logic_sample_stride == 0
        || waveforms.logic_samples.is_empty()
    {
        return None;
    }
    let local_sample = anchor_sample.saturating_sub(waveforms.logic_sample_offset);
    let bucket_index = usize::try_from(local_sample / waveforms.logic_sample_stride as u64)
        .ok()?
        .min(waveforms.logic_samples.len().saturating_sub(1));
    let mask = 1_u32 << channel;
    let state_high = exact_logic_bucket_state(waveforms.logic_samples[bucket_index], mask)?;
    let previous = logic_transition_before_bucket(waveforms, mask, bucket_index)?;
    let next = logic_transition_after_bucket(waveforms, mask, bucket_index)?;
    if previous.new_state_high != state_high || next.new_state_high == state_high {
        return None;
    }

    let cycle = if state_high {
        logic_transition_after_bucket(
            waveforms,
            mask,
            usize::try_from(
                next.sample.saturating_sub(waveforms.logic_sample_offset)
                    / waveforms.logic_sample_stride as u64,
            )
            .ok()?,
        )
        .filter(|transition| transition.new_state_high)
        .map(|next_rising| LogicTimingMeasurement {
            channel,
            period_start_sample: previous.sample,
            period_end_sample: next_rising.sample,
            period_samples: next_rising.sample.saturating_sub(previous.sample),
            high_samples: next.sample.saturating_sub(previous.sample),
        })
    } else {
        let falling_bucket = usize::try_from(
            previous
                .sample
                .saturating_sub(waveforms.logic_sample_offset)
                / waveforms.logic_sample_stride as u64,
        )
        .ok()?;
        logic_transition_before_bucket(waveforms, mask, falling_bucket.saturating_sub(1))
            .filter(|transition| transition.new_state_high)
            .map(|previous_rising| LogicTimingMeasurement {
                channel,
                period_start_sample: previous_rising.sample,
                period_end_sample: next.sample,
                period_samples: next.sample.saturating_sub(previous_rising.sample),
                high_samples: previous.sample.saturating_sub(previous_rising.sample),
            })
    }
    .filter(|timing| {
        timing.period_samples > 0
            && timing.high_samples > 0
            && timing.high_samples < timing.period_samples
    });

    Some(LogicSmartTimingMeasurement {
        channel,
        edge_start_sample: previous.sample,
        edge_end_sample: next.sample,
        cycle,
    })
}

fn logic_marker_sample_at_pointer(
    plot: Rect,
    viewport: WaveformViewport,
    waveforms: &WaveformStore,
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
    pointer: Pos2,
) -> Option<u64> {
    let sample = logic_sample_at_x(
        plot,
        viewport,
        waveforms.logic_sample_offset,
        waveforms.logic_sample_count,
        pointer.x.clamp(plot.left(), plot.right()),
    )?;
    let visible_samples_per_pixel = viewport.span
        * waveforms.logic_sample_count.saturating_sub(1) as f32
        / plot.width().max(1.0);
    let search_radius = (visible_samples_per_pixel * LOGIC_MARKER_EDGE_SNAP_DISTANCE).ceil() as u64;
    let Some(channel) = logic_channel_at_y(levels, enabled_channels, pointer.y) else {
        return Some(sample);
    };
    let Some(snapped) = nearest_logic_transition_sample(waveforms, channel, sample, search_radius)
    else {
        return Some(sample);
    };
    let snapped_x = logic_marker_x(
        plot,
        viewport,
        waveforms.logic_sample_offset,
        waveforms.logic_sample_count,
        snapped,
    )?;
    if (snapped_x - pointer.x).abs() <= LOGIC_MARKER_EDGE_SNAP_DISTANCE {
        Some(snapped)
    } else {
        Some(sample)
    }
}

fn pan_logic_marker_viewport_at_edge(
    viewport: &mut WaveformViewport,
    plot: Rect,
    pointer_x: f32,
) -> bool {
    const EDGE_WIDTH: f32 = 32.0;
    const PAN_STEP: f32 = 0.02;

    let edge_width = EDGE_WIDTH.min(plot.width() * 0.25).max(1.0);
    let direction = if pointer_x < plot.left() + edge_width {
        -((plot.left() + edge_width - pointer_x) / edge_width).clamp(0.0, 1.0)
    } else if pointer_x > plot.right() - edge_width {
        ((pointer_x - (plot.right() - edge_width)) / edge_width).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if direction == 0.0 {
        return false;
    }

    let previous_start = viewport.start;
    viewport.start = viewport.clamp_start(viewport.start + direction * viewport.span * PAN_STEP);
    if (viewport.start - previous_start).abs() <= f32::EPSILON {
        return false;
    }
    viewport.anchor_locked = false;
    true
}

fn update_logic_marker_gesture(
    ui: &egui::Ui,
    plot: Rect,
    waveforms: &WaveformStore,
    viewport: &mut WaveformViewport,
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
    settings: &mut LogicMarkerSettings,
) {
    if !settings.adding
        || settings.markers.len() >= LOGIC_MARKER_LIMIT
        || waveforms.logic_sample_count < 2
        || waveforms.logic_rate_hz == 0
    {
        settings.draft = None;
        return;
    }

    let (pressed, down, released, pointer_position) = ui.input(|input| {
        (
            input.pointer.button_pressed(egui::PointerButton::Primary),
            input.pointer.button_down(egui::PointerButton::Primary),
            input.pointer.button_released(egui::PointerButton::Primary),
            input.pointer.interact_pos(),
        )
    });
    if pointer_position.is_some_and(|pointer| plot.contains(pointer)) || settings.draft.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if pressed {
        if let Some(pointer) = pointer_position.filter(|pointer| plot.contains(*pointer)) {
            if let Some(sample) = logic_marker_sample_at_pointer(
                plot,
                *viewport,
                waveforms,
                levels,
                enabled_channels,
                pointer,
            ) {
                settings.start(sample);
            }
        }
    }
    if settings.draft.is_some() && (down || released) {
        if let Some(pointer) = pointer_position {
            if down && pan_logic_marker_viewport_at_edge(viewport, plot, pointer.x) {
                ui.ctx().request_repaint();
            }
            if let Some(sample) = logic_marker_sample_at_pointer(
                plot,
                *viewport,
                waveforms,
                levels,
                enabled_channels,
                pointer,
            ) {
                settings.update(sample);
            }
        }
    }
    if released && settings.draft.is_some() {
        settings.finish(waveforms.logic_rate_hz);
    }
}

fn paint_logic_marker_span(
    painter: &egui::Painter,
    plot: Rect,
    first_x: f32,
    second_x: f32,
    label: &str,
    color: Color32,
) {
    let left = first_x.min(second_x).clamp(plot.left(), plot.right());
    let right = first_x.max(second_x).clamp(plot.left(), plot.right());
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24);
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(left, plot.top()), Pos2::new(right, plot.bottom())),
        0.0,
        fill,
    );
    for x in [left, right] {
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.5_f32, color),
        );
    }
    painter.line_segment(
        [
            Pos2::new(left, plot.top() + 3.0),
            Pos2::new(right, plot.top() + 3.0),
        ],
        Stroke::new(1.5_f32, color),
    );
    painter.text(
        Pos2::new((left + right) * 0.5, plot.top() + 6.0),
        egui::Align2::CENTER_TOP,
        label,
        FontId::monospace(10.0),
        color,
    );
}

fn paint_logic_markers(
    ui: &egui::Ui,
    plot: Rect,
    waveforms: &WaveformStore,
    viewport: WaveformViewport,
    settings: &mut LogicMarkerSettings,
) {
    let last_sample = waveforms
        .logic_sample_offset
        .saturating_add(waveforms.logic_sample_count.saturating_sub(1) as u64);
    for (index, marker) in settings.markers.iter().enumerate() {
        if marker.end_sample < waveforms.logic_sample_offset || marker.start_sample > last_sample {
            continue;
        }
        let start = marker
            .start_sample
            .clamp(waveforms.logic_sample_offset, last_sample);
        let end = marker
            .end_sample
            .clamp(waveforms.logic_sample_offset, last_sample);
        let (Some(start_x), Some(end_x)) = (
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                start,
            ),
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                end,
            ),
        ) else {
            continue;
        };
        let marker_label = if marker.name.trim().is_empty() {
            format!("M{}", index + 1)
        } else {
            marker.name.trim().to_owned()
        };
        paint_logic_marker_span(
            ui.painter(),
            plot,
            start_x,
            end_x,
            &format!("{marker_label}  {}", format_duration(marker.duration())),
            marker.color,
        );
    }

    if let Some(draft) = settings.draft {
        if let (Some(start_x), Some(end_x)) = (
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                draft.start_sample,
            ),
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                draft.end_sample,
            ),
        ) {
            let duration = LogicMarker::new(
                draft.start_sample,
                draft.end_sample,
                waveforms.logic_rate_hz,
                ACCENT,
            )
            .map(|marker| format_duration(marker.duration()))
            .unwrap_or_else(|| "0.0 ns".to_owned());
            paint_logic_marker_span(
                ui.painter(),
                plot,
                start_x,
                end_x,
                &format!("New  {duration}"),
                ACCENT,
            );
        }
    }
}

fn paint_logic_timing_hint(
    painter: &egui::Painter,
    plot: Rect,
    waveforms: &WaveformStore,
    viewport: WaveformViewport,
    measurement: LogicSmartTimingMeasurement,
    color: Color32,
) {
    if plot.width() < 120.0 || plot.height() < 80.0 {
        return;
    }
    let painter = painter.with_clip_rect(plot);
    let channel = measurement.channel;

    if let Some(timing) = measurement.cycle {
        if let (Some(start_x), Some(end_x)) = (
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                timing.period_start_sample,
            ),
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                timing.period_end_sample,
            ),
        ) {
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(start_x.min(end_x), plot.top()),
                    Pos2::new(start_x.max(end_x), plot.bottom()),
                ),
                0.0,
                color.gamma_multiply(0.08),
            );
        }
    }
    for sample in [measurement.edge_start_sample, measurement.edge_end_sample] {
        if let Some(x) = logic_marker_x(
            plot,
            viewport,
            waveforms.logic_sample_offset,
            waveforms.logic_sample_count,
            sample,
        ) {
            painter.line_segment(
                [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                Stroke::new(0.75_f32, color.gamma_multiply(0.3)),
            );
        }
    }
    if let Some(timing) = measurement.cycle {
        for sample in [timing.period_start_sample, timing.period_end_sample] {
            if let Some(x) = logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                sample,
            ) {
                painter.line_segment(
                    [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                    Stroke::new(0.75_f32, color.gamma_multiply(0.5)),
                );
                painter.circle_filled(
                    Pos2::new(x, plot.top() + 3.0),
                    2.0,
                    color.gamma_multiply(0.5),
                );
            }
        }
    }

    let width = 205.0_f32.min(plot.width() - 16.0);
    let height = if measurement.cycle.is_some() {
        72.0
    } else {
        48.0
    };
    let hint = Rect::from_min_size(
        Pos2::new(plot.right() - width - 8.0, plot.top() + 26.0),
        Vec2::new(width, height),
    );
    painter.rect_filled(hint, 5.0, PANEL.gamma_multiply(0.88));
    let border = Stroke::new(0.75_f32, color.gamma_multiply(0.65));
    painter.line_segment([hint.left_top(), hint.right_top()], border);
    painter.line_segment([hint.right_top(), hint.right_bottom()], border);
    painter.line_segment([hint.right_bottom(), hint.left_bottom()], border);
    painter.line_segment([hint.left_bottom(), hint.left_top()], border);

    let left = hint.left() + 8.0;
    painter.text(
        Pos2::new(left, hint.top() + 7.0),
        egui::Align2::LEFT_TOP,
        format!("D{channel}  SMART TIMING"),
        FontId::monospace(10.0),
        color,
    );
    painter.text(
        Pos2::new(left, hint.top() + 23.0),
        egui::Align2::LEFT_TOP,
        format!(
            "EDGE-EDGE  {}",
            format_duration(Duration::from_secs_f64(
                measurement
                    .edge_end_sample
                    .saturating_sub(measurement.edge_start_sample) as f64
                    / waveforms.logic_rate_hz as f64,
            ))
        ),
        FontId::monospace(10.0),
        Color32::WHITE,
    );
    if let Some(timing) = measurement.cycle {
        painter.text(
            Pos2::new(left, hint.top() + 39.0),
            egui::Align2::LEFT_TOP,
            format!(
                "RISE-RISE  {}",
                format_duration(timing.period(waveforms.logic_rate_hz))
            ),
            FontId::monospace(10.0),
            Color32::WHITE,
        );
        painter.text(
            Pos2::new(left, hint.top() + 55.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{}   DUTY {:.1}%",
                format_frequency(timing.frequency_hz(waveforms.logic_rate_hz)),
                timing.duty_cycle_percent()
            ),
            FontId::monospace(10.0),
            ACCENT,
        );
    }
}

const LOGIC_PROTOCOL_LANE_HEIGHT: f32 = 30.0;
const LOGIC_PROTOCOL_LABEL_HORIZONTAL_PADDING: f32 = 6.0;
const LOGIC_PROTOCOL_LABEL_FONT_SIZE: f32 = 9.0;
const LOGIC_PROTOCOL_VALUE_FONT_SIZE: f32 = 13.0;

fn protocol_data_channel(analyzer: &ProtocolAnalyzerInstance) -> usize {
    let base_id = protocol_stack(analyzer.protocol_id)
        .and_then(|stack| stack.first().map(|protocol| protocol.id));
    let preferred_mapping = match base_id {
        Some("0:i2c" | "1:i2c") => analyzer.channel_mappings.get(1),
        Some("0:spi" | "1:spi") => analyzer
            .channel_mappings
            .get(2)
            .filter(|mapping| mapping.is_some())
            .or_else(|| analyzer.channel_mappings.get(1)),
        _ => None,
    };
    preferred_mapping
        .copied()
        .flatten()
        .or_else(|| {
            analyzer
                .channel_mappings
                .iter()
                .rev()
                .flatten()
                .copied()
                .next()
        })
        .unwrap_or(0)
        .min(LOGIC_CHANNEL_COUNT - 1)
}

fn logic_protocol_lane_layout(
    plot: Rect,
    maximum_heights: &[u8; LOGIC_CHANNEL_COUNT],
    analyzers: &[ProtocolAnalyzerInstance],
) -> (LogicSignalLayout, Vec<Rect>) {
    if analyzers.is_empty() {
        return (
            logic_signal_layout(plot.top(), plot.height(), maximum_heights),
            Vec::new(),
        );
    }
    let available_height = (plot.height() - 80.0).max(0.0);
    let total_height = (analyzers.len() as f32 * LOGIC_PROTOCOL_LANE_HEIGHT)
        .min(available_height)
        .min(plot.height() * 0.4);
    if total_height <= 0.0 {
        return (
            logic_signal_layout(plot.top(), plot.height(), maximum_heights),
            Vec::new(),
        );
    }
    let lane_height = total_height / analyzers.len() as f32;
    let mut signal_layout =
        logic_signal_layout(plot.top(), plot.height() - total_height, maximum_heights);
    let base_levels = signal_layout.levels;
    let targets = analyzers
        .iter()
        .map(protocol_data_channel)
        .collect::<Vec<_>>();
    let mut insertion_order = targets.iter().copied().enumerate().collect::<Vec<_>>();
    insertion_order.sort_by_key(|(analyzer_index, target)| (*target, *analyzer_index));
    let mut lanes = vec![plot; analyzers.len()];
    for (insertion_index, (analyzer_index, target)) in insertion_order.iter().copied().enumerate() {
        let base_y = if target == 0 {
            plot.top()
        } else {
            (base_levels[target - 1].1 + base_levels[target].0) * 0.5
        };
        let top = base_y + insertion_index as f32 * lane_height;
        lanes[analyzer_index] = Rect::from_min_max(
            Pos2::new(plot.left(), top),
            Pos2::new(plot.right(), top + lane_height),
        );
    }
    for (channel, (high, low)) in signal_layout.levels.iter_mut().enumerate() {
        let preceding_lanes = targets.iter().filter(|target| **target <= channel).count();
        let offset = preceding_lanes as f32 * lane_height;
        *high += offset;
        *low += offset;
    }
    (signal_layout, lanes)
}

fn paint_logic_protocol_lanes(
    ui: &egui::Ui,
    lanes: &[Rect],
    waveforms: &WaveformStore,
    viewport: WaveformViewport,
    settings: &ProtocolAnalyzerSettings,
    cache: &mut ProtocolDecodeCache,
    streamed: Option<&ProtocolDecodeSnapshot>,
) {
    if lanes.is_empty() {
        return;
    }
    let outcomes = if let Some(streamed) =
        streamed.filter(|snapshot| snapshot.matches_analyzers(&settings.analyzers))
    {
        streamed.waveform_outcomes()
    } else {
        cache.waveform_outcomes(waveforms, &settings.analyzers, None)
    };
    for (lane_index, ((lane, analyzer), outcome)) in lanes
        .iter()
        .zip(&settings.analyzers)
        .zip(outcomes)
        .enumerate()
    {
        let color = logic_marker_color(lane_index);
        let painter = ui.painter().with_clip_rect(*lane);
        let label = analyzer_transport_name(analyzer);
        let label_galley = painter.layout_no_wrap(
            label.to_owned(),
            FontId::monospace(LOGIC_PROTOCOL_LABEL_FONT_SIZE),
            color,
        );
        let label_width = (label_galley.size().x + LOGIC_PROTOCOL_LABEL_HORIZONTAL_PADDING * 2.0)
            .min(lane.width());
        painter.rect_filled(*lane, 0.0, PANEL.gamma_multiply(0.72));
        painter.line_segment(
            [lane.left_top(), lane.right_top()],
            Stroke::new(1.0_f32, GRID),
        );

        match outcome {
            ProtocolDecodeOutcome::Events(events) => {
                for (event_index, event) in events.iter().enumerate() {
                    let (Some(start_x), Some(end_x)) = (
                        logic_marker_x(
                            *lane,
                            viewport,
                            waveforms.logic_sample_offset,
                            waveforms.logic_sample_count,
                            event.start_sample,
                        ),
                        logic_marker_x(
                            *lane,
                            viewport,
                            waveforms.logic_sample_offset,
                            waveforms.logic_sample_count,
                            event.end_sample,
                        ),
                    ) else {
                        continue;
                    };
                    if end_x < lane.left() || start_x > lane.right() {
                        continue;
                    }
                    let event_rect = Rect::from_min_max(
                        Pos2::new(start_x.max(lane.left()), lane.top() + 3.0),
                        Pos2::new(
                            end_x.max(start_x + 1.0).min(lane.right()),
                            lane.bottom() - 3.0,
                        ),
                    );
                    let event_color = if event.warning {
                        Color32::LIGHT_RED
                    } else {
                        color
                    };
                    painter.rect_filled(event_rect, 3.0, event_color.gamma_multiply(0.22));
                    painter.rect_stroke(event_rect, 3.0, Stroke::new(0.8_f32, event_color));
                    let value_galley = painter.layout_no_wrap(
                        event.text.clone(),
                        FontId::monospace(LOGIC_PROTOCOL_VALUE_FONT_SIZE),
                        Color32::WHITE,
                    );
                    if event_rect.width() >= value_galley.size().x + 8.0 {
                        let text_origin = event_rect.center() - value_galley.size() * 0.5;
                        painter.galley(text_origin, value_galley, Color32::WHITE);
                    }
                    ui.interact(
                        event_rect.expand(2.0),
                        ui.make_persistent_id((
                            "protocol_decoded_event",
                            analyzer.instance_id,
                            event_index,
                        )),
                        Sense::hover(),
                    )
                    .on_hover_text(format!(
                        "{}\nSamples {}–{}",
                        event.detail, event.start_sample, event.end_sample
                    ));
                }
                if events.is_empty() {
                    painter.text(
                        Pos2::new(lane.left() + label_width + 8.0, lane.center().y),
                        egui::Align2::LEFT_CENTER,
                        if waveforms.logic_sample_count == 0 {
                            "Waiting for logic samples"
                        } else {
                            "No decoded bytes"
                        },
                        FontId::monospace(9.0),
                        TEXT_MUTED,
                    );
                }
            }
            ProtocolDecodeOutcome::Unavailable(message) => {
                painter.text(
                    Pos2::new(lane.left() + label_width + 8.0, lane.center().y),
                    egui::Align2::LEFT_CENTER,
                    message,
                    FontId::monospace(9.0),
                    TEXT_MUTED,
                );
            }
        }

        let label_rect = Rect::from_min_max(
            lane.min,
            Pos2::new((lane.left() + label_width).min(lane.right()), lane.bottom()),
        );
        painter.line_segment(
            [label_rect.right_top(), label_rect.right_bottom()],
            Stroke::new(1.0_f32, color.gamma_multiply(0.65)),
        );
        let label_origin = Pos2::new(
            label_rect.left() + LOGIC_PROTOCOL_LABEL_HORIZONTAL_PADDING,
            label_rect.center().y - label_galley.size().y * 0.5,
        );
        painter.galley(label_origin, label_galley, color);
    }
}

fn paint_logic(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    options: LogicWaveformOptions<'_>,
    viewport: &mut WaveformViewport,
) {
    let LogicWaveformOptions {
        channel_colors,
        channel_marker_opacities,
        channel_names,
        channel_settings_drafts,
        smart_timing_enabled,
        marker_settings,
        maximum_heights,
        enabled_channels,
        protocol_analyzer,
        protocol_decode_cache,
        protocol_stream,
        time_axis,
    } = options;
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    let waveform_area = rect;
    let (status_area, ruler_area, plot_area) = split_waveform_chrome(waveform_area);
    let plot = plot_area;
    let (status_bar, time_ruler) = align_waveform_chrome(status_area, ruler_area, plot);
    paint_plot_grid(ui, plot);
    let sample_count = waveforms.logic_sample_count;
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
    let (signal_layout, protocol_lanes) =
        logic_protocol_lane_layout(plot, maximum_heights, &protocol_analyzer.analyzers);
    let signal_plot = plot;
    let _ = update_waveform_viewport(
        ui,
        plot,
        "logic_waveform_zoom",
        viewport,
        display_sample_count,
        display_sample_rate_hz,
        WaveformViewportOptions {
            reset_on_double_click: true,
            primary_pan_enabled: !marker_settings.adding,
            zoom_anchor: WaveformZoomAnchor::Pointer,
            timebase_basis: None,
        },
    );
    update_logic_marker_gesture(
        ui,
        signal_plot,
        waveforms,
        viewport,
        &signal_layout.levels,
        enabled_channels,
        marker_settings,
    );
    for channel in 0..LOGIC_CHANNEL_COUNT {
        let lane_top = if channel == 0 {
            signal_plot.top()
        } else {
            (signal_layout.levels[channel - 1].1 + signal_layout.levels[channel].0) * 0.5
        };
        painter.line_segment(
            [
                Pos2::new(signal_plot.left(), lane_top),
                Pos2::new(signal_plot.right(), lane_top),
            ],
            Stroke::new(0.6_f32, GRID.gamma_multiply(0.45)),
        );
    }
    let visible_range = viewport.sample_range(sample_count);
    let visible_count = visible_range.len();
    let mut waveform_points = logic_waveform_points(
        LogicSampleView {
            samples: &waveforms.logic_samples,
            sample_count,
            sample_stride: waveforms.logic_sample_stride,
        },
        visible_range.clone(),
        signal_plot,
        *viewport,
        &signal_layout.levels,
        enabled_channels,
    );
    let marker_lane_height = signal_layout.base_height * LOGIC_SIGNAL_DEFAULT_STEPS_PER_LANE as f32;
    for channel in 0..LOGIC_CHANNEL_COUNT {
        if !enabled_channels[channel] {
            continue;
        }
        let channel_color = &mut channel_colors[channel];
        let channel_marker_opacity = &mut channel_marker_opacities[channel];
        let channel_name = &mut channel_names[channel];
        let channel_label = channel_label("D", channel, channel_name);
        let marker_text = logic_channel_marker_text(channel, channel_name);
        let (_, low) = signal_layout.levels[channel];
        paint_logic_zero_reference(painter, signal_plot, low, *channel_color);
        let marker_rect =
            logic_channel_marker_rect(signal_plot, low, marker_lane_height, &marker_text);
        let marker_response = ui
            .interact(
                marker_rect.expand(3.0),
                ui.make_persistent_id(("logic_channel_marker", channel)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!("Click to edit {channel_label} settings"));

        let popup_id = ui.make_persistent_id(("logic_channel_settings_popup", channel));
        if marker_response.clicked() {
            channel_settings_drafts[channel] = LogicChannelSettingsDraft {
                enabled: enabled_channels[channel],
                maximum_height: maximum_heights[channel],
                name: channel_name.clone(),
                color: *channel_color,
                marker_opacity: *channel_marker_opacity,
            };
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &marker_response,
            egui::popup::PopupCloseBehavior::CloseOnClickOutside,
            |popup_ui| {
                let draft = &mut channel_settings_drafts[channel];
                let mut save_requested = false;
                let mut cancel_requested = false;
                popup_ui.set_min_width(280.0);
                popup_ui.label(RichText::new(format!("D{channel} settings")).strong());
                popup_ui.checkbox(&mut draft.enabled, "Enabled");
                popup_ui.horizontal(|ui| {
                    ui.label("Max height");
                    ui.add(
                        egui::Slider::new(&mut draft.maximum_height, 1..=5)
                            .integer()
                            .suffix("×"),
                    );
                });
                popup_ui.separator();
                popup_ui.horizontal(|ui| {
                    ui.label("Name");
                    let name_edit = ui.add(
                        egui::TextEdit::singleline(&mut draft.name)
                            .char_limit(LOGIC_CHANNEL_NAME_MAX_CHARS)
                            .desired_width(190.0)
                            .hint_text("Optional name"),
                    );
                    if name_edit.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        save_requested = true;
                    }
                });
                popup_ui.separator();
                popup_ui.label("Color");
                egui::color_picker::color_picker_color32(
                    popup_ui,
                    &mut draft.color,
                    egui::color_picker::Alpha::Opaque,
                );
                if popup_ui.button("Reset color").clicked() {
                    draft.color = default_logic_channel_colors()[channel];
                }
                popup_ui.separator();
                popup_ui.horizontal(|ui| {
                    ui.label("Opacity");
                    let mut opacity_percent = f32::from(draft.marker_opacity) / 255.0 * 100.0;
                    if ui
                        .add(
                            egui::Slider::new(&mut opacity_percent, 10.0..=100.0)
                                .fixed_decimals(0)
                                .suffix("%"),
                        )
                        .changed()
                    {
                        draft.marker_opacity = (opacity_percent / 100.0 * 255.0).round() as u8;
                    }
                });
                popup_ui.separator();
                popup_ui.horizontal(|ui| {
                    save_requested |= ui.button("Save").clicked();
                    cancel_requested |= ui.button("Cancel").clicked();
                });
                if save_requested {
                    enabled_channels[channel] = draft.enabled;
                    maximum_heights[channel] = draft.maximum_height;
                    *channel_name = draft.name.clone();
                    *channel_color = draft.color;
                    *channel_marker_opacity = draft.marker_opacity;
                    popup_ui.memory_mut(egui::Memory::close_popup);
                } else if cancel_requested {
                    popup_ui.memory_mut(egui::Memory::close_popup);
                }
            },
        );

        if visible_count != 0 {
            let points = std::mem::take(&mut waveform_points[channel]);
            if points.len() > 1 {
                painter.add(egui::Shape::line(
                    points,
                    Stroke::new(1.2_f32, *channel_color),
                ));
            }
        }
        paint_logic_channel_marker(
            painter,
            signal_plot,
            low,
            marker_lane_height,
            &marker_text,
            *channel_color,
            *channel_marker_opacity,
        );
    }
    paint_logic_protocol_lanes(
        ui,
        &protocol_lanes,
        waveforms,
        *viewport,
        protocol_analyzer,
        protocol_decode_cache,
        protocol_stream,
    );
    paint_logic_markers(ui, plot, waveforms, *viewport, marker_settings);
    let smart_timing = smart_timing_enabled
        .then(|| ui.input(|input| input.pointer.hover_pos()))
        .flatten()
        .filter(|pointer| {
            signal_plot.contains(*pointer)
                && !protocol_lanes.iter().any(|lane| lane.contains(*pointer))
        })
        .and_then(|pointer| {
            let channel = logic_channel_at_y(&signal_layout.levels, enabled_channels, pointer.y)?;
            let sample = logic_sample_at_x(
                signal_plot,
                *viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                pointer.x,
            )?;
            logic_smart_timing_measurement(waveforms, channel, sample)
        });
    if let Some(measurement) = smart_timing {
        paint_logic_timing_hint(
            painter,
            signal_plot,
            waveforms,
            *viewport,
            measurement,
            channel_colors[measurement.channel],
        );
    }
    if visible_count == 0 {
        paint_empty(painter, signal_plot, "Waiting for logic samples");
    }
    paint_waveform_time_scale(
        painter,
        time_ruler,
        display_sample_count,
        display_sample_rate_hz,
        waveforms.logic_sample_offset,
        *viewport,
        time_axis,
    );
    paint_waveform_status(
        painter,
        status_bar,
        display_sample_count,
        display_sample_rate_hz,
        *viewport,
    );
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

struct ScopeOffsetArrowOptions<'a> {
    settings: ScopeCaptureSettings,
    channel_colors: [Color32; 2],
    channel_names: &'a [String; 2],
    vertical_offsets: &'a mut [u16; 2],
    controls_enabled: bool,
}

fn paint_scope_offset_arrows(ui: &egui::Ui, plot: Rect, options: ScopeOffsetArrowOptions<'_>) {
    let ScopeOffsetArrowOptions {
        settings,
        channel_colors,
        channel_names,
        vertical_offsets,
        controls_enabled,
    } = options;
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
                "{} vertical offset: {:+.2} div\nDrag to move · double-click to center",
                scope_channel_label(channel, channel_names),
                scope_vertical_offset_divisions(plot, vertical_offsets[channel])
            ))
    } else {
        response
    };

    if controls_enabled {
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

fn scope_trigger_level_y(plot: Rect, channel: ScopeChannelSettings, trigger_level: u8) -> f32 {
    scope_sample_y(
        plot,
        channel,
        scope_trigger_threshold_voltage(channel, trigger_level),
    )
}

fn scope_trigger_level_from_y(plot: Rect, y: f32, channel: ScopeChannelSettings) -> u8 {
    let zero_y = scope_vertical_offset_y(plot, channel.vertical_offset);
    let screen_divisions = (y - zero_y) / plot.height().max(1.0) * SCOPE_VERTICAL_DIVISIONS as f32;
    let reference_span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    let level = (f32::from(DSCOPE_VERTICAL_OFFSET_CENTER)
        + screen_divisions / SCOPE_ADC_VERTICAL_DIVISIONS * reference_span)
        .round()
        .clamp(
            f32::from(DSCOPE_TRIGGER_LEVEL_MIN),
            f32::from(DSCOPE_TRIGGER_LEVEL_MAX),
        );
    level as u8
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
    let reference_span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    let input_span = f32::from(channel.volts_per_div_mv) / 1_000.0
        * SCOPE_ADC_VERTICAL_DIVISIONS
        * f32::from(channel.probe_attenuation);
    (f32::from(DSCOPE_VERTICAL_OFFSET_CENTER) - f32::from(trigger_level)) / reference_span
        * input_span
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeTriggerCrossing {
    sample: f64,
    channel_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeSmartTimingMeasurement {
    start_sample: f64,
    end_sample: f64,
    channel_index: usize,
    end_voltage: f32,
}

impl ScopeSmartTimingMeasurement {
    fn duration(self, sample_rate_hz: u64) -> Option<Duration> {
        (sample_rate_hz != 0).then(|| {
            Duration::from_secs_f64(
                (self.end_sample - self.start_sample).abs() / sample_rate_hz as f64,
            )
        })
    }
}

fn scope_trigger_crossing_sample(
    waveforms: &WaveformStore,
    settings: ScopeCaptureSettings,
    trigger: ScopeTriggerSettings,
) -> Option<ScopeTriggerCrossing> {
    // The FPGA trigger pointer may lead or trail the analog threshold crossing
    // slightly because of the ADC/trigger pipeline and the footer byte offset.
    // Select the closest matching edge instead of requiring it to be exactly
    // one sample before the reported position.
    const TRIGGER_SEARCH_RADIUS: usize = 512;
    let hardware_trigger = waveforms
        .scope_trigger_sample_index
        .and_then(|trigger_sample| trigger_sample.checked_sub(waveforms.scope_sample_offset))
        .and_then(|trigger_sample| usize::try_from(trigger_sample).ok());
    if hardware_trigger.is_none() && trigger.source != DsCopeTriggerSource::Auto {
        return None;
    }
    let capture_boundaries = waveforms
        .scope_capture_boundaries
        .iter()
        .filter_map(|boundary| boundary.checked_sub(waveforms.scope_sample_offset))
        .filter_map(|boundary| usize::try_from(boundary).ok())
        .collect::<HashSet<_>>();
    let mut closest_crossing = None::<(ScopeTriggerCrossing, f32)>;

    for &channel_index in scope_trigger_channels(trigger) {
        let channel_settings = settings.channels[channel_index];
        if !channel_settings.enabled {
            continue;
        }
        let Some(samples) = waveforms.scope_channels.get(channel_index) else {
            continue;
        };
        if samples.len() < 2 {
            continue;
        }
        let reference_sample = match hardware_trigger {
            Some(sample) if sample < samples.len() => sample as f32,
            Some(_) => continue,
            None => f32::from(trigger.position_percent) / 100.0 * (samples.len() - 1) as f32,
        };
        let threshold =
            scope_trigger_threshold_voltage(channel_settings, trigger.levels[channel_index]);
        let (first_left, last_left) = if trigger.source == DsCopeTriggerSource::Auto {
            // Auto capture can return a free-running frame without setting the
            // hardware trigger flag. If that frame nevertheless contains the
            // requested edge, lock it to the trigger position just like a
            // hardware-triggered frame. Only free-run when no edge exists.
            (0, samples.len() - 1)
        } else {
            let sample = hardware_trigger.expect("non-auto trigger requires a hardware position");
            (
                sample.saturating_sub(TRIGGER_SEARCH_RADIUS),
                sample
                    .saturating_add(TRIGGER_SEARCH_RADIUS)
                    .min(samples.len() - 1),
            )
        };
        for left in first_left..last_left {
            let right = left + 1;
            if capture_boundaries.contains(&right) {
                continue;
            }
            let (left_value, right_value) = (samples[left], samples[right]);
            let crosses = match trigger.slope {
                DsCopeTriggerSlope::Rising => {
                    (left_value < threshold && right_value >= threshold)
                        || (left_value <= threshold && right_value > threshold)
                }
                DsCopeTriggerSlope::Falling => {
                    (left_value > threshold && right_value <= threshold)
                        || (left_value >= threshold && right_value < threshold)
                }
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
            let crossing = ScopeTriggerCrossing {
                sample: waveforms.scope_sample_offset as f64
                    + left as f64
                    + f64::from(interpolation),
                channel_index,
            };
            let distance = (left as f32 + interpolation - reference_sample).abs();
            if closest_crossing.map_or(true, |(_, current_distance)| distance < current_distance) {
                closest_crossing = Some((crossing, distance));
            }
        }
    }
    closest_crossing.map(|(crossing, _)| crossing)
}

fn scope_sample_fraction(sample: f64, sample_offset: u64, sample_count: usize) -> f32 {
    let local_sample = sample - sample_offset as f64;
    (local_sample / sample_count.saturating_sub(1).max(1) as f64) as f32
}

fn scope_smart_timing_measurement(
    waveforms: &WaveformStore,
    trigger_crossing: ScopeTriggerCrossing,
    plot: Rect,
    viewport: WaveformViewport,
    display_sample_count: usize,
    pointer: Pos2,
) -> Option<ScopeSmartTimingMeasurement> {
    if !plot.contains(pointer) || display_sample_count == 0 {
        return None;
    }
    let samples = waveforms
        .scope_channels
        .get(trigger_crossing.channel_index)?;
    if samples.is_empty() {
        return None;
    }
    let screen_fraction = (pointer.x - plot.left()) / plot.width().max(1.0);
    let domain_fraction = viewport.domain_fraction_at_screen(screen_fraction);
    let end_sample_index =
        (f64::from(domain_fraction) * display_sample_count.saturating_sub(1) as f64).round();
    if end_sample_index < 0.0 || end_sample_index > samples.len().saturating_sub(1) as f64 {
        return None;
    }
    let end_sample_index = end_sample_index as usize;
    Some(ScopeSmartTimingMeasurement {
        start_sample: trigger_crossing.sample,
        end_sample: waveforms.scope_sample_offset as f64 + end_sample_index as f64,
        channel_index: trigger_crossing.channel_index,
        end_voltage: samples[end_sample_index],
    })
}

fn scope_smart_timing_has_nonzero_trigger(
    settings: ScopeCaptureSettings,
    trigger_crossing: ScopeTriggerCrossing,
) -> bool {
    settings
        .channels
        .get(trigger_crossing.channel_index)
        .is_some_and(|channel| {
            scope_trigger_threshold_voltage(
                *channel,
                settings.trigger.levels[trigger_crossing.channel_index],
            )
            .abs()
                > f32::EPSILON
        })
}

struct ScopeSmartTimingPaintOptions {
    settings: ScopeCaptureSettings,
    viewport: WaveformViewport,
    display_sample_count: usize,
    sample_rate_hz: u64,
    color: Color32,
}

fn paint_scope_timing_hint(
    painter: &egui::Painter,
    plot: Rect,
    waveforms: &WaveformStore,
    measurement: ScopeSmartTimingMeasurement,
    options: ScopeSmartTimingPaintOptions,
) {
    let ScopeSmartTimingPaintOptions {
        settings,
        viewport,
        display_sample_count,
        sample_rate_hz,
        color,
    } = options;
    let painter = painter.with_clip_rect(plot);
    let Some(duration) = measurement.duration(sample_rate_hz) else {
        return;
    };
    let Some(channel) = settings.channels.get(measurement.channel_index).copied() else {
        return;
    };
    let start_x = waveform_sample_x(
        plot,
        viewport,
        measurement.start_sample - waveforms.scope_sample_offset as f64,
        display_sample_count,
    );
    let end_x = waveform_sample_x(
        plot,
        viewport,
        measurement.end_sample - waveforms.scope_sample_offset as f64,
        display_sample_count,
    );
    let start_y = scope_trigger_level_y(
        plot,
        channel,
        settings.trigger.levels[measurement.channel_index],
    );
    let end_y = scope_sample_y(plot, channel, measurement.end_voltage);

    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(start_x.min(end_x), plot.top()),
            Pos2::new(start_x.max(end_x), plot.bottom()),
        ),
        0.0,
        color.gamma_multiply(0.08),
    );
    for (x, opacity) in [(start_x, 0.3), (end_x, 0.5)] {
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(0.75_f32, color.gamma_multiply(opacity)),
        );
    }
    for point in [Pos2::new(start_x, start_y), Pos2::new(end_x, end_y)] {
        painter.circle_filled(point, 2.0, color.gamma_multiply(0.5));
    }

    if plot.width() < 120.0 || plot.height() < 70.0 {
        return;
    }
    let width = 190.0_f32.min(plot.width() - 16.0);
    let hint = Rect::from_min_size(
        Pos2::new(plot.right() - width - 8.0, plot.top() + 26.0),
        Vec2::new(width, 42.0),
    );
    painter.rect_filled(hint, 5.0, PANEL.gamma_multiply(0.88));
    let border = Stroke::new(0.75_f32, color.gamma_multiply(0.65));
    painter.line_segment([hint.left_top(), hint.right_top()], border);
    painter.line_segment([hint.right_top(), hint.right_bottom()], border);
    painter.line_segment([hint.right_bottom(), hint.left_bottom()], border);
    painter.line_segment([hint.left_bottom(), hint.left_top()], border);

    let left = hint.left() + 8.0;
    painter.text(
        Pos2::new(left, hint.top() + 6.0),
        egui::Align2::LEFT_TOP,
        format!("DURATION   {}", format_duration(duration)),
        FontId::monospace(10.0),
        Color32::WHITE,
    );
    painter.text(
        Pos2::new(left, hint.top() + 22.0),
        egui::Align2::LEFT_TOP,
        format!(
            "AMPLITUDE  {}",
            format_trigger_voltage(measurement.end_voltage)
        ),
        FontId::monospace(10.0),
        color,
    );
}

fn should_align_scope_trigger(
    anchor_locked: bool,
    scope_rolling: bool,
    has_trigger_crossing: bool,
) -> bool {
    anchor_locked && (!scope_rolling || has_trigger_crossing)
}

fn waveform_sample_x(
    plot: Rect,
    viewport: WaveformViewport,
    sample_index: f64,
    sample_count: usize,
) -> f32 {
    let sample_fraction = sample_index / sample_count.saturating_sub(1).max(1) as f64;
    let screen_fraction =
        (sample_fraction - f64::from(viewport.start)) / f64::from(viewport.span).max(f64::EPSILON);
    plot.left() + screen_fraction as f32 * plot.width()
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
        crossing.channel_index == channel_index && crossing.sample >= sample_offset as f64
    }) {
        let local_crossing = crossing.sample - sample_offset as f64;
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

fn scope_plot_sample_segments(
    visible_range: Range<usize>,
    step: usize,
    channel_index: usize,
    trigger_crossing: Option<ScopeTriggerCrossing>,
    sample_offset: u64,
    capture_boundaries: &VecDeque<u64>,
) -> Vec<Vec<usize>> {
    let mut indices = scope_plot_sample_indices(
        visible_range.clone(),
        step,
        channel_index,
        trigger_crossing,
        sample_offset,
    );
    let local_boundaries = capture_boundaries
        .iter()
        .filter_map(|boundary| boundary.checked_sub(sample_offset))
        .filter_map(|boundary| usize::try_from(boundary).ok())
        .filter(|boundary| *boundary > visible_range.start && *boundary < visible_range.end)
        .collect::<Vec<_>>();
    for boundary in &local_boundaries {
        indices.extend([boundary - 1, *boundary]);
    }
    indices.sort_unstable();
    indices.dedup();

    let mut segments = Vec::new();
    let mut current = Vec::new();
    for index in indices {
        if local_boundaries.binary_search(&index).is_ok() && !current.is_empty() {
            segments.push(std::mem::take(&mut current));
        }
        current.push(index);
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeDisplayBucket {
    center_index: f64,
    average: f32,
}

fn scope_downsampled_segments(
    samples: &VecDeque<f32>,
    visible_range: Range<usize>,
    step: usize,
    sample_offset: u64,
    capture_boundaries: &VecDeque<u64>,
) -> Vec<Vec<ScopeDisplayBucket>> {
    let end = visible_range.end.min(samples.len());
    let mut segment_start = visible_range.start.min(end);
    let mut segment_ends = capture_boundaries
        .iter()
        .filter_map(|boundary| boundary.checked_sub(sample_offset))
        .filter_map(|boundary| usize::try_from(boundary).ok())
        .filter(|boundary| *boundary > segment_start && *boundary < end)
        .collect::<Vec<_>>();
    segment_ends.push(end);

    let mut segments = Vec::with_capacity(segment_ends.len());
    for segment_end in segment_ends {
        let mut buckets =
            Vec::with_capacity(segment_end.saturating_sub(segment_start).div_ceil(step));
        for bucket_start in (segment_start..segment_end).step_by(step.max(1)) {
            let bucket_end = bucket_start.saturating_add(step).min(segment_end);
            let sum = samples
                .range(bucket_start..bucket_end)
                .map(|value| f64::from(*value))
                .sum::<f64>();
            buckets.push(ScopeDisplayBucket {
                center_index: (bucket_start as f64 + (bucket_end - 1) as f64) * 0.5,
                average: (sum / (bucket_end - bucket_start) as f64) as f32,
            });
        }
        if !buckets.is_empty() {
            segments.push(buckets);
        }
        segment_start = segment_end;
    }
    segments
}

fn paint_scope_downsampled_segments(
    painter: &egui::Painter,
    plot: Rect,
    segments: &[Vec<ScopeDisplayBucket>],
    channel: ScopeChannelSettings,
    viewport: WaveformViewport,
    sample_count: usize,
    color: Color32,
) {
    for segment in segments {
        let points = segment
            .iter()
            .map(|bucket| {
                Pos2::new(
                    waveform_sample_x(plot, viewport, bucket.center_index, sample_count),
                    scope_sample_y(plot, channel, bucket.average),
                )
            })
            .collect::<Vec<_>>();
        if points.len() == 1 {
            painter.circle_filled(points[0], 0.75, color);
        } else if points.len() > 1 {
            painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(4.0_f32, color.gamma_multiply(0.16)),
            ));
            painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, color)));
        }
    }
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

fn scope_trigger_position_in_view(trigger_sample_fraction: f32, viewport: WaveformViewport) -> u8 {
    let position_percent =
        ((trigger_sample_fraction - viewport.start) / viewport.span.max(f32::EPSILON) * 100.0)
            .round() as i32;
    position_percent.clamp(
        i32::from(DSCOPE_TRIGGER_POSITION_MIN_PERCENT),
        i32::from(DSCOPE_TRIGGER_POSITION_MAX_PERCENT),
    ) as u8
}

fn finish_scope_viewport_pan(
    trigger_sample_fraction: f32,
    viewport: &mut WaveformViewport,
    trigger_settings: &mut ScopeTriggerSettings,
) {
    trigger_settings.position_percent =
        scope_trigger_position_in_view(trigger_sample_fraction, *viewport);
    // A manual waveform pan must remain detached from the trigger. Relocking
    // here causes the next frame to recenter the viewport and makes time
    // markers appear to jump away from their capture positions.
    viewport.anchor_locked = false;
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
    .on_hover_text(format!("{}%", settings.position_percent));

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
        let arrow_y = scope_trigger_level_y(plot, channel_settings, settings.levels[channel]);
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
                settings.levels[channel] = DSCOPE_TRIGGER_LEVEL_CENTER;
            } else if response.dragged() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    settings.levels[channel] =
                        scope_trigger_level_from_y(plot, pointer.y, channel_settings);
                }
            }
        }

        let arrow_y = scope_trigger_level_y(plot, channel_settings, settings.levels[channel]);
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

struct ScopeMeasurementPaintOptions<'a> {
    visible_range: Range<usize>,
    sample_rate_hz: u64,
    settings: ScopeFunctionSettings,
    enabled_channels: [bool; 2],
    channel_colors: [Color32; 2],
    channel_names: &'a [String; 2],
    caches: &'a mut [ScopeMeasurementCache; 2],
    window_positions: &'a mut [Vec2; 2],
}

fn paint_scope_measurements(
    ui: &egui::Ui,
    painter: &egui::Painter,
    plot: Rect,
    waveforms: &WaveformStore,
    options: ScopeMeasurementPaintOptions<'_>,
) {
    let ScopeMeasurementPaintOptions {
        visible_range,
        sample_rate_hz,
        settings,
        enabled_channels,
        channel_colors,
        channel_names,
        caches,
        window_positions,
    } = options;
    const ROW_HEIGHT: f32 = 20.0;
    const COLUMN_WIDTH: f32 = 176.0;
    const PADDING: f32 = 8.0;
    const HEADER_HEIGHT: f32 = 25.0;
    const HEADER_FONT_SIZE: f32 = 10.0;
    const LABEL_FONT_SIZE: f32 = 10.0;
    const VALUE_FONT_SIZE: f32 = 12.0;

    for channel in 0..2 {
        let channel_settings = settings.measurements.channels[channel];
        if !enabled_channels[channel]
            || !channel_settings.enabled
            || waveforms.scope_channels.get(channel).is_none()
        {
            continue;
        }
        let selected = SCOPE_MEASUREMENT_KINDS
            .into_iter()
            .filter(|kind| channel_settings.selected[kind.index()])
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let results =
            caches[channel].results(waveforms, channel, visible_range.clone(), sample_rate_hz);
        let rows = selected.len();
        let desired_size = Vec2::new(
            COLUMN_WIDTH + PADDING * 2.0,
            HEADER_HEIGHT + rows as f32 * ROW_HEIGHT + PADDING * 2.0,
        );
        let panel_size = Vec2::new(
            desired_size.x.min(plot.width()),
            desired_size.y.min(plot.height()),
        );
        let mut panel = scope_measurement_window_rect(plot, panel_size, window_positions[channel]);
        let header = Rect::from_min_size(
            panel.min,
            Vec2::new(panel.width(), HEADER_HEIGHT.min(panel.height())),
        );
        let response = ui
            .interact(
                header,
                ui.make_persistent_id(("scope_measurement_window", channel)),
                Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab)
            .on_hover_text(format!(
                "Drag the {} measurement window",
                scope_channel_label(channel, channel_names)
            ));
        if response.dragged() {
            let delta = ui.input(|input| input.pointer.delta());
            panel = panel.translate(delta);
            panel = panel.translate(Vec2::new(
                panel
                    .left()
                    .clamp(plot.left(), plot.right() - panel.width())
                    - panel.left(),
                panel
                    .top()
                    .clamp(plot.top(), plot.bottom() - panel.height())
                    - panel.top(),
            ));
            window_positions[channel] = scope_measurement_window_position(plot, panel);
            ui.ctx().request_repaint();
        }

        let color = channel_colors[channel];
        painter.rect_filled(
            panel,
            egui::Rounding::same(5.0),
            Color32::from_rgba_unmultiplied(8, 12, 18, 230),
        );
        painter.rect_filled(
            Rect::from_min_size(panel.min, Vec2::new(panel.width(), HEADER_HEIGHT)),
            egui::Rounding::same(5.0),
            color.gamma_multiply(0.22),
        );
        painter.text(
            panel.left_top() + Vec2::new(PADDING, 5.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{} MEASUREMENTS",
                scope_channel_label(channel, channel_names)
            ),
            FontId::monospace(HEADER_FONT_SIZE),
            color,
        );
        painter.text(
            panel.right_top() + Vec2::new(-PADDING, 5.0),
            egui::Align2::RIGHT_TOP,
            "⠿",
            FontId::proportional(13.0),
            color,
        );

        for (ordinal, kind) in selected.into_iter().enumerate() {
            let row = ordinal;
            let origin = panel.left_top()
                + Vec2::new(PADDING, PADDING + HEADER_HEIGHT + row as f32 * ROW_HEIGHT);
            painter.text(
                origin,
                egui::Align2::LEFT_TOP,
                kind.label(),
                FontId::monospace(LABEL_FONT_SIZE),
                TEXT_MUTED,
            );
            painter.text(
                origin + Vec2::new(COLUMN_WIDTH - PADDING * 2.0, -1.0),
                egui::Align2::RIGHT_TOP,
                results.formatted(kind),
                FontId::monospace(VALUE_FONT_SIZE),
                color,
            );
        }
    }
}

fn scope_measurement_window_rect(plot: Rect, size: Vec2, position: Vec2) -> Rect {
    let travel = Vec2::new(
        (plot.width() - size.x).max(0.0),
        (plot.height() - size.y).max(0.0),
    );
    Rect::from_min_size(
        plot.min
            + Vec2::new(
                position.x.clamp(0.0, 1.0) * travel.x,
                position.y.clamp(0.0, 1.0) * travel.y,
            ),
        size,
    )
}

fn scope_measurement_window_position(plot: Rect, panel: Rect) -> Vec2 {
    let travel = Vec2::new(
        (plot.width() - panel.width()).max(0.0),
        (plot.height() - panel.height()).max(0.0),
    );
    Vec2::new(
        if travel.x > 0.0 {
            (panel.left() - plot.left()) / travel.x
        } else {
            0.0
        },
        if travel.y > 0.0 {
            (panel.top() - plot.top()) / travel.y
        } else {
            0.0
        },
    )
}

struct ScopeMarkerPaintOptions<'a> {
    settings: &'a mut ScopeMarkerSettings,
    scope_settings: ScopeCaptureSettings,
    channel_names: &'a [String; 2],
    sample_count: usize,
    sample_rate_hz: u64,
    viewport: WaveformViewport,
}

const SCOPE_MARKER_LINE_WIDTH: f32 = 0.5;

fn paint_scope_markers(
    ui: &egui::Ui,
    painter: &egui::Painter,
    plot: Rect,
    options: ScopeMarkerPaintOptions<'_>,
) {
    let ScopeMarkerPaintOptions {
        settings: marker_settings,
        scope_settings,
        channel_names,
        sample_count,
        sample_rate_hz,
        viewport,
    } = options;
    for channel in 0..2 {
        if !scope_settings.channels[channel].enabled {
            continue;
        }
        let channel_markers = &mut marker_settings.channels[channel];
        let horizontal_count = channel_markers.horizontal_count.min(SCOPE_MARKER_LIMIT);
        for (index, position) in channel_markers.horizontal_positions[..horizontal_count]
            .iter_mut()
            .enumerate()
        {
            let color = channel_markers.horizontal_colors[index];
            let x = egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain(position.clamp(0.0, 1.0)),
            );
            if (plot.left()..=plot.right()).contains(&x) {
                let hit_rect = Rect::from_min_max(
                    Pos2::new(x - 7.0, plot.top()),
                    Pos2::new(x + 7.0, plot.bottom()),
                );
                let response = ui
                    .interact(
                        hit_rect,
                        ui.make_persistent_id(("scope_time_marker", channel, index)),
                        Sense::drag(),
                    )
                    .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                    .on_hover_text(format!(
                        "Drag {} T{} horizontally",
                        scope_channel_label(channel, channel_names),
                        index + 1
                    ));
                if response.dragged() {
                    if let Some(pointer) = response.interact_pointer_pos() {
                        let screen_fraction = (pointer.x - plot.left()) / plot.width().max(1.0);
                        *position = viewport
                            .domain_fraction_at_screen(screen_fraction)
                            .clamp(0.0, 1.0);
                    }
                }
            }
            let x = egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain(position.clamp(0.0, 1.0)),
            );
            paint_dashed_line(
                painter,
                Pos2::new(x, plot.top()),
                Pos2::new(x, plot.bottom()),
                Stroke::new(SCOPE_MARKER_LINE_WIDTH, color),
            );
            paint_marker_handle(
                painter,
                Pos2::new(x, plot.top() + 10.0),
                &format!("T{channel}{}", index + 1),
                color,
            );
        }
        for index in 0..horizontal_count {
            let Some(other) = channel_markers.horizontal_comparisons[index]
                .filter(|other| *other < horizontal_count && *other != index)
            else {
                continue;
            };
            if other < index && channel_markers.horizontal_comparisons[other] == Some(index) {
                continue;
            }
            let first = channel_markers.horizontal_positions[index];
            let second = channel_markers.horizontal_positions[other];
            let Some(delta_seconds) =
                scope_time_marker_delta(first, second, sample_count, sample_rate_hz)
            else {
                continue;
            };
            let frequency = (delta_seconds > 0.0).then(|| 1.0 / delta_seconds);
            let text = match frequency {
                Some(frequency) => format!(
                    "{} T{}–T{}  {}  ·  {}",
                    scope_channel_label(channel, channel_names),
                    index + 1,
                    other + 1,
                    format_duration(Duration::from_secs_f64(delta_seconds)),
                    format_frequency(frequency)
                ),
                None => format!(
                    "{} T{}–T{}  {}",
                    scope_channel_label(channel, channel_names),
                    index + 1,
                    other + 1,
                    format_duration(Duration::from_secs_f64(delta_seconds))
                ),
            };
            let center_x = egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain((first + second) * 0.5),
            );
            paint_marker_readout(
                painter,
                plot,
                Pos2::new(
                    center_x,
                    plot.top() + 33.0 + index as f32 * 27.0 + channel as f32 * 13.0,
                ),
                &text,
                channel_markers.horizontal_colors[index],
            );
        }

        let vertical_count = channel_markers.vertical_count.min(SCOPE_MARKER_LIMIT);
        for (index, position) in channel_markers.vertical_positions[..vertical_count]
            .iter_mut()
            .enumerate()
        {
            let color = channel_markers.vertical_colors[index];
            let y = egui::lerp(plot.y_range(), position.clamp(0.0, 1.0));
            let hit_rect = Rect::from_min_max(
                Pos2::new(plot.left(), y - 7.0),
                Pos2::new(plot.right(), y + 7.0),
            );
            let response = ui
                .interact(
                    hit_rect,
                    ui.make_persistent_id(("scope_voltage_marker", channel, index)),
                    Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeVertical)
                .on_hover_text(format!(
                    "Drag {} V{} vertically",
                    scope_channel_label(channel, channel_names),
                    index + 1
                ));
            if response.dragged() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    *position = ((pointer.y - plot.top()) / plot.height().max(1.0)).clamp(0.0, 1.0);
                }
            }
            let y = egui::lerp(plot.y_range(), position.clamp(0.0, 1.0));
            paint_dashed_line(
                painter,
                Pos2::new(plot.left(), y),
                Pos2::new(plot.right(), y),
                Stroke::new(SCOPE_MARKER_LINE_WIDTH, color),
            );
            paint_marker_handle(
                painter,
                Pos2::new(plot.left() + 17.0, y),
                &format!("V{channel}{}", index + 1),
                color,
            );
        }
        for index in 0..vertical_count {
            let Some(other) = channel_markers.vertical_comparisons[index]
                .filter(|other| *other < vertical_count && *other != index)
            else {
                continue;
            };
            if other < index && channel_markers.vertical_comparisons[other] == Some(index) {
                continue;
            }
            let first = channel_markers.vertical_positions[index];
            let second = channel_markers.vertical_positions[other];
            let delta_voltage =
                scope_voltage_marker_delta(first, second, scope_settings.channels[channel]);
            let center_y = egui::lerp(plot.y_range(), (first + second) * 0.5);
            let center_x = if channel == 0 {
                plot.left() + 125.0
            } else {
                plot.right() - 125.0
            };
            paint_marker_readout(
                painter,
                plot,
                Pos2::new(center_x, center_y),
                &format!(
                    "{} V{}–V{}  {}",
                    scope_channel_label(channel, channel_names),
                    index + 1,
                    other + 1,
                    format_marker_voltage(delta_voltage)
                ),
                channel_markers.vertical_colors[index],
            );
        }
    }
}

fn paint_scope_quick_time_measurement(
    painter: &egui::Painter,
    plot: Rect,
    measurement: ScopeQuickTimeMeasurement,
    sample_count: usize,
    sample_rate_hz: u64,
    viewport: WaveformViewport,
) {
    const COLOR: Color32 = Color32::from_rgb(255, 214, 76);

    let (first_position, second_position) = match measurement {
        ScopeQuickTimeMeasurement::Idle => return,
        ScopeQuickTimeMeasurement::AwaitingSecond { first_position } => (first_position, None),
        ScopeQuickTimeMeasurement::Complete {
            first_position,
            second_position,
        } => (first_position, Some(second_position)),
    };
    let first_x = egui::lerp(
        plot.x_range(),
        viewport.screen_fraction_at_domain(first_position),
    );
    paint_dashed_line(
        painter,
        Pos2::new(first_x, plot.top()),
        Pos2::new(first_x, plot.bottom()),
        Stroke::new(SCOPE_MARKER_LINE_WIDTH, COLOR),
    );
    paint_marker_handle(
        painter,
        Pos2::new(first_x, plot.bottom() - 11.0),
        "Δt1",
        COLOR,
    );

    let Some(second_position) = second_position else {
        paint_marker_readout(
            painter,
            plot,
            Pos2::new(first_x, plot.bottom() - 39.0),
            "Click to place Δt2",
            COLOR,
        );
        return;
    };

    let second_x = egui::lerp(
        plot.x_range(),
        viewport.screen_fraction_at_domain(second_position),
    );
    paint_dashed_line(
        painter,
        Pos2::new(second_x, plot.top()),
        Pos2::new(second_x, plot.bottom()),
        Stroke::new(SCOPE_MARKER_LINE_WIDTH, COLOR),
    );
    paint_marker_handle(
        painter,
        Pos2::new(second_x, plot.bottom() - 11.0),
        "Δt2",
        COLOR,
    );
    let Some(delta_seconds) = scope_time_marker_delta(
        first_position,
        second_position,
        sample_count,
        sample_rate_hz,
    ) else {
        return;
    };
    paint_marker_readout(
        painter,
        plot,
        Pos2::new((first_x + second_x) * 0.5, plot.bottom() - 39.0),
        &format!(
            "Δt  {}",
            format_duration(Duration::from_secs_f64(delta_seconds))
        ),
        COLOR,
    );
}

fn scope_marker_color(channel: usize, marker: usize) -> Color32 {
    const CH0_COLORS: [Color32; SCOPE_MARKER_LIMIT] = [
        Color32::from_rgb(255, 204, 74),
        Color32::from_rgb(255, 132, 172),
        Color32::from_rgb(255, 145, 77),
        Color32::from_rgb(213, 126, 255),
        Color32::from_rgb(180, 230, 80),
        Color32::from_rgb(255, 225, 120),
        Color32::from_rgb(255, 105, 105),
        Color32::from_rgb(232, 170, 255),
        Color32::from_rgb(255, 180, 95),
        Color32::from_rgb(205, 240, 120),
    ];
    const CH1_COLORS: [Color32; SCOPE_MARKER_LIMIT] = [
        Color32::from_rgb(99, 179, 255),
        Color32::from_rgb(70, 210, 185),
        Color32::from_rgb(174, 130, 255),
        Color32::from_rgb(100, 205, 255),
        Color32::from_rgb(245, 100, 100),
        Color32::from_rgb(80, 235, 205),
        Color32::from_rgb(135, 205, 255),
        Color32::from_rgb(205, 165, 255),
        Color32::from_rgb(85, 225, 240),
        Color32::from_rgb(255, 135, 180),
    ];
    if channel == 0 {
        CH0_COLORS[marker % SCOPE_MARKER_LIMIT]
    } else {
        CH1_COLORS[marker % SCOPE_MARKER_LIMIT]
    }
}

fn paint_dashed_line(painter: &egui::Painter, start: Pos2, end: Pos2, stroke: Stroke) {
    const DASH_LENGTH: f32 = 6.0;
    const GAP_LENGTH: f32 = 4.0;
    let delta = end - start;
    let length = delta.length();
    if length <= f32::EPSILON {
        return;
    }
    let direction = delta / length;
    let mut offset = 0.0;
    while offset < length {
        let dash_end = (offset + DASH_LENGTH).min(length);
        painter.line_segment(
            [start + direction * offset, start + direction * dash_end],
            stroke,
        );
        offset += DASH_LENGTH + GAP_LENGTH;
    }
}

fn paint_marker_handle(painter: &egui::Painter, center: Pos2, label: &str, color: Color32) {
    let width = (label.chars().count() as f32 * 7.0 + 10.0).max(25.0);
    let rect = Rect::from_center_size(center, Vec2::new(width, 18.0));
    painter.rect_filled(rect, egui::Rounding::same(3.0), color);
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        label,
        FontId::monospace(10.0),
        marker_text_color(color),
    );
}

fn paint_marker_readout(
    painter: &egui::Painter,
    plot: Rect,
    center: Pos2,
    text: &str,
    color: Color32,
) {
    let size = Vec2::new(
        (text.chars().count() as f32 * 7.2 + 16.0).min(plot.width()),
        24.0_f32.min(plot.height()),
    );
    let half = size * 0.5;
    let center = Pos2::new(
        center.x.clamp(plot.left() + half.x, plot.right() - half.x),
        center.y.clamp(plot.top() + half.y, plot.bottom() - half.y),
    );
    let rect = Rect::from_center_size(center, size);
    painter.rect_filled(
        rect,
        egui::Rounding::same(4.0),
        Color32::from_rgba_unmultiplied(8, 12, 18, 230),
    );
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        text,
        FontId::monospace(12.0),
        color,
    );
}

fn scope_time_marker_delta(
    first_position: f32,
    second_position: f32,
    sample_count: usize,
    sample_rate_hz: u64,
) -> Option<f64> {
    if sample_count == 0 || sample_rate_hz == 0 {
        return None;
    }
    let capture_seconds = sample_count as f64 / sample_rate_hz as f64;
    Some(f64::from((second_position - first_position).abs()) * capture_seconds)
}

fn scope_voltage_marker_delta(
    first_position: f32,
    second_position: f32,
    channel: ScopeChannelSettings,
) -> f32 {
    let volts_per_division =
        f32::from(channel.volts_per_div_mv) / 1_000.0 * f32::from(channel.probe_attenuation);
    (second_position - first_position).abs() * volts_per_division * SCOPE_VERTICAL_DIVISIONS as f32
}

fn format_marker_voltage(value: f32) -> String {
    if value >= 1.0 {
        format!("{value:.3} V")
    } else if value >= 1.0e-3 {
        format!("{:.2} mV", value * 1.0e3)
    } else {
        format!("{:.2} µV", value * 1.0e6)
    }
}

fn scope_math_value(
    waveforms: &WaveformStore,
    settings: ScopeMathSettings,
    sample_index: usize,
) -> Option<f32> {
    let left = *waveforms
        .scope_channels
        .get(settings.left_channel)?
        .get(sample_index)?;
    let right = *waveforms
        .scope_channels
        .get(settings.right_channel)?
        .get(sample_index)?;
    settings.operation.apply(left, right)
}

fn scope_math_unit(operation: ScopeMathOperation) -> &'static str {
    match operation {
        ScopeMathOperation::Add | ScopeMathOperation::Subtract => "V",
        ScopeMathOperation::Multiply => "V²",
        ScopeMathOperation::Divide => "ratio",
    }
}

struct ScopeMathPaintOptions<'a> {
    viewport: WaveformViewport,
    sample_count: usize,
    visible_range: Range<usize>,
    step: usize,
    settings: ScopeMathSettings,
    channel_names: &'a [String; 2],
}

fn paint_scope_math_trace(
    painter: &egui::Painter,
    plot: Rect,
    waveforms: &WaveformStore,
    options: ScopeMathPaintOptions<'_>,
) {
    let ScopeMathPaintOptions {
        viewport,
        sample_count,
        visible_range,
        step,
        settings,
        channel_names,
    } = options;
    if !settings.enabled || visible_range.is_empty() {
        return;
    }
    let segments = scope_plot_sample_segments(
        visible_range,
        step,
        usize::MAX,
        None,
        waveforms.scope_sample_offset,
        &waveforms.scope_capture_boundaries,
    );
    let maximum = segments
        .iter()
        .flatten()
        .filter_map(|index| scope_math_value(waveforms, settings, *index))
        .map(f32::abs)
        .fold(0.0_f32, f32::max)
        .max(f32::EPSILON);
    for segment in segments {
        let points = segment
            .into_iter()
            .filter_map(|index| {
                scope_math_value(waveforms, settings, index).map(|value| {
                    let x = waveform_sample_x(plot, viewport, index as f64, sample_count);
                    let y = plot.center().y - value / maximum * plot.height() * 0.44;
                    Pos2::new(x, y)
                })
            })
            .collect::<Vec<_>>();
        if points.len() > 1 {
            painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(4.0_f32, MATH_COLOR.gamma_multiply(0.16)),
            ));
            painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, MATH_COLOR)));
        }
    }
    painter.text(
        plot.right_top() + Vec2::new(-8.0, 7.0),
        egui::Align2::RIGHT_TOP,
        format!(
            "M  {} {} {}  auto ±{:.3} {}",
            scope_channel_label(settings.left_channel, channel_names),
            settings.operation.symbol(),
            scope_channel_label(settings.right_channel, channel_names),
            maximum,
            scope_math_unit(settings.operation),
        ),
        FontId::monospace(10.0),
        MATH_COLOR,
    );
}

fn fft_in_place(values: &mut [(f64, f64)]) {
    let count = values.len();
    let mut target = 0;
    for source in 1..count {
        let mut bit = count >> 1;
        while target & bit != 0 {
            target ^= bit;
            bit >>= 1;
        }
        target ^= bit;
        if source < target {
            values.swap(source, target);
        }
    }

    let mut width = 2;
    while width <= count {
        let angle = -std::f64::consts::TAU / width as f64;
        let base = (angle.cos(), angle.sin());
        for start in (0..count).step_by(width) {
            let mut twiddle = (1.0, 0.0);
            for offset in 0..width / 2 {
                let even = values[start + offset];
                let odd = values[start + offset + width / 2];
                let rotated = (
                    odd.0 * twiddle.0 - odd.1 * twiddle.1,
                    odd.0 * twiddle.1 + odd.1 * twiddle.0,
                );
                values[start + offset] = (even.0 + rotated.0, even.1 + rotated.1);
                values[start + offset + width / 2] = (even.0 - rotated.0, even.1 - rotated.1);
                twiddle = (
                    twiddle.0 * base.0 - twiddle.1 * base.1,
                    twiddle.0 * base.1 + twiddle.1 * base.0,
                );
            }
        }
        width *= 2;
    }
}

fn scope_fft_spectrum(
    samples: &VecDeque<f32>,
    visible_range: Range<usize>,
    requested_length: usize,
    window_type: ScopeFftWindow,
) -> Vec<f32> {
    let available = visible_range.len().min(requested_length);
    if available < 8 {
        return Vec::new();
    }
    let length = 1_usize << (usize::BITS - 1 - available.leading_zeros());
    let start = visible_range.start + (visible_range.len() - length) / 2;
    let mut values = Vec::with_capacity(length);
    let mut window_sum = 0.0;
    for index in 0..length {
        let window = window_type.coefficient(index, length);
        window_sum += window;
        values.push((f64::from(samples[start + index]) * window, 0.0));
    }
    fft_in_place(&mut values);

    values[..=length / 2]
        .iter()
        .enumerate()
        .map(|(bin, (real, imaginary))| {
            let factor = if bin == 0 || bin == length / 2 {
                1.0
            } else {
                2.0_f64.sqrt()
            };
            let magnitude = (real * real + imaginary * imaginary).sqrt() * factor / window_sum;
            (20.0 * magnitude.max(1.0e-12).log10()) as f32
        })
        .collect()
}

fn format_frequency(frequency_hz: f64) -> String {
    if frequency_hz >= 1.0e9 {
        format!("{:.2} GHz", frequency_hz / 1.0e9)
    } else if frequency_hz >= 1.0e6 {
        format!("{:.2} MHz", frequency_hz / 1.0e6)
    } else if frequency_hz >= 1.0e3 {
        format!("{:.2} kHz", frequency_hz / 1.0e3)
    } else {
        format!("{frequency_hz:.1} Hz")
    }
}

fn fft_frequency_at_x(plot: Rect, x: f32, start_frequency_hz: f64, end_frequency_hz: f64) -> f64 {
    let fraction = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    start_frequency_hz + fraction as f64 * (end_frequency_hz - start_frequency_hz)
}

fn fft_bin_at_x(plot: Rect, x: f32, first_bin: usize, last_bin: usize) -> usize {
    let fraction = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    first_bin + (fraction * (last_bin - first_bin) as f32).round() as usize
}

fn update_scope_fft_viewport(
    ui: &egui::Ui,
    rect: Rect,
    viewport: &mut ScopeFftViewport,
    max_bin: usize,
) -> egui::Response {
    let minimum_span = (10.0 / max_bin as f32).clamp(0.001, 1.0);
    viewport.span = viewport.span.clamp(minimum_span, 1.0);
    viewport.start = viewport.start.clamp(0.0, 1.0 - viewport.span);

    let response = ui.interact(
        rect,
        ui.make_persistent_id("scope_fft_viewport"),
        Sense::click_and_drag(),
    );
    let cursor = if response.dragged_by(egui::PointerButton::Primary) {
        egui::CursorIcon::Grabbing
    } else {
        egui::CursorIcon::Crosshair
    };
    let response = response.on_hover_cursor(cursor);
    if response.double_clicked() {
        viewport.reset();
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        let delta_x = ui.input(|input| input.pointer.delta().x);
        viewport.pan(delta_x, rect.width());
        ui.ctx().request_repaint();
    }
    if response.hovered() {
        let scroll_delta = ui.input(|input| input.raw_scroll_delta.y);
        if scroll_delta != 0.0 {
            let pointer_fraction = response
                .hover_pos()
                .map(|pointer| (pointer.x - rect.left()) / rect.width().max(1.0))
                .unwrap_or(0.5);
            viewport.zoom(scroll_delta, pointer_fraction, minimum_span);
            ui.ctx().request_repaint();
        }
    }
    response
}

struct ScopeFftPaintOptions<'a> {
    viewport: WaveformViewport,
    fft_viewport: &'a mut ScopeFftViewport,
    settings: ScopeFftSettings,
    channel_names: &'a [String; 2],
    square_grid_cells: bool,
}

fn paint_scope_fft(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    options: ScopeFftPaintOptions<'_>,
) {
    let ScopeFftPaintOptions {
        viewport,
        fft_viewport,
        settings,
        channel_names,
        square_grid_cells,
    } = options;
    const MIN_DBV: f32 = -120.0;
    const MAX_DBV: f32 = 20.0;
    let rect = if square_grid_cells {
        fit_rect_to_grid(rect, 10, 4)
    } else {
        rect
    };
    paint_plot_background(ui, rect);
    let painter = ui.painter().with_clip_rect(rect);
    paint_plot_grid(ui, rect);
    let Some(samples) = waveforms.scope_channels.get(settings.channel) else {
        paint_empty(&painter, rect, "Waiting for FFT source samples");
        return;
    };
    let visible_range = if waveforms.scope_rolling {
        viewport.sample_range_in_domain(samples.len(), waveforms.scope_capacity)
    } else {
        viewport.sample_range(samples.len())
    };
    let spectrum = scope_fft_spectrum(samples, visible_range, settings.length, settings.window);
    if spectrum.len() < 2 || waveforms.scope_rate_hz == 0 {
        paint_empty(&painter, rect, "Waiting for FFT source samples");
        return;
    }

    let nyquist = waveforms.scope_rate_hz as f64 / 2.0;
    let last_bin = spectrum.len() - 1;
    let max_bin = last_bin;
    let response = update_scope_fft_viewport(ui, rect, fft_viewport, max_bin);
    let first_visible_bin = (fft_viewport.start * max_bin as f32).floor() as usize;
    let last_visible_bin = ((fft_viewport.start + fft_viewport.span) * max_bin as f32)
        .ceil()
        .clamp((first_visible_bin + 1) as f32, max_bin as f32) as usize;
    let first_trace_bin = first_visible_bin.max(1);
    let usable_bins = &spectrum[first_trace_bin..=last_visible_bin];
    let bins_per_pixel = (usable_bins.len() as f32 / rect.width().max(1.0))
        .ceil()
        .max(1.0) as usize;
    let points = usable_bins
        .chunks(bins_per_pixel)
        .enumerate()
        .map(|(chunk_index, chunk)| {
            let bin = first_trace_bin + chunk_index * bins_per_pixel;
            let value = chunk.iter().copied().fold(MIN_DBV, f32::max);
            let x = rect.left()
                + (bin - first_visible_bin) as f32 / (last_visible_bin - first_visible_bin) as f32
                    * rect.width();
            let y = rect.bottom()
                - ((value.clamp(MIN_DBV, MAX_DBV) - MIN_DBV) / (MAX_DBV - MIN_DBV)) * rect.height();
            Pos2::new(x, y)
        })
        .collect::<Vec<_>>();
    if points.len() > 1 {
        painter.add(egui::Shape::line(
            points.clone(),
            Stroke::new(4.0_f32, SCOPE_COLORS[settings.channel].gamma_multiply(0.16)),
        ));
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.5_f32, SCOPE_COLORS[settings.channel]),
        ));
    }

    let fft_length = (spectrum.len() - 1) * 2;
    let start_frequency_hz = nyquist * fft_viewport.start as f64;
    let end_frequency_hz = nyquist * (fft_viewport.start + fft_viewport.span) as f64;
    painter.text(
        rect.left_top() + Vec2::new(8.0, 7.0),
        egui::Align2::LEFT_TOP,
        format!(
            "FFT · {} · {} · {fft_length} pts",
            scope_channel_label(settings.channel, channel_names),
            settings.window.label(),
        ),
        FontId::monospace(10.0),
        SCOPE_COLORS[settings.channel],
    );
    painter.text(
        rect.left_bottom() + Vec2::new(5.0, -4.0),
        egui::Align2::LEFT_BOTTOM,
        format_frequency(start_frequency_hz),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.center_bottom() + Vec2::new(0.0, -4.0),
        egui::Align2::CENTER_BOTTOM,
        format_frequency((start_frequency_hz + end_frequency_hz) * 0.5),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.right_bottom() + Vec2::new(-5.0, -4.0),
        egui::Align2::RIGHT_BOTTOM,
        format_frequency(end_frequency_hz),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.right_top() + Vec2::new(-5.0, 5.0),
        egui::Align2::RIGHT_TOP,
        format!("{MAX_DBV:.0} dBV"),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.right_bottom() + Vec2::new(-5.0, -18.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{MIN_DBV:.0} dBV"),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );

    if let Some(pointer) = response.hover_pos() {
        let frequency = fft_frequency_at_x(rect, pointer.x, start_frequency_hz, end_frequency_hz);
        let bin = fft_bin_at_x(rect, pointer.x, first_visible_bin, last_visible_bin);
        let magnitude_dbv = spectrum[bin];
        painter.line_segment(
            [
                Pos2::new(pointer.x, rect.top()),
                Pos2::new(pointer.x, rect.bottom()),
            ],
            Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.75)),
        );
        response.on_hover_text(format!(
            "Frequency: {}\nMagnitude: {magnitude_dbv:.2} dBV\nScroll to zoom · drag to pan · double-click to reset",
            format_frequency(frequency),
        ));
    }
}

fn scope_lissajous_position(
    plot: Rect,
    x_channel: ScopeChannelSettings,
    y_channel: ScopeChannelSettings,
    x_voltage: f32,
    y_voltage: f32,
) -> Pos2 {
    let x_volts_per_division =
        f32::from(x_channel.volts_per_div_mv) / 1_000.0 * f32::from(x_channel.probe_attenuation);
    let y_volts_per_division =
        f32::from(y_channel.volts_per_div_mv) / 1_000.0 * f32::from(y_channel.probe_attenuation);
    let x_span = x_volts_per_division * SCOPE_VERTICAL_DIVISIONS as f32;
    let y_span = y_volts_per_division * SCOPE_VERTICAL_DIVISIONS as f32;
    let x_fraction = scope_vertical_offset_ratio(x_channel.vertical_offset) + x_voltage / x_span;
    let y_fraction = scope_vertical_offset_ratio(y_channel.vertical_offset) - y_voltage / y_span;
    Pos2::new(
        plot.left() + x_fraction * plot.width(),
        plot.top() + y_fraction * plot.height(),
    )
}

fn paint_scope_xy_graticule(painter: &egui::Painter, plot: Rect) {
    for index in 0..=SCOPE_VERTICAL_DIVISIONS {
        let fraction = index as f32 / SCOPE_VERTICAL_DIVISIONS as f32;
        let x = egui::lerp(plot.x_range(), fraction);
        let y = egui::lerp(plot.y_range(), fraction);
        let center = index == SCOPE_VERTICAL_DIVISIONS / 2;
        let stroke = Stroke::new(
            if center { 1.2_f32 } else { 0.8_f32 },
            if center {
                TEXT_MUTED.gamma_multiply(0.45)
            } else {
                GRID.gamma_multiply(0.8)
            },
        );
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            stroke,
        );
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            stroke,
        );
    }
}

struct ScopeLissajousPaintOptions<'a> {
    viewport: WaveformViewport,
    settings: ScopeCaptureSettings,
    lissajous: ScopeLissajousSettings,
    channel_colors: [Color32; 2],
    channel_names: &'a [String; 2],
}

fn paint_scope_lissajous(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    options: ScopeLissajousPaintOptions<'_>,
) {
    let ScopeLissajousPaintOptions {
        viewport,
        settings,
        lissajous,
        channel_colors,
        channel_names,
    } = options;
    let panel_side = rect.width().min(rect.height()).max(1.0);
    let panel = Rect::from_center_size(rect.center(), Vec2::splat(panel_side));
    paint_plot_background(ui, panel);
    let painter = ui.painter();
    let header_bottom = (panel.top() + WAVEFORM_STATUS_HEIGHT).min(panel.bottom());
    let header = Rect::from_min_max(panel.left_top(), Pos2::new(panel.right(), header_bottom));
    painter.rect_filled(header, 4.0, PANEL.gamma_multiply(0.9));
    painter.line_segment(
        [header.left_bottom(), header.right_bottom()],
        Stroke::new(1.0_f32, GRID),
    );

    let x_index = lissajous.x_channel.min(settings.channels.len() - 1);
    let y_index = lissajous.y_channel.min(settings.channels.len() - 1);
    let x_channel = settings.channels[x_index];
    let y_channel = settings.channels[y_index];
    painter.text(
        header.left_center() + Vec2::new(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "LISSAJOUS · XY",
        FontId::monospace(10.0),
        ACCENT,
    );
    painter.text(
        header.right_center() + Vec2::new(-6.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        format!(
            "X {} · Y {}",
            scope_channel_label(x_index, channel_names),
            scope_channel_label(y_index, channel_names)
        ),
        FontId::monospace(10.0),
        TEXT_MUTED,
    );

    let available_plot = Rect::from_min_max(
        Pos2::new(panel.left() + 8.0, header.bottom() + 8.0),
        Pos2::new(panel.right() - 8.0, panel.bottom() - 8.0),
    );
    let side = available_plot.width().min(available_plot.height()).max(1.0);
    let plot = Rect::from_center_size(available_plot.center(), Vec2::splat(side));
    let plot_painter = painter.with_clip_rect(plot);
    paint_scope_xy_graticule(&plot_painter, plot);

    let Some(x_samples) = waveforms.scope_channels.get(x_index) else {
        paint_empty(&plot_painter, plot, "Waiting for X source samples");
        return;
    };
    let Some(y_samples) = waveforms.scope_channels.get(y_index) else {
        paint_empty(&plot_painter, plot, "Waiting for Y source samples");
        return;
    };
    let sample_count = x_samples.len().min(y_samples.len());
    let visible_range = if waveforms.scope_rolling {
        viewport.sample_range_in_domain(sample_count, waveforms.scope_capacity)
    } else {
        viewport.sample_range(sample_count)
    };
    if visible_range.is_empty() {
        paint_empty(&plot_painter, plot, "Waiting for two-channel samples");
        return;
    }
    let target_points = (plot.width() * 2.0).max(256.0) as usize;
    let step = (visible_range.len() / target_points).max(1);
    let segments = scope_plot_sample_segments(
        visible_range,
        step,
        usize::MAX,
        None,
        waveforms.scope_sample_offset,
        &waveforms.scope_capture_boundaries,
    );
    for segment in segments {
        let points = segment
            .into_iter()
            .filter_map(|index| {
                Some(scope_lissajous_position(
                    plot,
                    x_channel,
                    y_channel,
                    *x_samples.get(index)?,
                    *y_samples.get(index)?,
                ))
            })
            .collect::<Vec<_>>();
        if points.len() > 1 {
            plot_painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(5.0_f32, ACCENT.gamma_multiply(0.14)),
            ));
            plot_painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, ACCENT)));
        }
    }

    painter.text(
        plot.left_bottom() + Vec2::new(4.0, -4.0),
        egui::Align2::LEFT_BOTTOM,
        format!(
            "X · {} · {} · {}× probe",
            scope_channel_label(x_index, channel_names),
            format_input_scale(x_channel.volts_per_div_mv),
            x_channel.probe_attenuation
        ),
        FontId::monospace(9.0),
        channel_colors[x_index],
    );
    painter.text(
        plot.left_top() + Vec2::new(4.0, 4.0),
        egui::Align2::LEFT_TOP,
        format!(
            "Y · {} · {} · {}× probe",
            scope_channel_label(y_index, channel_names),
            format_input_scale(y_channel.volts_per_div_mv),
            y_channel.probe_attenuation
        ),
        FontId::monospace(9.0),
        channel_colors[y_index],
    );
}

fn paint_scope(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    viewport: &mut WaveformViewport,
    fft_viewport: &mut ScopeFftViewport,
    square_grid_cells: bool,
    scope_options: ScopeWaveformOptions<'_>,
) {
    let fft_settings = scope_options.function_settings.fft;
    let channel_names = scope_options.channel_names;
    if !fft_settings.enabled {
        paint_scope_time_domain(
            ui,
            rect,
            waveforms,
            viewport,
            square_grid_cells,
            scope_options,
        );
        return;
    }

    const FFT_GAP: f32 = 8.0;
    let desired_fft_height = (rect.height() * 0.36).clamp(80.0, 220.0);
    let fft_height = desired_fft_height.min((rect.height() - FFT_GAP - 64.0).max(32.0));
    let time_height = (rect.height() - FFT_GAP - fft_height).max(0.0);
    let time_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), time_height));
    let fft_rect = Rect::from_min_size(
        Pos2::new(rect.left(), time_rect.bottom() + FFT_GAP),
        Vec2::new(rect.width(), fft_height),
    );
    paint_scope_time_domain(
        ui,
        time_rect,
        waveforms,
        viewport,
        square_grid_cells,
        scope_options,
    );
    paint_scope_fft(
        ui,
        fft_rect,
        waveforms,
        ScopeFftPaintOptions {
            viewport: *viewport,
            fft_viewport,
            settings: fft_settings,
            channel_names,
            square_grid_cells,
        },
    );
}

fn scope_plot_rect(area: Rect, square_grid_cells: bool) -> Rect {
    if square_grid_cells {
        fit_rect_to_grid(area, SCOPE_HORIZONTAL_DIVISIONS, SCOPE_VERTICAL_DIVISIONS)
    } else {
        area
    }
}

fn paint_scope_time_domain(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    viewport: &mut WaveformViewport,
    square_grid_cells: bool,
    scope_options: ScopeWaveformOptions<'_>,
) {
    let ScopeWaveformOptions {
        mut settings,
        auto_sample_rate,
        smart_timing_enabled,
        function_settings,
        channel_names,
        measurement_caches,
        measurement_window_positions,
        marker_settings,
        quick_time_measurement,
        channel_colors,
        vertical_offsets,
        trigger_settings,
        time_per_division_seconds,
        timebase_changed,
        offset_controls_enabled,
        trigger_controls_enabled,
        time_axis,
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
    let display_sample_count = if waveforms.scope_rolling {
        waveforms.scope_capacity
    } else if sample_count == 0 {
        settings.sample_count as usize
    } else {
        sample_count
    };
    let display_sample_rate_hz = if waveforms.scope_rate_hz == 0 {
        settings.sample_rate_hz
    } else {
        waveforms.scope_rate_hz
    };
    if let Some(selected_timebase) = *time_per_division_seconds {
        apply_time_per_division(
            viewport,
            display_sample_count,
            display_sample_rate_hz,
            selected_timebase,
        );
    }
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    let (status_area, ruler_area, plot_area) = split_waveform_chrome(rect);
    let plot = scope_plot_rect(plot_area, square_grid_cells);
    let (status_bar, time_ruler) = align_waveform_chrome(status_area, ruler_area, plot);
    let scope_painter = painter.with_clip_rect(plot);
    paint_scope_graticule(&scope_painter, plot);
    let trigger_crossing = scope_trigger_crossing_sample(waveforms, settings, *trigger_settings);
    let trigger_sample_fraction = trigger_crossing
        .map(|crossing| crossing.sample)
        .or_else(|| {
            waveforms
                .scope_trigger_sample_index
                .map(|sample| sample as f64)
        })
        .map(|sample| {
            scope_sample_fraction(sample, waveforms.scope_sample_offset, display_sample_count)
        })
        .unwrap_or_else(|| f32::from(settings.trigger.position_percent) / 100.0);
    let trigger_screen_fraction = f32::from(trigger_settings.position_percent) / 100.0;
    if should_align_scope_trigger(
        viewport.anchor_locked,
        waveforms.scope_rolling,
        trigger_crossing.is_some(),
    ) {
        viewport.align_trigger_to_screen(trigger_sample_fraction, trigger_screen_fraction);
    }
    let (viewport_panned, viewport_timebase_changed, waveform_click, requested_timebase) =
        update_waveform_viewport(
            ui,
            plot,
            "scope_waveform_zoom",
            viewport,
            display_sample_count,
            display_sample_rate_hz,
            WaveformViewportOptions {
                reset_on_double_click: false,
                primary_pan_enabled: true,
                zoom_anchor: WaveformZoomAnchor::Sample {
                    domain_fraction: trigger_sample_fraction,
                    screen_fraction: trigger_screen_fraction,
                },
                timebase_basis: *time_per_division_seconds,
            },
        );
    if let Some(click) = waveform_click {
        match click {
            WaveformPlotClick::Primary {
                double_clicked: true,
                position,
            } => quick_time_measurement
                .start(viewport.domain_fraction_at_screen(position).clamp(0.0, 1.0)),
            WaveformPlotClick::Primary {
                double_clicked: false,
                position,
            } => quick_time_measurement
                .finish(viewport.domain_fraction_at_screen(position).clamp(0.0, 1.0)),
            WaveformPlotClick::Secondary => quick_time_measurement.clear(),
        }
    }
    *timebase_changed |= viewport_timebase_changed || requested_timebase.is_some();
    if viewport_panned && !waveforms.scope_rolling {
        finish_scope_viewport_pan(trigger_sample_fraction, viewport, trigger_settings);
    }
    let displayed_timebase =
        viewport.time_per_division(display_sample_count, display_sample_rate_hz);
    *time_per_division_seconds = resolve_scope_timebase(
        *time_per_division_seconds,
        displayed_timebase,
        requested_timebase,
        viewport.span,
        auto_sample_rate,
    );
    for (channel, vertical_offset) in settings.channels.iter_mut().zip(*vertical_offsets) {
        channel.vertical_offset = vertical_offset;
    }
    settings.trigger = *trigger_settings;

    let visible_range = viewport.sample_range_in_domain(sample_count, display_sample_count);
    let visible_count = visible_range.len();
    if visible_count == 0 {
        paint_empty(&scope_painter, plot, "Waiting for oscilloscope samples");
        paint_scope_offset_arrows(
            ui,
            plot,
            ScopeOffsetArrowOptions {
                settings,
                channel_colors,
                channel_names,
                vertical_offsets,
                controls_enabled: offset_controls_enabled,
            },
        );
        paint_scope_trigger_level(
            ui,
            plot,
            settings,
            trigger_settings,
            channel_colors,
            vertical_offsets,
            trigger_controls_enabled,
        );
        paint_scope_markers(
            ui,
            &scope_painter,
            plot,
            ScopeMarkerPaintOptions {
                settings: marker_settings,
                scope_settings: settings,
                channel_names,
                sample_count: display_sample_count,
                sample_rate_hz: display_sample_rate_hz,
                viewport: *viewport,
            },
        );
        paint_scope_quick_time_measurement(
            &scope_painter,
            plot,
            *quick_time_measurement,
            display_sample_count,
            display_sample_rate_hz,
            *viewport,
        );
        paint_waveform_time_scale(
            painter,
            time_ruler,
            display_sample_count,
            display_sample_rate_hz,
            waveforms.scope_sample_offset,
            *viewport,
            time_axis,
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
            trigger_controls_enabled,
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
        let color = channel_colors[channel_index.min(channel_colors.len() - 1)];
        if step >= SCOPE_DOWNSAMPLE_MIN_SAMPLES_PER_PIXEL {
            let segments = scope_downsampled_segments(
                channel,
                visible_range.clone(),
                step,
                waveforms.scope_sample_offset,
                &waveforms.scope_capture_boundaries,
            );
            paint_scope_downsampled_segments(
                &scope_painter,
                plot,
                &segments,
                channel_settings,
                *viewport,
                display_sample_count,
                color,
            );
            continue;
        }
        let segments = scope_plot_sample_segments(
            visible_range.clone(),
            1,
            channel_index,
            trigger_crossing,
            waveforms.scope_sample_offset,
            &waveforms.scope_capture_boundaries,
        );
        for segment in segments {
            let points = segment
                .into_iter()
                .map(|index| {
                    let x = waveform_sample_x(plot, *viewport, index as f64, display_sample_count);
                    let y = scope_sample_y(plot, channel_settings, channel[index]);
                    Pos2::new(x, y)
                })
                .collect::<Vec<_>>();
            if points.len() > 1 {
                scope_painter.add(egui::Shape::line(
                    points.clone(),
                    Stroke::new(4.0_f32, color.gamma_multiply(0.16)),
                ));
                scope_painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, color)));
            }
        }
    }

    paint_scope_math_trace(
        &scope_painter,
        plot,
        waveforms,
        ScopeMathPaintOptions {
            viewport: *viewport,
            sample_count: display_sample_count,
            visible_range: visible_range.clone(),
            step,
            settings: function_settings.math,
            channel_names,
        },
    );

    // Keep channel and trigger references in the foreground so dense or bright
    // waveforms never obscure their arrows, labels, or dotted level lines.
    paint_scope_offset_arrows(
        ui,
        plot,
        ScopeOffsetArrowOptions {
            settings,
            channel_colors,
            channel_names,
            vertical_offsets,
            controls_enabled: offset_controls_enabled,
        },
    );
    paint_scope_trigger_level(
        ui,
        plot,
        settings,
        trigger_settings,
        channel_colors,
        vertical_offsets,
        trigger_controls_enabled,
    );
    paint_scope_markers(
        ui,
        &scope_painter,
        plot,
        ScopeMarkerPaintOptions {
            settings: marker_settings,
            scope_settings: settings,
            channel_names,
            sample_count: display_sample_count,
            sample_rate_hz: display_sample_rate_hz,
            viewport: *viewport,
        },
    );
    paint_scope_quick_time_measurement(
        &scope_painter,
        plot,
        *quick_time_measurement,
        display_sample_count,
        display_sample_rate_hz,
        *viewport,
    );
    paint_scope_measurements(
        ui,
        &scope_painter,
        plot,
        waveforms,
        ScopeMeasurementPaintOptions {
            visible_range,
            sample_rate_hz: display_sample_rate_hz,
            settings: function_settings,
            enabled_channels: settings.channels.map(|channel| channel.enabled),
            channel_colors,
            channel_names,
            caches: measurement_caches,
            window_positions: measurement_window_positions,
        },
    );
    let smart_timing = smart_timing_enabled
        .then(|| ui.input(|input| input.pointer.hover_pos()))
        .flatten()
        .zip(
            trigger_crossing
                .filter(|crossing| scope_smart_timing_has_nonzero_trigger(settings, *crossing)),
        )
        .and_then(|(pointer, crossing)| {
            scope_smart_timing_measurement(
                waveforms,
                crossing,
                plot,
                *viewport,
                display_sample_count,
                pointer,
            )
        });
    if let Some(measurement) = smart_timing {
        paint_scope_timing_hint(
            &scope_painter,
            plot,
            waveforms,
            measurement,
            ScopeSmartTimingPaintOptions {
                settings,
                viewport: *viewport,
                display_sample_count,
                sample_rate_hz: display_sample_rate_hz,
                color: channel_colors[measurement.channel_index],
            },
        );
    }
    paint_waveform_time_scale(
        painter,
        time_ruler,
        display_sample_count,
        display_sample_rate_hz,
        waveforms.scope_sample_offset,
        *viewport,
        time_axis,
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
        trigger_controls_enabled,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use wavecraft_core::{Iq8, IqBatch, LogicBatch, ScopeBatch};
    use wavecraft_sdr::RadioProviderAdapter;

    #[test]
    fn motor_controller_example_recording_opens_and_decodes() {
        let recording = include_bytes!("../../../docs/examples/motor-controller-uart.dsv");
        let schema =
            include_str!("../../../docs/examples/motor-controller.wavecraft-protocol.yaml");
        let mut analyzers = ProtocolAnalyzerSettings::default();
        assert!(analyzers.begin_add("1:uart"));
        let editor = analyzers.editor.clone().unwrap();
        analyzers.save_editor(editor);
        analyzers
            .begin_attach_custom_yaml(
                0,
                PathBuf::from("motor-controller.wavecraft-protocol.yaml"),
                schema.to_owned(),
            )
            .unwrap();
        let editor = analyzers.editor.clone().unwrap();
        analyzers.save_editor(editor);

        let loaded = read_gui_recording_with_analyzers(
            Cursor::new(recording.as_slice()),
            &analyzers.analyzers,
        )
        .unwrap();
        assert_eq!(loaded.completion, CompletionStatus::Complete);
        assert_eq!(loaded.logic_channel_names[0], "UART RX");

        let mut cache = ProtocolDecodeCache::default();
        let snapshot = loaded.capture.protocol_decode.snapshot();
        let ProtocolDecodeOutcome::Events(events) = &cache.outcomes(
            &loaded.capture.waveforms,
            &analyzers.analyzers,
            Some(&snapshot),
        )[0] else {
            panic!("example recording should decode");
        };
        assert_eq!(
            events
                .iter()
                .map(|event| event.text.as_str())
                .collect::<Vec<_>>(),
            ["Motor 1: 1200 rpm", "Motor 2: 2000 rpm", "Motor 3: 750 rpm"]
        );
    }

    #[test]
    fn streamed_protocol_history_survives_waveform_ring_eviction() {
        let analyzer = ProtocolAnalyzerInstance {
            instance_id: 7,
            protocol_id: "1:uart",
            channel_mappings: vec![Some(0)],
            data_format: ProtocolDataFormat::Ascii,
            custom_protocol: None,
        };
        let samples_per_bit = 10;
        let mut samples = vec![1_u32; samples_per_bit * 2];
        for byte in b"Hi" {
            samples.extend(std::iter::repeat(0).take(samples_per_bit));
            for bit in 0..8 {
                let level = u32::from(byte & (1 << bit) != 0);
                samples.extend(std::iter::repeat(level).take(samples_per_bit));
            }
            samples.extend(std::iter::repeat(1).take(samples_per_bit * 2));
        }

        let mut decoder = ProtocolCaptureDecoder::new(std::slice::from_ref(&analyzer));
        for (sequence, chunk) in samples.chunks(7).enumerate() {
            decoder.ingest_logic_frame("logic", sequence as u64, chunk, 1_152_000);
        }
        decoder.finish();

        let mut waveforms = WaveformStore::default();
        waveforms.configure_logic_capture(1);
        waveforms.ingest_logic_samples(&samples);
        assert!(waveforms.logic_sample_offset > 0);
        assert_eq!(waveforms.logic_sample_count, 1);

        let mut cache = ProtocolDecodeCache::default();
        let snapshot = decoder.snapshot();
        let ProtocolDecodeOutcome::Events(events) =
            &cache.outcomes(&waveforms, std::slice::from_ref(&analyzer), Some(&snapshot))[0]
        else {
            panic!("streamed UART history should remain available");
        };
        assert_eq!(
            events
                .iter()
                .map(|event| event.text.as_str())
                .collect::<Vec<_>>(),
            ["'H'", "'i'"]
        );
    }

    #[test]
    fn session_tabs_preserve_selected_device_ids() {
        let first = LogicSessionSettings {
            device_id: Some("usb-logic-a".to_owned()),
            ..LogicSessionSettings::default()
        };
        let mut sessions = SessionTabs::<(), _>::new(first);
        let second = LogicSessionSettings {
            device_id: Some("usb-logic-b".to_owned()),
            ..LogicSessionSettings::default()
        };

        assert!(sessions.add(second));
        assert_eq!(
            sessions.active_settings().device_id.as_deref(),
            Some("usb-logic-b")
        );
        assert!(sessions.select(0));
        assert_eq!(
            sessions.active_settings().device_id.as_deref(),
            Some("usb-logic-a")
        );
    }

    #[test]
    fn radio_settings_follow_selected_device_capabilities() {
        let capabilities = wavecraft_hackrf::hackrf_one_capabilities();
        let mut settings = RadioSettings::default();
        settings.capture.center_frequency_hz = 100;
        settings.capture.sample_rate_hz = 21_000_000;
        settings.capture.bandwidth_hz = 8_100_000;
        settings.capture.receive_channel = 7;
        settings.gains_db.insert("lna".to_owned(), 39);
        settings.gains_db.insert("stale".to_owned(), 10);

        normalize_radio_settings(&mut settings, &capabilities);

        assert_eq!(settings.capture.center_frequency_hz, 1_000_000);
        assert_eq!(settings.capture.sample_rate_hz, 20_000_000);
        assert_eq!(settings.capture.bandwidth_hz, 8_000_000);
        assert_eq!(settings.capture.receive_channel, 0);
        assert_eq!(settings.gains_db.get("lna"), Some(&40));
        assert_eq!(settings.gains_db.get("vga"), Some(&16));
        assert!(!settings.gains_db.contains_key("stale"));
        settings
            .capture
            .validate(&capabilities)
            .expect("normalized radio settings should validate");
    }

    #[test]
    fn radio_runtime_settings_include_only_live_hackrf_controls() {
        let capabilities = wavecraft_hackrf::hackrf_one_capabilities();
        let mut initial = RadioSettings::default();
        normalize_radio_settings(&mut initial, &capabilities);
        let mut edited = initial.clone();
        edited.capture.sample_rate_hz = 12_000_000;
        assert!(!radio_runtime_settings_differ(&edited, &initial));

        edited.capture.center_frequency_hz = 433_920_000;
        edited.capture.bandwidth_hz = 8_000_000;
        edited.gains_db.insert("lna".to_owned(), 24);
        edited.gains_db.insert("vga".to_owned(), 20);
        edited.amplifier_enabled = true;
        assert!(radio_runtime_settings_differ(&edited, &initial));
        assert_eq!(
            hackrf_runtime_settings(&edited).unwrap(),
            HackRfRuntimeSettings {
                center_frequency_hz: 433_920_000,
                bandwidth_hz: 8_000_000,
                lna_gain_db: Some(24),
                vga_gain_db: Some(20),
                amplifier_enabled: true,
            }
        );
    }

    #[test]
    fn radio_analyzer_settings_build_a_tagged_signal_profile() {
        let mut settings = RadioAnalyzerSettings::default();
        assert!(settings.signal_profile().unwrap().is_none());

        settings.enabled = true;
        settings.demodulation = RadioDemodulation::BinaryGfsk;
        settings.channel_offset_hz = 125_000;
        settings.gfsk_bt_milli = 500;
        let profile = settings
            .signal_profile()
            .unwrap()
            .expect("enabled analyzer should build a profile");

        assert_eq!(profile.channel.frequency_offset_hz, 125_000);
        let DemodulationProfile::BinaryGfsk(gfsk) = profile.demodulation else {
            panic!("GFSK settings should build the GFSK profile variant");
        };
        assert_eq!(gfsk.symbol_rate_hz, 100_000);
        assert_eq!(gfsk.deviation_hz, 50_000);
        assert_eq!(gfsk.bt, 0.5);
        assert!(matches!(gfsk.timing, FskTiming::Adaptive { .. }));
    }

    #[test]
    fn radio_analyzer_settings_round_trip_from_recording_metadata() {
        let metadata = BTreeMap::from([
            ("radio.analyzer.enabled".to_owned(), "true".to_owned()),
            ("radio.analyzer.demodulation".to_owned(), "gfsk2".to_owned()),
            (
                "radio.analyzer.channel_offset_hz".to_owned(),
                "-125000".to_owned(),
            ),
            (
                "radio.analyzer.channel_bandwidth_hz".to_owned(),
                "250000".to_owned(),
            ),
            (
                "radio.analyzer.output_sample_rate_hz".to_owned(),
                "1000000".to_owned(),
            ),
            (
                "radio.analyzer.filter_tap_count".to_owned(),
                "257".to_owned(),
            ),
            (
                "radio.analyzer.symbol_rate_hz".to_owned(),
                "100000".to_owned(),
            ),
            ("radio.analyzer.deviation_hz".to_owned(), "50000".to_owned()),
            ("radio.analyzer.gfsk_bt_milli".to_owned(), "300".to_owned()),
            ("radio.analyzer.inverted".to_owned(), "true".to_owned()),
            (
                "radio.analyzer.adaptive_timing".to_owned(),
                "false".to_owned(),
            ),
            (
                "radio.analyzer.timing_loop_bandwidth_milli".to_owned(),
                "50".to_owned(),
            ),
            (
                "radio.analyzer.max_clock_error_ppm".to_owned(),
                "20000".to_owned(),
            ),
        ]);

        let settings = radio_settings_from_recording_metadata(&metadata).analyzer;

        assert!(settings.enabled);
        assert_eq!(settings.demodulation, RadioDemodulation::BinaryGfsk);
        assert_eq!(settings.channel_offset_hz, -125_000);
        assert_eq!(settings.channel_bandwidth_hz, 250_000);
        assert_eq!(settings.output_sample_rate_hz, 1_000_000);
        assert_eq!(settings.filter_tap_count, 257);
        assert_eq!(settings.symbol_rate_hz, 100_000);
        assert_eq!(settings.deviation_hz, 50_000);
        assert_eq!(settings.gfsk_bt_milli, 300);
        assert!(settings.inverted);
        assert!(!settings.adaptive_timing);
        assert_eq!(settings.timing_loop_bandwidth_milli, 50);
        assert_eq!(settings.max_clock_error_ppm, 20_000);
    }

    #[test]
    fn radio_iq_pipeline_runs_the_selected_analyzer() {
        let mut settings = RadioAnalyzerSettings {
            enabled: true,
            channel_offset_hz: 10_000,
            channel_bandwidth_hz: 4_000,
            output_sample_rate_hz: 8_000,
            filter_tap_count: 65,
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            adaptive_timing: false,
            ..RadioAnalyzerSettings::default()
        };
        settings.demodulation = RadioDemodulation::BinaryFsk;
        let profile = settings.signal_profile().unwrap().unwrap();
        let registry = DemodulatorRegistry::with_builtins().unwrap();
        let mut analyzer = Some(SignalAnalyzer::new(profile, &registry).unwrap());
        let mut spectrum =
            SpectrumProcessor::new(RADIO_SPECTRUM_FFT_LENGTH, SpectrumWindow::Hann).unwrap();
        let bits = [true, false, true, true, false, false, true];
        let mut phase: f64 = 0.0;
        let samples = bits
            .into_iter()
            .flat_map(|bit| {
                let frequency_hz = 10_000.0 + if bit { 1_000.0 } else { -1_000.0 };
                (0..64).map(move |_| frequency_hz)
            })
            .map(|frequency_hz| {
                let sample = Iq8 {
                    i: (100.0 * phase.cos()).round() as i8,
                    q: (100.0 * phase.sin()).round() as i8,
                };
                phase += std::f64::consts::TAU * frequency_hz / 64_000.0;
                sample
            })
            .collect();
        let mut waveforms = WaveformStore::default();

        ingest_radio_iq(
            &mut waveforms,
            &mut spectrum,
            &mut analyzer,
            &IqBatch {
                sample_rate_hz: 64_000,
                center_frequency_hz: 100_000_000,
                first_sample_index: 0,
                discontinuity: false,
                samples,
            },
        );

        assert!(analyzer.is_some());
        assert_eq!(
            waveforms.radio_analysis.soft_symbol_alphabet_size(),
            Some(2)
        );
        assert_eq!(waveforms.radio_analysis.soft_symbols().len(), 5);
        assert_eq!(
            waveforms
                .radio_analysis
                .soft_symbols()
                .iter()
                .map(|symbol| symbol.decision())
                .collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(1), Some(0), Some(0)]
        );
        assert_eq!(waveforms.radio_analysis.error(), None);
    }

    #[test]
    fn displayed_hackrf_id_omits_serial_leading_zeroes() {
        let radio = DiscoveredRadio {
            descriptor: DeviceDescriptor {
                id: DeviceId::new("hackrf:0000000000001234"),
                name: "HackRF One".to_owned(),
                kind: InstrumentKind::RadioReceiver,
            },
            provider: RadioProvider::HackRf,
            model: wavecraft_sdr::RadioModel::HackRfOne,
            connection_backend: wavecraft_sdr::ConnectionBackend::Usb,
            persistent_id: Some("0000000000001234".to_owned()),
            capabilities: wavecraft_hackrf::hackrf_one_capabilities(),
        };

        assert_eq!(displayed_radio_id(&radio), "hackrf:1234");
    }

    #[test]
    fn stepped_radio_domains_choose_the_nearest_supported_value() {
        let domain = ValueDomain::Stepped {
            range: 10..=30,
            step: 8,
        };

        assert_eq!(nearest_domain_value(&domain, 9), 10);
        assert_eq!(nearest_domain_value(&domain, 15), 18);
        assert_eq!(nearest_domain_value(&domain, 23), 26);
        assert_eq!(nearest_domain_value(&domain, 100), 26);
    }

    #[test]
    fn radio_values_convert_fractional_mhz_to_hz() {
        let domain = ValueDomain::Stepped {
            range: 1_000_000..=6_000_000_000,
            step: 1,
        };

        assert_eq!(radio_hz_from_mhz(&domain, 433.920_125), 433_920_125);
        assert_eq!(radio_hz_from_mhz(&domain, 0.5), 1_000_000);
        assert_eq!(radio_hz_from_mhz(&domain, 6_100.0), 6_000_000_000);

        let sample_rate_domain = ValueDomain::Stepped {
            range: 2_000_000..=20_000_000,
            step: 1,
        };
        assert_eq!(radio_hz_from_mhz(&sample_rate_domain, 12.5), 12_500_000);
    }

    #[test]
    fn radio_mhz_numbers_omit_the_unit_and_trailing_zeroes() {
        assert_eq!(format_radio_mhz_number(1_750_000), "1.75");
        assert_eq!(format_radio_mhz_number(5_500_000), "5.5");
        assert_eq!(format_radio_mhz_number(20_000_000), "20");
    }

    #[test]
    fn combined_session_settings_preserve_both_device_ids() {
        let settings = CombinedSessionSettings {
            logic: LogicSessionSettings {
                device_id: Some("usb-logic".to_owned()),
                ..LogicSessionSettings::default()
            },
            scope: ScopeSessionSettings {
                device_id: Some("usb-scope".to_owned()),
                ..ScopeSessionSettings::default()
            },
            ..CombinedSessionSettings::default()
        };
        let restored = settings.clone();

        assert_eq!(restored.logic.device_id.as_deref(), Some("usb-logic"));
        assert_eq!(restored.scope.device_id.as_deref(), Some("usb-scope"));
    }

    #[test]
    fn logic_capture_defaults_to_stream_mode() {
        assert_eq!(
            LogicCaptureSettings::default().operation_mode,
            DsLogicOperationMode::Stream
        );
    }

    #[test]
    fn i2c_decode_lane_is_inserted_above_mapped_sda_channel() {
        let analyzer = ProtocolAnalyzerInstance {
            instance_id: 0,
            protocol_id: "1:i2c",
            channel_mappings: vec![Some(0), Some(5)],
            data_format: ProtocolDataFormat::Hexadecimal,
            custom_protocol: None,
        };
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 800.0));
        let maximum_heights = LogicCaptureSettings::default().maximum_heights;

        let (layout, lanes) = logic_protocol_lane_layout(plot, &maximum_heights, &[analyzer]);

        assert_eq!(lanes.len(), 1);
        assert!(lanes[0].top() > layout.levels[4].1);
        assert!(lanes[0].bottom() < layout.levels[5].0);
    }

    #[test]
    fn decoded_panel_groups_i2c_bytes_into_one_transaction() {
        let event = |kind, text: &str, start_sample, end_sample, warning| ProtocolDecodedEvent {
            start_sample,
            end_sample,
            text: text.to_owned(),
            detail: text.to_owned(),
            warning,
            kind,
        };
        let events = vec![
            event(
                ProtocolDecodedKind::I2cAddress {
                    address: 0x50,
                    read: false,
                },
                "ADDR 50 W",
                10,
                20,
                false,
            ),
            event(
                ProtocolDecodedKind::Acknowledge { acknowledged: true },
                "ACK",
                20,
                22,
                false,
            ),
            event(
                ProtocolDecodedKind::Data { value: 0x12 },
                "12",
                23,
                31,
                false,
            ),
            event(
                ProtocolDecodedKind::Acknowledge {
                    acknowledged: false,
                },
                "NACK",
                31,
                33,
                true,
            ),
        ];

        let entries = decoded_panel_entries(&events);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "I²C WRITE · address 0x50");
        assert!(entries[0].detail.contains("Address 0x50 (W) → ACK"));
        assert!(entries[0].detail.contains("Data 0x12 → NACK"));
        assert!(entries[0].warning);
        assert_eq!((entries[0].start_sample, entries[0].end_sample), (10, 33));
    }

    #[test]
    fn decoded_panel_lists_newest_operations_first() {
        let event = |value, start_sample| ProtocolDecodedEvent {
            start_sample,
            end_sample: start_sample + 9,
            text: format!("{value:02X}"),
            detail: String::new(),
            warning: false,
            kind: ProtocolDecodedKind::Uart { value },
        };

        let entries = decoded_panel_entries(&[event(0x11, 10), event(0x22, 30)]);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].start_sample, 30);
        assert_eq!(entries[1].start_sample, 10);
    }

    #[test]
    fn decoded_panel_renders_uart_ascii_as_readable_text() {
        let events = b"Hello\r\n"
            .iter()
            .copied()
            .enumerate()
            .map(|(index, value)| ProtocolDecodedEvent {
                start_sample: index as u64 * 10,
                end_sample: index as u64 * 10 + 9,
                text: char::from(value).to_string(),
                detail: String::new(),
                warning: false,
                kind: ProtocolDecodedKind::Uart { value },
            })
            .collect::<Vec<_>>();

        assert_eq!(uart_ascii_text(&events).as_deref(), Some("Hello\r\n"));
    }

    #[test]
    fn decoded_panel_timestamp_does_not_reset_when_the_waveform_buffer_moves() {
        let event_start_sample = 2_000_000;
        let waveform_sample_offset = 1_500_000;
        let sample_rate_hz = 1_000_000;

        let elapsed = protocol_event_elapsed(event_start_sample, sample_rate_hz)
            .expect("a nonzero sample rate should produce a timestamp");

        assert_eq!(elapsed, Duration::from_secs(2));
        assert_ne!(
            elapsed,
            Duration::from_secs_f64(
                event_start_sample.saturating_sub(waveform_sample_offset) as f64
                    / sample_rate_hz as f64,
            )
        );
    }

    #[test]
    fn protocol_analyzer_panel_renders_catalog_and_channel_mapping() {
        let context = egui::Context::default();
        let mut settings = ProtocolAnalyzerSettings::default();
        settings.picker_open = true;
        settings.search = "SPI flash".to_owned();
        assert!(settings.begin_add("spiflash"));
        let editor = settings.editor.clone().unwrap();
        settings.save_editor(editor);
        let enabled = [true; LOGIC_CHANNEL_COUNT];

        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_protocol_analyzer_settings(ui, &mut settings, &enabled);
            });
        });

        assert!(!output.shapes.is_empty());
        assert_eq!(settings.analyzers[0].channel_mappings.len(), 4);
    }

    #[test]
    fn protocol_settings_modal_renders_without_committing_the_draft() {
        let context = egui::Context::default();
        let mut settings = ProtocolAnalyzerSettings::default();
        assert!(settings.begin_add("i2cdemux"));
        let names = empty_logic_channel_names();
        let enabled = [true; LOGIC_CHANNEL_COUNT];

        let output = context.run(egui::RawInput::default(), |context| {
            paint_protocol_analyzer_modal(context, &mut settings, &names, &enabled);
        });

        assert!(!output.shapes.is_empty());
        assert!(settings.editor.is_some());
        assert!(settings.analyzers.is_empty());
    }

    #[test]
    fn logic_capture_menu_offers_five_milliseconds_at_125_megahertz() {
        let option = logic_capture_duration_options(LogicCaptureSettings {
            sample_rate_hz: 125_000_000,
            ..LogicCaptureSettings::default()
        })
        .into_iter()
        .find(|(duration, _)| (*duration - 0.005).abs() < f64::EPSILON)
        .expect("5 ms should be available");

        assert_eq!(option.1, 625_664);
        let settings = LogicCaptureSettings {
            sample_rate_hz: 125_000_000,
            sample_count: option.1,
            ..LogicCaptureSettings::default()
        };
        assert_eq!(logic_capture_duration_label(settings), "5.00 ms");
    }

    #[test]
    fn changing_logic_sample_rate_preserves_capture_duration() {
        let mut settings = LogicCaptureSettings {
            sample_rate_hz: 125_000_000,
            sample_count: 625_664,
            ..LogicCaptureSettings::default()
        };

        set_logic_sample_rate_preserving_duration(&mut settings, 250_000_000);

        assert_eq!(settings.sample_rate_hz, 250_000_000);
        assert_eq!(settings.sample_count, 1_250_304);
        assert_eq!(logic_capture_duration_label(settings), "5.00 ms");
    }

    #[test]
    fn stream_duration_choices_do_not_shrink_at_high_sample_rates() {
        let settings = LogicCaptureSettings {
            sample_rate_hz: 1_000_000_000,
            enabled_channels: [
                true, true, true, false, false, false, false, false, false, false, false, false,
                false, false, false, false,
            ],
            ..LogicCaptureSettings::default()
        };

        let ten_seconds = logic_capture_duration_options(settings)
            .into_iter()
            .find(|(duration, _)| (*duration - 10.0).abs() < f64::EPSILON)
            .expect("10 s should remain available in stream mode");

        assert_eq!(ten_seconds.1, 10_000_000_000);
    }

    #[test]
    fn switching_to_buffer_mode_clamps_capture_to_hardware_depth() {
        let mut settings = LogicCaptureSettings {
            sample_rate_hz: 125_000_000,
            sample_count: 1_250_000_128,
            ..LogicCaptureSettings::default()
        };

        set_logic_operation_mode_preserving_duration(&mut settings, DsLogicOperationMode::Buffer);

        assert_eq!(settings.sample_count, 134_217_728);
    }

    #[test]
    fn logic_settings_map_every_device_option() {
        let mut enabled_channels = [false; LOGIC_CHANNEL_COUNT];
        enabled_channels[0] = true;
        enabled_channels[3] = true;
        let mut trigger_conditions = [None; LOGIC_CHANNEL_COUNT];
        trigger_conditions[0] = Some(DsLogicTriggerCondition::RisingEdge);
        trigger_conditions[3] = Some(DsLogicTriggerCondition::FallingEdge);
        let settings = LogicCaptureSettings {
            sample_rate_hz: 250_000_000,
            sample_count: 625_664,
            enabled_channels,
            maximum_heights: [4; LOGIC_CHANNEL_COUNT],
            operation_mode: DsLogicOperationMode::Buffer,
            stop_option: DsLogicStopOption::StopImmediately,
            threshold_voltage: 2.5,
            input_filter: DsLogicInputFilter::OneSampleClock,
            rle_enabled: true,
            external_clock: true,
            falling_clock_edge: true,
            trigger_enabled: true,
            trigger_conditions,
            ..LogicCaptureSettings::default()
        };

        let config = settings
            .capture_config()
            .expect("logic settings should be valid");

        assert_eq!(config.operation_mode(), settings.operation_mode);
        assert_eq!(config.sample_rate_hz(), settings.sample_rate_hz);
        assert_eq!(config.sample_count(), settings.sample_count);
        assert_eq!(config.enabled_channels(), settings.enabled_channels);
        assert_eq!(config.stop_option(), settings.stop_option);
        assert_eq!(config.threshold_voltage(), settings.threshold_voltage);
        assert_eq!(config.input_filter(), settings.input_filter);
        assert!(config.rle_enabled());
        assert!(config.external_clock());
        assert!(config.falling_clock_edge());
        assert_eq!(
            config.trigger(),
            Some(DsLogicTriggerConfig::Simple(
                DsLogicSimpleTrigger::new(0, DsLogicTriggerCondition::RisingEdge)
                    .unwrap()
                    .with_channel_condition(3, DsLogicTriggerCondition::FallingEdge)
                    .unwrap()
            ))
        );
    }

    #[test]
    fn smart_logic_settings_build_ordered_hardware_stages() {
        let mut settings = LogicCaptureSettings {
            operation_mode: DsLogicOperationMode::Buffer,
            trigger_enabled: true,
            trigger_mode: LogicTriggerMode::Smart,
            smart_trigger_stage_count: 2,
            ..LogicCaptureSettings::default()
        };
        settings.smart_trigger_conditions =
            [[None; LOGIC_CHANNEL_COUNT]; DSLOGIC_TRIGGER_STAGE_COUNT];
        settings.smart_trigger_conditions[0][0] = Some(DsLogicTriggerCondition::RisingEdge);
        settings.smart_trigger_conditions[0][1] = Some(DsLogicTriggerCondition::High);
        settings.smart_trigger_conditions[1][2] = Some(DsLogicTriggerCondition::FallingEdge);

        let config = settings
            .capture_config()
            .expect("smart trigger settings should be valid");

        let first_stage = DsLogicTriggerStage::new(0, DsLogicTriggerCondition::RisingEdge)
            .unwrap()
            .with_channel_condition(1, DsLogicTriggerCondition::High)
            .unwrap();
        let second_stage =
            DsLogicTriggerStage::new(2, DsLogicTriggerCondition::FallingEdge).unwrap();
        assert_eq!(
            config.trigger(),
            Some(DsLogicTriggerConfig::Smart(
                DsLogicSmartTrigger::new(first_stage)
                    .with_stage(second_stage)
                    .unwrap()
            ))
        );
    }

    #[test]
    fn recording_metadata_restores_smart_trigger_stages() {
        let mut metadata = BTreeMap::new();
        metadata.insert("logic.operation_mode".to_owned(), "buffer".to_owned());
        metadata.insert("logic.trigger.enabled".to_owned(), "true".to_owned());
        metadata.insert("logic.trigger.mode".to_owned(), "smart".to_owned());
        metadata.insert("logic.trigger.stage_count".to_owned(), "2".to_owned());
        metadata.insert(
            "logic.trigger.stage0.ch0.condition".to_owned(),
            "rising_edge".to_owned(),
        );
        metadata.insert(
            "logic.trigger.stage0.ch1.condition".to_owned(),
            "high".to_owned(),
        );
        metadata.insert(
            "logic.trigger.stage1.ch2.condition".to_owned(),
            "falling_edge".to_owned(),
        );

        let settings = logic_settings_from_recording_metadata(&metadata);

        assert!(settings.trigger_enabled);
        assert_eq!(settings.trigger_mode, LogicTriggerMode::Smart);
        assert_eq!(settings.smart_trigger_stage_count, 2);
        assert_eq!(
            settings.smart_trigger_conditions[0][0],
            Some(DsLogicTriggerCondition::RisingEdge)
        );
        assert_eq!(
            settings.smart_trigger_conditions[0][1],
            Some(DsLogicTriggerCondition::High)
        );
        assert_eq!(
            settings.smart_trigger_conditions[1][2],
            Some(DsLogicTriggerCondition::FallingEdge)
        );
    }

    #[test]
    fn stream_mode_ignores_buffer_only_options() {
        let settings = LogicCaptureSettings {
            operation_mode: DsLogicOperationMode::Stream,
            stop_option: DsLogicStopOption::UploadCapturedData,
            rle_enabled: true,
            ..LogicCaptureSettings::default()
        };

        let config = settings
            .capture_config()
            .expect("logic settings should be valid");

        assert_eq!(config.stop_option(), DsLogicStopOption::StopImmediately);
        assert!(!config.rle_enabled());
    }

    #[test]
    fn gui_recording_reader_restores_full_capture_and_scope_settings() {
        let descriptors = vec![
            DeviceDescriptor {
                id: DeviceId::new("logic-test"),
                name: "Logic test".to_owned(),
                kind: InstrumentKind::LogicAnalyzer,
            },
            DeviceDescriptor {
                id: DeviceId::new("scope-test"),
                name: "Scope test".to_owned(),
                kind: InstrumentKind::Oscilloscope,
            },
        ];
        let mut metadata = BTreeMap::new();
        metadata.insert("scope.sample_rate_hz".to_owned(), "1000000".to_owned());
        metadata.insert("scope.sample_count".to_owned(), "1000000".to_owned());
        metadata.insert("scope.ch0.enabled".to_owned(), "false".to_owned());
        metadata.insert("scope.ch1.enabled".to_owned(), "true".to_owned());
        metadata.insert("scope.ch1.volts_per_div_mv".to_owned(), "500".to_owned());
        metadata.insert("scope.ch1.coupling".to_owned(), "ac".to_owned());
        metadata.insert("scope.ch1.probe_attenuation".to_owned(), "10".to_owned());
        metadata.insert("logic.operation_mode".to_owned(), "stream".to_owned());
        metadata.insert("logic.sample_rate_hz".to_owned(), "250000000".to_owned());
        metadata.insert("logic.sample_count".to_owned(), "1250304".to_owned());
        for channel in 0..LOGIC_CHANNEL_COUNT {
            metadata.insert(
                format!("logic.ch{channel}.enabled"),
                matches!(channel, 2 | 9).to_string(),
            );
            metadata.insert(
                format!("logic.ch{channel}.maximum_height"),
                (channel % 5 + 1).to_string(),
            );
        }
        metadata.insert(
            "logic.stop_option".to_owned(),
            "stop_immediately".to_owned(),
        );
        metadata.insert("logic.threshold_voltage".to_owned(), "2.5".to_owned());
        metadata.insert(
            "logic.input_filter".to_owned(),
            "one_sample_clock".to_owned(),
        );
        metadata.insert("logic.rle_enabled".to_owned(), "true".to_owned());
        metadata.insert("logic.external_clock".to_owned(), "true".to_owned());
        metadata.insert("logic.falling_clock_edge".to_owned(), "true".to_owned());
        metadata.insert("logic.trigger.enabled".to_owned(), "true".to_owned());
        metadata.insert("logic.trigger.mode".to_owned(), "simple".to_owned());
        metadata.insert("logic.trigger.channel".to_owned(), "9".to_owned());
        metadata.insert(
            "logic.trigger.condition".to_owned(),
            "falling_edge".to_owned(),
        );
        metadata.insert("logic.trigger.ch2.condition".to_owned(), "high".to_owned());
        metadata.insert("logic.ch0.name".to_owned(), "Clock".to_owned());
        metadata.insert("logic.ch1.name".to_owned(), "  Data  ".to_owned());
        metadata.insert("scope.ch0.name".to_owned(), "Input".to_owned());
        metadata.insert("scope.ch1.name".to_owned(), "Reference".to_owned());
        let mut bytes = Vec::new();
        {
            let mut writer = RecordingWriter::new(&mut bytes, &descriptors, metadata)
                .expect("recording header is valid");
            writer
                .write_event(&CaptureEvent::Frame(CaptureFrame {
                    device_id: DeviceId::new("logic-test"),
                    sequence: 0,
                    captured_at: Duration::ZERO,
                    payload: CaptureBatch::Logic(LogicBatch {
                        sample_rate_hz: 2_000_000,
                        channel_count: 4,
                        samples: vec![1, 2, 3],
                    }),
                }))
                .expect("logic frame is valid");
            writer
                .write_event(&CaptureEvent::Frame(CaptureFrame {
                    device_id: DeviceId::new("scope-test"),
                    sequence: 0,
                    captured_at: Duration::ZERO,
                    payload: CaptureBatch::Scope(ScopeBatch {
                        sample_rate_hz: 1_000_000,
                        trigger_sample_index: Some(1),
                        channels: vec![vec![0.25, -0.5]],
                    }),
                }))
                .expect("scope frame is valid");
            writer
                .finish(CompletionStatus::Complete)
                .expect("recording footer is valid");
        }

        let loaded = read_gui_recording(Cursor::new(bytes)).expect("recording should reopen");

        assert_eq!(loaded.completion, CompletionStatus::Complete);
        assert!(loaded.capture.has_logic);
        assert!(loaded.capture.has_scope);
        assert_eq!(loaded.capture.waveforms.logic_rate_hz, 2_000_000);
        assert_eq!(
            loaded.capture.waveforms.logic_samples,
            VecDeque::from(vec![
                LogicSampleBucket::new(1),
                LogicSampleBucket::new(2),
                LogicSampleBucket::new(3),
            ])
        );
        assert_eq!(loaded.capture.waveforms.logic_sample_count, 3);
        assert!(loaded.capture.waveforms.scope_channels[0].is_empty());
        assert_eq!(
            loaded.capture.waveforms.scope_channels[1],
            VecDeque::from(vec![0.25, -0.5])
        );
        let scope_settings = loaded.scope_settings.expect("scope metadata should load");
        assert!(!scope_settings.channels[0].enabled);
        assert!(scope_settings.channels[1].enabled);
        assert_eq!(scope_settings.channels[1].volts_per_div_mv, 500);
        assert_eq!(scope_settings.channels[1].coupling, DsCopeCoupling::Ac);
        assert_eq!(scope_settings.channels[1].probe_attenuation, 10);
        let logic_settings = loaded.logic_settings.expect("logic metadata should load");
        assert_eq!(logic_settings.operation_mode, DsLogicOperationMode::Stream);
        assert_eq!(logic_settings.sample_rate_hz, 250_000_000);
        assert_eq!(logic_settings.sample_count, 1_250_304);
        assert_eq!(
            logic_settings.enabled_channels,
            [
                false, false, true, false, false, false, false, false, false, true, false, false,
                false, false, false, false,
            ]
        );
        assert_eq!(
            logic_settings.stop_option,
            DsLogicStopOption::StopImmediately
        );
        assert_eq!(logic_settings.threshold_voltage, 2.5);
        assert_eq!(
            logic_settings.input_filter,
            DsLogicInputFilter::OneSampleClock
        );
        assert_eq!(
            logic_settings.maximum_heights,
            [1, 2, 3, 4, 5, 1, 2, 3, 4, 5, 1, 2, 3, 4, 5, 1]
        );
        assert!(logic_settings.rle_enabled);
        assert!(logic_settings.external_clock);
        assert!(logic_settings.falling_clock_edge);
        assert!(logic_settings.trigger_enabled);
        assert_eq!(
            logic_settings.trigger_conditions[2],
            Some(DsLogicTriggerCondition::High)
        );
        assert_eq!(
            logic_settings.trigger_conditions[9],
            Some(DsLogicTriggerCondition::FallingEdge)
        );
        assert_eq!(loaded.logic_channel_names[0], "Clock");
        assert_eq!(loaded.logic_channel_names[1], "Data");
        assert_eq!(loaded.scope_channel_names, ["Input", "Reference"]);
    }

    #[test]
    fn gui_recording_reader_restores_radio_iq_and_settings() {
        let descriptor = DeviceDescriptor {
            id: DeviceId::new("hackrf:test"),
            name: "HackRF One".to_owned(),
            kind: InstrumentKind::RadioReceiver,
        };
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "radio.center_frequency_hz".to_owned(),
            "433920000".to_owned(),
        );
        metadata.insert("radio.sample_rate_hz".to_owned(), "10000000".to_owned());
        metadata.insert("radio.bandwidth_hz".to_owned(), "8000000".to_owned());
        metadata.insert("radio.receive_channel".to_owned(), "0".to_owned());
        metadata.insert("radio.gain.lna_db".to_owned(), "24".to_owned());
        metadata.insert("radio.gain.vga_db".to_owned(), "20".to_owned());
        metadata.insert("radio.amplifier_enabled".to_owned(), "true".to_owned());
        metadata.insert("radio.iq_correction_enabled".to_owned(), "false".to_owned());
        metadata.insert("radio.antenna_power_enabled".to_owned(), "true".to_owned());
        let mut bytes = Vec::new();
        {
            let mut writer = RecordingWriter::new(&mut bytes, &[descriptor], metadata).unwrap();
            writer
                .write_event(&CaptureEvent::Frame(CaptureFrame {
                    device_id: DeviceId::new("hackrf:test"),
                    sequence: 0,
                    captured_at: Duration::ZERO,
                    payload: CaptureBatch::Iq(IqBatch {
                        sample_rate_hz: 10_000_000,
                        center_frequency_hz: 433_920_000,
                        first_sample_index: 0,
                        discontinuity: false,
                        samples: vec![Iq8 { i: 64, q: 0 }; RADIO_SPECTRUM_FFT_LENGTH],
                    }),
                }))
                .unwrap();
            writer.finish(CompletionStatus::Complete).unwrap();
        }

        let loaded = read_gui_recording(Cursor::new(bytes)).expect("radio recording should reopen");

        assert!(loaded.capture.has_radio);
        assert!(!loaded.capture.has_logic);
        assert!(!loaded.capture.has_scope);
        assert_eq!(loaded.capture.waveforms.radio.waterfall().len(), 1);
        let settings = loaded.radio_settings.expect("radio metadata should load");
        assert_eq!(settings.capture.center_frequency_hz, 433_920_000);
        assert_eq!(settings.capture.sample_rate_hz, 10_000_000);
        assert_eq!(settings.capture.bandwidth_hz, 8_000_000);
        assert_eq!(settings.gains_db.get("lna"), Some(&24));
        assert_eq!(settings.gains_db.get("vga"), Some(&20));
        assert!(!settings.iq_correction_enabled);
        assert!(settings.amplifier_enabled);
        assert!(settings.antenna_power_enabled);
    }

    #[test]
    #[ignore = "requires a connected HackRF One and installed libhackrf"]
    fn gui_hackrf_capture_pipeline_receives_and_spectrally_processes_iq() {
        let adapter = HackRfAdapter::new();
        let device = adapter
            .discover()
            .expect("GUI HackRF discovery should succeed")
            .into_iter()
            .next()
            .expect("a HackRF One should be connected");
        let request = RadioCaptureRequest {
            device,
            settings: RadioCaptureSettings {
                center_frequency_hz: 100_000_000,
                sample_rate_hz: 2_000_000,
                bandwidth_hz: 1_750_000,
                receive_channel: 0,
                sample_limit: Some(4_096),
            },
            provider_settings: RadioProviderSettings::HackRf(HackRfSettings::default()),
            iq_correction_enabled: true,
            analyzer_profile: None,
        };
        let logic_config = LogicCaptureSettings::default().capture_config().unwrap();
        let scope_config = ScopeCaptureSettings::default().capture_config().unwrap();
        let started = start_capture(
            Vec::new(),
            Some(request),
            Path::new("."),
            logic_config,
            scope_config,
            None,
        )
        .expect("GUI radio capture should start");
        let StartedCapture {
            session,
            mut radio_spectrum,
            ..
        } = started;
        let frame = match session
            .recv_timeout(Duration::from_secs(5))
            .expect("GUI radio capture should produce an event")
        {
            CaptureEvent::Frame(frame) => frame,
            CaptureEvent::SourceFailed { error, .. } => {
                panic!("GUI radio capture failed: {error}")
            }
            CaptureEvent::SourceStopped { .. } => {
                panic!("GUI radio capture stopped before producing IQ")
            }
        };
        let CaptureBatch::Iq(iq) = frame.payload else {
            panic!("GUI radio capture should emit IQ batches");
        };
        let spectra = radio_spectrum.push(&iq);

        assert!(!iq.samples.is_empty());
        assert_eq!(iq.sample_rate_hz, 2_000_000);
        assert!(!spectra.is_empty());
        assert!(spectra
            .iter()
            .all(|spectrum| spectrum.power_dbfs.len() == RADIO_SPECTRUM_FFT_LENGTH));
        session
            .stop()
            .expect("GUI radio capture should stop cleanly");
    }

    #[test]
    fn recordings_without_channel_names_use_hardware_identifiers() {
        let (logic_names, scope_names) = channel_names_from_recording_metadata(&BTreeMap::new());

        assert!(logic_names.iter().all(String::is_empty));
        assert!(scope_names.iter().all(String::is_empty));
        assert_eq!(channel_label("D", 0, &logic_names[0]), "D0");
        assert_eq!(scope_channel_label(1, &scope_names), "CH1");
    }

    #[test]
    fn recording_metadata_saves_normalized_channel_names() {
        let mut logic_names = empty_logic_channel_names();
        logic_names[0] = "  Clock  ".to_owned();
        let mut scope_names = empty_scope_channel_names();
        scope_names[1] = "Reference".to_owned();
        let mut metadata = BTreeMap::new();

        insert_channel_name_metadata(&mut metadata, &logic_names, &scope_names);

        assert_eq!(
            metadata.get("logic.ch0.name").map(String::as_str),
            Some("Clock")
        );
        assert_eq!(metadata.get("logic.ch1.name").map(String::as_str), Some(""));
        assert_eq!(
            metadata.get("scope.ch1.name").map(String::as_str),
            Some("Reference")
        );
    }

    #[test]
    fn gui_saleae_reader_restores_channels_names_and_samples() {
        let capture = SaleaeCapture {
            digital: Some(SaleaeDigitalCapture {
                sample_rate_hz: 1_000_000,
                samples: vec![0, 1, 3, 2],
                channels: vec![
                    SaleaeChannel {
                        index: 0,
                        name: "Clock".to_owned(),
                    },
                    SaleaeChannel {
                        index: 1,
                        name: "Data".to_owned(),
                    },
                ],
            }),
            analog_sample_rate_hz: Some(1_000_000),
            analog: vec![SaleaeAnalogChannel {
                index: 0,
                name: "Input".to_owned(),
                samples: vec![-1.0, 0.0, 1.0, 0.5],
                minimum_voltage: -2.0,
                maximum_voltage: 2.0,
            }],
        };
        let mut bytes = Cursor::new(Vec::new());
        write_saleae(&mut bytes, &capture).expect("Saleae capture should serialize");
        bytes.set_position(0);

        let loaded = read_gui_saleae(bytes).expect("Saleae capture should open");

        assert!(loaded.capture.has_logic);
        assert!(loaded.capture.has_scope);
        assert_eq!(loaded.logic_channel_names[0], "Clock");
        assert_eq!(loaded.logic_channel_names[1], "Data");
        assert_eq!(loaded.scope_channel_names[0], "Input");
        assert_eq!(loaded.capture.waveforms.logic_samples.len(), 4);
        assert_eq!(loaded.capture.waveforms.logic_samples[2].first, 3);
        assert_eq!(loaded.capture.waveforms.scope_channels[0].len(), 4);
        assert!((loaded.capture.waveforms.scope_channels[0][2] - 1.0).abs() < 0.002);
    }

    #[test]
    fn acquisition_modes_rearm_only_successful_continuous_runs() {
        for mode in [AcquisitionMode::Repetitive, AcquisitionMode::Run] {
            assert!(should_rearm_acquisition(
                mode,
                true,
                CompletionStatus::Complete,
                false,
            ));
        }
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Loop,
            true,
            CompletionStatus::Complete,
            false,
        ));
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Run,
            false,
            CompletionStatus::Complete,
            false,
        ));
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Run,
            true,
            CompletionStatus::Failed,
            true,
        ));
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Single,
            true,
            CompletionStatus::Complete,
            false,
        ));
    }

    #[test]
    fn continuous_scope_sequences_identify_each_hardware_frame() {
        assert!(is_scope_frame_start(0));
        assert!(!is_scope_frame_start(1));
        assert!(is_scope_frame_start(DSCOPE_FRAME_SEQUENCE_STRIDE));
        assert!(!is_scope_frame_start(DSCOPE_FRAME_SEQUENCE_STRIDE + 1));
    }

    #[test]
    fn repetitive_interval_matches_original_stream_and_buffer_behavior() {
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Stream, 7.5),
            Duration::from_millis(100)
        );
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Buffer, 1.5),
            Duration::from_millis(1_500)
        );
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Buffer, 0.0),
            Duration::from_millis(100)
        );
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Buffer, 20.0),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn trigger_controls_stay_enabled_while_continuous_capture_rearms() {
        assert!(scope_trigger_controls_are_enabled(true, true, true, None));
        assert!(scope_trigger_controls_are_enabled(
            false,
            true,
            true,
            Some(true)
        ));
        assert!(!scope_trigger_controls_are_enabled(true, false, true, None));
        assert!(!scope_trigger_controls_are_enabled(
            false,
            true,
            false,
            Some(false)
        ));
        assert!(scope_frontend_controls_are_enabled(true, true, true));
        assert!(!scope_frontend_controls_are_enabled(true, false, true));
        assert!(scope_frontend_controls_are_enabled(false, false, false));
    }

    #[test]
    fn manual_calibration_stays_available_during_continuous_capture() {
        assert!(manual_calibration_control_is_enabled(
            false,
            true,
            false,
            Some(AcquisitionMode::Run)
        ));
        assert!(manual_calibration_control_is_enabled(
            false, true, false, None
        ));
        assert!(!manual_calibration_control_is_enabled(
            true,
            true,
            false,
            Some(AcquisitionMode::Run)
        ));
        assert!(!manual_calibration_control_is_enabled(
            false,
            true,
            false,
            Some(AcquisitionMode::Single)
        ));
        assert!(!manual_calibration_control_is_enabled(
            false,
            true,
            true,
            Some(AcquisitionMode::Run)
        ));
    }

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
        assert_eq!(
            store.logic_samples.front(),
            Some(&LogicSampleBucket::new(10))
        );
        assert_eq!(store.logic_sample_count, MAX_WAVEFORM_SAMPLES);
        assert_eq!(store.logic_sample_offset, 10);
    }

    #[test]
    fn waveform_store_preserves_a_one_second_logic_domain_with_bounded_buckets() {
        let mut store = WaveformStore::default();
        store.configure_logic_capture(1_000_000_000);

        assert_eq!(store.logic_capacity, MAX_WAVEFORM_SAMPLES);
        assert_eq!(store.logic_sample_stride, 500);

        let mut samples = vec![0_u32; 1_000];
        samples[123] = 1;
        store.ingest(
            &CaptureBatch::Logic(LogicBatch {
                sample_rate_hz: 1_000_000_000,
                channel_count: 3,
                samples,
            }),
            [true, true],
        );

        assert_eq!(store.logic_sample_count, 1_000);
        assert_eq!(store.logic_samples.len(), 2);
        assert_eq!(store.logic_samples[0].sample_count, 500);
        assert_eq!(store.logic_samples[0].activity & 1, 1);
        assert_eq!(store.logic_sample_offset, 0);
    }

    #[test]
    fn loop_waveform_capacity_covers_the_configured_two_second_span() {
        let sample_rate_hz = 10_000_000_u64;
        let expected_sample_count = aligned_logic_sample_count(2.0, sample_rate_hz, u64::MAX)
            .expect("two seconds should fit the stream capture depth");
        let mut store = WaveformStore::default();

        store.configure_logic_capture(expected_sample_count);

        let represented_samples = store.logic_capacity * store.logic_sample_stride;
        assert!(store.logic_capacity <= MAX_WAVEFORM_SAMPLES);
        assert_eq!(store.logic_sample_stride, 11);
        assert!(represented_samples >= expected_sample_count as usize);
        assert!(represented_samples - (expected_sample_count as usize) < store.logic_sample_stride);
    }

    #[test]
    fn loop_restart_reuses_only_a_compatible_waveform_window() {
        let sample_rate_hz = 10_000_000_u64;
        let two_second_count = aligned_logic_sample_count(2.0, sample_rate_hz, u64::MAX).unwrap();
        let one_second_count = aligned_logic_sample_count(1.0, sample_rate_hz, u64::MAX).unwrap();
        let two_second_config = DsLogicCaptureConfig::new(sample_rate_hz, two_second_count)
            .unwrap()
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_loop_mode(true);
        let one_second_config = DsLogicCaptureConfig::new(sample_rate_hz, one_second_count)
            .unwrap()
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_loop_mode(true);
        let mut store = WaveformStore {
            logic_rate_hz: sample_rate_hz,
            ..WaveformStore::default()
        };
        store.configure_logic_capture(two_second_count);

        assert!(store.can_continue_logic_loop(two_second_config));
        assert!(!store.can_continue_logic_loop(one_second_config));
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
    fn combined_timeline_uses_timestamped_sample_overlap() {
        let mut store = WaveformStore::default();
        store.ingest_frame(
            &CaptureFrame {
                device_id: DeviceId::new("logic"),
                sequence: 0,
                captured_at: Duration::from_millis(10),
                payload: CaptureBatch::Logic(LogicBatch {
                    sample_rate_hz: 1_000,
                    channel_count: 1,
                    samples: vec![0; 100],
                }),
            },
            [true, true],
        );
        store.ingest_frame(
            &CaptureFrame {
                device_id: DeviceId::new("scope"),
                sequence: 0,
                captured_at: Duration::from_millis(20),
                payload: CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 2_000,
                    trigger_sample_index: None,
                    channels: vec![vec![0.0; 200], vec![0.0; 200]],
                }),
            },
            [true, true],
        );

        let timeline = store
            .combined_timeline()
            .expect("logic and scope ranges should overlap");
        assert!((timeline.logic.start_seconds - 0.010).abs() < f64::EPSILON);
        assert!((timeline.logic.end_seconds - 0.110).abs() < f64::EPSILON);
        assert!((timeline.scope.start_seconds - 0.020).abs() < f64::EPSILON);
        assert!((timeline.scope.end_seconds - 0.120).abs() < f64::EPSILON);
        assert!((timeline.shared.start_seconds - 0.020).abs() < f64::EPSILON);
        assert!((timeline.shared.end_seconds - 0.110).abs() < f64::EPSILON);
    }

    #[test]
    fn combined_timeline_maps_both_viewports_to_the_same_time_window() {
        let timeline = CombinedTimeline {
            shared: CaptureTimeRange {
                start_seconds: 0.020,
                end_seconds: 0.110,
            },
            logic: CaptureTimeRange {
                start_seconds: 0.010,
                end_seconds: 0.110,
            },
            scope: CaptureTimeRange {
                start_seconds: 0.020,
                end_seconds: 0.120,
            },
        };
        let shared = WaveformViewport {
            start: 0.25,
            span: 0.5,
            anchor_locked: false,
        };

        let logic = timeline.source_viewport(shared, timeline.logic);
        let scope = timeline.source_viewport(shared, timeline.scope);
        let logic_start = timeline.logic.start_seconds
            + f64::from(logic.start) * timeline.logic.duration_seconds();
        let scope_start = timeline.scope.start_seconds
            + f64::from(scope.start) * timeline.scope.duration_seconds();
        let logic_span = f64::from(logic.span) * timeline.logic.duration_seconds();
        let scope_span = f64::from(scope.span) * timeline.scope.duration_seconds();

        assert!((logic_start - scope_start).abs() < 1.0e-8);
        assert!((logic_span - scope_span).abs() < 1.0e-8);
        let round_trip = timeline.shared_viewport(logic, timeline.logic);
        assert!((round_trip.start - shared.start).abs() < 1.0e-6);
        assert!((round_trip.span - shared.span).abs() < 1.0e-6);
        assert_eq!(round_trip.anchor_locked, shared.anchor_locked);
    }

    #[test]
    fn waveform_store_keeps_trigger_coordinates_across_appended_captures() {
        let mut store = WaveformStore {
            scope_capacity: 3,
            ..WaveformStore::default()
        };
        for (capture_index, (trigger_sample_index, samples)) in
            [(Some(1), vec![0.0, 1.0]), (Some(0), vec![2.0, 3.0])]
                .into_iter()
                .enumerate()
        {
            if capture_index > 0 {
                store.mark_scope_capture_start([true, true]);
            }
            store.ingest(
                &CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 1_000_000,
                    trigger_sample_index,
                    channels: vec![samples.clone(), samples],
                }),
                [true, true],
            );
        }

        assert_eq!(store.scope_channels[0], VecDeque::from(vec![1.0, 2.0, 3.0]));
        assert_eq!(store.scope_trigger_sample_index, Some(2));
        assert_eq!(store.scope_sample_offset, 1);
        assert_eq!(store.scope_capture_boundaries, VecDeque::from([2]));
    }

    #[test]
    fn scope_plot_segments_do_not_join_independent_captures() {
        let segments = scope_plot_sample_segments(0..10, 4, 0, None, 100, &VecDeque::from([105]));

        assert_eq!(segments, vec![vec![0, 4], vec![5, 8, 9]]);
    }

    #[test]
    fn rolling_scope_discards_boundaries_evicted_from_the_ring() {
        let mut store = WaveformStore {
            scope_capacity: 3,
            scope_rolling: true,
            ..WaveformStore::default()
        };
        for samples in [vec![0.0, 1.0], vec![2.0, 3.0], vec![4.0, 5.0]] {
            store.mark_scope_capture_start([true, true]);
            store.ingest(
                &CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 1_000,
                    trigger_sample_index: None,
                    channels: vec![samples.clone(), samples],
                }),
                [true, true],
            );
        }

        assert_eq!(store.scope_sample_offset, 3);
        assert_eq!(store.scope_capture_boundaries, VecDeque::from([4]));
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
    fn scope_measurement_cache_tracks_new_samples_and_visible_ranges() {
        let mut store = WaveformStore {
            scope_capacity: 4,
            ..WaveformStore::default()
        };
        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000,
                trigger_sample_index: None,
                channels: vec![vec![-1.0, 1.0], vec![0.0, 0.0]],
            }),
            [true, true],
        );
        let mut cache = ScopeMeasurementCache::default();

        let first = cache.results(&store, 0, 0..2, 1_000);
        assert_eq!(
            first.get(measurements::ScopeMeasurementKind::Mean),
            Some(0.0)
        );
        assert_eq!(cache.key.unwrap().range_end, 2);

        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000,
                trigger_sample_index: None,
                channels: vec![vec![3.0, 3.0], vec![0.0, 0.0]],
            }),
            [true, true],
        );
        let updated = cache.results(&store, 0, 0..4, 1_000);
        assert_eq!(
            updated.get(measurements::ScopeMeasurementKind::Mean),
            Some(1.5)
        );

        let zoomed = cache.results(&store, 0, 2..4, 1_000);
        assert_eq!(
            zoomed.get(measurements::ScopeMeasurementKind::Mean),
            Some(3.0)
        );
    }

    #[test]
    fn scope_math_operations_use_captured_channel_values() {
        assert_eq!(ScopeMathOperation::Add.apply(6.0, 2.0), Some(8.0));
        assert_eq!(ScopeMathOperation::Subtract.apply(6.0, 2.0), Some(4.0));
        assert_eq!(ScopeMathOperation::Multiply.apply(6.0, 2.0), Some(12.0));
        assert_eq!(ScopeMathOperation::Divide.apply(6.0, 2.0), Some(3.0));
        assert_eq!(ScopeMathOperation::Divide.apply(6.0, 0.0), None);
    }

    #[test]
    fn scope_function_sources_follow_enabled_channels() {
        let mut settings = ScopeFunctionSettings {
            math: ScopeMathSettings {
                enabled: true,
                operation: ScopeMathOperation::Subtract,
                left_channel: 0,
                right_channel: 0,
            },
            fft: ScopeFftSettings {
                enabled: true,
                channel: 0,
                length: 4_096,
                window: ScopeFftWindow::Blackman,
            },
            lissajous: ScopeLissajousSettings {
                enabled: true,
                x_channel: 0,
                y_channel: 0,
            },
            measurements: ScopeMeasurementSettings::default(),
        };

        settings.clamp_to_channels([false, true]);

        assert_eq!(settings.math.left_channel, 1);
        assert_eq!(settings.math.right_channel, 1);
        assert_eq!(settings.fft.channel, 1);
        assert_eq!(settings.fft.window, ScopeFftWindow::Blackman);
        assert_eq!(settings.lissajous.x_channel, 1);
        assert_eq!(settings.lissajous.y_channel, 1);
        assert!(!settings.lissajous.enabled);
        assert!(settings
            .measurements
            .channels
            .iter()
            .all(|channel| channel.enabled));
    }

    #[test]
    fn lissajous_coordinates_use_each_channels_voltage_scale() {
        let plot = Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 800.0));
        let x_channel = ScopeChannelSettings {
            volts_per_div_mv: 1_000,
            ..ScopeChannelSettings::default()
        };
        let y_channel = ScopeChannelSettings {
            volts_per_div_mv: 500,
            ..ScopeChannelSettings::default()
        };

        let zero = scope_lissajous_position(plot, x_channel, y_channel, 0.0, 0.0);
        let expected_zero = plot.left_top()
            + Vec2::splat(
                scope_vertical_offset_ratio(DSCOPE_VERTICAL_OFFSET_CENTER) * plot.width(),
            );
        assert!((zero.x - expected_zero.x).abs() < 0.001);
        assert!((zero.y - expected_zero.y).abs() < 0.001);
        let point = scope_lissajous_position(plot, x_channel, y_channel, 1.0, 0.5);
        assert!((point.x - zero.x - 100.0).abs() < 0.001);
        assert!((zero.y - point.y - 100.0).abs() < 0.001);
    }

    #[test]
    fn waveform_grids_fit_square_division_cells() {
        let wide_area = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_600.0, 800.0));
        let scope_grid = fit_rect_to_grid(wide_area, 10, 8);
        assert!((scope_grid.width() - 1_000.0).abs() < 0.001);
        assert!((scope_grid.height() - 800.0).abs() < 0.001);
        assert!((scope_grid.center().x - wide_area.center().x).abs() < 0.001);
        assert!((scope_grid.width() / 10.0 - scope_grid.height() / 8.0).abs() < 0.001);
        assert_eq!(scope_plot_rect(wide_area, false), wide_area);
        assert_eq!(scope_plot_rect(wide_area, true), scope_grid);

        let square_area = Rect::from_min_size(Pos2::ZERO, Vec2::splat(800.0));
        let fft_grid = fit_rect_to_grid(square_area, 10, 4);
        assert!((fft_grid.width() - 800.0).abs() < 0.001);
        assert!((fft_grid.height() - 320.0).abs() < 0.001);
        assert!((fft_grid.width() / 10.0 - fft_grid.height() / 4.0).abs() < 0.001);
    }

    #[test]
    fn scope_fft_finds_the_input_tone() {
        let length = 1_024;
        let tone_bin = 64;
        let samples = (0..length)
            .map(|index| {
                (std::f32::consts::TAU * tone_bin as f32 * index as f32 / length as f32).sin()
            })
            .collect::<VecDeque<_>>();

        let spectrum = scope_fft_spectrum(&samples, 0..length, length, ScopeFftWindow::Hann);
        let peak_bin = spectrum
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(bin, _)| bin)
            .unwrap();

        assert_eq!(peak_bin, tone_bin);
        assert!((spectrum[peak_bin] + 3.0).abs() < 0.1);
    }

    #[test]
    fn scope_fft_supports_vendor_window_types() {
        let samples = (0..1_024)
            .map(|index| (std::f32::consts::TAU * 32.0 * index as f32 / 1_024.0).sin())
            .collect::<VecDeque<_>>();

        for window in [
            ScopeFftWindow::Rectangular,
            ScopeFftWindow::Hann,
            ScopeFftWindow::Hamming,
            ScopeFftWindow::Blackman,
            ScopeFftWindow::FlatTop,
        ] {
            let spectrum = scope_fft_spectrum(&samples, 0..1_024, 1_024, window);
            assert_eq!(spectrum.len(), 513);
            assert!(spectrum.iter().all(|magnitude| magnitude.is_finite()));
            assert_eq!(
                spectrum
                    .iter()
                    .enumerate()
                    .skip(1)
                    .max_by(|left, right| left.1.total_cmp(right.1))
                    .map(|(bin, _)| bin),
                Some(32),
            );
        }
    }

    #[test]
    fn scope_fft_mouse_position_maps_to_frequency() {
        let plot = Rect::from_min_size(Pos2::new(100.0, 20.0), Vec2::new(800.0, 200.0));

        assert_eq!(fft_frequency_at_x(plot, 100.0, 0.0, 500_000.0), 0.0);
        assert_eq!(fft_frequency_at_x(plot, 500.0, 0.0, 500_000.0), 250_000.0);
        assert_eq!(fft_frequency_at_x(plot, 900.0, 0.0, 500_000.0), 500_000.0);
        assert_eq!(fft_frequency_at_x(plot, 950.0, 0.0, 500_000.0), 500_000.0);
        assert_eq!(
            fft_frequency_at_x(plot, 100.0, 100_000.0, 200_000.0),
            100_000.0
        );
        assert_eq!(
            fft_frequency_at_x(plot, 900.0, 100_000.0, 200_000.0),
            200_000.0
        );
        assert_eq!(fft_bin_at_x(plot, 100.0, 128, 256), 128);
        assert_eq!(fft_bin_at_x(plot, 500.0, 128, 256), 192);
        assert_eq!(fft_bin_at_x(plot, 900.0, 128, 256), 256);
        assert_eq!(fft_bin_at_x(plot, 950.0, 128, 256), 256);
    }

    #[test]
    fn scope_fft_viewport_zooms_at_pointer_and_pans() {
        let mut viewport = ScopeFftViewport::default();

        viewport.zoom(1.0, 0.25, 0.01);
        assert!((viewport.span - 0.8).abs() < f32::EPSILON);
        assert!((viewport.start - 0.05).abs() < f32::EPSILON);

        viewport.pan(-100.0, 1_000.0);
        assert!((viewport.start - 0.13).abs() < f32::EPSILON);

        viewport.zoom(-1.0, 0.5, 0.01);
        assert_eq!(viewport, ScopeFftViewport::default());
    }

    #[test]
    fn waveform_tabs_require_their_connected_devices() {
        assert!(waveform_tabs(false, false, false, true).is_empty());
        assert_eq!(
            waveform_tabs(true, false, false, true),
            vec![WaveformTab::LogicAnalyzer]
        );
        assert_eq!(
            waveform_tabs(false, true, false, false),
            vec![WaveformTab::Oscilloscope]
        );
        assert_eq!(
            waveform_tabs(false, true, false, true),
            vec![WaveformTab::Oscilloscope, WaveformTab::Lissajous]
        );
        assert_eq!(
            waveform_tabs(true, true, false, false),
            vec![
                WaveformTab::LogicAnalyzer,
                WaveformTab::Oscilloscope,
                WaveformTab::Combined,
            ]
        );
        assert_eq!(
            waveform_tabs(true, true, false, true),
            vec![
                WaveformTab::LogicAnalyzer,
                WaveformTab::Oscilloscope,
                WaveformTab::Lissajous,
                WaveformTab::Combined,
            ]
        );
        assert_eq!(
            waveform_tabs(false, false, true, false),
            vec![WaveformTab::RadioReceiver]
        );
    }

    #[test]
    fn waveform_tabs_target_their_matching_instruments() {
        assert_eq!(
            WaveformTab::LogicAnalyzer.capture_target(),
            CaptureTarget::LogicAnalyzer
        );
        assert_eq!(
            WaveformTab::Oscilloscope.capture_target(),
            CaptureTarget::Oscilloscope
        );
        assert_eq!(
            WaveformTab::Lissajous.capture_target(),
            CaptureTarget::Oscilloscope
        );
        assert_eq!(
            WaveformTab::RadioReceiver.capture_target(),
            CaptureTarget::RadioReceiver
        );
        assert_eq!(
            WaveformTab::Combined.capture_target(),
            CaptureTarget::Combined
        );

        assert_eq!(
            WaveformTab::LogicAnalyzer.capture_settings_tab(),
            Some(CaptureSettingsTab::DsLogic)
        );
        assert_eq!(
            WaveformTab::Oscilloscope.capture_settings_tab(),
            Some(CaptureSettingsTab::DsCope)
        );
        assert_eq!(
            WaveformTab::Lissajous.capture_settings_tab(),
            Some(CaptureSettingsTab::DsCope)
        );
        assert_eq!(WaveformTab::RadioReceiver.capture_settings_tab(), None);
        assert_eq!(WaveformTab::Combined.capture_settings_tab(), None);
    }

    #[test]
    fn combined_capture_requires_both_instrument_types() {
        assert!(CaptureTarget::LogicAnalyzer.is_available(true, false, false));
        assert!(CaptureTarget::Oscilloscope.is_available(false, true, false));
        assert!(CaptureTarget::RadioReceiver.is_available(false, false, true));
        assert!(!CaptureTarget::Combined.is_available(true, false, false));
        assert!(!CaptureTarget::Combined.is_available(false, true, false));
        assert!(CaptureTarget::Combined.is_available(true, true, false));
    }

    #[test]
    fn scope_tools_panel_is_limited_to_scope_views() {
        assert!(scope_tools_panel_is_visible(Some(
            WaveformTab::Oscilloscope
        )));
        assert!(scope_tools_panel_is_visible(Some(WaveformTab::Lissajous)));
        assert!(!scope_tools_panel_is_visible(Some(
            WaveformTab::LogicAnalyzer
        )));
        assert!(!scope_tools_panel_is_visible(Some(WaveformTab::Combined)));
        assert!(!scope_tools_panel_is_visible(None));
    }

    #[test]
    fn logic_markers_normalize_reverse_drags_and_stop_at_twenty() {
        let mut settings = LogicMarkerSettings::default();
        for index in 0..LOGIC_MARKER_LIMIT {
            assert!(settings.arm());
            settings.start((index * 10 + 9) as u64);
            settings.update((index * 10) as u64);
            assert!(settings.finish(1_000));
        }

        assert_eq!(settings.markers.len(), LOGIC_MARKER_LIMIT);
        assert_eq!(settings.markers[0].start_sample, 0);
        assert_eq!(settings.markers[0].end_sample, 9);
        assert_eq!(settings.markers[0].duration(), Duration::from_millis(9));
        assert!(settings.markers[0].name.is_empty());
        assert_eq!(settings.markers[0].color, logic_marker_color(0));
        settings.markers[0].name = "Clock pulse".to_owned();
        settings.markers[0].color = Color32::RED;
        assert_eq!(settings.markers[0].name, "Clock pulse");
        assert_eq!(settings.markers[0].color, Color32::RED);
        assert!(!settings.arm());
    }

    #[test]
    fn zero_length_logic_marker_drag_remains_armed() {
        let mut settings = LogicMarkerSettings::default();
        assert!(settings.arm());
        settings.start(42);
        settings.update(42);

        assert!(!settings.finish(1_000_000));
        assert!(settings.adding);
        assert!(settings.markers.is_empty());
        assert!(settings.draft.is_none());
    }

    #[test]
    fn logic_marker_positions_snap_to_captured_samples() {
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 400.0));
        let viewport = WaveformViewport::default();

        assert_eq!(
            logic_sample_at_x(plot, viewport, 2_000, 101, 254.0),
            Some(2_025)
        );
        assert_eq!(
            logic_marker_x(plot, viewport, 2_000, 101, 2_025),
            Some(250.0)
        );
        assert_eq!(logic_marker_x(plot, viewport, 2_000, 101, 1_999), None);
    }

    #[test]
    fn logic_marker_edges_snap_on_the_channel_under_the_pointer() {
        let mut waveforms = WaveformStore {
            logic_channels: 2,
            logic_rate_hz: 1_000_000,
            ..WaveformStore::default()
        };
        waveforms.configure_logic_capture(101);
        let samples = (0..101)
            .map(|index| match index {
                0..20 => 0,
                20..80 => 0b01,
                _ => 0b11,
            })
            .collect::<Vec<_>>();
        waveforms.ingest_logic_samples(&samples);

        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 400.0));
        let viewport = WaveformViewport::default();
        let layout = logic_signal_layout(plot.top(), plot.height(), &[1; LOGIC_CHANNEL_COUNT]);
        let channel_y = |channel: usize| {
            let (high, low) = layout.levels[channel];
            (high + low) * 0.5
        };
        let enabled_channels = [true; LOGIC_CHANNEL_COUNT];

        assert_eq!(
            logic_marker_sample_at_pointer(
                plot,
                viewport,
                &waveforms,
                &layout.levels,
                &enabled_channels,
                Pos2::new(77.0, channel_y(1)),
            ),
            Some(80)
        );
        assert_eq!(
            logic_marker_sample_at_pointer(
                plot,
                viewport,
                &waveforms,
                &layout.levels,
                &enabled_channels,
                Pos2::new(77.0, channel_y(0)),
            ),
            Some(77)
        );
    }

    #[test]
    fn logic_smart_timing_follows_the_pulse_under_the_pointer() {
        let mut waveforms = WaveformStore {
            logic_channels: 1,
            logic_rate_hz: 1_000_000,
            ..WaveformStore::default()
        };
        waveforms.configure_logic_capture(101);
        let samples = (0..101)
            .map(|index| u32::from(matches!(index, 10..30 | 50..70 | 90..=100)))
            .collect::<Vec<_>>();
        waveforms.ingest_logic_samples(&samples);

        let measurement = logic_smart_timing_measurement(&waveforms, 0, 24).unwrap();
        assert_eq!(measurement.channel, 0);
        assert_eq!(measurement.edge_start_sample, 10);
        assert_eq!(measurement.edge_end_sample, 30);
        let cycle = measurement.cycle.unwrap();
        assert_eq!(cycle.period_start_sample, 10);
        assert_eq!(cycle.period_end_sample, 50);
        assert_eq!(cycle.period_samples, 40);
        assert_eq!(cycle.high_samples, 20);
        assert_eq!(
            cycle.period(waveforms.logic_rate_hz),
            Duration::from_micros(40)
        );
        assert_eq!(cycle.frequency_hz(waveforms.logic_rate_hz), 25_000.0);
        assert_eq!(cycle.duty_cycle_percent(), 50.0);

        let low_pulse = logic_smart_timing_measurement(&waveforms, 0, 35).unwrap();
        assert_eq!(low_pulse.edge_start_sample, 30);
        assert_eq!(low_pulse.edge_end_sample, 50);
        assert_eq!(low_pulse.cycle, Some(cycle));
        assert_eq!(logic_smart_timing_measurement(&waveforms, 0, 0), None);
    }

    #[test]
    fn logic_smart_timing_handles_edges_inside_downsampled_buckets() {
        let mut waveforms = WaveformStore {
            logic_channels: 1,
            logic_rate_hz: 1_000_000,
            logic_sample_stride: 4,
            ..WaveformStore::default()
        };
        let samples = (0..37)
            .map(|index| u32::from(matches!(index, 5..13 | 21..29)))
            .collect::<Vec<_>>();
        waveforms.ingest_logic_samples(&samples);

        let high_pulse = logic_smart_timing_measurement(&waveforms, 0, 9).unwrap();
        assert_eq!(high_pulse.edge_start_sample, 6);
        assert_eq!(high_pulse.edge_end_sample, 14);
        assert_eq!(
            high_pulse.cycle,
            Some(LogicTimingMeasurement {
                channel: 0,
                period_start_sample: 6,
                period_end_sample: 22,
                period_samples: 16,
                high_samples: 8,
            })
        );

        let low_pulse = logic_smart_timing_measurement(&waveforms, 0, 17).unwrap();
        assert_eq!(low_pulse.edge_start_sample, 14);
        assert_eq!(low_pulse.edge_end_sample, 22);
        assert_eq!(low_pulse.cycle, high_pulse.cycle);
    }

    #[test]
    fn logic_smart_timing_paint_is_clipped_to_the_waveform_plot() {
        let context = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let plot = Rect::from_min_max(Pos2::new(100.0, 100.0), Pos2::new(500.0, 400.0));
        let waveforms = WaveformStore {
            logic_channels: 1,
            logic_rate_hz: 1_000_000,
            logic_sample_count: 101,
            ..WaveformStore::default()
        };
        let measurement = LogicSmartTimingMeasurement {
            channel: 0,
            edge_start_sample: 10,
            edge_end_sample: 90,
            cycle: Some(LogicTimingMeasurement {
                channel: 0,
                period_start_sample: 10,
                period_end_sample: 90,
                period_samples: 80,
                high_samples: 40,
            }),
        };

        let output = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..egui::RawInput::default()
            },
            |context| {
                let painter = context.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("smart_timing_clip_test"),
                ));
                paint_logic_timing_hint(
                    &painter,
                    plot,
                    &waveforms,
                    WaveformViewport {
                        start: 0.4,
                        span: 0.2,
                        anchor_locked: false,
                    },
                    measurement,
                    ACCENT,
                );
            },
        );

        assert!(!output.shapes.is_empty());
        assert!(output.shapes.iter().all(|shape| shape.clip_rect == plot));
    }

    #[test]
    fn logic_timing_measurement_rejects_unresolved_bucket_activity() {
        let waveforms = WaveformStore {
            logic_samples: VecDeque::from([LogicSampleBucket::from_samples(&[0, 1, 0]).unwrap()]),
            logic_channels: 1,
            logic_rate_hz: 1_000_000,
            logic_sample_count: 3,
            logic_sample_stride: 3,
            ..WaveformStore::default()
        };

        assert_eq!(logic_smart_timing_measurement(&waveforms, 0, 1), None);

        let mut waveforms = WaveformStore {
            logic_channels: 1,
            logic_rate_hz: 1_000_000,
            logic_sample_stride: 4,
            ..WaveformStore::default()
        };
        waveforms.ingest_logic_samples(&[
            0, 0, 0, 0, // stable low
            0, 1, 0, 1, // three transitions cannot be located after compaction
            1, 1, 1, 1, // stable high under the pointer
            1, 0, 0, 0, // falling edge
            0, 0, 0, 0,
        ]);

        assert_ne!(waveforms.logic_samples[1].multiple_transitions & 1, 0);
        assert_eq!(logic_smart_timing_measurement(&waveforms, 0, 9), None);
    }

    #[test]
    fn logic_marker_drag_pans_the_zoomed_viewport_at_plot_edges() {
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 400.0));
        let mut viewport = WaveformViewport {
            start: 0.4,
            span: 0.2,
            anchor_locked: true,
        };

        assert!(pan_logic_marker_viewport_at_edge(
            &mut viewport,
            plot,
            plot.right()
        ));
        assert!(viewport.start > 0.4);
        let right_start = viewport.start;
        assert!(pan_logic_marker_viewport_at_edge(
            &mut viewport,
            plot,
            plot.left()
        ));
        assert!(viewport.start < right_start);
        assert!(!viewport.anchor_locked);

        viewport.start = 0.0;
        assert!(!pan_logic_marker_viewport_at_edge(
            &mut viewport,
            plot,
            plot.left()
        ));
    }

    #[test]
    fn capture_setup_tabs_follow_connected_devices() {
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
    fn capture_setup_stays_available_for_offline_capture_analysis() {
        assert_eq!(
            capture_settings_tabs_for_view(
                Some(WaveformTab::LogicAnalyzer),
                false,
                false,
                true,
                false,
            ),
            vec![CaptureSettingsTab::DsLogic]
        );
        assert_eq!(
            capture_settings_tabs_for_view(
                Some(WaveformTab::Oscilloscope),
                false,
                false,
                false,
                true,
            ),
            vec![CaptureSettingsTab::DsCope]
        );
        assert_eq!(
            capture_settings_tabs_for_view(Some(WaveformTab::Combined), false, false, true, true,),
            vec![CaptureSettingsTab::DsLogic, CaptureSettingsTab::DsCope]
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
    fn floating_measurement_windows_stay_relative_to_the_plot() {
        let plot = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 400.0));
        let size = Vec2::new(200.0, 100.0);

        let [channel_zero, channel_one] = default_scope_measurement_window_positions();
        let bottom_left = scope_measurement_window_rect(plot, size, channel_zero);
        assert_eq!(bottom_left.left_bottom(), plot.left_bottom());
        let bottom_right = scope_measurement_window_rect(plot, size, channel_one);
        assert_eq!(bottom_right.right_bottom(), plot.right_bottom());

        let top_right = scope_measurement_window_rect(plot, size, Vec2::new(1.0, 0.0));
        assert_eq!(top_right.right(), plot.right());
        assert_eq!(top_right.top(), plot.top());
        assert_eq!(
            scope_measurement_window_position(plot, top_right),
            Vec2::new(1.0, 0.0)
        );

        let center = scope_measurement_window_rect(plot, size, Vec2::new(0.5, 0.5));
        assert_eq!(center.center(), plot.center());
        assert_eq!(
            scope_measurement_window_position(plot, center),
            Vec2::new(0.5, 0.5)
        );
    }

    #[test]
    fn scope_markers_measure_horizontal_time_and_vertical_amplitude() {
        let markers = ScopeMarkerSettings::default();

        assert!(
            (scope_time_marker_delta(
                markers.channels[0].horizontal_positions[0],
                markers.channels[0].horizontal_positions[1],
                1_000,
                1_000,
            )
            .unwrap()
                - 0.1)
                .abs()
                < 0.000_001
        );
        assert!(
            (scope_voltage_marker_delta(
                markers.channels[0].vertical_positions[0],
                markers.channels[0].vertical_positions[1],
                ScopeCaptureSettings::default().channels[0]
            ) - 0.8)
                .abs()
                < 0.000_001
        );

        let mut scope_settings = ScopeCaptureSettings::default();
        scope_settings.channels[1].volts_per_div_mv = 500;
        scope_settings.channels[1].probe_attenuation = 10;
        assert!(
            (scope_voltage_marker_delta(
                markers.channels[1].vertical_positions[0],
                markers.channels[1].vertical_positions[1],
                scope_settings.channels[1]
            ) - 4.0)
                .abs()
                < 0.000_001
        );

        assert_ne!(
            markers.channels[0].horizontal_positions,
            markers.channels[1].horizontal_positions
        );
        assert_eq!(
            markers.channels[0].horizontal_positions.len(),
            SCOPE_MARKER_LIMIT
        );
        assert_eq!(
            markers.channels[1].vertical_positions.len(),
            SCOPE_MARKER_LIMIT
        );
        assert_eq!(format_marker_voltage(0.125), "125.00 mV");
    }

    #[test]
    fn quick_time_measurement_starts_then_finishes_with_clamped_positions() {
        let mut measurement = ScopeQuickTimeMeasurement::default();

        measurement.finish(0.8);
        assert_eq!(measurement, ScopeQuickTimeMeasurement::Idle);

        measurement.start(-0.2);
        assert_eq!(
            measurement,
            ScopeQuickTimeMeasurement::AwaitingSecond {
                first_position: 0.0,
            }
        );

        measurement.finish(1.2);
        assert_eq!(
            measurement,
            ScopeQuickTimeMeasurement::Complete {
                first_position: 0.0,
                second_position: 1.0,
            }
        );

        measurement.finish(0.4);
        assert_eq!(
            measurement,
            ScopeQuickTimeMeasurement::Complete {
                first_position: 0.0,
                second_position: 1.0,
            }
        );

        measurement.start(0.25);
        assert_eq!(
            measurement,
            ScopeQuickTimeMeasurement::AwaitingSecond {
                first_position: 0.25,
            }
        );

        measurement.clear();
        assert_eq!(measurement, ScopeQuickTimeMeasurement::Idle);
    }

    #[test]
    fn new_scope_markers_start_in_the_visible_pane_center() {
        let settings = ScopeMarkerSettings::default();
        let viewport = WaveformViewport {
            start: 0.6,
            span: 0.2,
            ..WaveformViewport::default()
        };

        let time = ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Time);
        let voltage = ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Voltage);

        assert!((time.position - 0.7).abs() < f32::EPSILON);
        assert!((voltage.position - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn scope_markers_are_saved_and_compared_individually() {
        let mut settings = ScopeMarkerSettings::default();
        let viewport = WaveformViewport::default();
        let mut first = ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Time);
        first.position = 0.2;
        first.color = Color32::RED;
        assert!(first.save(&mut settings));

        let mut second = ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Time);
        second.position = 0.7;
        second.comparison = Some(0);
        assert!(second.save(&mut settings));

        let channel = settings.channels[0];
        assert_eq!(channel.horizontal_count, 2);
        assert_eq!(channel.marker(ScopeMarkerKind::Time, 0).unwrap().0, 0.2);
        assert_eq!(
            channel.marker(ScopeMarkerKind::Time, 0).unwrap().2,
            Color32::RED
        );
        assert_eq!(channel.horizontal_comparisons[1], Some(0));
        assert!(
            (scope_time_marker_delta(
                channel.horizontal_positions[1],
                channel.horizontal_positions[0],
                1_000,
                1_000,
            )
            .unwrap()
                - 0.5)
                .abs()
                < 0.000_001
        );

        assert!(settings.channels[0].remove_marker(ScopeMarkerKind::Time, 0));
        assert_eq!(settings.channels[0].horizontal_count, 1);
        assert_eq!(settings.channels[0].horizontal_comparisons[0], None);
        assert_eq!(settings.channels[0].horizontal_positions[0], 0.7);
    }

    #[test]
    fn scope_marker_limit_is_shared_across_types_and_markers_can_change_type() {
        let mut settings = ScopeMarkerSettings::default();
        let viewport = WaveformViewport::default();
        for index in 0..SCOPE_MARKER_LIMIT {
            let kind = if index % 2 == 0 {
                ScopeMarkerKind::Time
            } else {
                ScopeMarkerKind::Voltage
            };
            let editor = ScopeMarkerEditor::for_new(settings, viewport, 0, kind);
            assert!(editor.save(&mut settings));
        }
        assert_eq!(settings.channels[0].total_count(), SCOPE_MARKER_LIMIT);
        assert!(
            !ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Time)
                .save(&mut settings)
        );

        let key = ScopeMarkerKey {
            channel: 0,
            kind: ScopeMarkerKind::Time,
            index: 0,
        };
        let mut editor = ScopeMarkerEditor::for_existing(settings, key).unwrap();
        editor.kind = ScopeMarkerKind::Voltage;
        assert!(editor.save(&mut settings));
        assert_eq!(settings.channels[0].total_count(), SCOPE_MARKER_LIMIT);
        assert_eq!(settings.channels[0].count(ScopeMarkerKind::Time), 4);
        assert_eq!(settings.channels[0].count(ScopeMarkerKind::Voltage), 6);
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
    fn default_resource_directory_contains_complete_upstream_resource_set() {
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
    fn logic_channel_markers_stay_compact_and_inside_their_lanes() {
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 320.0));
        let lane_height = 20.0;
        let marker = logic_channel_marker_rect(plot, lane_height * 0.75, lane_height, "15");
        let named_marker =
            logic_channel_marker_rect(plot, lane_height * 0.75, lane_height, "CLOCKCLK");

        assert_eq!(marker.width(), 26.0);
        assert_eq!(named_marker.width(), 62.0);
        assert!(marker.height() <= 13.0);
        assert!(marker.top() >= plot.top());
        assert!(marker.bottom() <= plot.top() + lane_height);
    }

    #[test]
    fn logic_maximum_height_uses_the_selected_multiple() {
        let mut maximum_heights = [1; LOGIC_CHANNEL_COUNT];

        for multiple in 1..=5 {
            maximum_heights[8] = multiple;
            let layout = logic_signal_layout(0.0, 320.0, &maximum_heights);
            let (high, low) = layout.levels[8];
            assert!((low - high - layout.base_height * f32::from(multiple)).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn logic_maximum_height_reflows_channels_inside_the_plot() {
        let height_patterns = [
            [1; LOGIC_CHANNEL_COUNT],
            [5; LOGIC_CHANNEL_COUNT],
            [5, 5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 5, 5],
            [5, 1, 5, 1, 5, 1, 5, 1, 5, 1, 5, 1, 5, 1, 5, 1],
        ];

        for maximum_heights in height_patterns {
            let layout = logic_signal_layout(40.0, 320.0, &maximum_heights);
            assert!(layout.levels[0].0 >= 40.0 + layout.base_height);
            assert!(layout.levels[LOGIC_CHANNEL_COUNT - 1].1 <= 360.0 - layout.base_height);
            for channel in 1..LOGIC_CHANNEL_COUNT {
                assert!(
                    layout.levels[channel - 1].1 + layout.base_height
                        <= layout.levels[channel].0 + f32::EPSILON
                );
            }
        }
    }

    #[test]
    fn logic_reflow_moves_neighbors_in_base_height_steps() {
        let defaults = logic_signal_layout(0.0, 320.0, &[1; LOGIC_CHANNEL_COUNT]);
        let mut maximum_heights = [1; LOGIC_CHANNEL_COUNT];
        maximum_heights[0] = 5;
        let enlarged_first = logic_signal_layout(0.0, 320.0, &maximum_heights);
        assert!((defaults.base_height - enlarged_first.base_height).abs() < f32::EPSILON);
        assert!(
            (enlarged_first.levels[1].0 - defaults.levels[1].0 - defaults.base_height).abs()
                < f32::EPSILON
        );

        maximum_heights = [1; LOGIC_CHANNEL_COUNT];
        maximum_heights[LOGIC_CHANNEL_COUNT - 1] = 5;
        let enlarged_last = logic_signal_layout(0.0, 320.0, &maximum_heights);
        let movement = (defaults.levels[14].0 - enlarged_last.levels[14].0) / defaults.base_height;
        assert!((movement - movement.round()).abs() < f32::EPSILON);
        assert!(movement >= 1.0);
    }

    #[test]
    fn logic_channel_marker_uses_name_limited_to_eight_characters() {
        assert_eq!(logic_channel_marker_text(3, ""), "3");
        assert_eq!(logic_channel_marker_text(3, "  CLOCK  "), "CLOCK");
        assert_eq!(logic_channel_marker_text(3, "DATA-LONG-NAME"), "DATA-LON");
    }

    #[test]
    fn logic_decimation_preserves_one_kilohertz_pulses_across_a_one_second_span() {
        let mut samples = VecDeque::from(vec![0_u32; 1_000_000]);
        for pulse in 0..1_000 {
            let start = pulse * 1_000 + 100;
            for sample in samples.range_mut(start..start + 20) {
                *sample = 1;
            }
        }
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 320.0));
        let mut enabled_channels = [false; LOGIC_CHANNEL_COUNT];
        enabled_channels[0] = true;
        let levels = logic_signal_layout(plot.top(), plot.height(), &[1; LOGIC_CHANNEL_COUNT]);

        let sample_count = samples.len();
        let sample_stride = 500;
        let buckets = samples
            .make_contiguous()
            .chunks(sample_stride)
            .filter_map(LogicSampleBucket::from_samples)
            .collect::<VecDeque<_>>();
        let points = logic_waveform_points(
            LogicSampleView {
                samples: &buckets,
                sample_count,
                sample_stride,
            },
            0..sample_count,
            plot,
            WaveformViewport::default(),
            &levels.levels,
            &enabled_channels,
        );
        let (high, low) = levels.levels[0];
        let pulses_in_first_200_ms = points[0]
            .windows(2)
            .filter(|edge| {
                edge[0].x < 200.0
                    && (edge[0].x - edge[1].x).abs() < f32::EPSILON
                    && edge[0].y == low
                    && edge[1].y == high
            })
            .count();

        assert_eq!(pulses_in_first_200_ms, 200);
    }

    #[test]
    fn logic_waveform_fills_the_plot_when_high_zoom_shows_one_sample() {
        let samples = VecDeque::from(vec![0_u32, 1, 1, 0]);
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 320.0));
        let viewport = WaveformViewport {
            start: 0.4,
            span: 0.01,
            anchor_locked: false,
        };
        let visible_range = viewport.sample_range(samples.len());
        assert_eq!(visible_range, 1..2);
        let mut enabled_channels = [false; LOGIC_CHANNEL_COUNT];
        enabled_channels[0] = true;
        let layout = logic_signal_layout(plot.top(), plot.height(), &[1; LOGIC_CHANNEL_COUNT]);
        let buckets = samples
            .iter()
            .copied()
            .filter_map(|sample| LogicSampleBucket::from_samples(&[sample]))
            .collect::<VecDeque<_>>();

        let points = logic_waveform_points(
            LogicSampleView {
                samples: &buckets,
                sample_count: samples.len(),
                sample_stride: 1,
            },
            visible_range,
            plot,
            viewport,
            &layout.levels,
            &enabled_channels,
        );

        assert_eq!(points[0].first().unwrap().x, plot.left());
        assert_eq!(points[0].last().unwrap().x, plot.right());
        assert_eq!(points[0].first().unwrap().y, points[0].last().unwrap().y);
    }

    #[test]
    fn high_zoom_keeps_a_visible_range_for_large_logic_captures() {
        let sample_count = 1_250_000_128;
        let viewport = WaveformViewport {
            start: 0.75,
            span: 1.0 / sample_count as f32,
            anchor_locked: false,
        };
        assert_eq!(viewport.start + viewport.span, viewport.start);

        let visible_range = viewport.sample_range(sample_count);

        assert!(!visible_range.is_empty());
        assert!(visible_range.len() <= 2);
    }

    #[test]
    fn waveform_zoom_preserves_the_sample_under_the_pointer() {
        let mut viewport = WaveformViewport::default();

        assert!(viewport.step_timebase(1.0, 1_000_000, 1_000_000, 0.25));

        assert!((viewport.span - 0.5).abs() < f32::EPSILON);
        assert!((viewport.start + viewport.span * 0.25 - 0.25).abs() < 0.000_001);
        assert!(!viewport.anchor_locked);
        assert!(viewport.sample_range(1_000).len() < 1_000);
        assert_eq!(viewport.time_per_division(1_000_000, 1_000_000), Some(0.05));
    }

    #[test]
    fn waveform_zoom_out_is_clamped_to_the_full_capture() {
        let mut viewport = WaveformViewport::default();
        viewport.step_timebase(1.0, 1_000_000, 1_000_000, 0.75);

        viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.75);
        assert!(!viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.25));

        assert_eq!(viewport.start, 0.0);
        assert_eq!(viewport.span, 1.0);
        assert!(!viewport.anchor_locked);
        assert_eq!(viewport.sample_range(1_000), 0..1_000);
    }

    #[test]
    fn scope_timebase_fits_when_selected_window_is_wider_than_the_capture() {
        let mut viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            ..WaveformViewport::default()
        };
        let selected_timebase = viewport.time_per_division(1_000_000, 1_000_000).unwrap();

        apply_time_per_division(&mut viewport, 250_000, 1_000_000, selected_timebase);

        assert_eq!(viewport.start, 0.0);
        assert_eq!(viewport.span, 1.0);
        assert!(
            (viewport.time_per_division(250_000, 1_000_000).unwrap() - 0.025).abs() < f64::EPSILON,
            "fit-to-capture should replace a wider persisted timebase"
        );
    }

    #[test]
    fn scope_auto_timebase_preserves_zoom_out_request_beyond_current_capture() {
        let displayed_timebase = 2.0e-3;
        let requested_timebase = adjacent_timebase(displayed_timebase, false).unwrap();
        assert_eq!(requested_timebase, 5.0e-3);

        let selected = resolve_scope_timebase(
            Some(displayed_timebase),
            Some(displayed_timebase),
            Some(requested_timebase),
            1.0,
            true,
        );
        assert_eq!(selected, Some(requested_timebase));
        assert_eq!(
            resolve_scope_timebase(selected, Some(displayed_timebase), None, 1.0, true),
            Some(requested_timebase),
            "the request must survive while the old capture is still displayed"
        );
        assert_eq!(
            resolve_scope_timebase(selected, Some(displayed_timebase), None, 1.0, false),
            Some(displayed_timebase),
            "manual sample rate remains limited by the captured buffer"
        );

        let settings = ScopeCaptureSettings::default();
        let adjusted_rate = settings.sample_rate_for_timebase(requested_timebase);
        assert_eq!(adjusted_rate, 20_000_000);
        let mut viewport = WaveformViewport::default();
        apply_time_per_division(
            &mut viewport,
            settings.sample_count as usize,
            adjusted_rate,
            requested_timebase,
        );
        assert_eq!(
            viewport.time_per_division(settings.sample_count as usize, adjusted_rate),
            Some(requested_timebase)
        );
    }

    #[test]
    fn rolling_scope_uses_the_full_span_as_its_sample_domain() {
        let viewport = WaveformViewport::default();
        let range = viewport.sample_range_in_domain(200_000, 1_000_000);
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 400.0));

        assert_eq!(range, 0..200_000);
        assert!((waveform_sample_x(plot, viewport, 199_999.0, 1_000_000) - 200.0).abs() < 0.01);
    }

    #[test]
    fn auto_trigger_does_not_imply_hardware_roll_mode() {
        assert!(!uses_rolling_scope_buffer(
            AcquisitionMode::Run,
            true,
            DsCopeTriggerSource::Auto,
        ));
        assert!(!uses_rolling_scope_buffer(
            AcquisitionMode::Single,
            true,
            DsCopeTriggerSource::Auto,
        ));
        assert!(!uses_rolling_scope_buffer(
            AcquisitionMode::Run,
            true,
            DsCopeTriggerSource::ChannelZero,
        ));

        let config = ScopeCaptureSettings::default().capture_config().unwrap();
        assert_eq!(scope_rolling_sample_capacity(config, Some(0.1)), 1_000_000);
        assert_eq!(
            scope_rolling_sample_capacity(config, Some(2.0)),
            SCOPE_ROLLING_MEMORY_BUDGET_BYTES / 2 / std::mem::size_of::<f32>()
        );
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
        assert_eq!(time_scale_label_stride(1_000.0), 1);
        assert_eq!(time_scale_label_stride(500.0), 2);
        assert_eq!(time_scale_label_stride(250.0), 5);
        assert_eq!(format_duration(Duration::from_nanos(2)), "2.0 ns");
    }

    #[test]
    fn timebase_steps_follow_the_vendor_one_two_five_sequence() {
        assert_eq!(TIMEBASE_STEPS_SECONDS.first(), Some(&2.0e-9));
        assert_eq!(TIMEBASE_STEPS_SECONDS.last(), Some(&10.0));
        assert!(TIMEBASE_STEPS_SECONDS.contains(&DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS));
        assert_eq!(adjacent_timebase(10.0e-9, true), Some(5.0e-9));
        assert_eq!(adjacent_timebase(10.0e-9, false), Some(20.0e-9));
        assert_eq!(adjacent_timebase(2.0e-9, true), None);
        assert_eq!(adjacent_timebase(10.0, false), None);

        let mut default_viewport = WaveformViewport::default();
        apply_time_per_division(
            &mut default_viewport,
            SAMPLE_COUNT as usize,
            SAMPLE_RATE_HZ,
            DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS,
        );
        assert!(
            (default_viewport
                .time_per_division(SAMPLE_COUNT as usize, SAMPLE_RATE_HZ)
                .unwrap()
                - DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS)
                .abs()
                < 1.0e-9
        );

        let mut viewport = WaveformViewport::default();
        assert!(!viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5));
        assert!(viewport.step_timebase(1.0, 1_000_000, 1_000_000, 0.5));
        assert_eq!(viewport.time_per_division(1_000_000, 1_000_000), Some(0.05));
        assert!(viewport.step_timebase(1.0, 1_000_000, 1_000_000, 0.5));
        assert!(
            (viewport.time_per_division(1_000_000, 1_000_000).unwrap() - 0.02).abs() < 0.000_001
        );
        assert!(viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5));
        assert!(viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5));
        assert_eq!(viewport.span, 1.0);
        assert_eq!(viewport.sample_range(1_000_000), 0..1_000_000);
        assert!(!viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5));
    }

    #[test]
    fn scope_timebase_selects_the_vendor_floor_sample_rate() {
        let mut settings = ScopeCaptureSettings::default();

        assert_eq!(settings.sample_rate_for_timebase(1.0e-3), 100_000_000);
        assert_eq!(settings.sample_rate_for_timebase(500.0e-6), 125_000_000);
        assert_eq!(settings.sample_rate_for_timebase(200.0e-6), 500_000_000);
        assert_eq!(settings.sample_rate_for_timebase(10.0), 10_000);
        assert_eq!(
            settings.sample_rate_for_timebase(f64::NAN),
            settings.sample_rate_hz
        );

        settings.channels[1].enabled = false;
        settings.sample_count = 2_000_000;
        assert_eq!(settings.sample_rate_for_timebase(200.0e-6), 1_000_000_000);
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
                    probe_attenuation: 100,
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
        assert_eq!(config.channels()[0].probe_attenuation(), 100);
        assert_eq!(
            config.channels()[0].vertical_offset(),
            DSCOPE_VERTICAL_OFFSET_CENTER
        );
        assert_eq!(config.channels()[1].volts_per_div_mv(), 2_000);
        assert_eq!(config.channels()[1].coupling(), DsCopeCoupling::Dc);
        assert_eq!(config.channels()[1].probe_attenuation(), 1);
        assert_eq!(
            config.channels()[1].vertical_offset(),
            DSCOPE_VERTICAL_OFFSET_CENTER
        );
        assert!(!config.channels()[1].enabled());
        assert_eq!(config.trigger().position_percent(), 75);
        assert_eq!(config.trigger().source(), DsCopeTriggerSource::ChannelZero);
        assert_eq!(config.trigger().slope(), DsCopeTriggerSlope::Falling);
        assert_eq!(config.trigger().holdoff_ns(), 250_000);
        assert_eq!(config.trigger().noise_sensitivity(), 12);
        assert_eq!(config.trigger().levels(), [64, 192]);
        assert_eq!(config.operation_mode(), DsCopeOperationMode::InternalTest);
        assert_eq!(config.bandwidth_limit(), DsCopeBandwidthLimit::Mhz20);
        let mut hardware_settings = settings;
        for channel in &mut hardware_settings.channels {
            channel.vertical_offset = DSCOPE_VERTICAL_OFFSET_CENTER;
        }
        assert_eq!(
            ScopeCaptureSettings::from_capture_config(config),
            hardware_settings
        );
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

        changed = original;
        changed.channels[1].enabled = false;
        assert!(changed.frontend_differs_from(original));
        assert!(changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.channels[0].volts_per_div_mv = 100;
        assert!(changed.frontend_differs_from(original));
        assert!(changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.channels[0].vertical_offset = 64;
        assert!(!changed.frontend_differs_from(original));
        assert!(!changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.channels[0].coupling = DsCopeCoupling::Ac;
        assert!(changed.frontend_differs_from(original));
        assert!(!changed.runtime_frontend_differs_from(original));

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
    fn scope_trigger_levels_use_centered_hardware_and_follow_display_offsets() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 20.0), Pos2::new(100.0, 220.0));
        let channel = ScopeChannelSettings::default();

        assert_eq!(scope_trigger_level_from_y(plot, plot.top(), channel), 34);
        assert_eq!(
            scope_trigger_level_from_y(plot, plot.bottom(), channel),
            222
        );
        assert_eq!(
            scope_trigger_level_from_y(plot, scope_trigger_level_y(plot, channel, 128), channel),
            128
        );
        assert!((scope_trigger_threshold_voltage(channel, 128)).abs() < f32::EPSILON);
        assert!((scope_trigger_threshold_voltage(channel, 104) - 1.021_276_6).abs() < 0.000_001);

        let shifted_channel = ScopeChannelSettings {
            vertical_offset: 160,
            ..channel
        };
        let shifted_zero_y = scope_vertical_offset_y(plot, 160);
        assert_eq!(
            scope_trigger_level_y(plot, shifted_channel, DSCOPE_TRIGGER_LEVEL_CENTER),
            shifted_zero_y
        );
        assert_eq!(
            scope_trigger_level_from_y(plot, shifted_zero_y, shifted_channel),
            DSCOPE_TRIGGER_LEVEL_CENTER
        );
        assert_eq!(
            scope_trigger_threshold_voltage(shifted_channel, 104),
            scope_trigger_threshold_voltage(channel, 104)
        );
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
    fn scope_trigger_anchor_selects_the_nearest_matching_edge() {
        let mut samples = vec![-1.0; 32];
        samples[11..16].fill(1.0);
        samples[21..].fill(1.0);
        let waveforms = WaveformStore {
            scope_channels: vec![VecDeque::from(samples), VecDeque::from(vec![0.0; 32])],
            scope_trigger_sample_index: Some(19),
            ..WaveformStore::default()
        };
        let trigger = ScopeTriggerSettings {
            source: DsCopeTriggerSource::ChannelZero,
            slope: DsCopeTriggerSlope::Rising,
            ..ScopeTriggerSettings::default()
        };

        assert_eq!(
            scope_trigger_crossing_sample(&waveforms, ScopeCaptureSettings::default(), trigger),
            Some(ScopeTriggerCrossing {
                sample: 20.5,
                channel_index: 0,
            })
        );
    }

    #[test]
    fn scope_auto_trigger_locks_a_free_running_frame_when_it_contains_an_edge() {
        let mut samples = vec![-1.0; 32];
        samples[11..16].fill(1.0);
        samples[21..].fill(1.0);
        let mut waveforms = WaveformStore {
            scope_channels: vec![VecDeque::from(samples), VecDeque::from(vec![0.0; 32])],
            scope_trigger_sample_index: None,
            ..WaveformStore::default()
        };
        let trigger = ScopeTriggerSettings {
            position_percent: 65,
            source: DsCopeTriggerSource::Auto,
            auto_channel: 0,
            slope: DsCopeTriggerSlope::Rising,
            ..ScopeTriggerSettings::default()
        };

        assert_eq!(
            scope_trigger_crossing_sample(&waveforms, ScopeCaptureSettings::default(), trigger),
            Some(ScopeTriggerCrossing {
                sample: 20.5,
                channel_index: 0,
            })
        );

        waveforms.scope_channels[0]
            .iter_mut()
            .for_each(|sample| *sample = 0.0);
        assert_eq!(
            scope_trigger_crossing_sample(&waveforms, ScopeCaptureSettings::default(), trigger),
            None
        );
    }

    #[test]
    fn rolling_scope_aligns_when_auto_trigger_finds_an_edge() {
        assert!(should_align_scope_trigger(true, true, true));
        assert!(!should_align_scope_trigger(true, true, false));
        assert!(should_align_scope_trigger(true, false, false));
        assert!(!should_align_scope_trigger(false, true, true));
    }

    #[test]
    fn scope_trigger_detection_ignores_capture_boundaries() {
        let waveforms = WaveformStore {
            scope_channels: vec![
                VecDeque::from(vec![-1.0, -1.0, 1.0, 1.0]),
                VecDeque::from(vec![0.0; 4]),
            ],
            scope_capture_boundaries: VecDeque::from([2]),
            ..WaveformStore::default()
        };

        assert_eq!(
            scope_trigger_crossing_sample(
                &waveforms,
                ScopeCaptureSettings::default(),
                ScopeTriggerSettings::default(),
            ),
            None
        );
    }

    #[test]
    fn scope_trace_and_trigger_use_the_same_sample_coordinates() {
        let plot = Rect::from_min_max(Pos2::new(100.0, 20.0), Pos2::new(900.0, 420.0));
        let viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            ..WaveformViewport::default()
        };

        assert!((scope_sample_fraction(1_050.0, 1_000, 101) - 0.5).abs() < f32::EPSILON);
        assert!((waveform_sample_x(plot, viewport, 50.0, 101) - 500.0).abs() < f32::EPSILON);
    }

    #[test]
    fn scope_smart_timing_snaps_the_pointer_to_the_triggering_signal() {
        let waveforms = WaveformStore {
            scope_channels: vec![
                VecDeque::from(vec![-1.0, -1.0, 1.0, 0.25, 0.5]),
                VecDeque::from(vec![0.0; 5]),
            ],
            scope_rate_hz: 1_000_000,
            scope_sample_offset: 1_000,
            scope_trigger_sample_index: Some(1_002),
            ..WaveformStore::default()
        };
        let settings = ScopeCaptureSettings::default();
        let trigger = ScopeTriggerSettings {
            source: DsCopeTriggerSource::ChannelZero,
            slope: DsCopeTriggerSlope::Rising,
            ..ScopeTriggerSettings::default()
        };
        let crossing = scope_trigger_crossing_sample(&waveforms, settings, trigger).unwrap();
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 100.0));

        let measurement = scope_smart_timing_measurement(
            &waveforms,
            crossing,
            plot,
            WaveformViewport::default(),
            5,
            Pos2::new(75.0, 90.0),
        )
        .unwrap();

        assert_eq!(measurement.start_sample, 1_001.5);
        assert_eq!(measurement.end_sample, 1_003.0);
        assert_eq!(measurement.channel_index, 0);
        assert_eq!(measurement.end_voltage, 0.25);
        assert_eq!(
            measurement.duration(waveforms.scope_rate_hz),
            Some(Duration::from_nanos(1_500))
        );
        assert_eq!(
            scope_smart_timing_measurement(
                &waveforms,
                crossing,
                plot,
                WaveformViewport::default(),
                5,
                Pos2::new(101.0, 50.0),
            ),
            None
        );
    }

    #[test]
    fn scope_smart_timing_ignores_a_zero_volt_trigger_level() {
        let crossing = ScopeTriggerCrossing {
            sample: 10.0,
            channel_index: 0,
        };
        let mut settings = ScopeCaptureSettings::default();

        assert!(!scope_smart_timing_has_nonzero_trigger(settings, crossing));

        settings.trigger.levels[0] = 104;
        assert!(scope_smart_timing_has_nonzero_trigger(settings, crossing));
    }

    #[test]
    fn scope_trigger_coordinates_preserve_sub_sample_precision_at_large_offsets() {
        let sample_offset = 10_000_000_000_u64;
        let fraction = scope_sample_fraction(sample_offset as f64 + 50.25, sample_offset, 101);

        assert!((fraction - 0.5025).abs() < 0.000_001);
    }

    #[test]
    fn scope_time_marker_coordinates_stay_attached_to_the_capture() {
        let plot = Rect::from_min_max(Pos2::new(100.0, 20.0), Pos2::new(900.0, 420.0));
        let mut viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            ..WaveformViewport::default()
        };
        let marker_position = viewport.domain_fraction_at_screen(0.5);

        assert!((marker_position - 0.5).abs() < f32::EPSILON);
        assert!(
            (egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain(marker_position)
            ) - 500.0)
                .abs()
                < f32::EPSILON
        );

        assert!(viewport.pan_by_screen_delta(-100.0, plot.width()));
        assert!(
            (egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain(marker_position)
            ) - 400.0)
                .abs()
                < f32::EPSILON
        );
        assert!(
            (scope_time_marker_delta(0.25, 0.5, 1_000, 1_000).unwrap() - 0.25).abs() < 0.000_001
        );
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
    fn scope_downsampling_averages_each_screen_bucket() {
        let samples = VecDeque::from(vec![
            0.0, 4.0, -3.0, 1.0, 0.5, 0.25, -0.5, 0.0, 1.0, -5.0, 6.0, 2.0, 1.5, 0.75, 0.5, 1.0,
        ]);

        let segments =
            scope_downsampled_segments(&samples, 0..samples.len(), 8, 0, &VecDeque::new());

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].len(), 2);
        assert_eq!(segments[0][0].average, 0.28125);
        assert_eq!(segments[0][1].average, 0.96875);
    }

    #[test]
    fn scope_downsampling_does_not_join_independent_captures() {
        let samples = VecDeque::from(vec![-1.0, -0.5, 4.0, 5.0]);

        let segments = scope_downsampled_segments(&samples, 0..4, 4, 100, &VecDeque::from([102]));

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0][0].average, -0.75);
        assert_eq!(segments[1][0].average, 4.5);
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
            ..WaveformViewport::default()
        };
        assert_eq!(scope_trigger_position_in_view(0.5, viewport), 50);
        assert!(viewport.pan_by_screen_delta(-100.0, 1_000.0));
        assert_eq!(scope_trigger_position_in_view(0.5, viewport), 40);
        let panned_start = viewport.start;
        let mut trigger_settings = ScopeTriggerSettings::default();
        finish_scope_viewport_pan(0.5, &mut viewport, &mut trigger_settings);
        assert_eq!(trigger_settings.position_percent, 40);
        assert_eq!(viewport.start, panned_start);
        assert!(!viewport.anchor_locked);

        viewport.start = 0.25;
        viewport.anchor_locked = true;
        viewport.move_screen_anchor(0.5, 0.25);
        assert!((viewport.start - 0.375).abs() < f32::EPSILON);
        assert!(((0.5 - viewport.start) / viewport.span - 0.25).abs() < f32::EPSILON);

        let mut full_span = WaveformViewport::default();
        full_span.align_trigger_to_screen(0.75, 0.5);
        assert_eq!(full_span.start, 0.0);
        assert_eq!(full_span.span, 1.0);
        assert!(((0.75 - full_span.start) / full_span.span - 0.75).abs() < f32::EPSILON);

        let mut hardware_anchored = WaveformViewport {
            start: 0.25,
            span: 0.5,
            anchor_locked: false,
        };
        hardware_anchored.step_timebase_at_sample(1.0, 1_000_000, 1_000_000, 0.503, 0.5);
        assert!(hardware_anchored.anchor_locked);
        assert!(hardware_anchored.span < 1.0);
        assert!(
            ((0.503 - hardware_anchored.start) / hardware_anchored.span - 0.5).abs() < f32::EPSILON
        );
        hardware_anchored.step_timebase_at_sample(-1.0, 1_000_000, 1_000_000, 0.503, 0.5);
        assert!(hardware_anchored.anchor_locked);
        assert!(
            ((0.503 - hardware_anchored.start) / hardware_anchored.span - 0.5).abs() < f32::EPSILON
        );
    }

    #[test]
    fn waveform_pan_tracks_mouse_motion_and_clamps_to_capture() {
        let mut viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            ..WaveformViewport::default()
        };

        assert!(viewport.pan_by_screen_delta(-100.0, 1_000.0));
        assert!((viewport.start - 0.3).abs() < 0.000_001);
        assert!(!viewport.anchor_locked);

        assert!(viewport.pan_by_screen_delta(10_000.0, 1_000.0));
        assert_eq!(viewport.start, 0.0);
        assert!(!viewport.pan_by_screen_delta(1.0, 1_000.0));
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
