//! USB discovery and profiles for the initial supported hardware.
//!
//! Discovery uses libusb and never opens, claims, or modifies a device. Device
//! handles and direct USB transport belong to the acquisition layer.

use rusb::UsbContext;
use std::error::Error;
use std::fmt::{Display, Formatter};
use wavecraft_core::InstrumentKind;

mod dscope;
mod dslogic;
mod prepare;
mod probe;
mod transport;

pub use dscope::{
    apply_dscope_configuration, auto_calibrate_dscope, auto_calibrate_dscope_with_progress,
    read_dscope_calibration, write_dscope_calibration, DsCopeBandwidthLimit,
    DsCopeCalibrationFrame, DsCopeCalibrationValues, DsCopeCaptureConfig, DsCopeCaptureSource,
    DsCopeChannelConfig, DsCopeCoupling, DsCopeOperationMode, DsCopeRuntimeCalibrationControl,
    DsCopeRuntimeTriggerControl, DsCopeTriggerConfig, DsCopeTriggerSlope, DsCopeTriggerSource,
    DSCOPE_FRAME_SEQUENCE_STRIDE, DSCOPE_SAMPLE_COUNTS, DSCOPE_SAMPLE_RATES_HZ,
    DSCOPE_TRIGGER_HOLDOFF_MAX_NS, DSCOPE_TRIGGER_LEVEL_CENTER, DSCOPE_TRIGGER_LEVEL_MAX,
    DSCOPE_TRIGGER_LEVEL_MIN, DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX,
    DSCOPE_TRIGGER_POSITION_CENTER_PERCENT, DSCOPE_TRIGGER_POSITION_MAX_PERCENT,
    DSCOPE_TRIGGER_POSITION_MIN_PERCENT, DSCOPE_VERTICAL_OFFSET_CENTER, DSCOPE_VERTICAL_OFFSET_MAX,
    DSCOPE_VERTICAL_OFFSET_MIN, DSCOPE_VOLTS_PER_DIV_MV,
};
pub use dslogic::{
    DsLogicCaptureConfig, DsLogicCaptureSource, DsLogicInputFilter, DsLogicOperationMode,
    DsLogicStopOption, DSLOGIC_MAX_BUFFER_SAMPLE_COUNT, DSLOGIC_MAX_SAMPLE_COUNT,
    DSLOGIC_MAX_STREAM_SAMPLE_COUNT, DSLOGIC_SAMPLE_ALIGNMENT, DSLOGIC_SAMPLE_RATES_HZ,
};
pub use prepare::{load_device_fpga, prepare_device, FpgaLoad, PreparedDevice};
pub use probe::{probe_device, DeviceProbe, UsbEndpointInfo};
pub use transport::{
    BulkReadOutcome, FirmwareVersion, HardwareStatus, HdlVersion, UsbDeviceSession,
};

pub const DREAMSOURCELAB_VENDOR_ID: u16 = 0x2a0e;
pub const USB_INTERFACE: u8 = 0;
pub const BULK_OUT_ENDPOINT: u8 = 0x02;
pub const BULK_IN_ENDPOINT: u8 = 0x86;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceModel {
    DsLogic,
    DsLogicPro,
    DsLogicPlus,
    DsLogicBasic,
    DsLogicU2Basic,
    DsLogicU3Pro16,
    DsLogicU3Pro32,
    DsLogicU2Pro16,
    DsLogicPlusPgl12,
    DsLogicU2BasicPgl12,
    DsLogicPlusPgl12Rev2,
    DsLogicU2BasicPgl12Rev2,
    DsCope,
    DsCope20,
    DsCopeB20,
    DsCopeC20,
    DsCopeC20P,
    DsCopeC20B,
    DsCopeU2B20,
    DsCopeU2P20,
    DsCopeU2B100,
    DsCopeU3P100,
}

impl DeviceModel {
    pub const fn instrument_kind(self) -> InstrumentKind {
        match self {
            Self::DsLogic
            | Self::DsLogicPro
            | Self::DsLogicPlus
            | Self::DsLogicBasic
            | Self::DsLogicU2Basic
            | Self::DsLogicU3Pro16
            | Self::DsLogicU3Pro32
            | Self::DsLogicU2Pro16
            | Self::DsLogicPlusPgl12
            | Self::DsLogicU2BasicPgl12
            | Self::DsLogicPlusPgl12Rev2
            | Self::DsLogicU2BasicPgl12Rev2 => InstrumentKind::LogicAnalyzer,
            Self::DsCope
            | Self::DsCope20
            | Self::DsCopeB20
            | Self::DsCopeC20
            | Self::DsCopeC20P
            | Self::DsCopeC20B
            | Self::DsCopeU2B20
            | Self::DsCopeU2P20
            | Self::DsCopeU2B100
            | Self::DsCopeU3P100 => InstrumentKind::Oscilloscope,
        }
    }

    pub const fn is_logic_analyzer(self) -> bool {
        matches!(self.instrument_kind(), InstrumentKind::LogicAnalyzer)
    }

    pub const fn is_oscilloscope(self) -> bool {
        matches!(self.instrument_kind(), InstrumentKind::Oscilloscope)
    }

    pub(crate) const fn requires_security_handshake(self) -> bool {
        matches!(
            self,
            Self::DsLogicU2Pro16
                | Self::DsLogicPlusPgl12
                | Self::DsLogicU2BasicPgl12
                | Self::DsLogicPlusPgl12Rev2
                | Self::DsLogicU2BasicPgl12Rev2
        )
    }

    pub const fn supports_capture(self) -> bool {
        self.is_logic_analyzer() || matches!(self, Self::DsCopeU3P100)
    }

    pub(crate) const fn logic_uses_adf4360(self) -> bool {
        matches!(
            self,
            Self::DsLogicU3Pro16 | Self::DsLogicU3Pro32 | Self::DsLogicU2Pro16
        )
    }

    pub(crate) const fn logic_uses_usb3_protocol(self) -> bool {
        matches!(self, Self::DsLogicU3Pro16 | Self::DsLogicU3Pro32)
    }

    pub(crate) const fn logic_uses_2v5_threshold(self) -> bool {
        matches!(
            self,
            Self::DsLogicPlusPgl12
                | Self::DsLogicU2BasicPgl12
                | Self::DsLogicPlusPgl12Rev2
                | Self::DsLogicU2BasicPgl12Rev2
        )
    }

    pub(crate) const fn logic_has_buffer(self) -> bool {
        !matches!(self, Self::DsLogicBasic)
    }

    pub(crate) const fn logic_hardware_sample_rate_hz(self) -> u64 {
        if self.logic_uses_adf4360() || matches!(self, Self::DsLogicU3Pro32) {
            500_000_000
        } else {
            100_000_000
        }
    }

    pub(crate) const fn logic_predivider(self) -> u32 {
        if self.logic_uses_adf4360() || matches!(self, Self::DsLogicU3Pro32) {
            5
        } else {
            1
        }
    }

    pub(crate) const fn logic_trigger_header_bytes(self) -> usize {
        if self.logic_uses_usb3_protocol() {
            1_024
        } else {
            512
        }
    }

    pub(crate) const fn logic_buffer_bits(self) -> u64 {
        match self {
            Self::DsLogic | Self::DsLogicPro | Self::DsLogicPlus => 256 * 1_048_576,
            Self::DsLogicBasic => 256 * 1_024,
            Self::DsLogicU2Basic | Self::DsLogicU2BasicPgl12 | Self::DsLogicU2BasicPgl12Rev2 => {
                64 * 1_048_576
            }
            Self::DsLogicU3Pro16 | Self::DsLogicU3Pro32 => 2 * 1_073_741_824,
            Self::DsLogicU2Pro16 => 4 * 1_073_741_824,
            Self::DsLogicPlusPgl12 | Self::DsLogicPlusPgl12Rev2 => 256 * 1_048_576,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum UsbSpeedClass {
    Unknown,
    LowSpeed,
    FullSpeed,
    HighSpeed,
    SuperSpeed,
}

impl UsbSpeedClass {
    pub fn description(self) -> &'static str {
        match self {
            Self::Unknown => "unknown USB speed",
            Self::LowSpeed => "USB LowSpeed (1.5 Mb/s)",
            Self::FullSpeed => "USB FullSpeed (12 Mb/s)",
            Self::HighSpeed => "USB 2 HighSpeed (480 Mb/s)",
            Self::SuperSpeed => "USB 3 SuperSpeed (5 Gb/s or faster)",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkStatus {
    Optimal,
    ReducedPerformance,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceProfile {
    pub model: DeviceModel,
    pub display_name: &'static str,
    pub instrument_kind: InstrumentKind,
    pub vendor_id: u16,
    pub product_id: u16,
    pub firmware_name: &'static str,
    pub fpga_bitstream_name: &'static str,
    pub channel_count: u8,
    pub minimum_speed: UsbSpeedClass,
    pub recommended_speed: UsbSpeedClass,
}

macro_rules! high_speed_profile {
    ($constant:ident, $model:ident, $name:literal, $pid:literal, $firmware:literal, $bitstream:literal, $kind:ident, $channels:literal) => {
        pub const $constant: DeviceProfile = DeviceProfile {
            model: DeviceModel::$model,
            display_name: $name,
            instrument_kind: InstrumentKind::$kind,
            vendor_id: DREAMSOURCELAB_VENDOR_ID,
            product_id: $pid,
            firmware_name: $firmware,
            fpga_bitstream_name: $bitstream,
            channel_count: $channels,
            minimum_speed: UsbSpeedClass::HighSpeed,
            recommended_speed: UsbSpeedClass::HighSpeed,
        };
    };
}

high_speed_profile!(
    DSLOGIC,
    DsLogic,
    "DSLogic",
    0x0001,
    "DSLogic.fw",
    "DSLogic33.bin",
    LogicAnalyzer,
    16
);
high_speed_profile!(
    DSLOGIC_PRO,
    DsLogicPro,
    "DSLogic Pro",
    0x0003,
    "DSLogicPro.fw",
    "DSLogicPro.bin",
    LogicAnalyzer,
    16
);
high_speed_profile!(
    DSLOGIC_PLUS,
    DsLogicPlus,
    "DSLogic Plus",
    0x0020,
    "DSLogicPlus.fw",
    "DSLogicPlus.bin",
    LogicAnalyzer,
    16
);
high_speed_profile!(
    DSLOGIC_BASIC,
    DsLogicBasic,
    "DSLogic Basic",
    0x0021,
    "DSLogicBasic.fw",
    "DSLogicBasic.bin",
    LogicAnalyzer,
    16
);
high_speed_profile!(
    DSLOGIC_U2BASIC,
    DsLogicU2Basic,
    "DSLogic U2Basic",
    0x0029,
    "DSLogicU2Basic.fw",
    "DSLogicU2Basic.bin",
    LogicAnalyzer,
    16
);

pub const DSLOGIC_U3PRO16: DeviceProfile = DeviceProfile {
    model: DeviceModel::DsLogicU3Pro16,
    display_name: "DSLogic U3Pro16",
    instrument_kind: InstrumentKind::LogicAnalyzer,
    vendor_id: DREAMSOURCELAB_VENDOR_ID,
    product_id: 0x002a,
    firmware_name: "DSLogicU3Pro16.fw",
    fpga_bitstream_name: "DSLogicU3Pro16.bin",
    channel_count: 16,
    minimum_speed: UsbSpeedClass::HighSpeed,
    recommended_speed: UsbSpeedClass::SuperSpeed,
};

pub const DSLOGIC_U2PRO16: DeviceProfile = DeviceProfile {
    model: DeviceModel::DsLogicU2Pro16,
    display_name: "DSLogic U2Pro16",
    instrument_kind: InstrumentKind::LogicAnalyzer,
    vendor_id: DREAMSOURCELAB_VENDOR_ID,
    product_id: 0x002d,
    firmware_name: "DSLogicU2Pro16.fw",
    fpga_bitstream_name: "DSLogicU2Pro16.bin",
    channel_count: 16,
    minimum_speed: UsbSpeedClass::HighSpeed,
    recommended_speed: UsbSpeedClass::HighSpeed,
};

pub const DSLOGIC_U3PRO32: DeviceProfile = DeviceProfile {
    model: DeviceModel::DsLogicU3Pro32,
    display_name: "DSLogic U3Pro32",
    instrument_kind: InstrumentKind::LogicAnalyzer,
    vendor_id: DREAMSOURCELAB_VENDOR_ID,
    product_id: 0x002c,
    firmware_name: "DSLogicU3Pro32.fw",
    fpga_bitstream_name: "DSLogicU3Pro32.bin",
    channel_count: 32,
    minimum_speed: UsbSpeedClass::HighSpeed,
    recommended_speed: UsbSpeedClass::SuperSpeed,
};
high_speed_profile!(
    DSLOGIC_PLUS_PGL12,
    DsLogicPlusPgl12,
    "DSLogic Plus (PGL12)",
    0x0030,
    "DSLogicPlus.fw",
    "DSLogicPlus-pgl12.bin",
    LogicAnalyzer,
    16
);
high_speed_profile!(
    DSLOGIC_U2BASIC_PGL12,
    DsLogicU2BasicPgl12,
    "DSLogic U2Basic (PGL12)",
    0x0031,
    "DSLogicU2Basic.fw",
    "DSLogicU2Basic-pgl12.bin",
    LogicAnalyzer,
    16
);
high_speed_profile!(
    DSLOGIC_PLUS_PGL12_REV2,
    DsLogicPlusPgl12Rev2,
    "DSLogic Plus (PGL12 rev. 2)",
    0x0034,
    "DSLogicPlus-pgl12-2.fw",
    "DSLogicPlus-pgl12-2.bin",
    LogicAnalyzer,
    16
);
high_speed_profile!(
    DSLOGIC_U2BASIC_PGL12_REV2,
    DsLogicU2BasicPgl12Rev2,
    "DSLogic U2Basic (PGL12 rev. 2)",
    0x0035,
    "DSLogicU2Basic-pgl12-2.fw",
    "DSLogicU2Basic-pgl12-2.bin",
    LogicAnalyzer,
    16
);

high_speed_profile!(
    DSCOPE,
    DsCope,
    "DSCope",
    0x0002,
    "DSCope.fw",
    "DSCope.bin",
    Oscilloscope,
    2
);
high_speed_profile!(
    DSCOPE20,
    DsCope20,
    "DSCope20",
    0x0004,
    "DSCope20.fw",
    "DSCope20.bin",
    Oscilloscope,
    2
);
high_speed_profile!(
    DSCOPE_B20,
    DsCopeB20,
    "DSCope B20",
    0x0022,
    "DSCopeB20.fw",
    "DSCope20.bin",
    Oscilloscope,
    2
);
high_speed_profile!(
    DSCOPE_C20,
    DsCopeC20,
    "DSCope C20",
    0x0023,
    "DSCopeC20.fw",
    "DSCopeC20P.bin",
    Oscilloscope,
    2
);
high_speed_profile!(
    DSCOPE_C20P,
    DsCopeC20P,
    "DSCope C20P",
    0x0024,
    "DSCopeC20P.fw",
    "DSCopeC20P.bin",
    Oscilloscope,
    2
);
high_speed_profile!(
    DSCOPE_C20B,
    DsCopeC20B,
    "DSCope C20 (basic)",
    0x0025,
    "DSCopeC20B.fw",
    "DSCopeC20B.bin",
    Oscilloscope,
    2
);
high_speed_profile!(
    DSCOPE_U2B20,
    DsCopeU2B20,
    "DSCope U2B20",
    0x0026,
    "DSCopeU2B20.fw",
    "DSCopeU2B20.bin",
    Oscilloscope,
    2
);
high_speed_profile!(
    DSCOPE_U2P20,
    DsCopeU2P20,
    "DSCope U2P20",
    0x0027,
    "DSCopeU2P20.fw",
    "DSCopeU2P20.bin",
    Oscilloscope,
    2
);
high_speed_profile!(
    DSCOPE_U2B100,
    DsCopeU2B100,
    "DSCope U2B100",
    0x0028,
    "DSCopeU2B100.fw",
    "DSCopeU2B100.bin",
    Oscilloscope,
    2
);

pub const DSCOPE_U3P100: DeviceProfile = DeviceProfile {
    model: DeviceModel::DsCopeU3P100,
    display_name: "DSCope U3P100",
    instrument_kind: InstrumentKind::Oscilloscope,
    vendor_id: DREAMSOURCELAB_VENDOR_ID,
    product_id: 0x002b,
    firmware_name: "DSCopeU3P100.fw",
    fpga_bitstream_name: "DSCopeU3P100.bin",
    channel_count: 2,
    minimum_speed: UsbSpeedClass::HighSpeed,
    recommended_speed: UsbSpeedClass::SuperSpeed,
};

pub const SUPPORTED_PROFILES: &[DeviceProfile] = &[
    DSLOGIC,
    DSLOGIC_PRO,
    DSLOGIC_PLUS,
    DSLOGIC_BASIC,
    DSLOGIC_U2BASIC,
    DSLOGIC_U3PRO16,
    DSLOGIC_U3PRO32,
    DSLOGIC_U2PRO16,
    DSLOGIC_PLUS_PGL12,
    DSLOGIC_U2BASIC_PGL12,
    DSLOGIC_PLUS_PGL12_REV2,
    DSLOGIC_U2BASIC_PGL12_REV2,
    DSCOPE,
    DSCOPE20,
    DSCOPE_B20,
    DSCOPE_C20,
    DSCOPE_C20P,
    DSCOPE_C20B,
    DSCOPE_U2B20,
    DSCOPE_U2P20,
    DSCOPE_U2B100,
    DSCOPE_U3P100,
];

/// Runtime resources shipped by the original DSView application. Some profile
/// firmware names are not present in the upstream resource directory because
/// those models store firmware in on-device EEPROM.
pub const REQUIRED_RESOURCE_FILES: &[&str] = &[
    "DSCope.bin",
    "DSCope.fw",
    "DSCope20.bin",
    "DSCope20.fw",
    "DSCopeC20B.bin",
    "DSCopeC20P.bin",
    "DSCopeU2B100.bin",
    "DSCopeU2B20.bin",
    "DSCopeU2P20.bin",
    "DSCopeU3P100.bin",
    "DSLogic.fw",
    "DSLogic33.bin",
    "DSLogic50.bin",
    "DSLogicBasic.bin",
    "DSLogicPlus-pgl12-2.bin",
    "DSLogicPlus-pgl12.bin",
    "DSLogicPlus.bin",
    "DSLogicPro.bin",
    "DSLogicPro.fw",
    "DSLogicU2Basic-pgl12-2.bin",
    "DSLogicU2Basic-pgl12.bin",
    "DSLogicU2Basic.bin",
    "DSLogicU2Pro16.bin",
    "DSLogicU3Pro16.bin",
    "DSLogicU3Pro32.bin",
];

pub fn profile_for(vendor_id: u16, product_id: u16) -> Option<&'static DeviceProfile> {
    SUPPORTED_PROFILES
        .iter()
        .find(|profile| profile.vendor_id == vendor_id && profile.product_id == product_id)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceLocation {
    /// Libusb bus and physical port topology, or a bus/address fallback.
    pub platform_path: String,
}

impl DeviceLocation {
    pub fn stable_id(&self) -> String {
        let normalized = self
            .platform_path
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>();
        format!("usb-{}", normalized.trim_matches('-'))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredDevice {
    pub profile: &'static DeviceProfile,
    pub location: DeviceLocation,
    pub speed: UsbSpeedClass,
}

impl DiscoveredDevice {
    pub fn link_status(&self) -> LinkStatus {
        if self.speed >= self.profile.recommended_speed {
            LinkStatus::Optimal
        } else if self.speed >= self.profile.minimum_speed {
            LinkStatus::ReducedPerformance
        } else {
            LinkStatus::Unsupported
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryReport {
    pub devices: Vec<DiscoveredDevice>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbError {
    message: String,
}

impl UsbError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for UsbError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl Error for UsbError {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RawUsbDevice {
    vendor_id: u16,
    product_id: u16,
    speed: UsbSpeedClass,
    platform_path: String,
}

/// Enumerate supported models without opening or claiming them.
pub fn discover_supported_devices() -> Result<DiscoveryReport, UsbError> {
    let mut devices = discover_raw_devices()?
        .into_iter()
        .filter_map(|raw| {
            let profile = profile_for(raw.vendor_id, raw.product_id)?;
            Some(DiscoveredDevice {
                profile,
                location: DeviceLocation {
                    platform_path: raw.platform_path,
                },
                speed: raw.speed,
            })
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| {
        left.location
            .platform_path
            .cmp(&right.location.platform_path)
    });
    Ok(DiscoveryReport { devices })
}

fn discover_raw_devices() -> Result<Vec<RawUsbDevice>, UsbError> {
    let context = rusb::Context::new()
        .map_err(|error| UsbError::new(format!("failed to initialize USB: {error}")))?;
    let usb_devices = context
        .devices()
        .map_err(|error| UsbError::new(format!("failed to enumerate USB devices: {error}")))?;
    let mut devices = Vec::new();
    for device in usb_devices.iter() {
        let Ok(descriptor) = device.device_descriptor() else {
            continue;
        };
        if profile_for(descriptor.vendor_id(), descriptor.product_id()).is_none() {
            continue;
        }

        let ports = device.port_numbers().unwrap_or_default();
        devices.push(RawUsbDevice {
            vendor_id: descriptor.vendor_id(),
            product_id: descriptor.product_id(),
            speed: speed_class(device.speed()),
            platform_path: topology_path(device.bus_number(), device.address(), &ports),
        });
    }
    Ok(devices)
}

fn speed_class(speed: rusb::Speed) -> UsbSpeedClass {
    match speed {
        rusb::Speed::Low => UsbSpeedClass::LowSpeed,
        rusb::Speed::Full => UsbSpeedClass::FullSpeed,
        rusb::Speed::High => UsbSpeedClass::HighSpeed,
        rusb::Speed::Super | rusb::Speed::SuperPlus => UsbSpeedClass::SuperSpeed,
        rusb::Speed::Unknown => UsbSpeedClass::Unknown,
        _ => UsbSpeedClass::Unknown,
    }
}

fn topology_path(bus: u8, address: u8, ports: &[u8]) -> String {
    if ports.is_empty() {
        return format!("bus-{bus:03}-address-{address:03}");
    }

    let port_path = ports
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(".");
    format!("bus-{bus:03}-ports-{port_path}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    #[test]
    fn upstream_profile_matrix_has_unique_usb_ids_and_resources() {
        assert_eq!(SUPPORTED_PROFILES.len(), 22);
        let mut ids = HashSet::new();
        for profile in SUPPORTED_PROFILES {
            assert!(ids.insert((profile.vendor_id, profile.product_id)));
            assert_eq!(
                profile_for(profile.vendor_id, profile.product_id),
                Some(profile)
            );
            assert!(REQUIRED_RESOURCE_FILES.contains(&profile.fpga_bitstream_name));
        }
    }

    #[test]
    fn complete_upstream_resource_set_is_bundled() {
        let resource_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/fpga");
        for file in REQUIRED_RESOURCE_FILES {
            assert!(
                resource_directory.join(file).is_file(),
                "missing original DSView resource {file}"
            );
        }
    }

    #[test]
    fn matches_requested_models_at_usb_two_and_three_speeds() {
        assert_eq!(
            profile_for(DREAMSOURCELAB_VENDOR_ID, 0x002a),
            Some(&DSLOGIC_U3PRO16)
        );
        assert_eq!(
            profile_for(DREAMSOURCELAB_VENDOR_ID, 0x002b),
            Some(&DSCOPE_U3P100)
        );
        assert_eq!(
            profile_for(DREAMSOURCELAB_VENDOR_ID, 0x002d),
            Some(&DSLOGIC_U2PRO16)
        );
    }

    #[test]
    fn models_report_their_instrument_family() {
        assert!(DeviceModel::DsLogicU3Pro16.is_logic_analyzer());
        assert!(DeviceModel::DsLogicU2Pro16.is_logic_analyzer());
        assert!(DeviceModel::DsCopeU3P100.is_oscilloscope());
        assert!(!DeviceModel::DsCopeU3P100.is_logic_analyzer());
    }

    #[test]
    fn rejects_unknown_devices() {
        assert_eq!(profile_for(DREAMSOURCELAB_VENDOR_ID, 0xffff), None);
    }

    #[test]
    fn stable_id_normalizes_platform_path() {
        let location = DeviceLocation {
            platform_path: "0x02100000 / 1".to_owned(),
        };

        assert_eq!(location.stable_id(), "usb-0x02100000---1");
    }

    #[test]
    fn builds_stable_topology_path_when_ports_are_available() {
        assert_eq!(topology_path(3, 9, &[1, 4, 2]), "bus-003-ports-1.4.2");
        assert_eq!(topology_path(3, 9, &[]), "bus-003-address-009");
    }

    #[test]
    fn maps_libusb_link_speeds() {
        assert_eq!(
            speed_class(rusb::Speed::SuperPlus),
            UsbSpeedClass::SuperSpeed
        );
        assert_eq!(speed_class(rusb::Speed::Super), UsbSpeedClass::SuperSpeed);
        assert_eq!(speed_class(rusb::Speed::High), UsbSpeedClass::HighSpeed);
        assert_eq!(speed_class(rusb::Speed::Full), UsbSpeedClass::FullSpeed);
        assert_eq!(speed_class(rusb::Speed::Low), UsbSpeedClass::LowSpeed);
        assert_eq!(speed_class(rusb::Speed::Unknown), UsbSpeedClass::Unknown);
    }

    #[test]
    fn warns_when_u3_device_uses_a_usb_two_link() {
        let device = DiscoveredDevice {
            profile: &DSLOGIC_U3PRO16,
            location: DeviceLocation {
                platform_path: "test".to_owned(),
            },
            speed: UsbSpeedClass::HighSpeed,
        };

        assert_eq!(device.link_status(), LinkStatus::ReducedPerformance);
    }

    #[test]
    fn u2pro16_is_optimal_on_its_usb_two_link() {
        let device = DiscoveredDevice {
            profile: &DSLOGIC_U2PRO16,
            location: DeviceLocation {
                platform_path: "test".to_owned(),
            },
            speed: UsbSpeedClass::HighSpeed,
        };

        assert_eq!(device.link_status(), LinkStatus::Optimal);
    }

    #[test]
    fn rejects_links_slower_than_usb_two_high_speed() {
        let device = DiscoveredDevice {
            profile: &DSCOPE_U3P100,
            location: DeviceLocation {
                platform_path: "test".to_owned(),
            },
            speed: UsbSpeedClass::FullSpeed,
        };

        assert_eq!(device.link_status(), LinkStatus::Unsupported);
    }
}
