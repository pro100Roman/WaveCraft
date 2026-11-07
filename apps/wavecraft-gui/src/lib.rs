mod acquisition;
mod app;
mod capture_settings;
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
mod theme;
mod ui;
pub mod waveform;

use acquisition::capture::*;
use acquisition::logic::*;
use acquisition::radio::*;
use acquisition::scope::*;
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
    logic_settings_from_recording_metadata, parse_color, parse_logic_acquisition_mode,
    radio_settings_from_recording_metadata, recording_metadata_value,
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
use std::sync::mpsc::{self, Receiver, TryRecvError};
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
use ui::view_tabs::*;
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
        app
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
        self.run_frame(context);
    }
}

impl WaveCraftApp {
    /// Runs one frame: polls workers, paints every panel, and applies what the
    /// user asked for. It does not use the `eframe::Frame`, so tests can call it
    /// with a bare `egui::Context`.
    fn run_frame(&mut self, context: &egui::Context) {
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
        let frame = self.frame_state();
        let radio_settings_before_ui = self.radio_settings.clone();

        self.paint_toolbar(context, &frame, &mut actions);

        self.paint_waveform_tabs(context);

        self.paint_capture_setup_panel(context, &frame, &mut actions);

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

        self.paint_scope_tools_panel(context, frame.trigger_controls_enabled);

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
        self.apply_scope_frontend_changes(&actions, &frame);
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
    use wavecraft_core::{LogicBatch, ScopeBatch};
    use wavecraft_sdr::RadioProviderAdapter;

    #[test]
    fn embedded_wavecraft_app_icon_is_valid() {
        let icon = wavecraft_app_icon().expect("embedded icon decodes");

        assert_eq!((icon.width, icon.height), (512, 512));
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
        assert_eq!(icon.rgba[3], 0);
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
    fn default_resource_directory_contains_complete_upstream_resource_set() {
        let directory = PathBuf::from(default_resource_directory());
        assert!(has_required_fpga_resources(&directory));
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
}
