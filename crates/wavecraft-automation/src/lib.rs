//! Shared device discovery, selection, and ownership for WaveCraft clients.
//!
//! This crate deliberately contains no device transport implementation. USB
//! behavior remains in `wavecraft-usb`, while deterministic simulated devices
//! remain in `wavecraft-sim`.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, Mutex, OnceLock};
use wavecraft_core::InstrumentKind as CoreInstrumentKind;
use wavecraft_sim::{SimulatedDeviceProfile, SIMULATED_DEVICE_PROFILES};
use wavecraft_usb::{
    discover_supported_devices, probe_device, DeviceModel, DiscoveredDevice, LinkStatus,
    UsbSpeedClass, DSCOPE_SAMPLE_RATES_HZ, DSLOGIC_SAMPLE_RATES_HZ,
};

pub const DEVICE_LIST_SCHEMA: &str = "wavecraft.devices/v1";
pub const DEVICE_PROBE_SCHEMA: &str = "wavecraft.device-probe/v1";
pub const ERROR_SCHEMA: &str = "wavecraft.error/v1";
const MAX_ERROR_CANDIDATES: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentKind {
    LogicAnalyzer,
    Oscilloscope,
}

impl Display for InstrumentKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LogicAnalyzer => formatter.write_str("logic_analyzer"),
            Self::Oscilloscope => formatter.write_str("oscilloscope"),
        }
    }
}

impl From<CoreInstrumentKind> for InstrumentKind {
    fn from(value: CoreInstrumentKind) -> Self {
        match value {
            CoreInstrumentKind::LogicAnalyzer => Self::LogicAnalyzer,
            CoreInstrumentKind::Oscilloscope => Self::Oscilloscope,
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
}

#[derive(Clone)]
pub struct AutomationManager {
    catalog: Arc<DeviceCatalog>,
    leases: Arc<Mutex<HashSet<String>>>,
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
        let records = self.catalog.discover("device.open")?;
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
            leases: Arc::clone(&self.leases),
            released: false,
        })
    }
}

pub struct DeviceHandle {
    descriptor: AutomationDevice,
    catalog: Arc<DeviceCatalog>,
    leases: Arc<Mutex<HashSet<String>>>,
    released: bool,
}

impl Debug for DeviceHandle {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceHandle")
            .field("descriptor", &self.descriptor)
            .field("released", &self.released)
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

    pub fn close(mut self) -> Result<(), AutomationError> {
        self.release()
    }

    fn release(&mut self) -> Result<(), AutomationError> {
        if self.released {
            return Ok(());
        }
        let mut leases = self.leases.lock().map_err(|_| {
            AutomationError::new(
                AutomationErrorCode::ServiceUnavailable,
                "device lease registry is unavailable",
                "device.close",
                true,
            )
            .for_device(&self.descriptor.connection_id)
        })?;
        leases.remove(&self.descriptor.connection_id);
        self.released = true;
        Ok(())
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        let _ = self.release();
    }
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
    use std::sync::MutexGuard;
    use wavecraft_usb::{DeviceLocation, DSCOPE_U3P100};

    fn usb_device(path: &str) -> DiscoveredDevice {
        DiscoveredDevice {
            profile: &DSCOPE_U3P100,
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
}
