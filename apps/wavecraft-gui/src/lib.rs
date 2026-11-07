mod capture_settings;
mod devices;
mod format;
mod measurements;
mod protocol_analyzer;
mod radio_analyzer;
mod radio_trigger;
mod recording;
mod recording_metadata;
mod sessions;
mod snapshot;
mod theme;
mod ui;
pub mod waveform;

use capture_settings::{
    CaptureSettingsDocument, CaptureSettingsProfile, CaptureSettingsRepository,
};
use devices::{
    capture_devices_for_target, first_device_id_for_role, first_radio_id, selected_device_for_role,
    selected_radio, usb_speed_warning, DeviceEntry, DeviceRole, UsbSpeedWarning,
};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use format::*;
use measurements::{
    calculate_scope_measurements, ScopeMeasurementResults, ScopeMeasurementSettings,
    SCOPE_MEASUREMENT_KINDS,
};
use protocol_analyzer::{
    analyzer_channels, analyzer_decoder_chain, analyzer_long_name, analyzer_name,
    analyzer_transport_name, decode_protocol_samples, decode_transport_samples, matching_protocols,
    protocol_stack, LogicDecodeOutput, LogicDecodeWorker, ProtocolAnalyzerInstance,
    ProtocolAnalyzerSettings, ProtocolCaptureDecoder, ProtocolDataFormat, ProtocolDecodeOutcome,
    ProtocolDecodeSnapshot, ProtocolDecodedEvent, ProtocolDecodedKind, PROTOCOLS,
};
use radio_analyzer::{
    process_radio_iq, RadioAnalyzerBank, RadioAnalyzerRuntime, RadioDataDecoderConfig,
    RadioDspOutput, RadioDspWorker,
};
use radio_trigger::{RadioSingleTrigger, RadioSingleTriggerConfig};
use recording::*;
use recording::{
    choose_decoded_data_save_path, choose_recording_save_path, choose_recording_to_open,
    choose_saleae_export_path, create_decoded_data_recorder, create_saleae_export_file,
    FileDecodedDataRecorder,
};
use recording_metadata::{
    channel_names_from_recording_metadata, insert_channel_name_metadata,
    logic_settings_from_recording_metadata, parse_color, parse_logic_acquisition_mode,
    radio_settings_from_recording_metadata, recording_metadata_value,
    saved_logic_settings_metadata, saved_radio_settings_metadata, saved_scope_settings_metadata,
    scope_settings_from_recording_metadata,
};
use sessions::{SessionTabs, MAX_SESSION_TABS, SESSION_NAME_MAX_CHARS};
use snapshot::{crop_snapshot_image, save_snapshot_image, PendingSnapshot};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek};
use std::ops::{Range, RangeInclusive};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use theme::{
    configure_style, toolbar_action_button, ACCENT, BACKGROUND, GRID, MATH_COLOR, ORANGE, PANEL,
    SCOPE_COLORS, TEXT_MUTED,
};
use ui::capture_settings_panel::*;
use ui::capture_setup_panel::CaptureSetupInputs;
use ui::central_panel::CentralPanelInputs;
use ui::decoded_panel::*;
use ui::dialogs::*;
use ui::frame_actions::FrameActions;
use ui::protocol_analyzer_dialog::*;
use ui::radio_settings::*;
use ui::scope_settings::*;
use ui::toolbar::ToolbarInputs;
use wavecraft_core::{
    CaptureBatch, CaptureCoordinator, CaptureEvent, CaptureFrame, CaptureSession, CaptureSource,
    DeviceDescriptor, DeviceId, InstrumentKind, ScopeBatch,
};
use wavecraft_decode::{
    AdditiveWhiteningConfig, BitOrder, BitTransformConfig, ByteStatus, RadioPacketFramerConfig,
    RadioPacketLength, RadioPacketPipelineConfig, RadioPacketPipelineOutput, RawRadioConfig,
    RawRadioMessageStart, RawRadioOutput, SampleRange, SyncWordConfig, TimedByte,
    WhiteningDirection,
};
use wavecraft_dsp::{
    BinaryFskProfile, BinaryGfskProfile, ChannelizerConfig, DemodulationProfile, FskPolarity,
    FskTiming, SignalAnalyzerProfile, SpectrumProcessor, SpectrumWindow,
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
use waveform::chrome::*;
use waveform::logic::*;
use waveform::radio::{RadioAnalysisStore, RadioDisplayStore, RadioViewOptions, RadioViewState};
use waveform::scope::*;
use waveform::scope_fft::*;
use waveform::scope_lissajous::*;
use waveform::store::*;
use waveform::viewport::*;

pub(crate) trait SelectableHoverText {
    fn on_hover_selectable_text(self, text: impl Into<egui::WidgetText>) -> Self;
    fn on_disabled_hover_selectable_text(self, text: impl Into<egui::WidgetText>) -> Self;
}

impl SelectableHoverText for egui::Response {
    fn on_hover_selectable_text(self, text: impl Into<egui::WidgetText>) -> Self {
        let text = text.into();
        self.on_hover_ui(move |ui| add_selectable_tooltip(ui, text))
    }

    fn on_disabled_hover_selectable_text(self, text: impl Into<egui::WidgetText>) -> Self {
        let text = text.into();
        self.on_disabled_hover_ui(move |ui| add_selectable_tooltip(ui, text))
    }
}

fn add_selectable_tooltip(ui: &mut egui::Ui, text: egui::WidgetText) {
    ui.set_max_width(ui.spacing().tooltip_width);
    ui.style_mut().interaction.selectable_labels = true;
    ui.add(egui::Label::new(text).selectable(true));
}

const SAMPLE_RATE_HZ: u64 = 1_000_000;
const SAMPLE_COUNT: u64 = 1_000_000;
const BUILD_VERSION_LABEL: &str = concat!("Version: ", env!("WAVECRAFT_BUILD_VERSION"));
const CPU_LOAD_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const LOGIC_CHANNEL_COUNT: usize = 16;
const RADIO_SPECTRUM_FFT_LENGTH: usize = 2_048;
const RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS: u64 = 1_000_000;
const MAX_RADIO_ANALYZER_CHANNELS: usize = 100;
const RAW_AUTOMATIC_PREAMBLE_BITS: usize = 24;
const MAX_CAPTURE_EVENTS_PER_POLL: usize = 8;
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

#[derive(Debug)]
struct CpuLoadMonitor {
    last_wall_time: Instant,
    last_process_time: Option<Duration>,
    load_percent: Option<f64>,
}

#[derive(Debug)]
struct MemoryUsageMonitor {
    last_sample_time: Instant,
    resident_bytes: Option<u64>,
}

impl MemoryUsageMonitor {
    fn new() -> Self {
        Self {
            last_sample_time: Instant::now(),
            resident_bytes: process_resident_memory_bytes(),
        }
    }

    fn update(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_sample_time) < CPU_LOAD_SAMPLE_INTERVAL {
            return;
        }
        self.resident_bytes = process_resident_memory_bytes();
        self.last_sample_time = now;
    }

    fn label(&self) -> String {
        const KIBIBYTE: f64 = 1024.0;
        const MEBIBYTE: f64 = KIBIBYTE * 1024.0;
        const GIBIBYTE: f64 = MEBIBYTE * 1024.0;

        match self.resident_bytes {
            Some(bytes) if bytes as f64 >= GIBIBYTE => {
                format!("RAM {:.2} GiB", bytes as f64 / GIBIBYTE)
            }
            Some(bytes) if bytes as f64 >= MEBIBYTE => {
                format!("RAM {:.1} MiB", bytes as f64 / MEBIBYTE)
            }
            Some(bytes) => format!("RAM {:.0} KiB", bytes as f64 / KIBIBYTE),
            None => "RAM —".to_owned(),
        }
    }
}

impl CpuLoadMonitor {
    fn new() -> Self {
        Self {
            last_wall_time: Instant::now(),
            last_process_time: process_cpu_time(),
            load_percent: None,
        }
    }

    fn update(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_wall_time) < CPU_LOAD_SAMPLE_INTERVAL {
            return;
        }
        self.update_at(now, process_cpu_time());
    }

    fn update_at(&mut self, now: Instant, process_time: Option<Duration>) {
        let wall_delta = now.duration_since(self.last_wall_time);
        self.load_percent = self
            .last_process_time
            .zip(process_time)
            .and_then(|(previous, current)| current.checked_sub(previous))
            .filter(|_| !wall_delta.is_zero())
            .map(|cpu_delta| cpu_delta.as_secs_f64() / wall_delta.as_secs_f64() * 100.0);
        self.last_wall_time = now;
        self.last_process_time = process_time;
    }

    fn label(&self) -> String {
        match self.load_percent {
            Some(percent) if percent >= 100.0 => format!("CPU {percent:.0}%"),
            Some(percent) => format!("CPU {percent:.1}%"),
            None => "CPU —".to_owned(),
        }
    }
}

#[cfg(unix)]
fn process_cpu_time() -> Option<Duration> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    let timeval_duration = |timeval: libc::timeval| {
        let seconds = u64::try_from(timeval.tv_sec).ok()?;
        let microseconds = u64::try_from(timeval.tv_usec).ok()?;
        Duration::from_secs(seconds).checked_add(Duration::from_micros(microseconds))
    };
    timeval_duration(usage.ru_utime)?.checked_add(timeval_duration(usage.ru_stime)?)
}

#[cfg(windows)]
fn process_cpu_time() -> Option<Duration> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return None;
    }
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    let hundred_nanoseconds = ticks(kernel).checked_add(ticks(user))?;
    Some(Duration::from_nanos(
        hundred_nanoseconds.saturating_mul(100),
    ))
}

#[cfg(not(any(unix, windows)))]
fn process_cpu_time() -> Option<Duration> {
    None
}

#[cfg(target_os = "macos")]
#[allow(deprecated)] // libc exposes the Mach task port without another platform dependency.
fn process_resident_memory_bytes() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info_data_t>::zeroed();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS {
        return None;
    }
    Some(unsafe { info.assume_init() }.resident_size)
}

#[cfg(target_os = "linux")]
fn process_resident_memory_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages = statm.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    let page_size = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    resident_pages.checked_mul(page_size)
}

#[cfg(windows)]
fn process_resident_memory_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) } == 0 {
        return None;
    }
    u64::try_from(counters.WorkingSetSize).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn process_resident_memory_bytes() -> Option<u64> {
    None
}
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

pub(crate) fn format_capture_timestamp(sample_index: u64, sample_rate_hz: u64) -> String {
    if sample_rate_hz == 0 {
        return "--:--:--.---".to_owned();
    }
    let total_milliseconds =
        u128::from(sample_index).saturating_mul(1_000) / u128::from(sample_rate_hz);
    let milliseconds = total_milliseconds % 1_000;
    let total_seconds = total_milliseconds / 1_000;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{milliseconds:03}")
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

fn wavecraft_app_icon() -> Option<egui::IconData> {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/wavecraft-icon.png")).ok()
}

pub fn run() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([920.0, 620.0]);
    // The icon is embedded at build time; if it ever fails to decode the window
    // simply opens with the platform's default icon.
    if let Some(icon) = wavecraft_app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
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
        default_logic_capture_config()
            .and_then(|config| {
                config
                    .with_operation_mode(self.operation_mode)
                    .with_enabled_channels(enabled_channels)
                    .ok()
            })
            .map_or(SAMPLE_RATE_HZ, |config| {
                config.maximum_sample_rate_hz(speed)
            })
    }

    fn maximum_sample_count(self) -> u64 {
        default_logic_capture_config().map_or(SAMPLE_COUNT, |config| {
            config
                .with_operation_mode(self.operation_mode)
                .maximum_sample_count()
        })
    }
}

/// The capture configuration the logic limits are derived from. The default
/// rate and count are valid constants, so `None` is not expected; callers fall
/// back to the default rate and count instead of panicking if it ever happens.
fn default_logic_capture_config() -> Option<DsLogicCaptureConfig> {
    DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT).ok()
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
    let Some((first_channel, first_condition)) = configured.next() else {
        return Err("Configure at least one logic trigger condition".to_owned());
    };
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
    let Some((first_channel, first_condition)) = configured.next() else {
        return Err(format!(
            "Configure at least one condition in smart trigger stage {}",
            stage_index + 1
        ));
    };
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SavedCaptureSettingsKind {
    LogicAnalyzer,
    Oscilloscope,
    RadioFrequency,
}

impl SavedCaptureSettingsKind {
    fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "LA",
            Self::Oscilloscope => "OSC",
            Self::RadioFrequency => "RF",
        }
    }
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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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

    fn includes_radio_receiver(self) -> bool {
        matches!(self, Self::RadioReceiver | Self::Combined)
    }

    fn captures_selected_radio(self, has_selected_radio: bool) -> bool {
        self == Self::RadioReceiver || (self == Self::Combined && has_selected_radio)
    }

    fn includes(self, target: Self) -> bool {
        self == target
            || (self == Self::Combined
                && matches!(
                    target,
                    Self::LogicAnalyzer | Self::Oscilloscope | Self::RadioReceiver
                ))
    }

    fn overlaps(self, other: Self) -> bool {
        (self.includes_logic_analyzer() && other.includes_logic_analyzer())
            || (self.includes_oscilloscope() && other.includes_oscilloscope())
            || (self.includes_radio_receiver() && other.includes_radio_receiver())
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
        ORANGE,
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

#[derive(Debug)]
struct ProtocolDecodeCacheKey {
    waveform_revision: u64,
    sample_offset: u64,
    sample_count: usize,
    analyzers: Vec<ProtocolAnalyzerInstance>,
}

impl ProtocolDecodeCacheKey {
    /// Compares in place so a cache hit allocates nothing; the key is only built
    /// when the inputs change.
    fn matches(&self, waveforms: &WaveformStore, analyzers: &[ProtocolAnalyzerInstance]) -> bool {
        self.waveform_revision == waveforms.logic_revision
            && self.sample_offset == waveforms.logic_sample_offset
            && self.sample_count == waveforms.logic_sample_count
            && self.analyzers == analyzers
    }
}

#[derive(Default)]
struct ProtocolDecodeCache {
    key: Option<ProtocolDecodeCacheKey>,
    outcomes: Arc<Vec<ProtocolDecodeOutcome>>,
    waveform_outcomes: Arc<Vec<ProtocolDecodeOutcome>>,
}

impl ProtocolDecodeCache {
    #[cfg(test)]
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

    /// Like [`Self::outcomes`], but returns a shared handle so the caller can
    /// keep the result while mutably borrowing the cache again, without copying
    /// every decoded event.
    fn shared_outcomes(
        &mut self,
        waveforms: &WaveformStore,
        analyzers: &[ProtocolAnalyzerInstance],
        streamed: Option<&ProtocolDecodeSnapshot>,
    ) -> Arc<Vec<ProtocolDecodeOutcome>> {
        if let Some(streamed) = streamed.filter(|decoder| decoder.matches_analyzers(analyzers)) {
            return streamed.shared_outcomes();
        }
        self.refresh(waveforms, analyzers);
        Arc::clone(&self.outcomes)
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
        if self
            .key
            .as_ref()
            .is_some_and(|key| key.matches(waveforms, analyzers))
        {
            return;
        }
        if waveforms.logic_sample_stride != 1
            || waveforms
                .logic_samples
                .iter()
                .any(|bucket| bucket.sample_count != 1)
        {
            let unavailable = Arc::new(
                analyzers
                    .iter()
                    .map(|_| {
                        ProtocolDecodeOutcome::Unavailable(
                            "Raw samples are unavailable for this display capture".to_owned(),
                        )
                    })
                    .collect::<Vec<_>>(),
            );
            self.waveform_outcomes = Arc::clone(&unavailable);
            self.outcomes = unavailable;
        } else {
            let samples = waveforms
                .logic_samples
                .iter()
                .map(|bucket| bucket.first)
                .collect::<Vec<_>>();
            self.outcomes = Arc::new(
                analyzers
                    .iter()
                    .map(|analyzer| {
                        decode_protocol_samples(
                            analyzer,
                            &samples,
                            waveforms.logic_sample_offset,
                            waveforms.logic_rate_hz,
                        )
                    })
                    .collect(),
            );
            self.waveform_outcomes = Arc::new(
                analyzers
                    .iter()
                    .map(|analyzer| {
                        decode_transport_samples(
                            analyzer,
                            &samples,
                            waveforms.logic_sample_offset,
                            waveforms.logic_rate_hz,
                        )
                    })
                    .collect(),
            );
        }
        self.key = Some(ProtocolDecodeCacheKey {
            waveform_revision: waveforms.logic_revision,
            sample_offset: waveforms.logic_sample_offset,
            sample_count: waveforms.logic_sample_count,
            analyzers: analyzers.to_vec(),
        });
    }
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

fn capture_recording_is_supported(mode: AcquisitionMode) -> bool {
    matches!(mode, AcquisitionMode::Single | AcquisitionMode::Run)
}

fn clears_completed_capture_on_start(mode: AcquisitionMode, continuing_request: bool) -> bool {
    match mode {
        AcquisitionMode::Single => true,
        AcquisitionMode::Run | AcquisitionMode::Loop => !continuing_request,
        AcquisitionMode::Repetitive => false,
    }
}

fn active_continuous_capture_accepts_recording(
    mode: AcquisitionMode,
    recording_active: bool,
) -> bool {
    matches!(mode, AcquisitionMode::Loop | AcquisitionMode::Run) && !recording_active
}

fn decoded_data_recording_is_supported(mode: AcquisitionMode, target: CaptureTarget) -> bool {
    mode == AcquisitionMode::Run && target == CaptureTarget::RadioReceiver
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

fn ingest_active_capture_frame(
    active: &mut ActiveCapture,
    frame: CaptureFrame,
    radio_squelch_threshold_dbfs: f32,
) -> Result<(), String> {
    let event = CaptureEvent::Frame(frame);
    if let Some(recorder) = active.recorder.as_mut() {
        recorder
            .write_event(&event)
            .map_err(|error| format!("failed to write recording: {error}"))?;
    }
    let CaptureEvent::Frame(frame) = event else {
        return Err("capture frame event changed variant".to_owned());
    };
    if matches!(&frame.payload, CaptureBatch::Logic(_)) {
        return active.logic_decode.push(frame);
    }
    if let CaptureBatch::Iq(iq) = &frame.payload {
        active
            .radio_dsp
            .push(iq.clone(), radio_squelch_threshold_dbfs)?;
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
    Ok(())
}

fn ingest_logic_decode_output(active: &mut ActiveCapture, output: LogicDecodeOutput) {
    active.protocol_decode_snapshot = output.snapshot;
    if let Some(frame) = output.frame {
        active.waveforms.ingest_frame(&frame, [false; 2]);
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
    protocol_decode_snapshot: ProtocolDecodeSnapshot,
    protocol_analyzers: Vec<ProtocolAnalyzerInstance>,
    logic_decode: LogicDecodeWorker,
    radio_spectrum: SpectrumProcessor,
    radio_dsp: RadioDspWorker,
    radio_trigger: Option<RadioSingleTrigger>,
    recorder: Option<FileRecorder>,
    recording_path: Option<PathBuf>,
    decoded_data_recorder: Option<FileDecodedDataRecorder>,
    decoded_data_path: Option<PathBuf>,
    has_logic: bool,
    has_scope: bool,
    has_radio: bool,
}

fn finish_logic_decode(active: &mut ActiveCapture) -> (ProtocolCaptureDecoder, Option<String>) {
    match active.logic_decode.finish() {
        Ok((decoder, outputs)) => {
            for output in outputs {
                ingest_logic_decode_output(active, output);
            }
            active.protocol_decode_snapshot = decoder.snapshot();
            (decoder, None)
        }
        Err(error) => {
            let mut decoder = ProtocolCaptureDecoder::new(&active.protocol_analyzers);
            decoder.retain_history_from(&active.protocol_decode_snapshot);
            decoder.finish();
            (decoder, Some(error))
        }
    }
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

struct RadioCaptureRequest {
    device: DiscoveredRadio,
    settings: RadioCaptureSettings,
    provider_settings: RadioProviderSettings,
    iq_correction_enabled: bool,
    analyzer_profiles: Vec<SignalAnalyzerProfile>,
    analyzer_channel_names: Vec<String>,
    decoder_config: RadioDataDecoderConfig,
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
    single_duration_millis: u64,
    trigger_enabled: bool,
    trigger_posttrigger_micros: u64,
    trigger_level_dbfs_tenths: i16,
    gains_db: BTreeMap<String, i32>,
    frequency_correction_milli_ppm: i32,
    iq_correction_enabled: bool,
    amplifier_enabled: bool,
    antenna_power_enabled: bool,
    analyzer: RadioAnalyzerSettings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RadioDemodulation {
    None,
    BinaryFsk,
    BinaryGfsk,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RadioDataMode {
    Raw,
    Packets,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RadioWhiteningPreset {
    Custom,
    Pn9X9X5,
}

impl RadioWhiteningPreset {
    fn label(self) -> &'static str {
        match self {
            Self::Custom => "Custom",
            Self::Pn9X9X5 => "PN9",
        }
    }

    fn metadata_value(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Pn9X9X5 => "pn9_x9_x5",
        }
    }

    fn description(self) -> Option<&'static str> {
        match self {
            Self::Custom => None,
            Self::Pn9X9X5 => Some("PN9 XORs packet data after the sync word using x⁹ + x⁵ + 1, resets to seed 0x1FF at each packet, and is compatible with standard SX127x FSK whitening"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RadioAnalyzerChannelSettings {
    name: String,
    frequency_offset_hz: i64,
}

impl RadioAnalyzerChannelSettings {
    fn numbered(index: usize, frequency_offset_hz: i64) -> Self {
        Self {
            name: format!("Channel {}", index + 1),
            frequency_offset_hz,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RadioAnalyzerSettings {
    demodulation: RadioDemodulation,
    channels: Vec<RadioAnalyzerChannelSettings>,
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
    data_mode: RadioDataMode,
    preamble_byte: u8,
    preamble_bytes: usize,
    sync_word: u64,
    sync_word_bytes: u8,
    maximum_preamble_bit_errors: usize,
    minimum_bit_confidence_milli: u16,
    maximum_uncertain_bits: usize,
    whitening_enabled: bool,
    whitening_preset: RadioWhiteningPreset,
    whitening_width: u8,
    whitening_polynomial: u64,
    whitening_seed: u64,
    whitening_direction: WhiteningDirection,
    whitening_invert_output: bool,
    whitening_start_bit: usize,
    whitening_end_bit: Option<usize>,
    whitening_advance_excluded_bits: bool,
}

impl Default for RadioAnalyzerSettings {
    fn default() -> Self {
        Self {
            demodulation: RadioDemodulation::None,
            // Capture both observed 868.1/868.5 MHz channels around 868.3 MHz;
            // retain 868.1 MHz as the first channel if analysis is enabled.
            channels: vec![RadioAnalyzerChannelSettings::numbered(0, -200_000)],
            channel_bandwidth_hz: 200_000,
            output_sample_rate_hz: 512_000,
            filter_tap_count: 257,
            symbol_rate_hz: 19_200,
            deviation_hz: 35_000,
            gfsk_bt_milli: 500,
            inverted: false,
            adaptive_timing: true,
            timing_loop_bandwidth_milli: 100,
            max_clock_error_ppm: 10_000,
            data_mode: RadioDataMode::Raw,
            preamble_byte: 0xaa,
            preamble_bytes: 4,
            sync_word: 0,
            sync_word_bytes: 0,
            maximum_preamble_bit_errors: 1,
            minimum_bit_confidence_milli: 0,
            maximum_uncertain_bits: 0,
            whitening_enabled: false,
            whitening_preset: RadioWhiteningPreset::Custom,
            whitening_width: 7,
            whitening_polynomial: 0b100_0001,
            whitening_seed: 0b101_1011,
            whitening_direction: WhiteningDirection::ShiftRight,
            whitening_invert_output: false,
            whitening_start_bit: 0,
            whitening_end_bit: None,
            whitening_advance_excluded_bits: false,
        }
    }
}

impl RadioAnalyzerSettings {
    fn is_enabled(&self) -> bool {
        self.demodulation != RadioDemodulation::None
    }

    fn channel_name(&self, index: usize) -> String {
        self.channels
            .get(index)
            .map(|channel| channel.name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Channel {}", index + 1))
    }

    fn raw_config(&self) -> RawRadioConfig {
        RawRadioConfig {
            preamble_byte: self.preamble_byte,
            preamble_bytes: self.preamble_bytes,
            sync_word: self.sync_word,
            sync_word_bytes: self.sync_word_bytes,
            maximum_preamble_bit_errors: self.maximum_preamble_bit_errors,
            bit_order: BitOrder::MostSignificantFirst,
            minimum_bit_confidence_milli: self.minimum_bit_confidence_milli,
            maximum_uncertain_bits: self.maximum_uncertain_bits,
        }
    }

    fn whitening_config(&self) -> AdditiveWhiteningConfig {
        match self.whitening_preset {
            RadioWhiteningPreset::Custom => AdditiveWhiteningConfig {
                width: self.whitening_width,
                polynomial: self.whitening_polynomial,
                seed: self.whitening_seed,
                direction: self.whitening_direction,
                invert_output: self.whitening_invert_output,
                start_bit: self.whitening_start_bit,
                end_bit: self.whitening_end_bit,
                advance_excluded_bits: self.whitening_advance_excluded_bits,
            },
            RadioWhiteningPreset::Pn9X9X5 => AdditiveWhiteningConfig {
                width: 9,
                polynomial: 0x21,
                seed: 0x1ff,
                direction: WhiteningDirection::ShiftRight,
                invert_output: false,
                start_bit: 0,
                end_bit: None,
                advance_excluded_bits: false,
            },
        }
    }

    fn decoder_config(&self) -> Result<RadioDataDecoderConfig, String> {
        match self.data_mode {
            RadioDataMode::Raw => {
                let config = self.raw_config();
                config.validate().map_err(|error| error.to_string())?;
                Ok(RadioDataDecoderConfig::Raw(config))
            }
            RadioDataMode::Packets => {
                if self.sync_word_bytes == 0 {
                    return Err("packet decoding requires a sync word".to_owned());
                }
                let preamble_bits = std::iter::repeat(self.preamble_byte)
                    .take(self.preamble_bytes)
                    .flat_map(|byte| {
                        (0..8).map(move |bit_index| byte & (1 << (7 - bit_index)) != 0)
                    })
                    .collect();
                let sync_bits = (0..self.sync_word_bytes)
                    .flat_map(|byte_index| {
                        let shift = u32::from(self.sync_word_bytes - byte_index - 1) * 8;
                        let byte = ((self.sync_word >> shift) & 0xff) as u8;
                        (0..8).map(move |bit_index| byte & (1 << (7 - bit_index)) != 0)
                    })
                    .collect();
                let whitening = self.whitening_config();
                let whitening = self.whitening_enabled.then_some(whitening);
                let config = RadioPacketPipelineConfig {
                    bit_transform: BitTransformConfig::default(),
                    framing: RadioPacketFramerConfig {
                        preamble: preamble_bits,
                        sync: SyncWordConfig {
                            bits: sync_bits,
                            max_bit_errors: self.maximum_preamble_bit_errors,
                        },
                        whitening,
                        bit_order: BitOrder::MostSignificantFirst,
                        length: RadioPacketLength::UntilEndOfStream,
                        max_search_bits: 1_048_576,
                    },
                    crc: None,
                };
                wavecraft_decode::RadioPacketPipeline::new(config.clone())
                    .map_err(|error| error.to_string())?;
                Ok(RadioDataDecoderConfig::Packets(config))
            }
        }
    }

    fn signal_profiles(&self) -> Result<Vec<SignalAnalyzerProfile>, String> {
        if !self.is_enabled() {
            return Ok(Vec::new());
        }
        if self.channels.is_empty() || self.channels.len() > MAX_RADIO_ANALYZER_CHANNELS {
            return Err(format!(
                "signal analyzer requires between 1 and {MAX_RADIO_ANALYZER_CHANNELS} channels"
            ));
        }
        let mut unique_offsets = HashSet::with_capacity(self.channels.len());
        if self
            .channels
            .iter()
            .any(|channel| !unique_offsets.insert(channel.frequency_offset_hz))
        {
            return Err("signal analyzer channel offsets must be unique".to_owned());
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
            RadioDemodulation::None => return Ok(Vec::new()),
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
        self.channels
            .iter()
            .map(|channel| {
                let profile = SignalAnalyzerProfile {
                    channel: ChannelizerConfig {
                        frequency_offset_hz: channel.frequency_offset_hz,
                        bandwidth_hz: self.channel_bandwidth_hz,
                        output_sample_rate_hz: self.output_sample_rate_hz,
                        filter_tap_count: self.filter_tap_count,
                    },
                    demodulation: demodulation.clone(),
                };
                profile.validate().map_err(|error| error.to_string())?;
                Ok(profile)
            })
            .collect()
    }
}

impl Default for RadioSettings {
    fn default() -> Self {
        Self {
            capture: RadioCaptureSettings {
                center_frequency_hz: 868_300_000,
                sample_rate_hz: 8_000_000,
                bandwidth_hz: 1_750_000,
                receive_channel: 0,
                sample_limit: None,
            },
            single_duration_millis: 1_000,
            trigger_enabled: false,
            trigger_posttrigger_micros: 2_000,
            trigger_level_dbfs_tenths: -500,
            gains_db: BTreeMap::new(),
            frequency_correction_milli_ppm: 0,
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
        radio_dsp: Box<RadioDspWorker>,
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
    capture_settings_repository: Option<CaptureSettingsRepository>,
    saved_capture_settings: CaptureSettingsDocument,
    resource_directory: String,
    settings_open: bool,
    auto_calibration_confirmation_open: bool,
    manual_calibration: Option<ManualCalibrationEditor>,
    manual_calibration_load_requested: bool,
    manual_calibration_save_requested: Option<DsCopeCalibrationValues>,
    record_capture: bool,
    recording_path: String,
    record_decoded_data: bool,
    decoded_data_path: String,
    capture_requests: HashSet<CaptureTarget>,
    requested_capture_target: Option<CaptureTarget>,
    logic_acquisition_mode: AcquisitionMode,
    repetitive_interval_seconds: f32,
    scheduled_repetitive_capture: Option<ScheduledRepetitiveCapture>,
    queued_capture_rearms: Vec<(AcquisitionMode, CaptureTarget)>,
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
    captures: Vec<ActiveCapture>,
    logic_sessions: SessionTabs<CompletedCapture, LogicSessionSettings>,
    scope_sessions: SessionTabs<CompletedCapture, ScopeSessionSettings>,
    session_name_editor: Option<SessionNameEditor>,
    combined_capture: Option<CompletedCapture>,
    radio_capture: Option<CompletedCapture>,
    combined_settings: CombinedSessionSettings,
    cpu_load: CpuLoadMonitor,
    memory_usage: MemoryUsageMonitor,
    status: String,
    last_error: Option<String>,
}

fn default_scope_measurement_window_positions() -> [Vec2; 2] {
    [Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0)]
}

impl WaveCraftApp {
    fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        configure_style(&creation_context.egui_ctx);
        let (capture_settings_repository, saved_capture_settings, settings_load_error) =
            match CaptureSettingsRepository::for_current_user() {
                Ok(repository) => match repository.load() {
                    Ok(settings) => (Some(repository), settings, None),
                    Err(error) => (
                        Some(repository),
                        CaptureSettingsDocument::default(),
                        Some(error),
                    ),
                },
                Err(error) => (None, CaptureSettingsDocument::default(), Some(error)),
            };
        let mut app = Self {
            capture_settings_repository,
            saved_capture_settings,
            resource_directory: default_resource_directory(),
            settings_open: false,
            auto_calibration_confirmation_open: false,
            manual_calibration: None,
            manual_calibration_load_requested: false,
            manual_calibration_save_requested: None,
            record_capture: false,
            recording_path: "capture.dsv".to_owned(),
            record_decoded_data: false,
            decoded_data_path: "decoded-data.jsonl".to_owned(),
            capture_requests: HashSet::new(),
            requested_capture_target: None,
            logic_acquisition_mode: AcquisitionMode::Single,
            repetitive_interval_seconds: 1.0,
            scheduled_repetitive_capture: None,
            queued_capture_rearms: Vec::new(),
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
            captures: Vec::new(),
            logic_sessions: SessionTabs::new(LogicSessionSettings::default()),
            scope_sessions: SessionTabs::new(ScopeSessionSettings::default()),
            session_name_editor: None,
            combined_capture: None,
            radio_capture: None,
            combined_settings: CombinedSessionSettings {
                logic_ratio: 0.55,
                ..CombinedSessionSettings::default()
            },
            cpu_load: CpuLoadMonitor::new(),
            memory_usage: MemoryUsageMonitor::new(),
            status: "Ready".to_owned(),
            last_error: None,
        };
        app.apply_saved_capture_settings();
        app.save_active_logic_session_settings();
        app.save_active_scope_session_settings();
        app.combined_settings = app.current_combined_session_settings();
        app.begin_refresh();
        if let Some(error) = settings_load_error {
            app.last_error = Some(format!(
                "Saved capture settings could not be loaded; using defaults: {error}"
            ));
        }
        app
    }

    fn apply_saved_capture_settings(&mut self) {
        if let Some(profile) = self.saved_capture_settings.profiles.logic_analyzer.as_ref() {
            self.logic_settings = logic_settings_from_recording_metadata(&profile.values);
            self.logic_acquisition_mode = profile
                .values
                .get("logic.acquisition_mode")
                .map(String::as_str)
                .and_then(parse_logic_acquisition_mode)
                .unwrap_or(AcquisitionMode::Single);
            self.repetitive_interval_seconds =
                recording_metadata_value(&profile.values, "logic.repetitive_interval_seconds")
                    .filter(|value: &f32| value.is_finite())
                    .unwrap_or(1.0)
                    .clamp(0.1, 10.0);
            self.logic_smart_timing_enabled =
                recording_metadata_value(&profile.values, "logic.smart_timing_enabled")
                    .unwrap_or(true);
            let (logic_names, _) = channel_names_from_recording_metadata(&profile.values);
            self.logic_channel_names = logic_names;
        }
        if let Some(profile) = self.saved_capture_settings.profiles.oscilloscope.as_ref() {
            self.scope_settings = scope_settings_from_recording_metadata(&profile.values);
            self.scope_auto_sample_rate =
                recording_metadata_value(&profile.values, "scope.auto_sample_rate").unwrap_or(true);
            self.scope_smart_timing_enabled =
                recording_metadata_value(&profile.values, "scope.smart_timing_enabled")
                    .unwrap_or(false);
            let (_, scope_names) = channel_names_from_recording_metadata(&profile.values);
            self.scope_channel_names = scope_names;
            for (index, color) in self.scope_channel_colors.iter_mut().enumerate() {
                if let Some(saved) = profile
                    .values
                    .get(&format!("scope.ch{index}.color"))
                    .and_then(|value| parse_color(value))
                {
                    *color = saved;
                }
            }
        }
        if let Some(profile) = self
            .saved_capture_settings
            .profiles
            .radio_frequency
            .as_ref()
        {
            self.radio_settings = radio_settings_from_recording_metadata(&profile.values);
        }
    }

    fn save_capture_settings(&mut self, kind: SavedCaptureSettingsKind) {
        let mut document = self.saved_capture_settings.clone();
        let profile = match kind {
            SavedCaptureSettingsKind::LogicAnalyzer => {
                CaptureSettingsProfile::new(saved_logic_settings_metadata(
                    self.logic_settings,
                    self.logic_acquisition_mode,
                    self.repetitive_interval_seconds,
                    self.logic_smart_timing_enabled,
                    &self.logic_channel_names,
                ))
            }
            SavedCaptureSettingsKind::Oscilloscope => {
                CaptureSettingsProfile::new(saved_scope_settings_metadata(
                    self.scope_settings,
                    self.scope_auto_sample_rate,
                    self.scope_smart_timing_enabled,
                    &self.scope_channel_names,
                    self.scope_channel_colors,
                ))
            }
            SavedCaptureSettingsKind::RadioFrequency => {
                CaptureSettingsProfile::new(saved_radio_settings_metadata(&self.radio_settings))
            }
        };
        match kind {
            SavedCaptureSettingsKind::LogicAnalyzer => {
                document.profiles.logic_analyzer = Some(profile)
            }
            SavedCaptureSettingsKind::Oscilloscope => {
                document.profiles.oscilloscope = Some(profile)
            }
            SavedCaptureSettingsKind::RadioFrequency => {
                document.profiles.radio_frequency = Some(profile)
            }
        }

        let Some(repository) = self.capture_settings_repository.as_ref() else {
            self.set_error(
                "Capture settings cannot be saved because the configuration directory is unavailable"
                    .to_owned(),
            );
            return;
        };
        match repository.save(&document) {
            Ok(()) => {
                self.saved_capture_settings = document;
                self.status = format!("{} settings saved", kind.label());
                self.last_error = None;
            }
            Err(error) => self.set_error(error),
        }
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
        let Some(pending) = self.pending_snapshot.take() else {
            return;
        };
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
        if self.pending.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.requested_capture_target = None;
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

    fn begin_open_decoded_data(&mut self, path: PathBuf) {
        if self.pending.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.requested_capture_target = None;
        self.scope_configuration_due = None;
        self.status = format!("Opening decoded RF data {}…", path.display());
        self.last_error = None;
        thread::spawn(move || {
            let result =
                load_gui_decoded_data(&path).map(|recording| WorkerSuccess::RecordingLoaded {
                    path,
                    recording: Box::new(recording),
                });
            let _ = sender.send(result);
        });
    }

    fn open_selected_recording(&mut self, path: PathBuf) {
        match decoded_data_open_request(&path, self.active_waveform_tab) {
            Ok(true) => self.begin_open_decoded_data(path),
            Ok(false) => self.begin_open_recording(path),
            Err(error) => self.set_error(error),
        }
    }

    fn begin_export_saleae(&mut self, path: PathBuf) {
        if self.pending.is_some() {
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
        if self.pending.is_some() || !self.captures.is_empty() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.requested_capture_target = None;
        self.capture_requests.clear();
        self.queued_capture_rearms.clear();
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
        let waits_for_trigger =
            mode == AcquisitionMode::Single && self.radio_settings.trigger_enabled;
        settings.sample_limit = (mode == AcquisitionMode::Single && !waits_for_trigger)
            .then(|| {
                radio_single_sample_limit(
                    settings.sample_rate_hz,
                    self.radio_settings.single_duration_millis,
                )
            })
            .transpose()?;
        settings
            .validate(&device.capabilities)
            .map_err(|error| error.to_string())?;
        let provider_settings = match device.provider {
            RadioProvider::HackRf => RadioProviderSettings::HackRf(HackRfSettings {
                lna_gain_db: radio_gain_u32(&self.radio_settings, "lna")?,
                vga_gain_db: radio_gain_u32(&self.radio_settings, "vga")?,
                amplifier_enabled: self.radio_settings.amplifier_enabled,
                antenna_power_enabled: self.radio_settings.antenna_power_enabled,
                frequency_correction_milli_ppm: self.radio_settings.frequency_correction_milli_ppm,
            }),
            provider => {
                return Err(format!(
                    "capture is not implemented for the {provider:?} radio provider"
                ));
            }
        };
        let analyzer_profiles = self.radio_settings.analyzer.signal_profiles()?;
        let analyzer_channel_names = (0..analyzer_profiles.len())
            .map(|index| self.radio_settings.analyzer.channel_name(index))
            .collect();
        let half_sample_rate_hz = settings.sample_rate_hz / 2;
        if !analyzer_profiles.is_empty()
            && self
                .radio_settings
                .analyzer
                .channels
                .iter()
                .any(|channel| channel.frequency_offset_hz.unsigned_abs() > half_sample_rate_hz)
        {
            return Err(
                "signal analyzer channel offset lies outside the captured RF span".to_owned(),
            );
        }
        let decoder_config = if analyzer_profiles.is_empty() {
            RadioDataDecoderConfig::Raw(self.radio_settings.analyzer.raw_config())
        } else {
            self.radio_settings.analyzer.decoder_config()?
        };
        Ok(RadioCaptureRequest {
            device,
            settings,
            provider_settings,
            iq_correction_enabled: self.radio_settings.iq_correction_enabled,
            analyzer_profiles,
            analyzer_channel_names,
            decoder_config,
        })
    }

    fn begin_capture(&mut self, mode: AcquisitionMode, target: CaptureTarget) {
        let continuing_request = self.capture_is_requested(target);
        if self.pending.is_some()
            || self.capture_target_is_active(target)
            || self
                .capture_requests
                .iter()
                .any(|requested| *requested != target && requested.overlaps(target))
        {
            return;
        }
        if !continuing_request && target.includes_logic_analyzer() {
            self.logic_marker_settings.clear();
        }
        self.scheduled_repetitive_capture = None;
        if self.record_capture && !capture_recording_is_supported(mode) {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error =
                Some("Capture recording is available in Single and Run modes only".to_owned());
            return;
        }
        if self.record_decoded_data && !decoded_data_recording_is_supported(mode, target) {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some("Decoded data recording requires RF Run mode".to_owned());
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
                self.record_capture = false;
                self.record_decoded_data = false;
                self.last_error = Some(error);
                return;
            }
        };
        let radio_request = if target.captures_selected_radio(
            selected_radio(&self.radios, self.selected_radio_id.as_deref()).is_some(),
        ) {
            match self.radio_capture_request(mode) {
                Ok(request) => Some(request),
                Err(error) => {
                    self.record_capture = false;
                    self.record_decoded_data = false;
                    self.last_error = Some(error);
                    return;
                }
            }
        } else {
            None
        };
        if self.record_capture && self.recording_path.trim().is_empty() {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some("Enter a recording file path".to_owned());
            return;
        }
        if self.record_decoded_data && self.decoded_data_path.trim().is_empty() {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some("Enter a decoded data recording file path".to_owned());
            return;
        }
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.record_capture = false;
                self.record_decoded_data = false;
                self.last_error = Some(error);
                return;
            }
        };
        let logic_config = match self.logic_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.record_capture = false;
                self.record_decoded_data = false;
                self.last_error = Some(error);
                return;
            }
        }
        .with_loop_mode(mode == AcquisitionMode::Loop);
        if matches!(mode, AcquisitionMode::Repetitive | AcquisitionMode::Loop)
            && target != CaptureTarget::LogicAnalyzer
        {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some(
                "Repetitive and Loop modes are available for Logic Analyzer capture only"
                    .to_owned(),
            );
            return;
        }
        if mode == AcquisitionMode::Loop
            && logic_config.operation_mode() != DsLogicOperationMode::Stream
        {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some("Loop mode requires DSLogic Stream mode".to_owned());
            return;
        }
        let calibration_values = self.manual_calibration.as_ref().map(|editor| editor.values);

        let retain_protocol_history = mode == AcquisitionMode::Repetitive && continuing_request;
        self.capture_requests.insert(target);
        self.queued_capture_rearms
            .retain(|(_, queued_target)| *queued_target != target);
        self.requested_capture_target = Some(target);
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        if clears_completed_capture_on_start(mode, continuing_request) {
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
                radio_dsp: Box::new(started.radio_dsp),
            });
            let _ = sender.send(result);
        });
    }

    fn begin_scope_configuration(&mut self) {
        if self.pending.is_some() || self.capture_target_is_active(CaptureTarget::Oscilloscope) {
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
        if self.pending.is_some() || self.capture_target_is_active(CaptureTarget::Oscilloscope) {
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
        if self.pending.is_some() || self.capture_target_is_active(CaptureTarget::Oscilloscope) {
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
        if self.pending.is_some() || self.capture_target_is_active(CaptureTarget::Oscilloscope) {
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
        if let Some(active) = self.capture_for_target(CaptureTarget::Oscilloscope) {
            if active.mode != AcquisitionMode::Run {
                return;
            }
            active.session.request_stop();
            self.manual_calibration_load_requested = true;
            self.scope_capture_restart_requested = false;
            self.status = "Loading calibration while continuous acquisition pauses…".to_owned();
            self.last_error = None;
        } else {
            self.begin_manual_calibration();
        }
    }

    fn begin_save_manual_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.pending.is_some() || self.capture_target_is_active(CaptureTarget::Oscilloscope) {
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
        if let Some(active) = self.capture_for_target(CaptureTarget::Oscilloscope) {
            active.session.request_stop();
            self.manual_calibration_save_requested = Some(values);
            self.scope_capture_restart_requested = false;
            self.status = "Pausing acquisition to save calibration…".to_owned();
            self.last_error = None;
        } else {
            self.begin_save_manual_calibration(values);
        }
    }

    fn attach_run_capture_recording(&mut self, target: CaptureTarget, path: PathBuf) {
        let logic_config = match self.logic_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        let recorder = {
            let Some(active) = self.capture_for_target(target) else {
                self.set_error(format!("{} Run capture is not active", target.label()));
                return;
            };
            if !matches!(active.mode, AcquisitionMode::Loop | AcquisitionMode::Run) {
                self.set_error(
                    "Recording can only attach to an active Run or Loop acquisition".to_owned(),
                );
                return;
            }
            if active.recorder.is_some() {
                self.set_error("Capture recording is already active".to_owned());
                return;
            }
            match start_gui_recording(
                &path,
                active.session.descriptors(),
                GuiRecordingSettings {
                    logic_config,
                    logic_settings: self.logic_settings,
                    scope_config: active.scope_config,
                    scope_settings: self.scope_settings,
                    radio_settings: &self.radio_settings,
                    logic_channel_names: &self.logic_channel_names,
                    scope_channel_names: &self.scope_channel_names,
                },
            ) {
                Ok(recorder) => recorder,
                Err(error) => {
                    self.set_error(error);
                    return;
                }
            }
        };
        let Some(active) = self.capture_for_target_mut(target) else {
            self.set_error("Run capture ended before recording could start".to_owned());
            return;
        };
        active.recorder = Some(recorder);
        active.recording_path = Some(path.clone());
        self.status = format!(
            "{} continuous acquisition · recording to {}",
            target.label(),
            path.display()
        );
        self.last_error = None;
    }

    fn attach_run_decoded_data_recording(&mut self, path: PathBuf) {
        let last_raw_message_sequence = {
            let Some(active) = self.capture_for_target(CaptureTarget::RadioReceiver) else {
                self.set_error("RF Run capture is not active".to_owned());
                return;
            };
            if active.mode != AcquisitionMode::Run || !active.has_radio {
                self.set_error("Decoded data recording can only attach to RF Run mode".to_owned());
                return;
            }
            if active.decoded_data_recorder.is_some() {
                self.set_error("Decoded data recording is already active".to_owned());
                return;
            }
            active
                .waveforms
                .radio_analysis
                .raw_messages()
                .back()
                .map_or(0, |message| message.sequence())
        };
        let mut recorder = match create_decoded_data_recorder(&path, &self.radio_settings) {
            Ok(recorder) => recorder,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        recorder.skip_raw_messages_through(last_raw_message_sequence);
        let Some(active) = self.capture_for_target_mut(CaptureTarget::RadioReceiver) else {
            self.set_error("RF Run capture ended before recording could start".to_owned());
            return;
        };
        active.decoded_data_recorder = Some(recorder);
        active.decoded_data_path = Some(path.clone());
        self.status = format!("RF Run · recording decoded data to {}", path.display());
        self.last_error = None;
    }

    fn stop_capture(&mut self, target: CaptureTarget) {
        let Some(index) = self
            .captures
            .iter()
            .position(|capture| capture.target.includes(target))
        else {
            return;
        };
        let mut active = self.captures.swap_remove(index);
        self.capture_requests.remove(&active.target);
        self.queued_capture_rearms
            .retain(|(_, target)| *target != active.target);
        if self
            .scheduled_repetitive_capture
            .is_some_and(|scheduled| scheduled.target == active.target)
        {
            self.scheduled_repetitive_capture = None;
        }
        self.status = "Stopping capture…".to_owned();
        active.session.request_stop();
        let (protocol_decode, logic_decode_error) = finish_logic_decode(&mut active);
        let stop_result = active.session.stop();
        let radio_dsp_result = active.radio_dsp.finish();
        let mut error = stop_result.as_ref().err().map(ToString::to_string);
        match radio_dsp_result {
            Ok(outputs) => {
                for output in outputs {
                    if let Err(recording_error) =
                        record_decoded_radio_packets(&mut active.decoded_data_recorder, &output)
                    {
                        append_error(&mut error, recording_error);
                    }
                    ingest_radio_dsp_output(&mut active.waveforms, output);
                    if let Err(recording_error) = record_decoded_raw_messages(
                        &mut active.decoded_data_recorder,
                        &active.waveforms.radio_analysis,
                    ) {
                        append_error(&mut error, recording_error);
                    }
                }
            }
            Err(dsp_error) => append_error(&mut error, dsp_error),
        }
        if let Some(logic_decode_error) = logic_decode_error {
            append_error(&mut error, logic_decode_error);
        }
        active.waveforms.radio_analysis.finish_raw_capture();
        if let Err(recording_error) = record_decoded_raw_messages(
            &mut active.decoded_data_recorder,
            &active.waveforms.radio_analysis,
        ) {
            append_error(&mut error, recording_error);
        }
        let completion = if stop_result.is_ok() && error.is_none() {
            CompletionStatus::Cancelled
        } else {
            CompletionStatus::Failed
        };
        if let Err(recording_error) = finish_gui_recording(&mut active.recorder, completion) {
            append_error(&mut error, recording_error);
        }
        if let Err(recording_error) =
            finish_decoded_data_recording(&mut active.decoded_data_recorder, completion)
        {
            append_error(&mut error, recording_error);
        }
        let recording_path = active.recording_path.take();
        let decoded_data_path = active.decoded_data_path.take();
        self.record_capture = false;
        self.record_decoded_data = false;
        let has_logic = active.has_logic;
        let has_scope = active.has_scope;
        let has_radio = active.has_radio;
        if !active.preserve_display_until_complete {
            self.set_completed_capture_for_target(
                active.target,
                CompletedCapture {
                    waveforms: active.waveforms,
                    protocol_decode,
                    has_logic,
                    has_scope,
                    has_radio,
                },
            );
        }
        match error {
            Some(error) => self.set_error(error),
            None => {
                let saved_path = decoded_data_path.or(recording_path);
                self.status = saved_path.map_or_else(
                    || "Capture stopped".to_owned(),
                    |path| format!("Capture stopped · recording saved to {}", path.display()),
                );
            }
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
                radio_dsp,
            }) => {
                let logic_config = *logic_config;
                if !self.capture_is_requested(target) {
                    self.requested_capture_target = None;
                    self.record_capture = false;
                    self.record_decoded_data = false;
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
                let mut recorder = match recording_path.as_deref() {
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
                let decoded_data_path = self
                    .record_decoded_data
                    .then(|| PathBuf::from(self.decoded_data_path.trim()));
                let decoded_data_recorder = match decoded_data_path.as_deref() {
                    Some(path) => match create_decoded_data_recorder(path, &self.radio_settings) {
                        Ok(recorder) => Some(recorder),
                        Err(error) => {
                            let _ = finish_gui_recording(&mut recorder, CompletionStatus::Failed);
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
                let radio_trigger = if mode == AcquisitionMode::Single
                    && target.includes_radio_receiver()
                    && has_radio
                    && self.radio_settings.trigger_enabled
                {
                    let config = radio_single_trigger_config(&self.radio_settings);
                    match config.and_then(RadioSingleTrigger::new) {
                        Ok(trigger) => Some(trigger),
                        Err(error) => {
                            let cleanup = session.stop().err();
                            self.set_error(match cleanup {
                                Some(cleanup) => format!(
                                    "failed to arm RF trigger: {error}; additionally failed to stop: {cleanup}"
                                ),
                                None => format!("failed to arm RF trigger: {error}"),
                            });
                            return;
                        }
                    }
                } else {
                    None
                };
                let waiting_for_radio_trigger = radio_trigger.is_some();
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
                let protocol_decode_snapshot = protocol_decode.snapshot();
                let protocol_analyzers = self.protocol_analyzer.analyzers.clone();
                let logic_decode = LogicDecodeWorker::start(protocol_decode);
                if !continue_logic_loop {
                    waveforms.configure_logic_capture(logic_config.sample_count());
                }
                waveforms.scope_capacity = scope_capacity;
                waveforms.scope_rolling = rolling;
                self.captures.push(ActiveCapture {
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
                    protocol_decode_snapshot,
                    protocol_analyzers,
                    logic_decode,
                    radio_spectrum: *radio_spectrum,
                    radio_dsp: *radio_dsp,
                    radio_trigger,
                    recorder,
                    recording_path: recording_path.clone(),
                    decoded_data_recorder,
                    decoded_data_path: decoded_data_path.clone(),
                    has_logic,
                    has_scope,
                    has_radio,
                });
                self.requested_capture_target = None;
                self.record_capture = false;
                self.record_decoded_data = false;
                self.status = match (mode, recording_path.as_ref()) {
                    (AcquisitionMode::Single, Some(path)) if waiting_for_radio_trigger => format!(
                        "Single · waiting for RF trigger at {:.1} dBFS; recording to {}",
                        f32::from(self.radio_settings.trigger_level_dbfs_tenths) / 10.0,
                        path.display()
                    ),
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
                        if waiting_for_radio_trigger {
                            format!(
                                "Single · waiting for RF trigger at {:.1} dBFS",
                                f32::from(self.radio_settings.trigger_level_dbfs_tenths) / 10.0
                            )
                        } else if has_logic_trigger {
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
                    (AcquisitionMode::Run, Some(path)) => format!(
                        "{} Run acquisition from {count} instrument(s) · recording to {}",
                        target.label(),
                        path.display()
                    ),
                    (AcquisitionMode::Run, None) => {
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
                if let Some(path) = decoded_data_path {
                    self.status = format!(
                        "RF Run acquisition from {count} instrument(s) · decoded data to {}",
                        path.display()
                    );
                }
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
                let Some(target) = recording_capture_target(
                    capture.has_logic,
                    capture.has_scope,
                    capture.has_radio,
                ) else {
                    self.set_error("recording does not contain a supported capture".to_owned());
                    return;
                };
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
                self.set_completed_capture_for_target(target, capture);
                self.logic_marker_settings.clear();
                self.logic_channel_names = logic_channel_names;
                self.scope_channel_names = scope_channel_names;
                self.record_capture = false;
                self.record_decoded_data = false;
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
                if let Some(target) = self.requested_capture_target {
                    if self.capture_is_requested(target) {
                        self.begin_capture(AcquisitionMode::Run, target);
                    }
                }
            }
            Ok(WorkerSuccess::ScopeCalibrationSaved) => {
                self.manual_calibration = None;
                self.status = "DSCope manual calibration saved and verified".to_owned();
                if let Some(target) = self.requested_capture_target {
                    if self.capture_is_requested(target) {
                        self.begin_capture(AcquisitionMode::Run, target);
                    }
                }
            }
            Err(error) => {
                self.record_capture = false;
                self.record_decoded_data = false;
                self.set_error(error);
            }
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
        let targets = self
            .captures
            .iter()
            .map(|capture| capture.target)
            .collect::<Vec<_>>();
        for target in targets {
            self.poll_capture_target(target);
        }
    }

    fn poll_capture_target(&mut self, target: CaptureTarget) {
        let Some(index) = self
            .captures
            .iter()
            .position(|capture| capture.target == target)
        else {
            return;
        };
        let mut active = self.captures.swap_remove(index);
        let mut finish_status = None::<(Result<String, String>, CompletionStatus)>;
        let mut capture_status = None::<String>;
        let protocol_analyzers = self.protocol_analyzer.analyzers.clone();
        let radio_trigger_threshold_dbfs =
            f32::from(self.radio_settings.trigger_level_dbfs_tenths) / 10.0;
        let radio_trigger_posttrigger_samples = radio_microsecond_sample_count(
            self.radio_settings.capture.sample_rate_hz,
            self.radio_settings.trigger_posttrigger_micros,
        );
        {
            let active = &mut active;
            if active.protocol_analyzers != protocol_analyzers {
                match active.logic_decode.reconfigure(protocol_analyzers.clone()) {
                    Ok(()) => active.protocol_analyzers = protocol_analyzers.clone(),
                    Err(error) => {
                        active.session.request_stop();
                        finish_status = Some((Err(error), CompletionStatus::Failed));
                    }
                }
            }
            for _ in 0..MAX_CAPTURE_EVENTS_PER_POLL {
                match active.session.try_recv() {
                    Ok(event) => match event {
                        CaptureEvent::Frame(frame) => {
                            if let Some(trigger) = active.radio_trigger.as_mut() {
                                let update_result = radio_trigger_posttrigger_samples
                                    .as_ref()
                                    .map_err(Clone::clone)
                                    .and_then(|samples| {
                                        trigger.set_threshold_dbfs(radio_trigger_threshold_dbfs)?;
                                        trigger.set_posttrigger_samples(*samples);
                                        Ok(())
                                    });
                                if let Err(error) = update_result {
                                    active.session.request_stop();
                                    finish_status = Some((
                                        Err(format!("RF trigger update failed: {error}")),
                                        CompletionStatus::Failed,
                                    ));
                                    break;
                                }
                            }
                            let trigger_armed = active
                                .radio_trigger
                                .as_ref()
                                .is_some_and(RadioSingleTrigger::is_armed);
                            if trigger_armed {
                                if let CaptureBatch::Iq(iq) = &frame.payload {
                                    for spectrum in active.radio_spectrum.push(iq) {
                                        active.waveforms.radio.ingest(spectrum);
                                    }
                                }
                            }
                            let frame_is_iq = matches!(&frame.payload, CaptureBatch::Iq(_));
                            let (frames, trigger_event, trigger_complete) =
                                if let (Some(trigger), true) =
                                    (active.radio_trigger.as_mut(), frame_is_iq)
                                {
                                    match trigger.push(frame) {
                                        Ok(output) => {
                                            (output.frames, output.event, output.complete)
                                        }
                                        Err(error) => {
                                            active.session.request_stop();
                                            finish_status = Some((
                                                Err(format!("RF trigger failed: {error}")),
                                                CompletionStatus::Failed,
                                            ));
                                            break;
                                        }
                                    }
                                } else {
                                    (vec![frame], None, false)
                                };
                            if let Some(event) = trigger_event {
                                active.radio_spectrum.reset();
                                capture_status = Some(format!(
                                    "RF trigger · {:.3} MHz at {:.1} dBFS; capturing…",
                                    event.frequency_hz / 1_000_000.0,
                                    event.power_dbfs
                                ));
                            }
                            for frame in frames {
                                if let Err(error) = ingest_active_capture_frame(
                                    active,
                                    frame,
                                    radio_trigger_threshold_dbfs,
                                ) {
                                    active.session.request_stop();
                                    finish_status = Some((Err(error), CompletionStatus::Failed));
                                    break;
                                }
                            }
                            if finish_status.is_some() {
                                break;
                            }
                            if trigger_complete {
                                active.session.request_stop();
                                finish_status = Some((
                                    Ok("RF triggered capture complete".to_owned()),
                                    CompletionStatus::Complete,
                                ));
                                break;
                            }
                        }
                        CaptureEvent::SourceFailed { device_id, error } => {
                            if let Some(recorder) = active.recorder.as_mut() {
                                if let Err(write_error) =
                                    recorder.write_event(&CaptureEvent::SourceFailed {
                                        device_id: device_id.clone(),
                                        error: error.clone(),
                                    })
                                {
                                    active.session.request_stop();
                                    finish_status = Some((
                                        Err(format!("failed to write recording: {write_error}")),
                                        CompletionStatus::Failed,
                                    ));
                                    break;
                                }
                            }
                            active.session.request_stop();
                            finish_status = Some((
                                Err(format!("{device_id}: {error}")),
                                CompletionStatus::Failed,
                            ));
                            break;
                        }
                        CaptureEvent::SourceStopped { device_id } => {
                            if let Some(recorder) = active.recorder.as_mut() {
                                if let Err(error) =
                                    recorder.write_event(&CaptureEvent::SourceStopped {
                                        device_id: device_id.clone(),
                                    })
                                {
                                    active.session.request_stop();
                                    finish_status = Some((
                                        Err(format!("failed to write recording: {error}")),
                                        CompletionStatus::Failed,
                                    ));
                                    break;
                                }
                            }
                            active.stopped.insert(device_id);
                            if active.stopped.len() == active.session.descriptors().len() {
                                finish_status = Some((
                                    Ok("Capture complete".to_owned()),
                                    CompletionStatus::Complete,
                                ));
                                break;
                            }
                        }
                    },
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
            loop {
                match active.logic_decode.try_recv() {
                    Ok(output) => ingest_logic_decode_output(active, output),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        if finish_status.is_none() {
                            active.session.request_stop();
                            finish_status = Some((
                                Err("logic decode worker disconnected".to_owned()),
                                CompletionStatus::Failed,
                            ));
                        }
                        break;
                    }
                }
            }
            loop {
                match active.radio_dsp.try_recv() {
                    Ok(output) => {
                        if let Err(error) =
                            record_decoded_radio_packets(&mut active.decoded_data_recorder, &output)
                        {
                            active.session.request_stop();
                            finish_status = Some((Err(error), CompletionStatus::Failed));
                            break;
                        }
                        ingest_radio_dsp_output(&mut active.waveforms, output);
                        if let Err(error) = record_decoded_raw_messages(
                            &mut active.decoded_data_recorder,
                            &active.waveforms.radio_analysis,
                        ) {
                            active.session.request_stop();
                            finish_status = Some((Err(error), CompletionStatus::Failed));
                            break;
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        if finish_status.is_none() {
                            active.session.request_stop();
                            finish_status = Some((
                                Err("radio DSP worker disconnected".to_owned()),
                                CompletionStatus::Failed,
                            ));
                        }
                        break;
                    }
                }
            }
        }

        if let Some(status) = capture_status {
            self.status = status;
        }

        let Some((result, completion)) = finish_status else {
            self.captures.push(active);
            return;
        };
        {
            let mode = active.mode;
            let target = active.target;
            let preserve_display_until_complete = active.preserve_display_until_complete;
            let recording_path = active.recording_path.take();
            let decoded_data_path = active.decoded_data_path.take();
            let has_logic = active.has_logic;
            let has_scope = active.has_scope;
            let has_radio = active.has_radio;
            let restart_for_frontend = self.scope_capture_restart_requested
                && self.capture_is_requested(target)
                && target.includes_oscilloscope();
            self.scope_capture_restart_requested = false;
            let (protocol_decode, logic_decode_error) = finish_logic_decode(&mut active);
            let stop_result = active.session.stop();
            let mut error = result.as_ref().err().cloned();
            if let Err(stop_error) = &stop_result {
                append_error(&mut error, format!("failed to stop capture: {stop_error}"));
            }
            match active.radio_dsp.finish() {
                Ok(outputs) => {
                    for output in outputs {
                        if let Err(recording_error) =
                            record_decoded_radio_packets(&mut active.decoded_data_recorder, &output)
                        {
                            append_error(&mut error, recording_error);
                        }
                        ingest_radio_dsp_output(&mut active.waveforms, output);
                        if let Err(recording_error) = record_decoded_raw_messages(
                            &mut active.decoded_data_recorder,
                            &active.waveforms.radio_analysis,
                        ) {
                            append_error(&mut error, recording_error);
                        }
                    }
                }
                Err(dsp_error) => append_error(&mut error, dsp_error),
            }
            if let Some(decode_error) = logic_decode_error {
                append_error(&mut error, decode_error);
            }
            active.waveforms.radio_analysis.finish_raw_capture();
            if let Err(recording_error) = record_decoded_raw_messages(
                &mut active.decoded_data_recorder,
                &active.waveforms.radio_analysis,
            ) {
                append_error(&mut error, recording_error);
            }
            let recording_completion = if stop_result.is_ok() && error.is_none() {
                completion
            } else {
                CompletionStatus::Failed
            };
            let recording_result = finish_gui_recording(&mut active.recorder, recording_completion);
            let decoded_recording_result = finish_decoded_data_recording(
                &mut active.decoded_data_recorder,
                recording_completion,
            );
            if let Err(recording_error) = recording_result {
                append_error(
                    &mut error,
                    format!("failed to finish recording: {recording_error}"),
                );
            }
            if let Err(recording_error) = decoded_recording_result {
                append_error(
                    &mut error,
                    format!("failed to finish decoded data recording: {recording_error}"),
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
                        protocol_decode,
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
                        self.capture_is_requested(target),
                        completion,
                        false,
                    ));
            if !repeat {
                self.capture_requests.remove(&target);
            }
            self.record_capture = false;
            self.record_decoded_data = false;
            match error {
                Some(error) => self.set_error(error),
                None if restart_for_frontend => {
                    self.status = "Reconfiguring oscilloscope frontend…".to_owned();
                }
                None => {
                    let capture_status =
                        result.ok().unwrap_or_else(|| "Capture complete".to_owned());
                    self.status = decoded_data_path
                        .or(recording_path)
                        .map_or(capture_status.clone(), |path| {
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
                } else if self.pending.is_none() {
                    self.begin_capture(mode, target);
                } else if !self
                    .queued_capture_rearms
                    .iter()
                    .any(|(_, queued_target)| *queued_target == target)
                {
                    self.queued_capture_rearms.push((mode, target));
                }
            }
        }
    }

    fn poll_repetitive_capture(&mut self) {
        let Some(scheduled) = self.scheduled_repetitive_capture else {
            return;
        };
        if !self.capture_is_requested(scheduled.target) {
            self.scheduled_repetitive_capture = None;
            return;
        }
        if Instant::now() < scheduled.due {
            return;
        }
        self.scheduled_repetitive_capture = None;
        self.begin_capture(AcquisitionMode::Repetitive, scheduled.target);
    }

    fn poll_queued_capture_rearms(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let Some(index) = self.queued_capture_rearms.iter().position(|(_, target)| {
            self.capture_is_requested(*target) && !self.capture_target_is_active(*target)
        }) else {
            return;
        };
        let (mode, target) = self.queued_capture_rearms.remove(index);
        self.begin_capture(mode, target);
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
        if target.includes_logic_analyzer() {
            self.protocol_decode_cache = ProtocolDecodeCache::default();
        }
        if target.includes_oscilloscope() {
            self.scope_measurement_caches = [ScopeMeasurementCache::default(); 2];
        }
        if target.includes_radio_receiver() {
            self.radio_view.borrow_mut().clear_capture();
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
        if let Some(target) = self.requested_capture_target.take() {
            self.capture_requests.remove(&target);
            self.queued_capture_rearms
                .retain(|(_, queued_target)| *queued_target != target);
        }
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
        let displayed_logic = self.captures.iter().any(|capture| capture.has_logic)
            || self.logic_sessions.any(|capture| capture.has_logic)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_logic);
        let displayed_scope = self.captures.iter().any(|capture| capture.has_scope)
            || self.scope_sessions.any(|capture| capture.has_scope)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_scope);
        let displayed_radio = self.captures.iter().any(|capture| capture.has_radio)
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
        let has_logic_capture = self.captures.iter().any(|capture| capture.has_logic)
            || self.logic_sessions.any(|capture| capture.has_logic)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_logic);
        let has_scope_capture = self.captures.iter().any(|capture| capture.has_scope)
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

    fn capture_is_requested(&self, target: CaptureTarget) -> bool {
        self.capture_requests.contains(&target)
    }

    fn capture_for_target(&self, target: CaptureTarget) -> Option<&ActiveCapture> {
        self.captures
            .iter()
            .find(|capture| capture.target.includes(target))
    }

    fn capture_for_target_mut(&mut self, target: CaptureTarget) -> Option<&mut ActiveCapture> {
        self.captures
            .iter_mut()
            .find(|capture| capture.target.includes(target))
    }

    fn capture_target_is_active(&self, target: CaptureTarget) -> bool {
        self.captures
            .iter()
            .any(|capture| capture.target.overlaps(target))
    }

    fn capture_target_can_start(&self, target: CaptureTarget) -> bool {
        self.pending.is_none()
            && !self.capture_target_is_active(target)
            && !self
                .capture_requests
                .iter()
                .any(|requested| requested.overlaps(target))
    }

    fn active_capture_for_view(&self) -> Option<&ActiveCapture> {
        self.active_waveform_tab
            .map(WaveformTab::capture_target)
            .and_then(|target| self.capture_for_target(target))
    }

    fn is_busy(&self) -> bool {
        self.pending.is_some()
            || !self.captures.is_empty()
            || self.scheduled_repetitive_capture.is_some()
    }
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

fn radio_single_sample_limit(sample_rate_hz: u64, duration_millis: u64) -> Result<u64, String> {
    if sample_rate_hz == 0 || duration_millis == 0 {
        return Err("RF Single duration and sample rate must be greater than zero".to_owned());
    }
    radio_duration_sample_count(sample_rate_hz, duration_millis)
}

fn radio_duration_sample_count(sample_rate_hz: u64, duration_millis: u64) -> Result<u64, String> {
    if sample_rate_hz == 0 {
        return Err("RF sample rate must be greater than zero".to_owned());
    }
    let samples = u128::from(sample_rate_hz)
        .saturating_mul(u128::from(duration_millis))
        .div_ceil(1_000);
    u64::try_from(samples).map_err(|_| "RF Single capture is too long".to_owned())
}

fn radio_microsecond_sample_count(
    sample_rate_hz: u64,
    duration_micros: u64,
) -> Result<u64, String> {
    if sample_rate_hz == 0 {
        return Err("RF sample rate must be greater than zero".to_owned());
    }
    let samples = u128::from(sample_rate_hz)
        .saturating_mul(u128::from(duration_micros))
        .div_ceil(1_000_000);
    u64::try_from(samples).map_err(|_| "RF post-trigger interval is too long".to_owned())
}

fn radio_single_trigger_config(
    settings: &RadioSettings,
) -> Result<RadioSingleTriggerConfig, String> {
    if settings.trigger_posttrigger_micros > RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS {
        return Err(format!(
            "RF post-trigger duration must not exceed {} ms",
            RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS / 1_000
        ));
    }
    Ok(RadioSingleTriggerConfig {
        threshold_dbfs: f32::from(settings.trigger_level_dbfs_tenths) / 10.0,
        posttrigger_samples: radio_microsecond_sample_count(
            settings.capture.sample_rate_hz,
            settings.trigger_posttrigger_micros,
        )?,
        fft_length: RADIO_SPECTRUM_FFT_LENGTH,
        iq_correction_enabled: settings.iq_correction_enabled,
    })
}

fn radio_trigger_level_controls_are_enabled(
    capture_settings_enabled: bool,
    trigger_runtime_enabled: bool,
) -> bool {
    capture_settings_enabled || trigger_runtime_enabled
}

fn radio_trigger_level_runtime_controls_are_enabled(
    run_runtime_enabled: bool,
    single_trigger_runtime_enabled: bool,
) -> bool {
    run_runtime_enabled || single_trigger_runtime_enabled
}

fn hackrf_runtime_settings(settings: &RadioSettings) -> Result<HackRfRuntimeSettings, String> {
    Ok(HackRfRuntimeSettings {
        center_frequency_hz: settings.capture.center_frequency_hz,
        bandwidth_hz: settings.capture.bandwidth_hz,
        lna_gain_db: radio_gain_u32(settings, "lna")?,
        vga_gain_db: radio_gain_u32(settings, "vga")?,
        amplifier_enabled: settings.amplifier_enabled,
        frequency_correction_milli_ppm: settings.frequency_correction_milli_ppm,
    })
}

fn radio_runtime_settings_differ(current: &RadioSettings, previous: &RadioSettings) -> bool {
    current.capture.center_frequency_hz != previous.capture.center_frequency_hz
        || current.capture.bandwidth_hz != previous.capture.bandwidth_hz
        || current.gains_db != previous.gains_db
        || current.amplifier_enabled != previous.amplifier_enabled
        || current.frequency_correction_milli_ppm != previous.frequency_correction_milli_ppm
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
        self.cpu_load.update();
        self.memory_usage.update();
        context.request_repaint_after(CPU_LOAD_SAMPLE_INTERVAL);
        self.poll_calibration_progress();
        self.poll_worker();
        self.reconcile_capture_settings_tab();
        self.reconcile_waveform_tab();
        self.reconcile_device_selections();
        self.reconcile_radio_settings();
        self.poll_capture();
        self.poll_repetitive_capture();
        self.poll_queued_capture_rearms();
        self.finish_pending_snapshot(context);

        let mut actions = FrameActions::default();
        let control_target = self.active_waveform_tab.map(WaveformTab::capture_target);
        let capture_settings_enabled = control_target
            .map_or(true, |target| !self.capture_target_is_active(target))
            && self.pending.is_none();
        let radio_runtime_controls_enabled = self
            .capture_for_target(CaptureTarget::RadioReceiver)
            .is_some_and(|active| {
                active.mode == AcquisitionMode::Run
                    && active.has_radio
                    && active.radio_runtime_control.is_some()
            });
        let can_attach_run_capture_recording = control_target
            .and_then(|target| self.capture_for_target(target))
            .is_some_and(|active| {
                active_continuous_capture_accepts_recording(active.mode, active.recorder.is_some())
            });
        let active_radio_run_recorders = self
            .capture_for_target(CaptureTarget::RadioReceiver)
            .filter(|active| active.mode == AcquisitionMode::Run && active.has_radio)
            .map(|active| {
                (
                    active.recorder.is_some(),
                    active.decoded_data_recorder.is_some(),
                )
            });
        let can_attach_run_decoded_recording =
            active_radio_run_recorders.is_some_and(|(_, decoded_active)| !decoded_active);
        let radio_trigger_runtime_controls_enabled = self
            .capture_for_target(CaptureTarget::RadioReceiver)
            .is_some_and(|active| {
                active.has_radio
                    && active
                        .radio_trigger
                        .as_ref()
                        .is_some_and(RadioSingleTrigger::is_armed)
            });
        let radio_trigger_level_runtime_controls_enabled =
            radio_trigger_level_runtime_controls_are_enabled(
                radio_runtime_controls_enabled,
                radio_trigger_runtime_controls_enabled,
            );
        let rf_trigger_level_controls_enabled = radio_trigger_level_controls_are_enabled(
            capture_settings_enabled,
            radio_trigger_level_runtime_controls_enabled,
        );
        let radio_settings_before_ui = self.radio_settings.clone();
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
        let scope_capture_requested = self
            .capture_requests
            .iter()
            .any(|target| target.includes_oscilloscope());
        let active_scope_capture = self.capture_for_target(CaptureTarget::Oscilloscope);
        let manual_calibration_enabled = manual_calibration_control_is_enabled(
            self.pending.is_some(),
            selected_scope_device,
            self.manual_calibration.is_some(),
            active_scope_capture.map(|active| active.mode),
        );
        let trigger_controls_enabled = scope_trigger_controls_are_enabled(
            self.pending.is_some(),
            scope_capture_requested,
            selected_scope_device,
            active_scope_capture.map(|active| !active.scope_trigger_controls.is_empty()),
        );
        let frontend_controls_enabled = scope_frontend_controls_are_enabled(
            self.pending.is_some(),
            scope_capture_requested,
            selected_scope_device,
        ) && !((self.record_capture || self.record_decoded_data)
            && self.pending.is_some());
        let scope_settings_before_ui = self.scope_settings;
        let scope_auto_sample_rate_before_ui = self.scope_auto_sample_rate;
        let manual_calibration_before_ui =
            self.manual_calibration.as_ref().map(|editor| editor.values);
        let manual_calibration_range_before_ui =
            self.manual_calibration.as_ref().map(|editor| editor.range);

        self.paint_toolbar(
            context,
            ToolbarInputs {
                control_target,
                control_target_available,
                can_attach_run_capture_recording,
                can_attach_run_decoded_recording,
            },
            &mut actions,
        );

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

        self.paint_capture_setup_panel(
            context,
            CaptureSetupInputs {
                capture_settings_enabled,
                radio_runtime_controls_enabled,
                radio_trigger_runtime_controls_enabled,
                frontend_controls_enabled,
                manual_calibration_enabled,
                logic_settings_speed,
            },
            &mut actions,
        );

        if let Some(kind) = actions.save_capture_settings_kind {
            self.save_capture_settings(kind);
        }

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

        self.paint_scope_tools_panel(context, trigger_controls_enabled);

        self.paint_status_bar(context);

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
            let sessions_enabled = self.pending.is_none() && !self.capture_target_is_active(target);
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
                                                .on_hover_selectable_text(if sessions_enabled {
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
                                            .on_hover_selectable_text(if !sessions_enabled {
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
                                    .on_hover_selectable_text(if tab_count >= MAX_SESSION_TABS {
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
                // Radio and combined views have no session tabs.
                CaptureTarget::RadioReceiver | CaptureTarget::Combined => false,
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
                            CaptureTarget::RadioReceiver | CaptureTarget::Combined => None,
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

        self.paint_central_panel(
            context,
            CentralPanelInputs {
                trigger_controls_enabled,
                frontend_controls_enabled,
                rf_trigger_level_controls_enabled,
                scope_settings_before_ui,
                scope_auto_sample_rate_before_ui,
                snapshot_clicked: actions.snapshot_clicked,
            },
        );

        self.paint_settings_window(
            context,
            radio_runtime_controls_enabled,
            radio_trigger_runtime_controls_enabled,
            &mut actions.refresh_clicked,
            &mut actions.radio_device_selection_changed,
        );

        let dismiss_usb_speed_warning = self
            .usb_speed_warning
            .as_ref()
            .is_some_and(|warning| paint_usb_speed_warning(context, warning));
        if dismiss_usb_speed_warning {
            self.usb_speed_warning = None;
        }

        let auto_confirmed = self.paint_auto_calibration_confirmation(context);

        let save_manual_values = self.paint_manual_calibration_window(context);

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

        if actions.record_now_clicked {
            self.record_capture = false;
            self.record_decoded_data = false;
            if let Some(path) = choose_recording_save_path(&self.recording_path) {
                self.recording_path = path.to_string_lossy().into_owned();
                if can_attach_run_capture_recording {
                    if let Some(target) = control_target {
                        self.attach_run_capture_recording(target, path);
                    }
                } else if let Some(target) = control_target {
                    self.record_capture = true;
                    let mode = if target == CaptureTarget::RadioReceiver {
                        AcquisitionMode::Run
                    } else {
                        AcquisitionMode::Single
                    };
                    actions.start_request = Some((mode, target));
                }
            }
        }
        if actions.record_decoded_data_clicked {
            self.record_capture = false;
            self.record_decoded_data = false;
            if let Some(path) = choose_decoded_data_save_path(&self.decoded_data_path) {
                self.decoded_data_path = path.to_string_lossy().into_owned();
                if can_attach_run_decoded_recording {
                    self.attach_run_decoded_data_recording(path);
                } else if control_target == Some(CaptureTarget::RadioReceiver) {
                    self.record_decoded_data = true;
                    actions.start_request =
                        Some((AcquisitionMode::Run, CaptureTarget::RadioReceiver));
                }
            }
        }
        if actions.open_recording_clicked {
            if let Some(path) = choose_recording_to_open() {
                self.open_selected_recording(path);
            }
        }
        if actions.export_saleae_clicked {
            if let Some(path) = choose_saleae_export_path() {
                self.begin_export_saleae(path);
            }
        }
        if actions.refresh_clicked {
            self.begin_refresh();
        }
        if !actions.open_recording_clicked {
            if let Some((mode, target)) = actions.start_request {
                self.begin_capture(mode, target);
            }
        }
        if actions.stop_clicked {
            if let Some(target) = control_target {
                let requested = self
                    .capture_requests
                    .iter()
                    .copied()
                    .find(|requested| requested.includes(target));
                if let Some(requested) = requested {
                    self.capture_requests.remove(&requested);
                    self.queued_capture_rearms
                        .retain(|(_, target)| *target != requested);
                    if self.requested_capture_target == Some(requested) {
                        self.requested_capture_target = None;
                    }
                    if self
                        .scheduled_repetitive_capture
                        .is_some_and(|scheduled| scheduled.target == requested)
                    {
                        self.scheduled_repetitive_capture = None;
                    }
                }
                self.scope_capture_restart_requested = false;
                self.scope_configuration_due = None;
                if self.capture_target_is_active(target) {
                    self.stop_capture(target);
                } else if self.pending.is_some() {
                    self.status = "Stopping acquisition after device preparation…".to_owned();
                } else {
                    self.status = "Acquisition stopped".to_owned();
                }
            }
        }
        match actions.calibration_action {
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
        if actions.logic_device_selection_changed || actions.scope_device_selection_changed {
            if let Some(active) = self.active_waveform_tab {
                self.save_waveform_view_settings(active);
            }
            self.last_error = None;
            self.status = "Capture device selection updated".to_owned();
        }
        if actions.radio_device_selection_changed {
            self.last_error = None;
            self.status = "Radio device selection updated".to_owned();
        }
        if radio_runtime_settings_differ(&self.radio_settings, &radio_settings_before_ui) {
            let runtime_control = self
                .capture_for_target(CaptureTarget::RadioReceiver)
                .and_then(|active| {
                    (active.mode == AcquisitionMode::Run && active.has_radio)
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
        if actions.scope_device_selection_changed
            && self.selected_scope_device_ref().is_some()
            && self.pending.is_none()
            && !self.capture_target_is_active(CaptureTarget::Oscilloscope)
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
                self.capture_for_target(CaptureTarget::Oscilloscope),
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
        } else if trigger_changed && self.capture_target_is_active(CaptureTarget::Oscilloscope) {
            let update_result = self
                .scope_settings
                .trigger
                .capture_config()
                .and_then(|trigger| {
                    let Some(active) = self.capture_for_target_mut(CaptureTarget::Oscilloscope)
                    else {
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
        if !actions.refresh_clicked
            && actions.start_request.is_none()
            && !actions.stop_clicked
            && scope_configuration_is_due
        {
            if scope_capture_requested && selected_scope_device {
                self.scope_configuration_due = None;
                self.scope_capture_restart_requested = true;
                if let Some(active) = self.capture_for_target(CaptureTarget::Oscilloscope) {
                    active.session.request_stop();
                }
                self.status = "Reconfiguring oscilloscope frontend…".to_owned();
            } else if self.pending.is_none()
                && !self.capture_target_is_active(CaptureTarget::Oscilloscope)
            {
                self.begin_scope_configuration();
            }
        }
        if self.pending.is_some() || !self.captures.is_empty() {
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
    radio_dsp: RadioDspWorker,
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
    let radio_analyzers = radio_request
        .as_ref()
        .map(|request| {
            request
                .analyzer_profiles
                .iter()
                .cloned()
                .zip(request.analyzer_channel_names.iter())
                .map(|(profile, name)| {
                    RadioAnalyzerRuntime::new_named(
                        profile,
                        request.decoder_config.clone(),
                        name.clone(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
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
    let radio_dsp_spectrum =
        SpectrumProcessor::new(RADIO_SPECTRUM_FFT_LENGTH, SpectrumWindow::Hann)?
            .with_iq_correction(iq_correction_enabled);
    let radio_dsp = RadioDspWorker::start(radio_dsp_spectrum, radio_analyzers);
    let session = CaptureCoordinator::start(sources).map_err(|error| error.to_string())?;
    Ok(StartedCapture {
        session,
        scope_trigger_controls,
        scope_calibration_controls,
        radio_runtime_control,
        radio_spectrum,
        radio_dsp,
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
    paint: impl FnOnce(&mut egui::Ui, Rect),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use wavecraft_core::{Iq8, IqBatch, LogicBatch, ScopeBatch};
    use wavecraft_sdr::RadioProviderAdapter;

    #[test]
    fn embedded_wavecraft_app_icon_is_valid() {
        let icon = wavecraft_app_icon().expect("embedded icon decodes");

        assert_eq!((icon.width, icon.height), (512, 512));
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
        assert_eq!(icon.rgba[3], 0);
    }

    #[test]
    fn default_logic_capture_config_is_valid() {
        // The limit helpers fall back to the default rate and count when this
        // configuration cannot be built, so it must stay buildable.
        assert!(default_logic_capture_config().is_some());
    }

    #[test]
    fn recording_capture_target_covers_every_instrument_combination() {
        let expected = [
            ((false, false, false), None),
            ((true, false, false), Some(CaptureTarget::LogicAnalyzer)),
            ((false, true, false), Some(CaptureTarget::Oscilloscope)),
            ((false, false, true), Some(CaptureTarget::RadioReceiver)),
            ((true, true, false), Some(CaptureTarget::Combined)),
            ((true, true, true), Some(CaptureTarget::Combined)),
            // Combinations that include radio but not both wired instruments
            // cannot be shown together and must be rejected, not panic.
            ((true, false, true), None),
            ((false, true, true), None),
        ];
        for ((logic, scope, radio), target) in expected {
            assert_eq!(
                recording_capture_target(logic, scope, radio),
                target,
                "logic={logic} scope={scope} radio={radio}"
            );
        }
    }

    #[test]
    fn cpu_load_monitor_compares_process_time_with_wall_time() {
        let start = Instant::now();
        let mut monitor = CpuLoadMonitor {
            last_wall_time: start,
            last_process_time: Some(Duration::from_secs(1)),
            load_percent: None,
        };

        monitor.update_at(
            start + Duration::from_secs(2),
            Some(Duration::from_millis(3_500)),
        );

        assert_eq!(monitor.load_percent, Some(125.0));
        assert_eq!(monitor.label(), "CPU 125%");
    }

    #[test]
    fn memory_usage_monitor_formats_resident_memory() {
        let monitor = MemoryUsageMonitor {
            last_sample_time: Instant::now(),
            resident_bytes: Some(384 * 1024 * 1024),
        };

        assert_eq!(monitor.label(), "RAM 384.0 MiB");
    }

    #[test]
    fn saved_logic_settings_round_trip_independently() {
        let mut settings = LogicCaptureSettings {
            sample_rate_hz: 100_000_000,
            sample_count: 2_000_000,
            enabled_channels: std::array::from_fn(|channel| channel < 8),
            maximum_heights: std::array::from_fn(|channel| (channel % 5 + 1) as u8),
            operation_mode: DsLogicOperationMode::Buffer,
            stop_option: DsLogicStopOption::StopImmediately,
            threshold_voltage: 2.5,
            input_filter: DsLogicInputFilter::OneSampleClock,
            rle_enabled: true,
            external_clock: true,
            falling_clock_edge: true,
            trigger_enabled: true,
            trigger_mode: LogicTriggerMode::Smart,
            trigger_conditions: [None; LOGIC_CHANNEL_COUNT],
            smart_trigger_stage_count: 3,
            smart_trigger_conditions: [[None; LOGIC_CHANNEL_COUNT]; DSLOGIC_TRIGGER_STAGE_COUNT],
        };
        settings.smart_trigger_conditions[0][0] = Some(DsLogicTriggerCondition::RisingEdge);
        settings.smart_trigger_conditions[1][1] = Some(DsLogicTriggerCondition::High);
        settings.smart_trigger_conditions[2][2] = Some(DsLogicTriggerCondition::FallingEdge);
        let channel_names = std::array::from_fn(|channel| format!("L{channel}"));

        let metadata = saved_logic_settings_metadata(
            settings,
            AcquisitionMode::Repetitive,
            2.5,
            false,
            &channel_names,
        );

        assert_eq!(logic_settings_from_recording_metadata(&metadata), settings);
        assert_eq!(metadata["logic.acquisition_mode"], "repetitive".to_owned());
        assert_eq!(metadata["logic.repetitive_interval_seconds"], "2.5");
        assert_eq!(metadata["logic.smart_timing_enabled"], "false");
        assert_eq!(
            channel_names_from_recording_metadata(&metadata).0,
            channel_names
        );
    }

    #[test]
    fn saved_scope_settings_round_trip_independently() {
        let settings = ScopeCaptureSettings {
            sample_rate_hz: 500_000_000,
            sample_count: 1_000_000,
            channels: [
                ScopeChannelSettings {
                    enabled: true,
                    volts_per_div_mv: 500,
                    coupling: DsCopeCoupling::Ac,
                    probe_attenuation: 10,
                    vertical_offset: 120,
                },
                ScopeChannelSettings {
                    enabled: true,
                    volts_per_div_mv: 2_000,
                    coupling: DsCopeCoupling::Dc,
                    probe_attenuation: 100,
                    vertical_offset: 140,
                },
            ],
            trigger: ScopeTriggerSettings {
                position_percent: 70,
                source: DsCopeTriggerSource::ChannelZeroOrOne,
                auto_channel: 1,
                slope: DsCopeTriggerSlope::Falling,
                holdoff_ns: 2_000_000,
                holdoff_unit: TriggerHoldoffUnit::Milliseconds,
                noise_sensitivity: 4,
                levels: [100, 140],
            },
            operation_mode: DsCopeOperationMode::InternalTest,
            bandwidth_limit: DsCopeBandwidthLimit::Mhz20,
        };
        let names = ["Input".to_owned(), "Reference".to_owned()];
        let colors = [Color32::RED, Color32::BLUE];

        let metadata = saved_scope_settings_metadata(settings, false, true, &names, colors);

        assert_eq!(scope_settings_from_recording_metadata(&metadata), settings);
        assert_eq!(metadata["scope.auto_sample_rate"], "false");
        assert_eq!(metadata["scope.smart_timing_enabled"], "true");
        assert_eq!(channel_names_from_recording_metadata(&metadata).1, names);
        assert_eq!(parse_color(&metadata["scope.ch0.color"]), Some(colors[0]));
        assert_eq!(parse_color(&metadata["scope.ch1.color"]), Some(colors[1]));
    }

    /// Every field is listed and set away from its default, so a field added to
    /// `RadioSettings` or `RadioAnalyzerSettings` fails to compile here until it is
    /// given a value, and a field that is not saved or restored fails the
    /// comparison instead of silently reopening with its default.
    #[test]
    fn saved_rf_settings_round_trip_every_field() {
        let settings = RadioSettings {
            capture: RadioCaptureSettings {
                center_frequency_hz: 915_000_000,
                sample_rate_hz: 10_000_000,
                bandwidth_hz: 8_000_000,
                receive_channel: 1,
                // Derived from `single_duration_millis` when a Single capture
                // starts, so it is deliberately not saved.
                sample_limit: None,
            },
            single_duration_millis: 2_500,
            trigger_enabled: true,
            trigger_posttrigger_micros: 25_000,
            trigger_level_dbfs_tenths: -350,
            gains_db: BTreeMap::from([("lna".to_owned(), 24), ("vga".to_owned(), 20)]),
            frequency_correction_milli_ppm: 1_250,
            iq_correction_enabled: false,
            amplifier_enabled: true,
            antenna_power_enabled: true,
            analyzer: RadioAnalyzerSettings {
                demodulation: RadioDemodulation::BinaryGfsk,
                channels: vec![
                    RadioAnalyzerChannelSettings {
                        name: "Weather".to_owned(),
                        frequency_offset_hz: 125_000,
                    },
                    RadioAnalyzerChannelSettings {
                        name: "Control".to_owned(),
                        frequency_offset_hz: -250_000,
                    },
                ],
                channel_bandwidth_hz: 250_000,
                output_sample_rate_hz: 600_000,
                filter_tap_count: 129,
                symbol_rate_hz: 38_400,
                deviation_hz: 50_000,
                gfsk_bt_milli: 300,
                inverted: true,
                adaptive_timing: false,
                timing_loop_bandwidth_milli: 50,
                max_clock_error_ppm: 5_000,
                data_mode: RadioDataMode::Packets,
                preamble_byte: 0x55,
                preamble_bytes: 6,
                sync_word: 0x2dd4,
                sync_word_bytes: 2,
                maximum_preamble_bit_errors: 3,
                minimum_bit_confidence_milli: 250,
                maximum_uncertain_bits: 2,
                whitening_enabled: true,
                whitening_preset: RadioWhiteningPreset::Pn9X9X5,
                whitening_width: 9,
                whitening_polynomial: 0x11,
                whitening_seed: 0x1ff,
                whitening_direction: WhiteningDirection::ShiftLeft,
                whitening_invert_output: true,
                whitening_start_bit: 8,
                whitening_end_bit: Some(160),
                whitening_advance_excluded_bits: true,
            },
        };

        let metadata = saved_radio_settings_metadata(&settings);

        assert_eq!(radio_settings_from_recording_metadata(&metadata), settings);
    }

    #[test]
    fn saved_rf_settings_round_trip_independently() {
        let mut settings = RadioSettings::default();
        settings.capture.center_frequency_hz = 915_000_000;
        settings.capture.sample_rate_hz = 10_000_000;
        settings.capture.bandwidth_hz = 8_000_000;
        settings.single_duration_millis = 2_500;
        settings.trigger_enabled = true;
        settings.trigger_posttrigger_micros = 25_000;
        settings.trigger_level_dbfs_tenths = -350;
        settings.frequency_correction_milli_ppm = 1_250;
        settings.iq_correction_enabled = false;
        settings.amplifier_enabled = true;
        settings.antenna_power_enabled = true;
        settings.gains_db.insert("lna".to_owned(), 24);
        settings.gains_db.insert("vga".to_owned(), 20);
        settings.analyzer.demodulation = RadioDemodulation::BinaryFsk;
        settings.analyzer.channels = vec![
            RadioAnalyzerChannelSettings {
                name: "Weather".to_owned(),
                frequency_offset_hz: 125_000,
            },
            RadioAnalyzerChannelSettings {
                name: "Control".to_owned(),
                frequency_offset_hz: -250_000,
            },
        ];
        settings.analyzer.sync_word = 0xd391;
        settings.analyzer.sync_word_bytes = 2;
        settings.analyzer.data_mode = RadioDataMode::Packets;
        settings.analyzer.whitening_enabled = true;
        settings.analyzer.whitening_width = 9;
        settings.analyzer.whitening_polynomial = 0x11;
        settings.analyzer.whitening_seed = 0x1ff;
        settings.analyzer.whitening_direction = WhiteningDirection::ShiftLeft;
        settings.analyzer.whitening_invert_output = true;
        settings.analyzer.whitening_start_bit = 8;
        settings.analyzer.whitening_end_bit = Some(160);
        settings.analyzer.whitening_advance_excluded_bits = true;

        let metadata = saved_radio_settings_metadata(&settings);

        assert_eq!(radio_settings_from_recording_metadata(&metadata), settings);
    }

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
    fn protocol_decode_cache_reuses_outcomes_until_inputs_change() {
        let uart = |instance_id| ProtocolAnalyzerInstance {
            instance_id,
            protocol_id: "1:uart",
            channel_mappings: vec![Some(0)],
            data_format: ProtocolDataFormat::Ascii,
            custom_protocol: None,
        };
        let analyzers = vec![uart(1)];
        let mut waveforms = WaveformStore::default();
        let mut cache = ProtocolDecodeCache::default();

        let first = cache.shared_outcomes(&waveforms, &analyzers, None);
        let again = cache.shared_outcomes(&waveforms, &analyzers, None);
        assert!(
            Arc::ptr_eq(&first, &again),
            "unchanged inputs must not recompute or copy outcomes"
        );

        waveforms.logic_revision += 1;
        let after_new_samples = cache.shared_outcomes(&waveforms, &analyzers, None);
        assert!(!Arc::ptr_eq(&first, &after_new_samples));

        let reconfigured = vec![uart(2)];
        let after_edit = cache.shared_outcomes(&waveforms, &reconfigured, None);
        assert!(!Arc::ptr_eq(&after_new_samples, &after_edit));
    }

    #[test]
    fn protocol_decode_cache_shares_a_matching_streamed_snapshot() {
        let analyzers = vec![ProtocolAnalyzerInstance {
            instance_id: 1,
            protocol_id: "1:uart",
            channel_mappings: vec![Some(0)],
            data_format: ProtocolDataFormat::Ascii,
            custom_protocol: None,
        }];
        let snapshot = ProtocolCaptureDecoder::new(&analyzers).snapshot();
        let waveforms = WaveformStore::default();
        let mut cache = ProtocolDecodeCache::default();

        let outcomes = cache.shared_outcomes(&waveforms, &analyzers, Some(&snapshot));

        assert!(Arc::ptr_eq(&outcomes, &snapshot.shared_outcomes()));
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
    fn radio_defaults_start_with_signal_analysis_disabled() {
        let settings = RadioSettings::default();

        assert_eq!(settings.capture.center_frequency_hz, 868_300_000);
        assert_eq!(settings.capture.sample_rate_hz, 8_000_000);
        assert_eq!(settings.capture.bandwidth_hz, 1_750_000);
        assert_eq!(settings.analyzer.demodulation, RadioDemodulation::None);
        assert!(!settings.analyzer.is_enabled());
        assert_eq!(settings.analyzer.symbol_rate_hz, 19_200);
        assert_eq!(settings.analyzer.deviation_hz, 35_000);
        assert_eq!(settings.analyzer.channels[0].name, "Channel 1");
        assert_eq!(settings.analyzer.channels[0].frequency_offset_hz, -200_000);
        assert_eq!(settings.analyzer.output_sample_rate_hz, 512_000);
        assert_eq!(settings.analyzer.filter_tap_count, 257);
        assert_eq!(settings.analyzer.maximum_preamble_bit_errors, 1);
        assert_eq!(settings.analyzer.minimum_bit_confidence_milli, 0);
        assert_eq!(settings.analyzer.maximum_uncertain_bits, 0);
        assert_eq!(settings.trigger_posttrigger_micros, 2_000);
        settings
            .capture
            .validate(&wavecraft_hackrf::hackrf_one_capabilities())
            .expect("default radio settings should be supported by HackRF One");
    }

    #[test]
    fn analyzer_render_does_not_clamp_values_to_a_temporary_dependent_minimum() {
        let context = egui::Context::default();
        let mut settings = RadioAnalyzerSettings {
            demodulation: RadioDemodulation::BinaryGfsk,
            output_sample_rate_hz: 0,
            symbol_rate_hz: 19_200,
            deviation_hz: 35_000,
            preamble_bytes: 0,
            sync_word: 0xd391,
            sync_word_bytes: 2,
            ..RadioAnalyzerSettings::default()
        };

        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_radio_analyzer_configuration(ui, &mut settings, 0, true);
            });
        });

        assert_eq!(settings.symbol_rate_hz, 19_200);
        assert_eq!(settings.deviation_hz, 35_000);
        assert_eq!(settings.preamble_bytes, 0);
    }

    #[test]
    fn radio_single_duration_derives_an_exact_bounded_sample_limit() {
        assert_eq!(
            radio_single_sample_limit(10_000_000, 1_500).unwrap(),
            15_000_000
        );
        assert_eq!(radio_single_sample_limit(3, 1).unwrap(), 1);
        assert!(radio_single_sample_limit(10_000_000, 0).is_err());
    }

    #[test]
    fn radio_single_trigger_derives_fractional_millisecond_posttrigger_samples() {
        let mut settings = RadioSettings::default();
        settings.capture.sample_rate_hz = 10_000_000;
        settings.trigger_posttrigger_micros = 125;

        let config = radio_single_trigger_config(&settings).unwrap();

        assert_eq!(config.posttrigger_samples, 1_250);
        assert_eq!(config.fft_length, RADIO_SPECTRUM_FFT_LENGTH);
    }

    #[test]
    fn radio_trigger_level_stays_editable_during_run_and_while_single_is_armed() {
        assert!(radio_trigger_level_runtime_controls_are_enabled(
            true, false
        ));
        assert!(radio_trigger_level_runtime_controls_are_enabled(
            false, true
        ));
        assert!(!radio_trigger_level_runtime_controls_are_enabled(
            false, false
        ));
        assert!(radio_trigger_level_controls_are_enabled(false, true));
        assert!(radio_trigger_level_controls_are_enabled(true, false));
        assert!(!radio_trigger_level_controls_are_enabled(false, false));
    }

    #[test]
    fn radio_runtime_settings_include_only_live_hackrf_controls() {
        let capabilities = wavecraft_hackrf::hackrf_one_capabilities();
        let mut initial = RadioSettings::default();
        normalize_radio_settings(&mut initial, &capabilities);
        let mut edited = initial.clone();
        edited.capture.sample_rate_hz = 12_000_000;
        assert!(!radio_runtime_settings_differ(&edited, &initial));

        let mut correction_only = initial.clone();
        correction_only.frequency_correction_milli_ppm = -8_250;
        assert!(radio_runtime_settings_differ(&correction_only, &initial));

        edited.capture.center_frequency_hz = 433_920_000;
        edited.capture.bandwidth_hz = 8_000_000;
        edited.gains_db.insert("lna".to_owned(), 24);
        edited.gains_db.insert("vga".to_owned(), 20);
        edited.amplifier_enabled = true;
        edited.frequency_correction_milli_ppm = 12_500;
        assert!(radio_runtime_settings_differ(&edited, &initial));
        assert_eq!(
            hackrf_runtime_settings(&edited).unwrap(),
            HackRfRuntimeSettings {
                center_frequency_hz: 433_920_000,
                bandwidth_hz: 8_000_000,
                lna_gain_db: Some(24),
                vga_gain_db: Some(20),
                amplifier_enabled: true,
                frequency_correction_milli_ppm: 12_500,
            }
        );
    }

    #[test]
    fn radio_analyzer_settings_build_a_tagged_signal_profile() {
        let mut settings = RadioAnalyzerSettings::default();
        assert!(settings.signal_profiles().unwrap().is_empty());

        settings.demodulation = RadioDemodulation::BinaryGfsk;
        settings.channels = vec![
            RadioAnalyzerChannelSettings::numbered(0, 125_000),
            RadioAnalyzerChannelSettings::numbered(1, -250_000),
        ];
        settings.gfsk_bt_milli = 500;
        let profiles = settings.signal_profiles().unwrap();
        assert_eq!(profiles.len(), 2);
        let profile = profiles[0].clone();

        assert_eq!(profile.channel.frequency_offset_hz, 125_000);
        assert_eq!(profiles[1].channel.frequency_offset_hz, -250_000);
        let DemodulationProfile::BinaryGfsk(gfsk) = profile.demodulation else {
            panic!("GFSK settings should build the GFSK profile variant");
        };
        assert_eq!(gfsk.symbol_rate_hz, 19_200);
        assert_eq!(gfsk.deviation_hz, 35_000);
        assert_eq!(gfsk.bt, 0.5);
        assert!(matches!(gfsk.timing, FskTiming::Adaptive { .. }));
    }

    #[test]
    fn radio_analyzer_supports_up_to_one_hundred_unique_channels() {
        let mut settings = RadioAnalyzerSettings {
            demodulation: RadioDemodulation::BinaryGfsk,
            channels: (0..MAX_RADIO_ANALYZER_CHANNELS)
                .map(|index| RadioAnalyzerChannelSettings::numbered(index, index as i64 * 1_000))
                .collect(),
            ..RadioAnalyzerSettings::default()
        };

        assert_eq!(
            settings.signal_profiles().unwrap().len(),
            MAX_RADIO_ANALYZER_CHANNELS
        );

        settings
            .channels
            .push(RadioAnalyzerChannelSettings::numbered(
                MAX_RADIO_ANALYZER_CHANNELS,
                200_000,
            ));
        assert!(settings
            .signal_profiles()
            .unwrap_err()
            .contains("between 1 and 100"));

        settings.channels = vec![
            RadioAnalyzerChannelSettings::numbered(0, 100_000),
            RadioAnalyzerChannelSettings::numbered(1, 100_000),
        ];
        assert!(settings
            .signal_profiles()
            .unwrap_err()
            .contains("must be unique"));
    }

    #[test]
    fn radio_analyzer_settings_build_a_post_sync_whitening_pipeline() {
        let mut settings = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Packets,
            sync_word: 0xd391,
            sync_word_bytes: 2,
            maximum_preamble_bit_errors: 1,
            whitening_enabled: true,
            whitening_width: 9,
            whitening_polynomial: 0x11,
            whitening_seed: 0x1ff,
            whitening_direction: WhiteningDirection::ShiftLeft,
            whitening_invert_output: true,
            whitening_start_bit: 8,
            whitening_end_bit: Some(160),
            whitening_advance_excluded_bits: true,
            ..RadioAnalyzerSettings::default()
        };

        let RadioDataDecoderConfig::Packets(config) = settings.decoder_config().unwrap() else {
            panic!("packet settings should build a packet decoder");
        };
        assert_eq!(
            config.framing.preamble,
            [true, false, true, false, true, false, true, false].repeat(4)
        );
        assert_eq!(
            config.framing.sync.bits,
            [
                true, true, false, true, false, false, true, true, true, false, false, true, false,
                false, false, true
            ]
        );
        assert_eq!(config.framing.sync.max_bit_errors, 1);
        assert_eq!(config.framing.length, RadioPacketLength::UntilEndOfStream);
        assert_eq!(
            config.framing.whitening,
            Some(AdditiveWhiteningConfig {
                width: 9,
                polynomial: 0x11,
                seed: 0x1ff,
                direction: WhiteningDirection::ShiftLeft,
                invert_output: true,
                start_bit: 8,
                end_bit: Some(160),
                advance_excluded_bits: true,
            })
        );

        settings.sync_word_bytes = 0;
        assert_eq!(
            settings.decoder_config().unwrap_err(),
            "packet decoding requires a sync word"
        );
    }

    #[test]
    fn radio_analyzer_allows_zero_preamble_length() {
        let raw = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Raw,
            preamble_bytes: 0,
            sync_word: 0xd391,
            sync_word_bytes: 2,
            ..RadioAnalyzerSettings::default()
        };
        assert!(matches!(
            raw.decoder_config().unwrap(),
            RadioDataDecoderConfig::Raw(RawRadioConfig {
                preamble_bytes: 0,
                sync_word: 0xd391,
                sync_word_bytes: 2,
                ..
            })
        ));

        let packets = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Packets,
            preamble_bytes: 0,
            sync_word: 0xd391,
            sync_word_bytes: 2,
            ..RadioAnalyzerSettings::default()
        };
        let RadioDataDecoderConfig::Packets(config) = packets.decoder_config().unwrap() else {
            panic!("packet settings should build a packet decoder");
        };
        assert!(config.framing.preamble.is_empty());

        let raw_without_sync = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Raw,
            preamble_bytes: 0,
            sync_word_bytes: 0,
            ..RadioAnalyzerSettings::default()
        };
        assert!(matches!(
            raw_without_sync.decoder_config().unwrap(),
            RadioDataDecoderConfig::Raw(RawRadioConfig {
                preamble_bytes: 0,
                sync_word_bytes: 0,
                ..
            })
        ));
    }

    #[test]
    fn pn9_whitening_preset_uses_sx127x_compatible_parameters() {
        let settings = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Packets,
            sync_word: 0xd391,
            sync_word_bytes: 2,
            whitening_enabled: true,
            whitening_preset: RadioWhiteningPreset::Pn9X9X5,
            // Named presets must ignore retained custom values.
            whitening_width: 3,
            whitening_polynomial: 1,
            whitening_seed: 1,
            whitening_direction: WhiteningDirection::ShiftLeft,
            whitening_invert_output: true,
            whitening_start_bit: 8,
            whitening_end_bit: Some(16),
            whitening_advance_excluded_bits: true,
            ..RadioAnalyzerSettings::default()
        };

        let RadioDataDecoderConfig::Packets(config) = settings.decoder_config().unwrap() else {
            panic!("packet settings should build a packet decoder");
        };
        assert_eq!(
            config.framing.whitening,
            Some(AdditiveWhiteningConfig {
                width: 9,
                polynomial: 0x21,
                seed: 0x1ff,
                direction: WhiteningDirection::ShiftRight,
                invert_output: false,
                start_bit: 0,
                end_bit: None,
                advance_excluded_bits: false,
            })
        );
    }

    #[test]
    fn radio_analyzer_settings_round_trip_from_recording_metadata() {
        let metadata = BTreeMap::from([
            ("radio.analyzer.enabled".to_owned(), "true".to_owned()),
            (
                "radio.analyzer.whitening_preset".to_owned(),
                "pn9_x9_x5".to_owned(),
            ),
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
            ("radio.analyzer.data_mode".to_owned(), "packets".to_owned()),
            ("radio.analyzer.preamble_byte".to_owned(), "85".to_owned()),
            ("radio.analyzer.preamble_bytes".to_owned(), "12".to_owned()),
            ("radio.analyzer.sync_word".to_owned(), "11732".to_owned()),
            ("radio.analyzer.sync_word_bytes".to_owned(), "2".to_owned()),
            (
                "radio.analyzer.maximum_preamble_bit_errors".to_owned(),
                "2".to_owned(),
            ),
            (
                "radio.analyzer.minimum_bit_confidence_milli".to_owned(),
                "650".to_owned(),
            ),
            (
                "radio.analyzer.maximum_uncertain_bits".to_owned(),
                "3".to_owned(),
            ),
            (
                "radio.analyzer.whitening_enabled".to_owned(),
                "true".to_owned(),
            ),
            ("radio.analyzer.whitening_width".to_owned(), "9".to_owned()),
            (
                "radio.analyzer.whitening_polynomial".to_owned(),
                "17".to_owned(),
            ),
            ("radio.analyzer.whitening_seed".to_owned(), "511".to_owned()),
            (
                "radio.analyzer.whitening_direction".to_owned(),
                "left".to_owned(),
            ),
            (
                "radio.analyzer.whitening_invert_output".to_owned(),
                "true".to_owned(),
            ),
            (
                "radio.analyzer.whitening_start_bit".to_owned(),
                "8".to_owned(),
            ),
            (
                "radio.analyzer.whitening_end_bit".to_owned(),
                "160".to_owned(),
            ),
            (
                "radio.analyzer.whitening_advance_excluded_bits".to_owned(),
                "true".to_owned(),
            ),
        ]);

        let settings = radio_settings_from_recording_metadata(&metadata).analyzer;

        assert!(settings.is_enabled());
        assert_eq!(settings.demodulation, RadioDemodulation::BinaryGfsk);
        assert_eq!(settings.channels[0].name, "Channel 1");
        assert_eq!(settings.channels[0].frequency_offset_hz, -125_000);
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
        assert_eq!(settings.preamble_byte, 0x55);
        assert_eq!(settings.preamble_bytes, 12);
        assert_eq!(settings.sync_word, 0x2dd4);
        assert_eq!(settings.sync_word_bytes, 2);
        assert_eq!(settings.maximum_preamble_bit_errors, 2);
        assert_eq!(settings.minimum_bit_confidence_milli, 650);
        assert_eq!(settings.maximum_uncertain_bits, 3);
        assert_eq!(settings.data_mode, RadioDataMode::Packets);
        assert!(settings.whitening_enabled);
        assert_eq!(settings.whitening_preset, RadioWhiteningPreset::Pn9X9X5);
        assert_eq!(settings.whitening_width, 9);
        assert_eq!(settings.whitening_polynomial, 0x11);
        assert_eq!(settings.whitening_seed, 0x1ff);
        assert_eq!(settings.whitening_direction, WhiteningDirection::ShiftLeft);
        assert!(settings.whitening_invert_output);
        assert_eq!(settings.whitening_start_bit, 8);
        assert_eq!(settings.whitening_end_bit, Some(160));
        assert!(settings.whitening_advance_excluded_bits);
    }

    #[test]
    fn legacy_disabled_radio_analyzer_loads_as_none() {
        let metadata = BTreeMap::from([
            ("radio.analyzer.enabled".to_owned(), "false".to_owned()),
            ("radio.analyzer.demodulation".to_owned(), "gfsk2".to_owned()),
        ]);

        let settings = radio_settings_from_recording_metadata(&metadata).analyzer;

        assert_eq!(settings.demodulation, RadioDemodulation::None);
        assert!(!settings.is_enabled());
        assert!(settings.signal_profiles().unwrap().is_empty());
    }

    #[test]
    fn radio_iq_pipeline_runs_the_selected_analyzer() {
        let settings = RadioAnalyzerSettings {
            demodulation: RadioDemodulation::BinaryFsk,
            channels: vec![RadioAnalyzerChannelSettings::numbered(0, 10_000)],
            channel_bandwidth_hz: 4_000,
            output_sample_rate_hz: 8_000,
            filter_tap_count: 65,
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            adaptive_timing: false,
            ..RadioAnalyzerSettings::default()
        };
        let profile = settings.signal_profiles().unwrap().remove(0);
        let mut analyzers = RadioAnalyzerBank::new(vec![RadioAnalyzerRuntime::new(
            profile,
            settings.decoder_config().unwrap(),
        )
        .unwrap()]);
        // Keep the FFT shorter than this focused synthetic burst so the
        // analyzer's burst gate receives a channel-power observation.
        let mut spectrum = SpectrumProcessor::new(256, SpectrumWindow::Hann).unwrap();
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
            &mut analyzers,
            &IqBatch {
                sample_rate_hz: 64_000,
                center_frequency_hz: 100_000_000,
                first_sample_index: 0,
                discontinuity: false,
                samples,
            },
            -50.0,
        );

        assert_eq!(analyzers.len(), 1);
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
    fn decoded_panel_retains_older_view_when_new_messages_are_inserted_above() {
        assert_eq!(retained_decoded_scroll_offset(0.0, 80.0, 500.0), 0.0);
        assert_eq!(retained_decoded_scroll_offset(120.0, 80.0, 500.0), 200.0);
        assert_eq!(retained_decoded_scroll_offset(470.0, 80.0, 500.0), 500.0);
        assert_eq!(retained_decoded_scroll_offset(120.0, -20.0, 500.0), 120.0);
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
        metadata.insert("radio.single_duration_millis".to_owned(), "2500".to_owned());
        metadata.insert("radio.trigger_enabled".to_owned(), "true".to_owned());
        metadata.insert(
            "radio.trigger_posttrigger_micros".to_owned(),
            "25000".to_owned(),
        );
        metadata.insert(
            "radio.trigger_level_dbfs_tenths".to_owned(),
            "-425".to_owned(),
        );
        metadata.insert(
            "radio.frequency_correction_milli_ppm".to_owned(),
            "-12500".to_owned(),
        );
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
        assert_eq!(settings.single_duration_millis, 2_500);
        assert!(settings.trigger_enabled);
        assert_eq!(settings.trigger_posttrigger_micros, 25_000);
        assert_eq!(settings.trigger_level_dbfs_tenths, -425);
        assert_eq!(settings.frequency_correction_milli_ppm, -12_500);
        assert_eq!(settings.gains_db.get("lna"), Some(&24));
        assert_eq!(settings.gains_db.get("vga"), Some(&20));
        assert!(!settings.iq_correction_enabled);
        assert!(settings.amplifier_enabled);
        assert!(settings.antenna_power_enabled);
    }

    #[test]
    fn decoded_jsonl_opens_packets_in_the_rf_view() {
        let jsonl = concat!(
            "{\"type\":\"session\",\"format\":\"wavecraft-decoded-radio\",\"format_version\":1,\"center_frequency_hz\":433920000,\"sample_rate_hz\":2000000,\"bandwidth_hz\":1750000,\"data_mode\":\"packets\",\"channels\":[{\"name\":\"Weather\",\"frequency_offset_hz\":125000}]}\n",
            "{\"type\":\"packet\",\"sequence\":1,\"channel_name\":\"Weather\",\"channel_frequency_offset_hz\":125000,\"sample_start\":100,\"sample_end\":124,\"confidence\":0.9,\"uncertain_bits\":0,\"preamble\":[170],\"data\":[18,52]}\n",
            "{\"type\":\"footer\",\"status\":\"complete\",\"record_count\":1}\n",
        );

        let loaded = read_gui_decoded_data(Cursor::new(jsonl)).unwrap();

        assert!(loaded.capture.has_radio);
        assert!(!loaded.capture.has_logic);
        assert!(!loaded.capture.has_scope);
        assert_eq!(
            loaded.capture.waveforms.radio_analysis.raw_messages().len(),
            1
        );
        let message = loaded
            .capture
            .waveforms
            .radio_analysis
            .raw_messages()
            .front()
            .unwrap();
        assert_eq!(message.channel_name(), "Weather");
        assert_eq!(message.preamble_bytes(), 1);
        assert_eq!(
            message
                .data_bytes()
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0x12, 0x34]
        );
        let settings = loaded.radio_settings.unwrap();
        assert_eq!(settings.analyzer.data_mode, RadioDataMode::Packets);
        assert_eq!(settings.capture.center_frequency_hz, 433_920_000);
        assert_eq!(loaded.completion, CompletionStatus::Complete);
    }

    #[test]
    fn decoded_jsonl_open_requires_the_rf_view() {
        let path = Path::new("decoded-data.JSONL");

        assert_eq!(
            decoded_data_open_request(path, Some(WaveformTab::RadioReceiver)),
            Ok(true)
        );
        assert!(decoded_data_open_request(path, Some(WaveformTab::Oscilloscope)).is_err());
        assert_eq!(
            decoded_data_open_request(Path::new("capture.dsv"), Some(WaveformTab::Oscilloscope)),
            Ok(false)
        );
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
                center_frequency_hz: 868_300_000,
                sample_rate_hz: 8_000_000,
                bandwidth_hz: 1_750_000,
                receive_channel: 0,
                sample_limit: Some(4_096),
            },
            provider_settings: RadioProviderSettings::HackRf(HackRfSettings::default()),
            iq_correction_enabled: true,
            analyzer_profiles: Vec::new(),
            analyzer_channel_names: Vec::new(),
            decoder_config: RadioDataDecoderConfig::Raw(RawRadioConfig {
                preamble_byte: 0xaa,
                preamble_bytes: 4,
                sync_word: 0,
                sync_word_bytes: 0,
                maximum_preamble_bit_errors: 0,
                bit_order: BitOrder::MostSignificantFirst,
                minimum_bit_confidence_milli: 500,
                maximum_uncertain_bits: 0,
            }),
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
            mut radio_dsp,
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
        radio_dsp
            .push(iq.clone(), -50.0)
            .expect("GUI radio DSP worker should accept IQ");
        let spectra = radio_dsp
            .finish()
            .expect("GUI radio DSP worker should finish")
            .into_iter()
            .flat_map(|output| output.spectrum)
            .collect::<Vec<_>>();

        assert!(!iq.samples.is_empty());
        assert_eq!(iq.sample_rate_hz, 8_000_000);
        assert!(!spectra.is_empty());
        assert!(spectra
            .iter()
            .all(|spectrum| spectrum.power_dbfs.len() == RADIO_SPECTRUM_FFT_LENGTH));
        session
            .stop()
            .expect("GUI radio capture should stop cleanly");
    }

    #[test]
    #[ignore = "requires a connected HackRF One and installed libhackrf"]
    fn gui_hackrf_trigger_gate_stops_an_unbounded_single_capture() {
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
                sample_limit: None,
            },
            provider_settings: RadioProviderSettings::HackRf(HackRfSettings::default()),
            iq_correction_enabled: true,
            analyzer_profiles: Vec::new(),
            analyzer_channel_names: Vec::new(),
            decoder_config: RadioDataDecoderConfig::Raw(RawRadioConfig {
                preamble_byte: 0xaa,
                preamble_bytes: 4,
                sync_word: 0,
                sync_word_bytes: 0,
                maximum_preamble_bit_errors: 0,
                bit_order: BitOrder::MostSignificantFirst,
                minimum_bit_confidence_milli: 500,
                maximum_uncertain_bits: 0,
            }),
        };
        let started = start_capture(
            Vec::new(),
            Some(request),
            Path::new("."),
            LogicCaptureSettings::default().capture_config().unwrap(),
            ScopeCaptureSettings::default().capture_config().unwrap(),
            None,
        )
        .expect("GUI radio capture should start");
        let mut trigger = RadioSingleTrigger::new(RadioSingleTriggerConfig {
            threshold_dbfs: -120.0,
            posttrigger_samples: RADIO_SPECTRUM_FFT_LENGTH as u64,
            fft_length: RADIO_SPECTRUM_FFT_LENGTH,
            iq_correction_enabled: true,
        })
        .unwrap();
        let session = started.session;
        let mut retained_samples = 0;
        loop {
            match session
                .recv_timeout(Duration::from_secs(5))
                .expect("triggered HackRF capture should produce an event")
            {
                CaptureEvent::Frame(frame) => {
                    let output = trigger.push(frame).expect("IQ trigger gate should run");
                    retained_samples += output
                        .frames
                        .iter()
                        .map(|frame| frame.payload.sample_count())
                        .sum::<usize>();
                    if output.complete {
                        break;
                    }
                }
                CaptureEvent::SourceFailed { error, .. } => {
                    panic!("GUI HackRF capture failed: {error}")
                }
                CaptureEvent::SourceStopped { .. } => {
                    panic!("unbounded HackRF capture stopped before the RF trigger completed")
                }
            }
        }

        session
            .stop()
            .expect("triggered HackRF capture should stop");
        assert_eq!(retained_samples, RADIO_SPECTRUM_FFT_LENGTH * 2);
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
    fn capture_recording_supports_single_and_run_modes() {
        assert!(capture_recording_is_supported(AcquisitionMode::Single));
        assert!(capture_recording_is_supported(AcquisitionMode::Run));
        assert!(!capture_recording_is_supported(AcquisitionMode::Repetitive));
        assert!(!capture_recording_is_supported(AcquisitionMode::Loop));
    }

    #[test]
    fn new_single_run_and_loop_captures_release_the_previous_capture() {
        for mode in [
            AcquisitionMode::Single,
            AcquisitionMode::Run,
            AcquisitionMode::Loop,
        ] {
            assert!(clears_completed_capture_on_start(mode, false));
        }
        assert!(!clears_completed_capture_on_start(
            AcquisitionMode::Repetitive,
            false,
        ));

        assert!(clears_completed_capture_on_start(
            AcquisitionMode::Single,
            true,
        ));
        assert!(!clears_completed_capture_on_start(
            AcquisitionMode::Run,
            true,
        ));
        assert!(!clears_completed_capture_on_start(
            AcquisitionMode::Loop,
            true,
        ));
        assert!(!clears_completed_capture_on_start(
            AcquisitionMode::Repetitive,
            true,
        ));
    }

    #[test]
    fn active_continuous_capture_accepts_a_new_recording() {
        assert!(active_continuous_capture_accepts_recording(
            AcquisitionMode::Run,
            false
        ));
        assert!(active_continuous_capture_accepts_recording(
            AcquisitionMode::Loop,
            false
        ));
        assert!(!active_continuous_capture_accepts_recording(
            AcquisitionMode::Run,
            true
        ));
        assert!(!active_continuous_capture_accepts_recording(
            AcquisitionMode::Single,
            false
        ));
        assert!(!active_continuous_capture_accepts_recording(
            AcquisitionMode::Repetitive,
            false
        ));
    }

    #[test]
    fn decoded_data_recording_requires_rf_run_mode() {
        assert!(decoded_data_recording_is_supported(
            AcquisitionMode::Run,
            CaptureTarget::RadioReceiver,
        ));
        assert!(!decoded_data_recording_is_supported(
            AcquisitionMode::Single,
            CaptureTarget::RadioReceiver,
        ));
        assert!(!decoded_data_recording_is_supported(
            AcquisitionMode::Run,
            CaptureTarget::LogicAnalyzer,
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
        assert!(CaptureTarget::Combined.includes_radio_receiver());
        assert!(CaptureTarget::Combined.captures_selected_radio(true));
        assert!(!CaptureTarget::Combined.captures_selected_radio(false));
        assert!(CaptureTarget::RadioReceiver.captures_selected_radio(false));
        assert!(CaptureTarget::Combined.includes(CaptureTarget::LogicAnalyzer));
        assert!(CaptureTarget::Combined.includes(CaptureTarget::Oscilloscope));
        assert!(CaptureTarget::Combined.includes(CaptureTarget::RadioReceiver));
        assert!(!CaptureTarget::LogicAnalyzer.includes(CaptureTarget::RadioReceiver));
    }

    #[test]
    fn independent_instrument_targets_can_capture_concurrently() {
        assert!(!CaptureTarget::LogicAnalyzer.overlaps(CaptureTarget::Oscilloscope));
        assert!(!CaptureTarget::LogicAnalyzer.overlaps(CaptureTarget::RadioReceiver));
        assert!(!CaptureTarget::Oscilloscope.overlaps(CaptureTarget::RadioReceiver));
        assert!(CaptureTarget::LogicAnalyzer.overlaps(CaptureTarget::LogicAnalyzer));
        assert!(CaptureTarget::Combined.overlaps(CaptureTarget::LogicAnalyzer));
        assert!(CaptureTarget::Combined.overlaps(CaptureTarget::Oscilloscope));
        assert!(CaptureTarget::Combined.overlaps(CaptureTarget::RadioReceiver));
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
