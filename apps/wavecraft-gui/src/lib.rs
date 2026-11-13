mod acquisition;
mod app;
mod capture_settings;
mod debug_probes;
mod devices;
mod format;
#[cfg(test)]
mod frame_tests;
mod measurements;
mod process_stats;
mod protocol_analyzer;
mod protocol_decode_cache;
mod radio_analyzer;
mod radio_trigger;
mod recording;
mod recording_metadata;
mod sessions;
mod snapshot;
mod system_trace;
#[cfg(test)]
mod tests;
mod theme;
mod ui;
pub mod waveform;

use acquisition::capture::*;
use acquisition::logic::*;
use acquisition::radio::*;
use acquisition::radio_backend::*;
use acquisition::scope::*;
use capture_settings::{
    CaptureSettingsDocument, CaptureSettingsProfile, CaptureSettingsRepository,
};
use debug_probes::*;
use devices::{
    capture_devices_for_target, default_resource_directory, first_device_id_for_role,
    first_radio_id, selected_device_for_role, selected_radio, usb_speed_warning, DeviceEntry,
    DeviceRole, UsbSpeedWarning,
};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use format::*;
use measurements::{
    calculate_scope_measurements, ScopeMeasurementResults, ScopeMeasurementSettings,
    SCOPE_MEASUREMENT_KINDS,
};
use process_stats::{CpuLoadMonitor, MemoryUsageMonitor};
use protocol_analyzer::{
    analyzer_channels, analyzer_decoder_chain, analyzer_long_name, analyzer_name,
    analyzer_transport_name, decode_protocol_samples, decode_transport_samples, matching_protocols,
    protocol_stack, LogicDecodeOutput, LogicDecodeWorker, ProtocolAnalyzerInstance,
    ProtocolAnalyzerSettings, ProtocolCaptureDecoder, ProtocolDataFormat, ProtocolDecodeOutcome,
    ProtocolDecodeSnapshot, ProtocolDecodedEvent, ProtocolDecodedKind, PROTOCOLS,
};
use protocol_decode_cache::*;
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
    insert_radio_analyzer_channels_metadata, logic_settings_from_recording_metadata, parse_color,
    parse_logic_acquisition_mode, radio_settings_from_recording_metadata, recording_metadata_value,
    saved_logic_settings_metadata, saved_radio_settings_metadata, saved_scope_settings_metadata,
    scope_settings_from_recording_metadata,
};
use sessions::{
    CombinedSessionSettings, SessionNameEditor, SessionTabAction, SessionTabs, MAX_SESSION_TABS,
    SESSION_NAME_MAX_CHARS,
};
use snapshot::{crop_snapshot_image, save_snapshot_image, PendingSnapshot};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek};
use std::ops::{Range, RangeInclusive};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use theme::{
    configure_style, toolbar_action_button, ACCENT, BACKGROUND, GRID, MATH_COLOR, ORANGE, PANEL,
    SCOPE_COLORS, TEXT_MUTED,
};
use ui::capture_settings_panel::*;
use ui::decoded_panel::*;
use ui::dialogs::*;
use ui::frame_actions::FrameActions;
use ui::protocol_analyzer_dialog::*;
use ui::radio_settings::*;
use ui::scope_settings::*;
use ui::system_trace::*;
use ui::view_tabs::*;
use wavecraft_core::{
    CaptureBatch, CaptureCoordinator, CaptureEvent, CaptureFrame, CaptureSession, CaptureSource,
    DeviceDescriptor, DeviceId, InstrumentKind, ScopeBatch,
};
use wavecraft_decode::{
    AdditiveWhiteningConfig, BitOrder, BitTransformConfig, ByteStatus, RadioPacketFramerConfig,
    RadioPacketLength, RadioPacketPipelineConfig, RadioPacketPipelineOutput, RawRadioConfig,
    RawRadioMessageStart, RawRadioOutput, SampleRange, SyncWordConfig, SyncWordSuggestion,
    TimedByte, WhiteningDirection,
};
use wavecraft_dsp::{
    automatic_filter_tap_count, recommended_fsk_channel_bandwidth_hz, BinaryFskProfile,
    BinaryGfskProfile, ChannelizerConfig, DemodulationProfile, FskPolarity, FskTiming,
    SignalAnalyzerProfile, SpectrumProcessor, SpectrumWindow,
};
use wavecraft_recording::{
    read_saleae, write_saleae, CompletionStatus, RecordingReader, RecordingWriter,
    SaleaeAnalogChannel, SaleaeCapture, SaleaeChannel, SaleaeDigitalCapture,
};
use wavecraft_sdr::{
    DiscoveredRadio, RadioCapabilities, RadioCaptureSettings, RadioDiscoveryReport, RadioProvider,
    RadioProviderFailure, RadioProviderSettings, ValueDomain,
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
use waveform::combined::*;
use waveform::logic::*;
use waveform::radio::{RadioAnalysisStore, RadioDisplayStore, RadioViewOptions, RadioViewState};
use waveform::scope::*;
use waveform::scope_fft::*;
use waveform::scope_lissajous::*;
use waveform::scope_markers::*;
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
const MAX_RADIO_DSP_OUTPUTS_PER_POLL: usize = 8;
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
    active_workspace_view: Option<WorkspaceView>,
    active_waveform_tab: Option<WaveformTab>,
    system_trace_settings: SystemTraceSettings,
    system_trace_setup_panel_open: bool,
    system_trace: system_trace::SystemTraceDisplayState,
    system_trace_events: Option<Receiver<wavecraft_trace::TraceEvent>>,
    system_trace_terminal_lines: Option<Receiver<wavecraft_debug_probe::RttTerminalLine>>,
    system_trace_connection: system_trace::SystemTraceConnectionState,
    system_trace_worker_commands: Option<Sender<system_trace::SystemTraceWorkerCommand>>,
    system_trace_worker_events: Option<Receiver<system_trace::SystemTraceWorkerEvent>>,
    debug_probes: Vec<wavecraft_debug_probe::ProbeDescriptor>,
    debug_probe_target_groups: Vec<wavecraft_debug_probe::ProbeTargetGroup>,
    debug_probe_discovery_error: Option<String>,
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
    scope_tools_panel_open: bool,
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
        let mut app = Self::with_settings(capture_settings_repository, saved_capture_settings);
        app.begin_refresh();
        if let Some(error) = settings_load_error {
            app.last_error = Some(format!(
                "Saved capture settings could not be loaded; using defaults: {error}"
            ));
        }
        app
    }

    /// Builds the application state from saved settings. It neither discovers
    /// devices nor reads or writes the settings file, so tests can use it.
    fn with_settings(
        capture_settings_repository: Option<CaptureSettingsRepository>,
        saved_capture_settings: CaptureSettingsDocument,
    ) -> Self {
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
            active_workspace_view: None,
            active_waveform_tab: None,
            system_trace_settings: SystemTraceSettings::default(),
            system_trace_setup_panel_open: true,
            system_trace: system_trace::SystemTraceDisplayState::default(),
            system_trace_events: None,
            system_trace_terminal_lines: None,
            system_trace_connection: system_trace::SystemTraceConnectionState::default(),
            system_trace_worker_commands: None,
            system_trace_worker_events: None,
            debug_probes: Vec::new(),
            debug_probe_target_groups: Vec::new(),
            debug_probe_discovery_error: None,
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
            scope_tools_panel_open: true,
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
        app
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

    fn is_busy(&self) -> bool {
        self.pending.is_some()
            || !self.captures.is_empty()
            || self.scheduled_repetitive_capture.is_some()
    }
}

impl eframe::App for WaveCraftApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.run_frame(context);
    }
}

impl WaveCraftApp {
    fn poll_system_trace_events(&mut self) {
        let mut disconnected = false;
        if let Some(events) = &self.system_trace_events {
            loop {
                match events.try_recv() {
                    Ok(event) => self.system_trace.ingest(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.system_trace_events = None;
        }

        let mut terminal_disconnected = false;
        if let Some(lines) = &self.system_trace_terminal_lines {
            loop {
                match lines.try_recv() {
                    Ok(line) => self.system_trace.ingest_terminal_line(line),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        terminal_disconnected = true;
                        break;
                    }
                }
            }
        }
        if terminal_disconnected {
            self.system_trace_terminal_lines = None;
        }
    }

    /// Runs one frame: polls workers, paints every panel, and applies what the
    /// user asked for. It does not use the `eframe::Frame`, so tests can call it
    /// with a bare `egui::Context`.
    fn run_frame(&mut self, context: &egui::Context) {
        self.cpu_load.update();
        self.memory_usage.update();
        context.request_repaint_after(CPU_LOAD_SAMPLE_INTERVAL);
        self.poll_calibration_progress();
        self.poll_worker();
        self.poll_system_trace_events();
        self.poll_system_trace_connection();
        self.reconcile_capture_settings_tab();
        self.reconcile_waveform_tab();
        self.reconcile_device_selections();
        self.reconcile_radio_settings();
        self.poll_capture();
        self.poll_repetitive_capture();
        self.poll_queued_capture_rearms();
        self.finish_pending_snapshot(context);

        let mut actions = FrameActions::default();
        let frame = self.frame_state();
        let radio_settings_before_ui = self.radio_settings.clone();

        self.paint_toolbar(context, &frame, &mut actions);

        self.paint_waveform_tabs(context);

        if self.system_trace_view_is_active() {
            self.paint_system_trace_setup_panel(context, &mut actions);
        } else {
            self.paint_capture_setup_panel(context, &frame, &mut actions);
        }

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

        self.paint_status_bar(context);

        self.paint_session_tabs(context);

        self.paint_central_panel(context, &frame, actions.snapshot_clicked);

        self.paint_settings_window(
            context,
            &frame,
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

        self.apply_toolbar_actions(&mut actions, &frame);
        self.apply_system_trace_action(actions.system_trace_action);
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
                match runtime_control.update(&self.radio_settings) {
                    Ok(()) => {
                        self.status = runtime_control.update_status().to_owned();
                        self.last_error = None;
                    }
                    Err(error) => self.set_error(error),
                }
            }
        }
        self.apply_scope_frontend_changes(&actions, &frame);
        if self.pending.is_some()
            || !self.captures.is_empty()
            || self.system_trace_connection.needs_fast_poll()
        {
            context.request_repaint_after(Duration::from_millis(16));
        } else if let Some(scheduled) = self.scheduled_repetitive_capture {
            context.request_repaint_after(scheduled.due.saturating_duration_since(Instant::now()));
        } else if let Some(due) = self.scope_configuration_due {
            context.request_repaint_after(due.saturating_duration_since(Instant::now()));
        }
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

const COMBINED_DIVIDER_HEIGHT: f32 = 10.0;
const MIN_LOGIC_PANE_HEIGHT: f32 = 260.0;
const MIN_SCOPE_PANE_HEIGHT: f32 = 165.0;
