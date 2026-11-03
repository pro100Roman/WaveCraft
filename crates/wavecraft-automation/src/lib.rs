//! Shared device discovery, selection, and ownership for WaveCraft clients.
//!
//! This crate deliberately contains no device transport implementation. USB
//! behavior remains in `wavecraft-usb`, while deterministic simulated devices
//! remain in `wavecraft-sim`.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use wavecraft_core::{
    CaptureCoordinator, CaptureEvent as CoreCaptureEvent, CaptureSource, DeviceDescriptor,
    DeviceId, InstrumentKind as CoreInstrumentKind,
};
use wavecraft_recording::{CompletionStatus, RecordingWriter};
use wavecraft_sim::{
    SimulatedDeviceProfile, SimulatedLogicAnalyzer, SimulatedOscilloscope,
    SIMULATED_DEVICE_PROFILES,
};
use wavecraft_usb::{
    discover_supported_devices, prepare_device, probe_device, DeviceModel, DiscoveredDevice,
    DsCopeBandwidthLimit as UsbScopeBandwidthLimit, DsCopeCaptureConfig, DsCopeCaptureSource,
    DsCopeChannelConfig, DsCopeCoupling as UsbScopeCoupling, DsCopeOperationMode,
    DsCopeTriggerConfig, DsCopeTriggerSlope as UsbScopeTriggerSlope,
    DsCopeTriggerSource as UsbScopeTriggerSource, DsLogicCaptureConfig, DsLogicCaptureSource,
    DsLogicOperationMode, LinkStatus, UsbSpeedClass, DSCOPE_SAMPLE_RATES_HZ,
    DSCOPE_TRIGGER_LEVEL_CENTER, DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX,
    DSCOPE_TRIGGER_POSITION_CENTER_PERCENT, DSCOPE_VERTICAL_OFFSET_CENTER,
    DSLOGIC_SAMPLE_ALIGNMENT, DSLOGIC_SAMPLE_RATES_HZ,
};

pub const DEVICE_LIST_SCHEMA: &str = "wavecraft.devices/v1";
pub const DEVICE_PROBE_SCHEMA: &str = "wavecraft.device-probe/v1";
pub const CAPTURE_CONFIGURATION_SCHEMA: &str = "wavecraft.capture-configuration/v1";
pub const CAPTURE_EVENT_SCHEMA: &str = "wavecraft.capture-event/v1";
pub const CAPTURE_RESULT_SCHEMA: &str = "wavecraft.capture-result/v1";
pub const ERROR_SCHEMA: &str = "wavecraft.error/v1";
const MAX_ERROR_CANDIDATES: usize = 16;
const CAPTURE_EVENT_CAPACITY: usize = 64;
const SIMULATOR_BATCH_SIZE: usize = 4_096;
const MAX_CAPTURE_SAMPLE_COUNT: u64 = 1_000_000_000;
const MAX_CAPTURE_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const USB_DISCOVERY_ATTEMPTS: usize = 5;
const USB_DISCOVERY_RETRY_DELAY: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentKind {
    LogicAnalyzer,
    Oscilloscope,
    RadioReceiver,
}

impl Display for InstrumentKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LogicAnalyzer => formatter.write_str("logic_analyzer"),
            Self::Oscilloscope => formatter.write_str("oscilloscope"),
            Self::RadioReceiver => formatter.write_str("radio_receiver"),
        }
    }
}

impl From<CoreInstrumentKind> for InstrumentKind {
    fn from(value: CoreInstrumentKind) -> Self {
        match value {
            CoreInstrumentKind::LogicAnalyzer => Self::LogicAnalyzer,
            CoreInstrumentKind::Oscilloscope => Self::Oscilloscope,
            CoreInstrumentKind::RadioReceiver => Self::RadioReceiver,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceBackend {
    Simulator,
    Usb,
}

impl Display for DeviceBackend {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Simulator => formatter.write_str("simulator"),
            Self::Usb => formatter.write_str("usb"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceLinkSpeed {
    Unknown,
    LowSpeed,
    FullSpeed,
    HighSpeed,
    SuperSpeed,
}

impl From<UsbSpeedClass> for DeviceLinkSpeed {
    fn from(value: UsbSpeedClass) -> Self {
        match value {
            UsbSpeedClass::Unknown => Self::Unknown,
            UsbSpeedClass::LowSpeed => Self::LowSpeed,
            UsbSpeedClass::FullSpeed => Self::FullSpeed,
            UsbSpeedClass::HighSpeed => Self::HighSpeed,
            UsbSpeedClass::SuperSpeed => Self::SuperSpeed,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceLinkStatus {
    Optimal,
    ReducedPerformance,
    Unsupported,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceOwnershipState {
    Available,
    InUse,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DevicePreparationState {
    Unknown,
    Ready,
    RequiresPreparation,
}

impl From<LinkStatus> for DeviceLinkStatus {
    fn from(value: LinkStatus) -> Self {
        match value {
            LinkStatus::Optimal => Self::Optimal,
            LinkStatus::ReducedPerformance => Self::ReducedPerformance,
            LinkStatus::Unsupported => Self::Unsupported,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceLink {
    pub speed: Option<DeviceLinkSpeed>,
    pub status: DeviceLinkStatus,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceCapabilities {
    pub channel_count: u8,
    pub supports_capture: bool,
    pub sample_rates_hz: Vec<u64>,
    pub operation_modes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutomationDevice {
    /// Identifies this connection for the current USB topology or simulator.
    pub connection_id: String,
    /// Identifies physical hardware across ports when the device exposes one.
    pub persistent_id: Option<String>,
    pub display_name: String,
    pub model: String,
    pub kind: InstrumentKind,
    pub backend: DeviceBackend,
    pub physical_location: Option<String>,
    pub ownership: DeviceOwnershipState,
    pub preparation: DevicePreparationState,
    pub link: DeviceLink,
    pub capabilities: DeviceCapabilities,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceList {
    pub schema: String,
    pub devices: Vec<AutomationDevice>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<InstrumentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<DeviceBackend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_channel_count: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_capture: Option<bool>,
}

impl DeviceSelector {
    pub fn connection_id(connection_id: impl Into<String>) -> Self {
        Self {
            connection_id: Some(connection_id.into()),
            ..Self::default()
        }
    }

    pub fn model(model: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            ..Self::default()
        }
    }

    pub fn persistent_id(persistent_id: impl Into<String>) -> Self {
        Self {
            persistent_id: Some(persistent_id.into()),
            ..Self::default()
        }
    }

    pub fn with_kind(mut self, kind: InstrumentKind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn with_backend(mut self, backend: DeviceBackend) -> Self {
        self.backend = Some(backend);
        self
    }

    pub fn with_minimum_channel_count(mut self, minimum_channel_count: u8) -> Self {
        self.minimum_channel_count = Some(minimum_channel_count);
        self
    }

    pub fn requiring_capture(mut self) -> Self {
        self.requires_capture = Some(true);
        self
    }

    fn matches(&self, device: &AutomationDevice) -> bool {
        self.connection_id
            .as_ref()
            .map_or(true, |value| value == &device.connection_id)
            && self
                .persistent_id
                .as_ref()
                .map_or(true, |value| device.persistent_id.as_ref() == Some(value))
            && self
                .model
                .as_ref()
                .map_or(true, |value| value == &device.model)
            && self.kind.map_or(true, |value| value == device.kind)
            && self.backend.map_or(true, |value| value == device.backend)
            && self
                .minimum_channel_count
                .map_or(true, |value| device.capabilities.channel_count >= value)
            && self
                .requires_capture
                .map_or(true, |value| device.capabilities.supports_capture == value)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CaptureId(String);

impl CaptureId {
    fn next() -> Self {
        static NEXT_CAPTURE_ID: AtomicU64 = AtomicU64::new(1);
        Self(format!(
            "capture-{}-{}",
            std::process::id(),
            NEXT_CAPTURE_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for CaptureId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    Finite,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeCoupling {
    Dc,
    Ac,
}

impl Display for ScopeCoupling {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dc => formatter.write_str("dc"),
            Self::Ac => formatter.write_str("ac"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeBandwidthLimit {
    Full,
    Mhz20,
}

impl Display for ScopeBandwidthLimit {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => formatter.write_str("full"),
            Self::Mhz20 => formatter.write_str("20_mhz"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeTriggerSource {
    Auto,
    ChannelZero,
    ChannelOne,
    ChannelZeroAndOne,
    ChannelZeroOrOne,
}

impl Display for ScopeTriggerSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => formatter.write_str("auto"),
            Self::ChannelZero => formatter.write_str("channel_zero"),
            Self::ChannelOne => formatter.write_str("channel_one"),
            Self::ChannelZeroAndOne => formatter.write_str("channel_zero_and_one"),
            Self::ChannelZeroOrOne => formatter.write_str("channel_zero_or_one"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeTriggerSlope {
    Rising,
    Falling,
}

impl Display for ScopeTriggerSlope {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rising => formatter.write_str("rising"),
            Self::Falling => formatter.write_str("falling"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopeChannelConfiguration {
    pub volts_per_div_mv: u16,
    pub coupling: ScopeCoupling,
    pub probe_attenuation: u8,
    pub vertical_offset: u16,
}

impl Default for ScopeChannelConfiguration {
    fn default() -> Self {
        Self {
            volts_per_div_mv: 1_000,
            coupling: ScopeCoupling::Dc,
            probe_attenuation: 1,
            vertical_offset: DSCOPE_VERTICAL_OFFSET_CENTER,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopeCaptureConfiguration {
    pub channel_settings: [ScopeChannelConfiguration; 2],
    pub bandwidth_limit: ScopeBandwidthLimit,
}

impl Default for ScopeCaptureConfiguration {
    fn default() -> Self {
        Self {
            channel_settings: [ScopeChannelConfiguration::default(); 2],
            bandwidth_limit: ScopeBandwidthLimit::Full,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopeTriggerConfiguration {
    pub position_percent: u8,
    pub source: ScopeTriggerSource,
    pub auto_channel: u8,
    pub slope: ScopeTriggerSlope,
    pub holdoff_ns: u64,
    pub noise_sensitivity: u8,
    pub levels: [u8; 2],
}

impl Default for ScopeTriggerConfiguration {
    fn default() -> Self {
        Self {
            position_percent: DSCOPE_TRIGGER_POSITION_CENTER_PERCENT,
            source: ScopeTriggerSource::Auto,
            auto_channel: 0,
            slope: ScopeTriggerSlope::Rising,
            holdoff_ns: 0,
            noise_sensitivity: DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX / 2 + 1,
            levels: [DSCOPE_TRIGGER_LEVEL_CENTER; 2],
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureTrigger {
    None,
    Scope(ScopeTriggerConfiguration),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureConfiguration {
    pub schema: String,
    pub mode: CaptureMode,
    pub sample_rate_hz: u64,
    pub sample_count: u64,
    pub channels: Vec<u8>,
    pub trigger: CaptureTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeCaptureConfiguration>,
    pub timeout_ms: u64,
    pub output_path: PathBuf,
}

impl CaptureConfiguration {
    pub fn finite_logic(
        sample_rate_hz: u64,
        sample_count: u64,
        channels: Vec<u8>,
        output_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            schema: CAPTURE_CONFIGURATION_SCHEMA.to_owned(),
            mode: CaptureMode::Finite,
            sample_rate_hz,
            sample_count,
            channels,
            trigger: CaptureTrigger::None,
            scope: None,
            timeout_ms: 30_000,
            output_path: output_path.into(),
        }
    }

    pub fn finite_scope(
        sample_rate_hz: u64,
        sample_count: u64,
        channels: Vec<u8>,
        output_path: impl Into<PathBuf>,
    ) -> Self {
        let auto_channel = channels.first().copied().unwrap_or(0);
        Self {
            schema: CAPTURE_CONFIGURATION_SCHEMA.to_owned(),
            mode: CaptureMode::Finite,
            sample_rate_hz,
            sample_count,
            channels,
            trigger: CaptureTrigger::Scope(ScopeTriggerConfiguration {
                auto_channel,
                ..ScopeTriggerConfiguration::default()
            }),
            scope: Some(ScopeCaptureConfiguration::default()),
            timeout_ms: 30_000,
            output_path: output_path.into(),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        self
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    Preparing,
    Armed,
    Capturing,
    Finalizing,
    Completed,
    Cancelled,
    TimedOut,
    Failed,
}

impl CaptureStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::TimedOut | Self::Failed
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationEventKind {
    Status {
        status: CaptureStatus,
    },
    Progress {
        captured_samples: u64,
        requested_samples: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutomationEvent {
    pub schema: String,
    pub capture_id: CaptureId,
    pub sequence: u64,
    pub elapsed_ms: u64,
    #[serde(flatten)]
    pub event: AutomationEventKind,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureSummary {
    pub capture_id: CaptureId,
    pub status: CaptureStatus,
    pub device: AutomationDevice,
    pub requested_configuration: CaptureConfiguration,
    pub effective_configuration: CaptureConfiguration,
    pub frame_count: u64,
    pub captured_samples: u64,
    pub elapsed_ms: u64,
    pub recording_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompletedCapture {
    pub schema: String,
    pub summary: CaptureSummary,
}

impl CompletedCapture {
    pub fn summary(&self) -> &CaptureSummary {
        &self.summary
    }

    pub fn recording_path(&self) -> &Path {
        &self.summary.recording_path
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationErrorCode {
    DeviceNotFound,
    AmbiguousDeviceMatch,
    DeviceBusy,
    DeviceDisconnected,
    DiscoveryFailed,
    DuplicateDeviceId,
    ProbeFailed,
    PreparationFailed,
    UnsupportedConfiguration,
    OutputExists,
    RecordingFailed,
    CaptureFailed,
    CaptureCancelled,
    CaptureTimedOut,
    ServiceUnavailable,
}

impl Display for AutomationErrorCode {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::DeviceNotFound => "device_not_found",
            Self::AmbiguousDeviceMatch => "ambiguous_device_match",
            Self::DeviceBusy => "device_busy",
            Self::DeviceDisconnected => "device_disconnected",
            Self::DiscoveryFailed => "discovery_failed",
            Self::DuplicateDeviceId => "duplicate_device_id",
            Self::ProbeFailed => "probe_failed",
            Self::PreparationFailed => "preparation_failed",
            Self::UnsupportedConfiguration => "unsupported_configuration",
            Self::OutputExists => "output_exists",
            Self::RecordingFailed => "recording_failed",
            Self::CaptureFailed => "capture_failed",
            Self::CaptureCancelled => "capture_cancelled",
            Self::CaptureTimedOut => "capture_timed_out",
            Self::ServiceUnavailable => "service_unavailable",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AutomationError {
    pub schema: &'static str,
    pub code: AutomationErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub operation: &'static str,
    pub retryable: bool,
    pub candidates: Box<[AutomationDevice]>,
    pub context: Box<BTreeMap<String, String>>,
}

impl AutomationError {
    fn new(
        code: AutomationErrorCode,
        message: impl Into<String>,
        operation: &'static str,
        retryable: bool,
    ) -> Self {
        Self {
            schema: ERROR_SCHEMA,
            code,
            message: message.into(),
            device_id: None,
            operation,
            retryable,
            candidates: Box::default(),
            context: Box::default(),
        }
    }

    fn for_device(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    fn with_candidates(mut self, candidates: Vec<AutomationDevice>) -> Self {
        self.context
            .insert("candidate_count".to_owned(), candidates.len().to_string());
        self.candidates = candidates
            .into_iter()
            .take(MAX_ERROR_CANDIDATES)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self
    }
}

impl Display for AutomationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for AutomationError {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UsbEndpointProbe {
    pub address: u8,
    pub max_packet_size: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceProbeResult {
    pub schema: String,
    pub device: AutomationDevice,
    pub preparation: DevicePreparationState,
    pub firmware_version: Option<String>,
    pub hardware_status_raw: Option<u8>,
    pub configuration: Option<u8>,
    pub interface: Option<u8>,
    pub alternate_setting: Option<u8>,
    pub bulk_out: Option<UsbEndpointProbe>,
    pub bulk_in: Option<UsbEndpointProbe>,
}

#[derive(Clone)]
enum DeviceRecord {
    Simulator(SimulatedDeviceProfile),
    Usb(DiscoveredDevice),
}

impl DeviceRecord {
    fn describe(&self) -> AutomationDevice {
        match self {
            Self::Simulator(profile) => describe_simulator(profile),
            Self::Usb(device) => describe_usb(device),
        }
    }
}

type UsbDiscovery = dyn Fn() -> Result<Vec<DiscoveredDevice>, String> + Send + Sync + 'static;

struct DeviceCatalog {
    usb_discovery: Arc<UsbDiscovery>,
    simulators: Vec<SimulatedDeviceProfile>,
}

impl DeviceCatalog {
    fn discover(&self, operation: &'static str) -> Result<Vec<DeviceRecord>, AutomationError> {
        let mut records = self
            .simulators
            .iter()
            .copied()
            .map(DeviceRecord::Simulator)
            .collect::<Vec<_>>();
        let usb_devices = (self.usb_discovery)().map_err(|message| {
            AutomationError::new(
                AutomationErrorCode::DiscoveryFailed,
                message,
                operation,
                true,
            )
        })?;
        records.extend(usb_devices.into_iter().map(DeviceRecord::Usb));
        records.sort_by_key(|record| record.describe().connection_id);

        let mut ids = HashSet::new();
        for record in &records {
            let descriptor = record.describe();
            if !ids.insert(descriptor.connection_id.clone()) {
                return Err(AutomationError::new(
                    AutomationErrorCode::DuplicateDeviceId,
                    format!(
                        "device discovery returned duplicate connection ID {}",
                        descriptor.connection_id
                    ),
                    operation,
                    false,
                )
                .for_device(descriptor.connection_id));
            }
        }
        Ok(records)
    }

    fn discover_for_selector(
        &self,
        selector: &DeviceSelector,
        operation: &'static str,
    ) -> Result<Vec<DeviceRecord>, AutomationError> {
        let exact_usb_id = selector
            .connection_id
            .as_deref()
            .filter(|connection_id| connection_id.starts_with("usb-"));
        let attempts = exact_usb_id.map_or(1, |_| USB_DISCOVERY_ATTEMPTS);
        for attempt in 0..attempts {
            let records = self.discover(operation)?;
            if exact_usb_id.map_or(true, |connection_id| {
                records
                    .iter()
                    .any(|record| record.describe().connection_id == connection_id)
            }) || attempt + 1 == attempts
            {
                return Ok(records);
            }
            thread::sleep(USB_DISCOVERY_RETRY_DELAY);
        }
        unreachable!("USB discovery attempt count is always positive")
    }
}

#[derive(Clone)]
pub struct AutomationManager {
    catalog: Arc<DeviceCatalog>,
    leases: Arc<Mutex<HashSet<String>>>,
    resource_directory: Option<Arc<PathBuf>>,
}

impl Default for AutomationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AutomationManager {
    /// Creates a manager that discovers supported USB devices and built-in
    /// deterministic simulated devices.
    pub fn new() -> Self {
        Self::from_parts(
            || {
                discover_supported_devices()
                    .map(|report| report.devices)
                    .map_err(|error| format!("failed to discover USB devices: {error}"))
            },
            SIMULATED_DEVICE_PROFILES.to_vec(),
            process_lease_registry(),
        )
    }

    /// Creates a manager containing only deterministic simulated devices.
    /// This path never initializes libusb and is suitable for CI and tests.
    pub fn simulated() -> Self {
        Self::from_parts(
            || Ok(Vec::new()),
            SIMULATED_DEVICE_PROFILES.to_vec(),
            process_lease_registry(),
        )
    }

    /// Configure the directory containing the model-specific FPGA resources
    /// required to prepare physical instruments.
    pub fn with_resource_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.resource_directory = Some(Arc::new(path.into()));
        self
    }

    #[cfg(test)]
    fn from_usb_discovery(
        usb_discovery: impl Fn() -> Result<Vec<DiscoveredDevice>, String> + Send + Sync + 'static,
        simulators: Vec<SimulatedDeviceProfile>,
    ) -> Self {
        Self::from_parts(
            usb_discovery,
            simulators,
            Arc::new(Mutex::new(HashSet::new())),
        )
    }

    fn from_parts(
        usb_discovery: impl Fn() -> Result<Vec<DiscoveredDevice>, String> + Send + Sync + 'static,
        simulators: Vec<SimulatedDeviceProfile>,
        leases: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        Self {
            catalog: Arc::new(DeviceCatalog {
                usb_discovery: Arc::new(usb_discovery),
                simulators,
            }),
            leases,
            resource_directory: None,
        }
    }

    pub fn list_devices(&self) -> Result<Vec<AutomationDevice>, AutomationError> {
        let mut devices = self
            .catalog
            .discover("device.list")?
            .iter()
            .map(DeviceRecord::describe)
            .collect::<Vec<_>>();
        let leases = self.leases.lock().map_err(|_| {
            AutomationError::new(
                AutomationErrorCode::ServiceUnavailable,
                "device lease registry is unavailable",
                "device.list",
                true,
            )
        })?;
        for device in &mut devices {
            if leases.contains(&device.connection_id) {
                device.ownership = DeviceOwnershipState::InUse;
            }
        }
        Ok(devices)
    }

    pub fn device_list(&self) -> Result<DeviceList, AutomationError> {
        Ok(DeviceList {
            schema: DEVICE_LIST_SCHEMA.to_owned(),
            devices: self.list_devices()?,
        })
    }

    pub fn open_device(&self, selector: DeviceSelector) -> Result<DeviceHandle, AutomationError> {
        let records = self
            .catalog
            .discover_for_selector(&selector, "device.open")?;
        let mut matches = records
            .into_iter()
            .filter(|record| selector.matches(&record.describe()))
            .collect::<Vec<_>>();

        if matches.is_empty() {
            let mut error = AutomationError::new(
                AutomationErrorCode::DeviceNotFound,
                "device selector matched no connected instruments",
                "device.open",
                true,
            );
            if let Some(connection_id) = selector.connection_id {
                error = error.for_device(connection_id);
            }
            return Err(error);
        }
        if matches.len() > 1 {
            let candidates = matches.iter().map(DeviceRecord::describe).collect();
            return Err(AutomationError::new(
                AutomationErrorCode::AmbiguousDeviceMatch,
                "device selector matched multiple instruments; use an exact connection ID",
                "device.open",
                false,
            )
            .with_candidates(candidates));
        }

        let Some(record) = matches.pop() else {
            return Err(AutomationError::new(
                AutomationErrorCode::ServiceUnavailable,
                "device selection result was lost before acquiring its lease",
                "device.open",
                true,
            ));
        };
        let mut descriptor = record.describe();
        let mut leases = self.leases.lock().map_err(|_| {
            AutomationError::new(
                AutomationErrorCode::ServiceUnavailable,
                "device lease registry is unavailable",
                "device.open",
                true,
            )
        })?;
        if !leases.insert(descriptor.connection_id.clone()) {
            return Err(AutomationError::new(
                AutomationErrorCode::DeviceBusy,
                format!("{} is already in use", descriptor.display_name),
                "device.open",
                true,
            )
            .for_device(descriptor.connection_id));
        }
        descriptor.ownership = DeviceOwnershipState::InUse;
        drop(leases);

        Ok(DeviceHandle {
            descriptor,
            catalog: Arc::clone(&self.catalog),
            resource_directory: self.resource_directory.clone(),
            lease: Arc::new(DeviceLease {
                connection_id: record.describe().connection_id,
                registry: Arc::clone(&self.leases),
                released: AtomicBool::new(false),
            }),
        })
    }
}

struct DeviceLease {
    connection_id: String,
    registry: Arc<Mutex<HashSet<String>>>,
    released: AtomicBool,
}

impl DeviceLease {
    fn release(&self) -> Result<(), AutomationError> {
        if self.released.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut leases = self.registry.lock().map_err(|_| {
            AutomationError::new(
                AutomationErrorCode::ServiceUnavailable,
                "device lease registry is unavailable",
                "device.close",
                true,
            )
            .for_device(&self.connection_id)
        })?;
        if self.released.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        leases.remove(&self.connection_id);
        Ok(())
    }
}

impl Drop for DeviceLease {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

pub struct DeviceHandle {
    descriptor: AutomationDevice,
    catalog: Arc<DeviceCatalog>,
    resource_directory: Option<Arc<PathBuf>>,
    lease: Arc<DeviceLease>,
}

impl Debug for DeviceHandle {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceHandle")
            .field("descriptor", &self.descriptor)
            .field("released", &self.lease.released.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl DeviceHandle {
    pub fn descriptor(&self) -> &AutomationDevice {
        &self.descriptor
    }

    pub fn capabilities(&self) -> &DeviceCapabilities {
        &self.descriptor.capabilities
    }

    pub fn probe(&self) -> Result<DeviceProbeResult, AutomationError> {
        let connection_id = &self.descriptor.connection_id;
        let record = self
            .catalog
            .discover("device.probe")?
            .into_iter()
            .find(|record| record.describe().connection_id == *connection_id)
            .ok_or_else(|| {
                AutomationError::new(
                    AutomationErrorCode::DeviceDisconnected,
                    format!("device {connection_id} disconnected after selection"),
                    "device.probe",
                    true,
                )
                .for_device(connection_id)
            })?;

        match record {
            DeviceRecord::Simulator(_) => Ok(DeviceProbeResult {
                schema: DEVICE_PROBE_SCHEMA.to_owned(),
                device: self.descriptor.clone(),
                preparation: DevicePreparationState::Ready,
                firmware_version: None,
                hardware_status_raw: None,
                configuration: None,
                interface: None,
                alternate_setting: None,
                bulk_out: None,
                bulk_in: None,
            }),
            DeviceRecord::Usb(device) => {
                let probe = probe_device(&device).map_err(|error| {
                    AutomationError::new(
                        AutomationErrorCode::ProbeFailed,
                        format!("failed to probe device {connection_id}: {error}"),
                        "device.probe",
                        true,
                    )
                    .for_device(connection_id)
                })?;
                let preparation = if probe.hardware_status.fpga_configured {
                    DevicePreparationState::Ready
                } else {
                    DevicePreparationState::RequiresPreparation
                };
                let mut descriptor = self.descriptor.clone();
                descriptor.preparation = preparation;
                Ok(DeviceProbeResult {
                    schema: DEVICE_PROBE_SCHEMA.to_owned(),
                    device: descriptor,
                    preparation,
                    firmware_version: Some(probe.firmware_version.to_string()),
                    hardware_status_raw: Some(probe.hardware_status.raw),
                    configuration: Some(probe.configuration),
                    interface: Some(probe.interface),
                    alternate_setting: Some(probe.alternate_setting),
                    bulk_out: Some(UsbEndpointProbe {
                        address: probe.bulk_out.address,
                        max_packet_size: probe.bulk_out.max_packet_size,
                    }),
                    bulk_in: Some(UsbEndpointProbe {
                        address: probe.bulk_in.address,
                        max_packet_size: probe.bulk_in.max_packet_size,
                    }),
                })
            }
        }
    }

    pub fn start_capture(
        &self,
        requested_configuration: CaptureConfiguration,
    ) -> Result<CaptureHandle, AutomationError> {
        validate_capture_configuration(&self.descriptor, &requested_configuration)?;

        let mut effective_configuration = requested_configuration.clone();
        effective_configuration.channels.sort_unstable();
        match (self.descriptor.kind, self.descriptor.backend) {
            (InstrumentKind::LogicAnalyzer, DeviceBackend::Simulator) => {
                self.start_simulated_logic_capture(requested_configuration, effective_configuration)
            }
            (InstrumentKind::LogicAnalyzer, DeviceBackend::Usb) => {
                self.start_usb_logic_capture(requested_configuration, effective_configuration)
            }
            (InstrumentKind::Oscilloscope, DeviceBackend::Simulator) => {
                self.start_simulated_scope_capture(requested_configuration, effective_configuration)
            }
            (InstrumentKind::Oscilloscope, DeviceBackend::Usb) => {
                self.start_usb_scope_capture(requested_configuration, effective_configuration)
            }
            (InstrumentKind::RadioReceiver, _) => Err(AutomationError::new(
                AutomationErrorCode::UnsupportedConfiguration,
                "radio receiver automation capture is not implemented",
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id)),
        }
    }

    fn start_simulated_logic_capture(
        &self,
        requested_configuration: CaptureConfiguration,
        effective_configuration: CaptureConfiguration,
    ) -> Result<CaptureHandle, AutomationError> {
        let enabled_channel_mask = effective_configuration
            .channels
            .iter()
            .fold(0_u32, |mask, channel| mask | (1_u32 << channel));
        let source = SimulatedLogicAnalyzer::new(
            &self.descriptor.connection_id,
            effective_configuration.sample_rate_hz,
            self.descriptor.capabilities.channel_count,
            SIMULATOR_BATCH_SIZE,
        )
        .and_then(|source| source.with_sample_limit(effective_configuration.sample_count))
        .and_then(|source| source.with_enabled_channel_mask(enabled_channel_mask))
        .map_err(|error| {
            AutomationError::new(
                AutomationErrorCode::UnsupportedConfiguration,
                error.to_string(),
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id)
        })?;

        self.start_capture_with_factory(
            requested_configuration,
            effective_configuration,
            Box::new(move || Ok(Box::new(source))),
        )
    }

    fn start_simulated_scope_capture(
        &self,
        requested_configuration: CaptureConfiguration,
        effective_configuration: CaptureConfiguration,
    ) -> Result<CaptureHandle, AutomationError> {
        let source = SimulatedOscilloscope::new(
            &self.descriptor.connection_id,
            effective_configuration.sample_rate_hz,
            effective_configuration.channels.len(),
            SIMULATOR_BATCH_SIZE,
        )
        .and_then(|source| source.with_sample_limit(effective_configuration.sample_count))
        .map_err(|error| {
            AutomationError::new(
                AutomationErrorCode::UnsupportedConfiguration,
                error.to_string(),
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id)
        })?;

        self.start_capture_with_factory(
            requested_configuration,
            effective_configuration,
            Box::new(move || Ok(Box::new(source))),
        )
    }

    fn start_usb_logic_capture(
        &self,
        requested_configuration: CaptureConfiguration,
        mut effective_configuration: CaptureConfiguration,
    ) -> Result<CaptureHandle, AutomationError> {
        effective_configuration.sample_count = requested_configuration.sample_count
            - (requested_configuration.sample_count % DSLOGIC_SAMPLE_ALIGNMENT);
        if effective_configuration.sample_count < DSLOGIC_SAMPLE_ALIGNMENT {
            return Err(AutomationError::new(
                AutomationErrorCode::UnsupportedConfiguration,
                format!("DSLogic captures require at least {DSLOGIC_SAMPLE_ALIGNMENT} samples"),
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id));
        }

        let usb_device = self.current_usb_device("capture.start")?;
        if !usb_device.profile.model.is_logic_analyzer()
            || !usb_device.profile.model.supports_capture()
        {
            return Err(AutomationError::new(
                AutomationErrorCode::UnsupportedConfiguration,
                format!(
                    "{} is not a supported DSLogic capture device",
                    usb_device.profile.display_name
                ),
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id));
        }
        let resource_directory = self.resource_directory.as_ref().ok_or_else(|| {
            AutomationError::new(
                AutomationErrorCode::PreparationFailed,
                "USB capture requires an FPGA resource directory",
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id)
        })?;
        let bitstream_path = resource_directory.join(usb_device.profile.fpga_bitstream_name);
        if !bitstream_path.is_file() {
            return Err(AutomationError::new(
                AutomationErrorCode::PreparationFailed,
                format!("FPGA bitstream {} is missing", bitstream_path.display()),
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id));
        }

        let config =
            dslogic_capture_config(&self.descriptor.connection_id, &effective_configuration)?;
        let source = DsLogicCaptureSource::new(usb_device.clone(), config).map_err(|error| {
            AutomationError::new(
                AutomationErrorCode::UnsupportedConfiguration,
                error.to_string(),
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id)
        })?;
        let device_id = self.descriptor.connection_id.clone();
        self.start_capture_with_factory(
            requested_configuration,
            effective_configuration,
            Box::new(move || {
                prepare_device(&usb_device, &bitstream_path).map_err(|error| {
                    AutomationError::new(
                        AutomationErrorCode::PreparationFailed,
                        format!("failed to prepare {device_id}: {error}"),
                        "capture.prepare",
                        true,
                    )
                    .for_device(&device_id)
                })?;
                Ok(Box::new(source))
            }),
        )
    }

    fn start_usb_scope_capture(
        &self,
        requested_configuration: CaptureConfiguration,
        effective_configuration: CaptureConfiguration,
    ) -> Result<CaptureHandle, AutomationError> {
        let usb_device = self.current_usb_device("capture.start")?;
        if usb_device.profile.model != DeviceModel::DsCopeU3P100 {
            return Err(AutomationError::new(
                AutomationErrorCode::UnsupportedConfiguration,
                format!(
                    "{} is not a supported DSCope capture device",
                    usb_device.profile.display_name
                ),
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id));
        }
        let resource_directory = self.resource_directory.as_ref().ok_or_else(|| {
            AutomationError::new(
                AutomationErrorCode::PreparationFailed,
                "USB capture requires an FPGA resource directory",
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id)
        })?;
        let bitstream_path = resource_directory.join(usb_device.profile.fpga_bitstream_name);
        if !bitstream_path.is_file() {
            return Err(AutomationError::new(
                AutomationErrorCode::PreparationFailed,
                format!("FPGA bitstream {} is missing", bitstream_path.display()),
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id));
        }

        let config =
            dscope_capture_config(&self.descriptor.connection_id, &effective_configuration)?;
        let source = DsCopeCaptureSource::new(usb_device.clone(), config).map_err(|error| {
            AutomationError::new(
                AutomationErrorCode::UnsupportedConfiguration,
                error.to_string(),
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id)
        })?;
        let device_id = self.descriptor.connection_id.clone();
        self.start_capture_with_factory(
            requested_configuration,
            effective_configuration,
            Box::new(move || {
                prepare_device(&usb_device, &bitstream_path).map_err(|error| {
                    AutomationError::new(
                        AutomationErrorCode::PreparationFailed,
                        format!("failed to prepare {device_id}: {error}"),
                        "capture.prepare",
                        true,
                    )
                    .for_device(&device_id)
                })?;
                Ok(Box::new(source))
            }),
        )
    }

    fn current_usb_device(
        &self,
        operation: &'static str,
    ) -> Result<DiscoveredDevice, AutomationError> {
        let connection_id = &self.descriptor.connection_id;
        let selector = DeviceSelector::connection_id(connection_id);
        let record = self
            .catalog
            .discover_for_selector(&selector, operation)?
            .into_iter()
            .find(|record| record.describe().connection_id == *connection_id)
            .ok_or_else(|| {
                AutomationError::new(
                    AutomationErrorCode::DeviceDisconnected,
                    format!("device {connection_id} disconnected after selection"),
                    operation,
                    true,
                )
                .for_device(connection_id)
            })?;
        match record {
            DeviceRecord::Usb(device) => Ok(device),
            DeviceRecord::Simulator(_) => Err(AutomationError::new(
                AutomationErrorCode::ServiceUnavailable,
                format!("device {connection_id} changed backend after selection"),
                operation,
                true,
            )
            .for_device(connection_id)),
        }
    }

    fn start_capture_with_factory(
        &self,
        requested_configuration: CaptureConfiguration,
        effective_configuration: CaptureConfiguration,
        source_factory: CaptureSourceFactory,
    ) -> Result<CaptureHandle, AutomationError> {
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&effective_configuration.output_path)
            .map_err(|error| {
                output_open_error(&effective_configuration.output_path, error)
                    .for_device(&self.descriptor.connection_id)
            })?;
        let core_descriptor = DeviceDescriptor {
            id: DeviceId::new(&self.descriptor.connection_id),
            name: self.descriptor.display_name.clone(),
            kind: match self.descriptor.kind {
                InstrumentKind::LogicAnalyzer => CoreInstrumentKind::LogicAnalyzer,
                InstrumentKind::Oscilloscope => CoreInstrumentKind::Oscilloscope,
                InstrumentKind::RadioReceiver => CoreInstrumentKind::RadioReceiver,
            },
        };
        let metadata = capture_metadata(
            &self.descriptor,
            &requested_configuration,
            &effective_configuration,
        );
        let recorder = RecordingWriter::new(BufWriter::new(output), &[core_descriptor], metadata)
            .map_err(|error| {
            AutomationError::new(
                AutomationErrorCode::RecordingFailed,
                format!(
                    "failed to initialize recording {}: {error}",
                    effective_configuration.output_path.display()
                ),
                "capture.start",
                false,
            )
            .for_device(&self.descriptor.connection_id)
        })?;

        let capture_id = CaptureId::next();
        let status = Arc::new(Mutex::new(CaptureStatus::Preparing));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let (event_sender, event_receiver) = mpsc::sync_channel(CAPTURE_EVENT_CAPACITY);
        let (result_sender, result_receiver) = mpsc::channel();
        let mut publisher =
            CaptureEventPublisher::new(capture_id.clone(), Arc::clone(&status), event_sender);
        publisher.set_status(CaptureStatus::Preparing);

        let worker_capture_id = capture_id.clone();
        let worker_device = self.descriptor.clone();
        let worker_stop_requested = Arc::clone(&stop_requested);
        let worker_lease = Arc::clone(&self.lease);
        let worker = thread::Builder::new()
            .name(format!("wavecraft-{capture_id}"))
            .spawn(move || {
                let _lease = worker_lease;
                let result = run_capture(
                    worker_capture_id,
                    worker_device,
                    requested_configuration,
                    effective_configuration,
                    source_factory,
                    recorder,
                    &worker_stop_requested,
                    &mut publisher,
                );
                let terminal_status = match &result {
                    Ok(_) => CaptureStatus::Completed,
                    Err(error) => match error.code {
                        AutomationErrorCode::CaptureCancelled => CaptureStatus::Cancelled,
                        AutomationErrorCode::CaptureTimedOut => CaptureStatus::TimedOut,
                        _ => CaptureStatus::Failed,
                    },
                };
                publisher.set_status(terminal_status);
                let _ = result_sender.send(result);
            })
            .map_err(|error| {
                AutomationError::new(
                    AutomationErrorCode::CaptureFailed,
                    format!("failed to start capture worker: {error}"),
                    "capture.start",
                    true,
                )
                .for_device(&self.descriptor.connection_id)
            })?;

        Ok(CaptureHandle {
            id: capture_id,
            status,
            stop_requested,
            events: event_receiver,
            result: result_receiver,
            worker: Some(worker),
        })
    }

    pub fn close(self) -> Result<(), AutomationError> {
        if Arc::strong_count(&self.lease) == 1 {
            self.lease.release()?;
        }
        Ok(())
    }
}

pub struct CaptureHandle {
    id: CaptureId,
    status: Arc<Mutex<CaptureStatus>>,
    stop_requested: Arc<AtomicBool>,
    events: Receiver<AutomationEvent>,
    result: Receiver<Result<CompletedCapture, AutomationError>>,
    worker: Option<JoinHandle<()>>,
}

impl Debug for CaptureHandle {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureHandle")
            .field("id", &self.id)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl CaptureHandle {
    pub fn id(&self) -> &CaptureId {
        &self.id
    }

    pub fn status(&self) -> CaptureStatus {
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn next_event(
        &self,
        timeout: Duration,
    ) -> Result<Option<AutomationEvent>, AutomationError> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) if self.status().is_terminal() => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(AutomationError::new(
                AutomationErrorCode::CaptureFailed,
                "capture event stream ended before reaching a terminal state",
                "capture.events",
                true,
            )),
        }
    }

    pub fn request_stop(&self) -> Result<(), AutomationError> {
        self.stop_requested.store(true, Ordering::Release);
        Ok(())
    }

    pub fn wait(mut self) -> Result<CompletedCapture, AutomationError> {
        let result = self.result.recv().map_err(|_| {
            AutomationError::new(
                AutomationErrorCode::CaptureFailed,
                "capture worker ended without returning a result",
                "capture.wait",
                true,
            )
        })?;
        self.join_worker()?;
        result
    }

    fn join_worker(&mut self) -> Result<(), AutomationError> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        worker.join().map_err(|_| {
            AutomationError::new(
                AutomationErrorCode::CaptureFailed,
                "capture worker panicked",
                "capture.wait",
                true,
            )
        })
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::Release);
        let _ = self.join_worker();
    }
}

struct CaptureEventPublisher {
    capture_id: CaptureId,
    status: Arc<Mutex<CaptureStatus>>,
    sender: SyncSender<AutomationEvent>,
    sequence: u64,
    started_at: Instant,
}

impl CaptureEventPublisher {
    fn new(
        capture_id: CaptureId,
        status: Arc<Mutex<CaptureStatus>>,
        sender: SyncSender<AutomationEvent>,
    ) -> Self {
        Self {
            capture_id,
            status,
            sender,
            sequence: 0,
            started_at: Instant::now(),
        }
    }

    fn set_status(&mut self, status: CaptureStatus) {
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = status;
        self.publish(AutomationEventKind::Status { status });
    }

    fn progress(&mut self, captured_samples: u64, requested_samples: u64) {
        self.publish(AutomationEventKind::Progress {
            captured_samples,
            requested_samples,
        });
    }

    fn publish(&mut self, event: AutomationEventKind) {
        let automation_event = AutomationEvent {
            schema: CAPTURE_EVENT_SCHEMA.to_owned(),
            capture_id: self.capture_id.clone(),
            sequence: self.sequence,
            elapsed_ms: duration_millis(self.started_at.elapsed()),
            event,
        };
        self.sequence = self.sequence.saturating_add(1);
        match self.sender.try_send(automation_event) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

enum CaptureRunOutcome {
    Completed,
    Cancelled,
    TimedOut,
    Failed(AutomationError),
}

type CaptureSourceFactory =
    Box<dyn FnOnce() -> Result<Box<dyn CaptureSource>, AutomationError> + Send + 'static>;

#[allow(clippy::too_many_arguments)]
fn run_capture(
    capture_id: CaptureId,
    device: AutomationDevice,
    requested_configuration: CaptureConfiguration,
    effective_configuration: CaptureConfiguration,
    source_factory: CaptureSourceFactory,
    mut recorder: RecordingWriter<BufWriter<File>>,
    stop_requested: &AtomicBool,
    publisher: &mut CaptureEventPublisher,
) -> Result<CompletedCapture, AutomationError> {
    let started_at = Instant::now();
    let source = match source_factory() {
        Ok(source) => source,
        Err(error) => {
            recorder.finish(CompletionStatus::Failed).map_err(|recording_error| {
                AutomationError::new(
                    AutomationErrorCode::RecordingFailed,
                    format!(
                        "capture setup failed ({error}); additionally failed to finalize recording {}: {recording_error}",
                        effective_configuration.output_path.display()
                    ),
                    "capture.finalize",
                    false,
                )
                .for_device(&device.connection_id)
            })?;
            return Err(error);
        }
    };
    publisher.set_status(CaptureStatus::Armed);
    let session = CaptureCoordinator::start(vec![source]).map_err(|error| {
        let _ = recorder.finish(CompletionStatus::Failed);
        AutomationError::new(
            AutomationErrorCode::CaptureFailed,
            format!("failed to arm capture: {error}"),
            "capture.run",
            true,
        )
        .for_device(&device.connection_id)
    })?;
    publisher.set_status(CaptureStatus::Capturing);

    let deadline = started_at + effective_configuration.timeout();
    let mut frame_count = 0_u64;
    let mut captured_samples = 0_u64;
    let outcome = loop {
        if stop_requested.load(Ordering::Acquire) {
            session.request_stop();
            break CaptureRunOutcome::Cancelled;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            session.request_stop();
            break CaptureRunOutcome::TimedOut;
        }

        match session.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok(event) => {
                if let Err(error) = recorder.write_event(&event) {
                    session.request_stop();
                    break CaptureRunOutcome::Failed(
                        AutomationError::new(
                            AutomationErrorCode::RecordingFailed,
                            format!(
                                "failed to write recording {}: {error}",
                                effective_configuration.output_path.display()
                            ),
                            "capture.record",
                            false,
                        )
                        .for_device(&device.connection_id),
                    );
                }
                match event {
                    CoreCaptureEvent::Frame(frame) => {
                        frame_count = frame_count.saturating_add(1);
                        captured_samples =
                            captured_samples.saturating_add(frame.payload.sample_count() as u64);
                        publisher.progress(captured_samples, effective_configuration.sample_count);
                    }
                    CoreCaptureEvent::SourceFailed { error, .. } => {
                        break CaptureRunOutcome::Failed(
                            AutomationError::new(
                                AutomationErrorCode::CaptureFailed,
                                format!("capture source failed: {error}"),
                                "capture.run",
                                true,
                            )
                            .for_device(&device.connection_id),
                        );
                    }
                    CoreCaptureEvent::SourceStopped { .. } => {
                        if captured_samples == effective_configuration.sample_count {
                            break CaptureRunOutcome::Completed;
                        }
                        break CaptureRunOutcome::Failed(
                            AutomationError::new(
                                AutomationErrorCode::CaptureFailed,
                                format!(
                                    "capture stopped after {captured_samples} of {} requested samples",
                                    effective_configuration.sample_count
                                ),
                                "capture.run",
                                true,
                            )
                            .for_device(&device.connection_id),
                        );
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                break CaptureRunOutcome::Failed(
                    AutomationError::new(
                        AutomationErrorCode::CaptureFailed,
                        "capture event stream ended unexpectedly",
                        "capture.run",
                        true,
                    )
                    .for_device(&device.connection_id),
                );
            }
        }
    };

    publisher.set_status(CaptureStatus::Finalizing);
    let stop_result = session.stop();
    let outcome = match (outcome, stop_result) {
        (CaptureRunOutcome::Completed, Err(error)) => CaptureRunOutcome::Failed(
            AutomationError::new(
                AutomationErrorCode::CaptureFailed,
                format!("failed to stop capture source cleanly: {error}"),
                "capture.finalize",
                true,
            )
            .for_device(&device.connection_id),
        ),
        (outcome, _) => outcome,
    };
    let completion_status = match &outcome {
        CaptureRunOutcome::Completed => CompletionStatus::Complete,
        CaptureRunOutcome::Cancelled | CaptureRunOutcome::TimedOut => CompletionStatus::Cancelled,
        CaptureRunOutcome::Failed(_) => CompletionStatus::Failed,
    };
    recorder.finish(completion_status).map_err(|error| {
        AutomationError::new(
            AutomationErrorCode::RecordingFailed,
            format!(
                "failed to finalize recording {}: {error}",
                effective_configuration.output_path.display()
            ),
            "capture.finalize",
            false,
        )
        .for_device(&device.connection_id)
    })?;

    let elapsed_ms = duration_millis(started_at.elapsed());
    match outcome {
        CaptureRunOutcome::Completed => Ok(CompletedCapture {
            schema: CAPTURE_RESULT_SCHEMA.to_owned(),
            summary: CaptureSummary {
                capture_id,
                status: CaptureStatus::Completed,
                device,
                requested_configuration,
                effective_configuration: effective_configuration.clone(),
                frame_count,
                captured_samples,
                elapsed_ms,
                recording_path: effective_configuration.output_path,
            },
        }),
        CaptureRunOutcome::Cancelled => Err(AutomationError::new(
            AutomationErrorCode::CaptureCancelled,
            format!("capture was cancelled after {captured_samples} samples"),
            "capture.wait",
            false,
        )
        .for_device(device.connection_id)),
        CaptureRunOutcome::TimedOut => Err(AutomationError::new(
            AutomationErrorCode::CaptureTimedOut,
            format!(
                "capture timed out after {} ms and {captured_samples} samples",
                effective_configuration.timeout_ms
            ),
            "capture.wait",
            true,
        )
        .for_device(device.connection_id)),
        CaptureRunOutcome::Failed(error) => Err(error),
    }
}

fn dslogic_capture_config(
    device_id: &str,
    configuration: &CaptureConfiguration,
) -> Result<DsLogicCaptureConfig, AutomationError> {
    let mut enabled_channels = [false; 32];
    for channel in &configuration.channels {
        enabled_channels[usize::from(*channel)] = true;
    }
    DsLogicCaptureConfig::new(configuration.sample_rate_hz, configuration.sample_count)
        .and_then(|config| config.with_enabled_channels_32(enabled_channels))
        .map(|config| config.with_operation_mode(DsLogicOperationMode::Stream))
        .map_err(|error| {
            AutomationError::new(
                AutomationErrorCode::UnsupportedConfiguration,
                error.to_string(),
                "capture.start",
                false,
            )
            .for_device(device_id)
        })
}

fn dscope_capture_config(
    device_id: &str,
    configuration: &CaptureConfiguration,
) -> Result<DsCopeCaptureConfig, AutomationError> {
    let unsupported = |error: wavecraft_core::CaptureError| {
        AutomationError::new(
            AutomationErrorCode::UnsupportedConfiguration,
            error.to_string(),
            "capture.start",
            false,
        )
        .for_device(device_id)
    };
    let scope = configuration.scope.ok_or_else(|| {
        AutomationError::new(
            AutomationErrorCode::UnsupportedConfiguration,
            "oscilloscope capture requires scope configuration",
            "capture.start",
            false,
        )
        .for_device(device_id)
    })?;
    let enabled_channels = configuration
        .channels
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut channels = [DsCopeChannelConfig::default(); 2];
    for (index, (destination, source)) in
        channels.iter_mut().zip(scope.channel_settings).enumerate()
    {
        *destination = DsCopeChannelConfig::new(
            source.volts_per_div_mv,
            match source.coupling {
                ScopeCoupling::Dc => UsbScopeCoupling::Dc,
                ScopeCoupling::Ac => UsbScopeCoupling::Ac,
            },
            source.probe_attenuation,
        )
        .and_then(|channel| channel.with_vertical_offset(source.vertical_offset))
        .map(|channel| channel.with_enabled(enabled_channels.contains(&(index as u8))))
        .map_err(unsupported)?;
    }
    let CaptureTrigger::Scope(trigger) = configuration.trigger else {
        return Err(AutomationError::new(
            AutomationErrorCode::UnsupportedConfiguration,
            "oscilloscope capture requires scope trigger configuration",
            "capture.start",
            false,
        )
        .for_device(device_id));
    };
    let trigger = DsCopeTriggerConfig::new(
        match trigger.source {
            ScopeTriggerSource::Auto => UsbScopeTriggerSource::Auto,
            ScopeTriggerSource::ChannelZero => UsbScopeTriggerSource::ChannelZero,
            ScopeTriggerSource::ChannelOne => UsbScopeTriggerSource::ChannelOne,
            ScopeTriggerSource::ChannelZeroAndOne => UsbScopeTriggerSource::ChannelZeroAndOne,
            ScopeTriggerSource::ChannelZeroOrOne => UsbScopeTriggerSource::ChannelZeroOrOne,
        },
        match trigger.slope {
            ScopeTriggerSlope::Rising => UsbScopeTriggerSlope::Rising,
            ScopeTriggerSlope::Falling => UsbScopeTriggerSlope::Falling,
        },
    )
    .with_position_percent(trigger.position_percent)
    .and_then(|value| value.with_auto_channel(trigger.auto_channel))
    .and_then(|value| value.with_holdoff_ns(trigger.holdoff_ns))
    .and_then(|value| value.with_noise_sensitivity(trigger.noise_sensitivity))
    .and_then(|value| value.with_levels(trigger.levels))
    .map_err(unsupported)?;
    DsCopeCaptureConfig::with_channel_configs(
        configuration.sample_rate_hz,
        configuration.sample_count,
        channels,
    )
    .and_then(|config| config.with_trigger_config(trigger))
    .map(|config| {
        config.with_options(
            DsCopeOperationMode::Normal,
            match scope.bandwidth_limit {
                ScopeBandwidthLimit::Full => UsbScopeBandwidthLimit::Full,
                ScopeBandwidthLimit::Mhz20 => UsbScopeBandwidthLimit::Mhz20,
            },
        )
    })
    .map_err(unsupported)
}

fn validate_capture_configuration(
    device: &AutomationDevice,
    configuration: &CaptureConfiguration,
) -> Result<(), AutomationError> {
    let unsupported = |message: String| {
        AutomationError::new(
            AutomationErrorCode::UnsupportedConfiguration,
            message,
            "capture.start",
            false,
        )
        .for_device(&device.connection_id)
    };
    if configuration.schema != CAPTURE_CONFIGURATION_SCHEMA {
        return Err(unsupported(format!(
            "capture configuration schema {} is unsupported",
            configuration.schema
        )));
    }
    if !device.capabilities.supports_capture {
        return Err(unsupported(
            "selected device does not support capture".to_owned(),
        ));
    }
    if configuration.mode != CaptureMode::Finite {
        return Err(unsupported(
            "only finite capture mode is currently supported".to_owned(),
        ));
    }
    match device.kind {
        InstrumentKind::LogicAnalyzer => {
            if configuration.trigger != CaptureTrigger::None || configuration.scope.is_some() {
                return Err(unsupported(
                    "logic capture requires an untriggered logic configuration".to_owned(),
                ));
            }
        }
        InstrumentKind::Oscilloscope => {
            if !matches!(configuration.trigger, CaptureTrigger::Scope(_))
                || configuration.scope.is_none()
            {
                return Err(unsupported(
                    "oscilloscope capture requires scope frontend and trigger configuration"
                        .to_owned(),
                ));
            }
        }
        InstrumentKind::RadioReceiver => {
            return Err(unsupported(
                "radio receiver capture configuration is not implemented".to_owned(),
            ));
        }
    }
    if !device
        .capabilities
        .sample_rates_hz
        .contains(&configuration.sample_rate_hz)
    {
        return Err(unsupported(format!(
            "sample rate {} Hz is not supported by {}",
            configuration.sample_rate_hz, device.display_name
        )));
    }
    if configuration.sample_count == 0 || configuration.sample_count > MAX_CAPTURE_SAMPLE_COUNT {
        return Err(unsupported(format!(
            "sample count must be between 1 and {MAX_CAPTURE_SAMPLE_COUNT}"
        )));
    }
    if configuration.timeout().is_zero() || configuration.timeout() > MAX_CAPTURE_TIMEOUT {
        return Err(unsupported(format!(
            "capture timeout must be between 1 ms and {} ms",
            MAX_CAPTURE_TIMEOUT.as_millis()
        )));
    }
    if configuration.channels.is_empty() {
        return Err(unsupported(
            "at least one channel must be enabled".to_owned(),
        ));
    }
    let unique_channels = configuration
        .channels
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    if unique_channels.len() != configuration.channels.len() {
        return Err(unsupported(
            "enabled channels must not contain duplicates".to_owned(),
        ));
    }
    if let Some(channel) = configuration
        .channels
        .iter()
        .find(|channel| **channel >= device.capabilities.channel_count)
    {
        return Err(unsupported(format!(
            "channel {channel} is outside the device range 0..{}",
            device.capabilities.channel_count.saturating_sub(1)
        )));
    }
    if configuration.output_path.as_os_str().is_empty() {
        return Err(unsupported(
            "capture output path must not be empty".to_owned(),
        ));
    }
    Ok(())
}

fn output_open_error(path: &Path, error: std::io::Error) -> AutomationError {
    let code = if error.kind() == std::io::ErrorKind::AlreadyExists {
        AutomationErrorCode::OutputExists
    } else {
        AutomationErrorCode::RecordingFailed
    };
    AutomationError::new(
        code,
        format!(
            "failed to create capture recording {} without overwriting: {error}",
            path.display()
        ),
        "capture.start",
        false,
    )
}

fn capture_metadata(
    device: &AutomationDevice,
    requested: &CaptureConfiguration,
    effective: &CaptureConfiguration,
) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "automation.configuration_schema".to_owned(),
        requested.schema.clone(),
    );
    metadata.insert(
        "capture.connection_id".to_owned(),
        device.connection_id.clone(),
    );
    metadata.insert("capture.model".to_owned(), device.model.clone());
    metadata.insert("capture.backend".to_owned(), device.backend.to_string());
    metadata.insert(
        "capture.sample_rate_hz".to_owned(),
        effective.sample_rate_hz.to_string(),
    );
    metadata.insert(
        "capture.requested_sample_count".to_owned(),
        requested.sample_count.to_string(),
    );
    metadata.insert(
        "capture.effective_sample_count".to_owned(),
        effective.sample_count.to_string(),
    );
    metadata.insert(
        "capture.enabled_channels".to_owned(),
        effective
            .channels
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(","),
    );
    if let Some(scope) = effective.scope {
        metadata.insert(
            "scope.bandwidth_limit".to_owned(),
            scope.bandwidth_limit.to_string(),
        );
        for (index, channel) in scope.channel_settings.into_iter().enumerate() {
            metadata.insert(
                format!("scope.channel_{index}.volts_per_div_mv"),
                channel.volts_per_div_mv.to_string(),
            );
            metadata.insert(
                format!("scope.channel_{index}.coupling"),
                channel.coupling.to_string(),
            );
            metadata.insert(
                format!("scope.channel_{index}.probe_attenuation"),
                channel.probe_attenuation.to_string(),
            );
            metadata.insert(
                format!("scope.channel_{index}.vertical_offset"),
                channel.vertical_offset.to_string(),
            );
        }
    }
    if let CaptureTrigger::Scope(trigger) = effective.trigger {
        metadata.insert(
            "scope.trigger.position_percent".to_owned(),
            trigger.position_percent.to_string(),
        );
        metadata.insert(
            "scope.trigger.source".to_owned(),
            trigger.source.to_string(),
        );
        metadata.insert(
            "scope.trigger.auto_channel".to_owned(),
            trigger.auto_channel.to_string(),
        );
        metadata.insert("scope.trigger.slope".to_owned(), trigger.slope.to_string());
        metadata.insert(
            "scope.trigger.holdoff_ns".to_owned(),
            trigger.holdoff_ns.to_string(),
        );
        metadata.insert(
            "scope.trigger.noise_sensitivity".to_owned(),
            trigger.noise_sensitivity.to_string(),
        );
        metadata.insert(
            "scope.trigger.levels".to_owned(),
            trigger
                .levels
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    metadata
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn describe_simulator(profile: &SimulatedDeviceProfile) -> AutomationDevice {
    AutomationDevice {
        connection_id: profile.connection_id.to_owned(),
        persistent_id: Some(profile.connection_id.to_owned()),
        display_name: profile.display_name.to_owned(),
        model: profile.model.to_owned(),
        kind: profile.kind.into(),
        backend: DeviceBackend::Simulator,
        physical_location: None,
        ownership: DeviceOwnershipState::Available,
        preparation: DevicePreparationState::Ready,
        link: DeviceLink {
            speed: None,
            status: DeviceLinkStatus::NotApplicable,
            warning: None,
        },
        capabilities: DeviceCapabilities {
            channel_count: profile.channel_count,
            supports_capture: true,
            sample_rates_hz: profile.sample_rates_hz.to_vec(),
            operation_modes: vec!["finite".to_owned(), "continuous".to_owned()],
        },
    }
}

fn describe_usb(device: &DiscoveredDevice) -> AutomationDevice {
    let link_status = device.link_status();
    let warning = match link_status {
        LinkStatus::Optimal => None,
        LinkStatus::ReducedPerformance => Some(format!(
            "{} is using {}; {} is recommended",
            device.profile.display_name,
            device.speed.description(),
            device.profile.recommended_speed.description()
        )),
        LinkStatus::Unsupported => Some(format!(
            "{} is using {}; {} or faster is required",
            device.profile.display_name,
            device.speed.description(),
            device.profile.minimum_speed.description()
        )),
    };
    let sample_rates_hz = if device.profile.model.is_logic_analyzer() {
        DSLOGIC_SAMPLE_RATES_HZ.to_vec()
    } else if device.profile.model == DeviceModel::DsCopeU3P100 {
        DSCOPE_SAMPLE_RATES_HZ.to_vec()
    } else {
        Vec::new()
    };
    let operation_modes = if device.profile.model.is_logic_analyzer() {
        vec!["stream".to_owned(), "buffer".to_owned()]
    } else if device.profile.model == DeviceModel::DsCopeU3P100 {
        vec!["buffer".to_owned(), "roll".to_owned()]
    } else {
        Vec::new()
    };

    AutomationDevice {
        connection_id: device.location.stable_id(),
        persistent_id: None,
        display_name: device.profile.display_name.to_owned(),
        model: usb_model_name(device.profile.model).to_owned(),
        kind: device.profile.instrument_kind.into(),
        backend: DeviceBackend::Usb,
        physical_location: Some(device.location.platform_path.clone()),
        ownership: DeviceOwnershipState::Available,
        preparation: DevicePreparationState::Unknown,
        link: DeviceLink {
            speed: Some(device.speed.into()),
            status: link_status.into(),
            warning,
        },
        capabilities: DeviceCapabilities {
            channel_count: device.profile.channel_count,
            supports_capture: device.profile.model.supports_capture()
                && link_status != LinkStatus::Unsupported,
            sample_rates_hz,
            operation_modes,
        },
    }
}

fn process_lease_registry() -> Arc<Mutex<HashSet<String>>> {
    static LEASES: OnceLock<Arc<Mutex<HashSet<String>>>> = OnceLock::new();
    Arc::clone(LEASES.get_or_init(|| Arc::new(Mutex::new(HashSet::new()))))
}

fn usb_model_name(model: DeviceModel) -> &'static str {
    match model {
        DeviceModel::DsLogic => "dslogic",
        DeviceModel::DsLogicPro => "dslogic_pro",
        DeviceModel::DsLogicPlus => "dslogic_plus",
        DeviceModel::DsLogicBasic => "dslogic_basic",
        DeviceModel::DsLogicU2Basic => "dslogic_u2basic",
        DeviceModel::DsLogicU3Pro16 => "dslogic_u3pro16",
        DeviceModel::DsLogicU3Pro32 => "dslogic_u3pro32",
        DeviceModel::DsLogicU2Pro16 => "dslogic_u2pro16",
        DeviceModel::DsLogicPlusPgl12 => "dslogic_plus_pgl12",
        DeviceModel::DsLogicU2BasicPgl12 => "dslogic_u2basic_pgl12",
        DeviceModel::DsLogicPlusPgl12Rev2 => "dslogic_plus_pgl12_rev2",
        DeviceModel::DsLogicU2BasicPgl12Rev2 => "dslogic_u2basic_pgl12_rev2",
        DeviceModel::DsCope => "dscope",
        DeviceModel::DsCope20 => "dscope20",
        DeviceModel::DsCopeB20 => "dscope_b20",
        DeviceModel::DsCopeC20 => "dscope_c20",
        DeviceModel::DsCopeC20P => "dscope_c20p",
        DeviceModel::DsCopeC20B => "dscope_c20b",
        DeviceModel::DsCopeU2B20 => "dscope_u2b20",
        DeviceModel::DsCopeU2P20 => "dscope_u2p20",
        DeviceModel::DsCopeU2B100 => "dscope_u2b100",
        DeviceModel::DsCopeU3P100 => "dscope_u3p100",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;
    use std::sync::MutexGuard;
    use wavecraft_recording::{CompletionStatus, RecordingReader};
    use wavecraft_usb::{DeviceLocation, DSCOPE_U3P100, DSLOGIC_U3PRO16};

    struct TestRecordingPath(PathBuf);

    impl TestRecordingPath {
        fn new(name: &str) -> Self {
            static NEXT_PATH: AtomicU64 = AtomicU64::new(1);
            Self(std::env::temp_dir().join(format!(
                "wavecraft-automation-{}-{}-{name}.dsv",
                std::process::id(),
                NEXT_PATH.fetch_add(1, Ordering::Relaxed)
            )))
        }

        fn as_path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRecordingPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn usb_device(path: &str) -> DiscoveredDevice {
        DiscoveredDevice {
            profile: &DSCOPE_U3P100,
            location: DeviceLocation {
                platform_path: path.to_owned(),
            },
            speed: UsbSpeedClass::SuperSpeed,
        }
    }

    fn usb_logic_device(path: &str) -> DiscoveredDevice {
        DiscoveredDevice {
            profile: &DSLOGIC_U3PRO16,
            location: DeviceLocation {
                platform_path: path.to_owned(),
            },
            speed: UsbSpeedClass::SuperSpeed,
        }
    }

    fn test_manager(devices: Arc<Mutex<Vec<DiscoveredDevice>>>) -> AutomationManager {
        AutomationManager::from_usb_discovery(
            move || {
                Ok(devices
                    .lock()
                    .expect("test discovery should not be poisoned")
                    .clone())
            },
            Vec::new(),
        )
    }

    fn simulator_manager() -> AutomationManager {
        AutomationManager::from_usb_discovery(|| Ok(Vec::new()), SIMULATED_DEVICE_PROFILES.to_vec())
    }

    fn logic_configuration(path: &Path, sample_count: u64) -> CaptureConfiguration {
        CaptureConfiguration::finite_logic(100_000, sample_count, vec![2, 0], path)
            .with_timeout(Duration::from_secs(2))
    }

    fn usb_logic_configuration(path: &Path, sample_count: u64) -> CaptureConfiguration {
        CaptureConfiguration::finite_logic(1_000_000, sample_count, vec![2, 0], path)
            .with_timeout(Duration::from_secs(2))
    }

    fn scope_configuration(path: &Path, sample_count: u64) -> CaptureConfiguration {
        CaptureConfiguration::finite_scope(50_000, sample_count, vec![1], path)
            .with_timeout(Duration::from_secs(2))
    }

    fn usb_scope_configuration(path: &Path, sample_count: u64) -> CaptureConfiguration {
        CaptureConfiguration::finite_scope(1_000_000, sample_count, vec![1, 0], path)
            .with_timeout(Duration::from_secs(2))
    }

    fn recording_completion(path: &Path) -> CompletionStatus {
        let file = File::open(path).expect("recording should exist");
        let mut reader =
            RecordingReader::new(BufReader::new(file)).expect("recording header should be valid");
        while reader
            .read_event()
            .expect("recording event should be valid")
            .is_some()
        {}
        reader
            .completion_status()
            .expect("recording should have a footer")
    }

    fn devices<'a>(
        state: &'a Arc<Mutex<Vec<DiscoveredDevice>>>,
    ) -> MutexGuard<'a, Vec<DiscoveredDevice>> {
        state
            .lock()
            .expect("test device state should not be poisoned")
    }

    #[test]
    fn default_manager_lists_both_simulator_kinds() {
        let manager = AutomationManager::from_usb_discovery(
            || Ok(Vec::new()),
            SIMULATED_DEVICE_PROFILES.to_vec(),
        );
        let listing = manager.device_list().expect("listing should succeed");

        assert_eq!(listing.schema, DEVICE_LIST_SCHEMA);
        assert_eq!(listing.devices.len(), 2);
        assert!(listing
            .devices
            .iter()
            .any(|device| device.kind == InstrumentKind::LogicAnalyzer));
        assert!(listing
            .devices
            .iter()
            .any(|device| device.kind == InstrumentKind::Oscilloscope));
    }

    #[test]
    fn exact_connection_id_resolves_one_of_identical_models() {
        let state = Arc::new(Mutex::new(vec![usb_device("port-1"), usb_device("port-2")]));
        let manager = test_manager(state);
        let expected_id = usb_device("port-2").location.stable_id();

        let handle = manager
            .open_device(DeviceSelector::connection_id(&expected_id))
            .expect("exact connection ID should resolve");

        assert_eq!(handle.descriptor().connection_id, expected_id);
    }

    #[test]
    fn model_selector_rejects_ambiguous_identical_devices() {
        let state = Arc::new(Mutex::new(vec![
            usb_device("port-1"),
            usb_device("port-2"),
            usb_device("port-3"),
        ]));
        let manager = test_manager(state);

        let error = manager
            .open_device(DeviceSelector::model("dscope_u3p100"))
            .expect_err("model-only selector should be ambiguous");

        assert_eq!(error.code, AutomationErrorCode::AmbiguousDeviceMatch);
        assert_eq!(error.candidates.len(), 3);
        assert_eq!(error.context["candidate_count"], "3");
    }

    #[test]
    fn ambiguous_error_bounds_candidate_descriptions() {
        let state = Arc::new(Mutex::new(
            (0..20)
                .map(|index| usb_device(&format!("port-{index}")))
                .collect(),
        ));
        let manager = test_manager(state);

        let error = manager
            .open_device(DeviceSelector::model("dscope_u3p100"))
            .expect_err("model-only selector should be ambiguous");

        assert_eq!(error.candidates.len(), MAX_ERROR_CANDIDATES);
        assert_eq!(error.context["candidate_count"], "20");
    }

    #[test]
    fn zero_matches_returns_structured_not_found_error() {
        let manager = test_manager(Arc::new(Mutex::new(Vec::new())));

        let error = manager
            .open_device(DeviceSelector::connection_id("usb-missing"))
            .expect_err("missing selector should fail");

        assert_eq!(error.schema, ERROR_SCHEMA);
        assert_eq!(error.code, AutomationErrorCode::DeviceNotFound);
        assert_eq!(error.operation, "device.open");
        assert!(error.retryable);
    }

    #[test]
    fn lease_rejects_second_open_and_releases_on_drop() {
        let state = Arc::new(Mutex::new(vec![usb_device("port-1")]));
        let manager = test_manager(state);
        let selector = DeviceSelector::model("dscope_u3p100");
        let first = manager
            .open_device(selector.clone())
            .expect("first open should acquire the lease");

        let error = manager
            .open_device(selector.clone())
            .expect_err("second open should report busy");
        assert_eq!(error.code, AutomationErrorCode::DeviceBusy);

        drop(first);
        manager
            .open_device(selector)
            .expect("dropping the handle should release the lease");
    }

    #[test]
    fn device_listing_reports_active_process_lease() {
        let state = Arc::new(Mutex::new(vec![usb_device("port-1")]));
        let manager = test_manager(state);
        let handle = manager
            .open_device(DeviceSelector::model("dscope_u3p100"))
            .expect("device should open");

        let listing = manager.list_devices().expect("listing should succeed");

        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].connection_id, handle.descriptor().connection_id);
        assert_eq!(listing[0].ownership, DeviceOwnershipState::InUse);
    }

    #[test]
    fn production_managers_share_process_wide_leases() {
        let manager = || {
            AutomationManager::from_parts(
                || Ok(Vec::new()),
                SIMULATED_DEVICE_PROFILES.to_vec(),
                process_lease_registry(),
            )
        };
        let first_manager = manager();
        let second_manager = manager();
        let selector = DeviceSelector::connection_id("sim:logic-0");
        let first = first_manager
            .open_device(selector.clone())
            .expect("first manager should acquire the simulator lease");

        let error = second_manager
            .open_device(selector.clone())
            .expect_err("second manager should observe the process-wide lease");
        assert_eq!(error.code, AutomationErrorCode::DeviceBusy);

        drop(first);
        second_manager
            .open_device(selector)
            .expect("dropping the first handle should release the shared lease");
    }

    #[test]
    fn probe_reports_disconnect_after_selection() {
        let state = Arc::new(Mutex::new(vec![usb_device("port-1")]));
        let manager = test_manager(Arc::clone(&state));
        let handle = manager
            .open_device(DeviceSelector::model("dscope_u3p100"))
            .expect("device should initially resolve");
        devices(&state).clear();

        let error = handle
            .probe()
            .expect_err("probing a disconnected selection should fail");

        assert_eq!(error.code, AutomationErrorCode::DeviceDisconnected);
        assert_eq!(
            error.device_id,
            Some(handle.descriptor().connection_id.clone())
        );
    }

    #[test]
    fn usb_capture_reports_disconnect_after_selection() {
        let path = TestRecordingPath::new("usb-disconnected");
        let state = Arc::new(Mutex::new(vec![usb_logic_device("logic-port")]));
        let manager = test_manager(Arc::clone(&state));
        let handle = manager
            .open_device(DeviceSelector::model("dslogic_u3pro16"))
            .expect("logic device should initially resolve");
        devices(&state).clear();

        let error = handle
            .start_capture(usb_logic_configuration(path.as_path(), 10_001))
            .expect_err("disconnected selection should fail");

        assert_eq!(error.code, AutomationErrorCode::DeviceDisconnected);
        assert!(!path.as_path().exists());
    }

    #[test]
    fn usb_capture_requires_configured_fpga_resources() {
        let path = TestRecordingPath::new("usb-resources");
        let state = Arc::new(Mutex::new(vec![usb_logic_device("logic-port")]));
        let manager = test_manager(state);
        let handle = manager
            .open_device(DeviceSelector::model("dslogic_u3pro16"))
            .expect("logic device should resolve");

        let error = handle
            .start_capture(usb_logic_configuration(path.as_path(), 10_001))
            .expect_err("USB capture without resources should fail");

        assert_eq!(error.code, AutomationErrorCode::PreparationFailed);
        assert!(!path.as_path().exists());
    }

    #[test]
    fn dslogic_configuration_uses_sorted_channels_and_aligned_sample_count() {
        let mut requested = usb_logic_configuration(Path::new("unused.dsv"), 10_001);
        requested.channels = vec![7, 1, 3];
        let mut effective = requested.clone();
        effective.channels.sort_unstable();
        effective.sample_count =
            requested.sample_count - requested.sample_count % DSLOGIC_SAMPLE_ALIGNMENT;
        let config = dslogic_capture_config("usb:test", &effective)
            .expect("effective DSLogic configuration should be valid");

        assert_eq!(effective.channels, [1, 3, 7]);
        assert_eq!(effective.sample_count, 9_216);
        assert_eq!(config.sample_count(), 9_216);
        assert_eq!(config.enabled_channel_count(), 3);
        assert_eq!(config.operation_mode(), DsLogicOperationMode::Stream);
    }

    #[test]
    fn dscope_configuration_maps_frontend_trigger_and_sorted_channels() {
        let mut configuration = usb_scope_configuration(Path::new("unused.dsv"), 100_000);
        let mut scope = configuration.scope.expect("scope settings should exist");
        scope.channel_settings[1] = ScopeChannelConfiguration {
            volts_per_div_mv: 200,
            coupling: ScopeCoupling::Ac,
            probe_attenuation: 10,
            vertical_offset: 144,
        };
        scope.bandwidth_limit = ScopeBandwidthLimit::Mhz20;
        configuration.scope = Some(scope);
        configuration.trigger = CaptureTrigger::Scope(ScopeTriggerConfiguration {
            position_percent: 25,
            source: ScopeTriggerSource::ChannelOne,
            auto_channel: 1,
            slope: ScopeTriggerSlope::Falling,
            holdoff_ns: 2_000,
            noise_sensitivity: 5,
            levels: [120, 140],
        });
        configuration.channels.sort_unstable();

        let config = dscope_capture_config("usb:test", &configuration)
            .expect("DSCope configuration should be valid");

        assert_eq!(configuration.channels, [0, 1]);
        assert_eq!(config.sample_count(), 100_000);
        assert_eq!(config.enabled_channel_count(), 2);
        assert_eq!(config.channels()[1].volts_per_div_mv(), 200);
        assert_eq!(config.channels()[1].coupling(), UsbScopeCoupling::Ac);
        assert_eq!(config.channels()[1].probe_attenuation(), 10);
        assert_eq!(config.channels()[1].vertical_offset(), 144);
        assert_eq!(config.trigger().source(), UsbScopeTriggerSource::ChannelOne);
        assert_eq!(config.trigger().slope(), UsbScopeTriggerSlope::Falling);
        assert_eq!(config.bandwidth_limit(), UsbScopeBandwidthLimit::Mhz20);
    }

    #[test]
    fn simulated_probe_uses_the_same_handle_contract() {
        let manager = AutomationManager::from_usb_discovery(
            || Ok(Vec::new()),
            SIMULATED_DEVICE_PROFILES.to_vec(),
        );
        let handle = manager
            .open_device(DeviceSelector::connection_id("sim:logic-0"))
            .expect("simulator should resolve");

        let probe = handle.probe().expect("simulator probe should succeed");

        assert_eq!(probe.schema, DEVICE_PROBE_SCHEMA);
        assert_eq!(probe.device.backend, DeviceBackend::Simulator);
        assert_eq!(probe.device.capabilities.channel_count, 8);
        assert_eq!(probe.firmware_version, None);
    }

    #[test]
    fn capability_selector_resolves_matching_simulator() {
        let manager = AutomationManager::from_usb_discovery(
            || Ok(Vec::new()),
            SIMULATED_DEVICE_PROFILES.to_vec(),
        );
        let selector = DeviceSelector::default()
            .with_kind(InstrumentKind::LogicAnalyzer)
            .with_minimum_channel_count(8)
            .requiring_capture();

        let handle = manager
            .open_device(selector)
            .expect("capability selector should resolve the logic simulator");

        assert_eq!(handle.descriptor().connection_id, "sim:logic-0");
    }

    #[test]
    fn structured_error_serializes_stable_code() {
        let manager = test_manager(Arc::new(Mutex::new(Vec::new())));
        let error = manager
            .open_device(DeviceSelector::connection_id("usb-missing"))
            .expect_err("missing selector should fail");

        let value = serde_json::to_value(error).expect("error should serialize");

        assert_eq!(value["schema"], ERROR_SCHEMA);
        assert_eq!(value["code"], "device_not_found");
    }

    #[test]
    fn finite_simulated_capture_writes_exact_recording_and_lifecycle() {
        let path = TestRecordingPath::new("complete");
        let manager = simulator_manager();
        let device = manager
            .open_device(DeviceSelector::connection_id("sim:logic-0"))
            .expect("simulator should open");
        let capture = device
            .start_capture(logic_configuration(path.as_path(), 4_097))
            .expect("capture should start");
        let mut events = Vec::new();
        while !capture.status().is_terminal() {
            if let Some(event) = capture
                .next_event(Duration::from_millis(100))
                .expect("event read should succeed")
            {
                events.push(event);
            }
        }
        while let Some(event) = capture
            .next_event(Duration::ZERO)
            .expect("event drain should succeed")
        {
            events.push(event);
        }

        let completed = capture.wait().expect("capture should complete");

        assert_eq!(completed.schema, CAPTURE_RESULT_SCHEMA);
        assert_eq!(completed.summary.status, CaptureStatus::Completed);
        assert_eq!(completed.summary.captured_samples, 4_097);
        assert_eq!(completed.summary.frame_count, 2);
        assert_eq!(completed.summary.requested_configuration.channels, [2, 0]);
        assert_eq!(completed.summary.effective_configuration.channels, [0, 2]);
        assert_eq!(completed.recording_path(), path.as_path());
        assert_eq!(
            recording_completion(path.as_path()),
            CompletionStatus::Complete
        );

        assert!(events
            .windows(2)
            .all(|pair| pair[1].sequence == pair[0].sequence + 1));
        let statuses = events
            .iter()
            .filter_map(|event| match event.event {
                AutomationEventKind::Status { status } => Some(status),
                AutomationEventKind::Progress { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            statuses,
            [
                CaptureStatus::Preparing,
                CaptureStatus::Armed,
                CaptureStatus::Capturing,
                CaptureStatus::Finalizing,
                CaptureStatus::Completed,
            ]
        );

        let file = File::open(path.as_path()).expect("recording should exist");
        let mut reader =
            RecordingReader::new(BufReader::new(file)).expect("recording header should be valid");
        let mut recorded_samples = 0_u64;
        while let Some(event) = reader.read_event().expect("recording should be readable") {
            if let CoreCaptureEvent::Frame(frame) = event {
                recorded_samples += frame.payload.sample_count() as u64;
                if let wavecraft_core::CaptureBatch::Logic(batch) = frame.payload {
                    assert!(batch.samples.iter().all(|sample| sample & !0b101 == 0));
                }
            }
        }
        assert_eq!(recorded_samples, 4_097);
    }

    #[test]
    fn finite_simulated_scope_capture_uses_the_shared_contract() {
        let path = TestRecordingPath::new("scope-complete");
        let manager = simulator_manager();
        let device = manager
            .open_device(DeviceSelector::connection_id("sim:scope-0"))
            .expect("scope simulator should open");
        let completed = device
            .start_capture(scope_configuration(path.as_path(), 4_097))
            .expect("scope capture should start")
            .wait()
            .expect("scope capture should complete");

        assert_eq!(completed.summary.status, CaptureStatus::Completed);
        assert_eq!(completed.summary.captured_samples, 4_097);
        assert_eq!(completed.summary.frame_count, 2);
        assert!(completed.summary.effective_configuration.scope.is_some());
        assert_eq!(
            recording_completion(path.as_path()),
            CompletionStatus::Complete
        );

        let file = File::open(path.as_path()).expect("recording should exist");
        let mut reader =
            RecordingReader::new(BufReader::new(file)).expect("recording header should be valid");
        let mut recorded_samples = 0_u64;
        while let Some(event) = reader.read_event().expect("recording should be readable") {
            if let CoreCaptureEvent::Frame(frame) = event {
                recorded_samples += frame.payload.sample_count() as u64;
                if let wavecraft_core::CaptureBatch::Scope(batch) = frame.payload {
                    assert_eq!(batch.channels.len(), 1);
                }
            }
        }
        assert_eq!(recorded_samples, 4_097);
    }

    #[test]
    fn capture_rejects_unsupported_configuration_before_creating_output() {
        let path = TestRecordingPath::new("invalid-rate");
        let manager = simulator_manager();
        let device = manager
            .open_device(DeviceSelector::connection_id("sim:logic-0"))
            .expect("simulator should open");
        let configuration =
            CaptureConfiguration::finite_logic(123_456, 100, vec![0], path.as_path());

        let error = device
            .start_capture(configuration)
            .expect_err("unsupported sample rate should fail");

        assert_eq!(error.code, AutomationErrorCode::UnsupportedConfiguration);
        assert!(!path.as_path().exists());
    }

    #[test]
    fn capture_never_overwrites_an_existing_recording() {
        let path = TestRecordingPath::new("existing");
        std::fs::write(path.as_path(), b"keep me").expect("fixture should be created");
        let manager = simulator_manager();
        let device = manager
            .open_device(DeviceSelector::connection_id("sim:logic-0"))
            .expect("simulator should open");

        let error = device
            .start_capture(logic_configuration(path.as_path(), 100))
            .expect_err("existing output should fail");

        assert_eq!(error.code, AutomationErrorCode::OutputExists);
        assert_eq!(
            std::fs::read(path.as_path()).expect("fixture should remain readable"),
            b"keep me"
        );
    }

    #[test]
    fn cancelled_capture_writes_cancelled_footer() {
        let path = TestRecordingPath::new("cancelled");
        let manager = simulator_manager();
        let device = manager
            .open_device(DeviceSelector::connection_id("sim:logic-0"))
            .expect("simulator should open");
        let capture = device
            .start_capture(logic_configuration(path.as_path(), 1_000_000))
            .expect("capture should start");

        capture.request_stop().expect("stop request should succeed");
        let error = capture.wait().expect_err("capture should be cancelled");

        assert_eq!(error.code, AutomationErrorCode::CaptureCancelled);
        assert_eq!(
            recording_completion(path.as_path()),
            CompletionStatus::Cancelled
        );
    }

    #[test]
    fn timed_out_capture_writes_cancelled_footer() {
        let path = TestRecordingPath::new("timed-out");
        let manager = simulator_manager();
        let device = manager
            .open_device(DeviceSelector::connection_id("sim:logic-0"))
            .expect("simulator should open");
        let configuration =
            logic_configuration(path.as_path(), 100_000).with_timeout(Duration::from_millis(1));
        let capture = device
            .start_capture(configuration)
            .expect("capture should start");

        let error = capture.wait().expect_err("capture should time out");

        assert_eq!(error.code, AutomationErrorCode::CaptureTimedOut);
        assert_eq!(
            recording_completion(path.as_path()),
            CompletionStatus::Cancelled
        );
    }

    #[test]
    fn source_failure_writes_failed_footer_and_structured_error() {
        struct FailingSource {
            descriptor: DeviceDescriptor,
        }

        impl CaptureSource for FailingSource {
            fn descriptor(&self) -> DeviceDescriptor {
                self.descriptor.clone()
            }

            fn run(
                self: Box<Self>,
                context: wavecraft_core::CaptureContext,
            ) -> Result<(), wavecraft_core::CaptureError> {
                context.synchronize_start()?;
                context.emit(
                    0,
                    context.elapsed(),
                    wavecraft_core::CaptureBatch::Logic(wavecraft_core::LogicBatch {
                        sample_rate_hz: 100_000,
                        channel_count: 8,
                        samples: vec![0; 16],
                    }),
                )?;
                Err(wavecraft_core::CaptureError::new(
                    "injected simulator failure",
                ))
            }
        }

        let path = TestRecordingPath::new("failed");
        let manager = simulator_manager();
        let device = manager
            .open_device(DeviceSelector::connection_id("sim:logic-0"))
            .expect("simulator should open");
        let requested = logic_configuration(path.as_path(), 100);
        let mut effective = requested.clone();
        effective.channels.sort_unstable();
        let source = FailingSource {
            descriptor: DeviceDescriptor {
                id: DeviceId::new("sim:logic-0"),
                name: "Failing Simulator".to_owned(),
                kind: CoreInstrumentKind::LogicAnalyzer,
            },
        };
        let capture = device
            .start_capture_with_factory(
                requested,
                effective,
                Box::new(move || Ok(Box::new(source))),
            )
            .expect("capture should start");

        let error = capture
            .wait()
            .expect_err("source failure should fail capture");

        assert_eq!(error.code, AutomationErrorCode::CaptureFailed);
        assert!(error.message.contains("injected simulator failure"));
        assert_eq!(
            recording_completion(path.as_path()),
            CompletionStatus::Failed
        );
    }

    #[test]
    fn capture_does_not_block_when_progress_events_are_not_consumed() {
        let path = TestRecordingPath::new("bounded-events");
        let manager = simulator_manager();
        let device = manager
            .open_device(DeviceSelector::connection_id("sim:logic-0"))
            .expect("simulator should open");
        let configuration =
            CaptureConfiguration::finite_logic(10_000_000, 400_000, vec![0], path.as_path())
                .with_timeout(Duration::from_secs(1));
        let capture = device
            .start_capture(configuration)
            .expect("capture should start");

        let completed = capture
            .wait()
            .expect("full bounded event queue must not block capture");

        assert_eq!(completed.summary.captured_samples, 400_000);
    }

    #[test]
    fn running_capture_retains_device_lease_after_device_handle_is_dropped() {
        let path = TestRecordingPath::new("lease");
        let manager = simulator_manager();
        let selector = DeviceSelector::connection_id("sim:logic-0");
        let device = manager
            .open_device(selector.clone())
            .expect("simulator should open");
        let capture = device
            .start_capture(logic_configuration(path.as_path(), 1_000_000))
            .expect("capture should start");
        drop(device);

        let error = manager
            .open_device(selector.clone())
            .expect_err("running capture should retain the lease");
        assert_eq!(error.code, AutomationErrorCode::DeviceBusy);

        capture.request_stop().expect("stop request should succeed");
        let error = capture.wait().expect_err("capture should be cancelled");
        assert_eq!(error.code, AutomationErrorCode::CaptureCancelled);
        manager
            .open_device(selector)
            .expect("completed capture should release its retained lease");
    }
}
