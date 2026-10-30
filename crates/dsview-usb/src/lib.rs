//! USB discovery and profiles for the initial supported hardware.
//!
//! Discovery uses libusb and never opens, claims, or modifies a device. Device
//! handles and direct USB transport belong to the acquisition layer.

use dsview_core::InstrumentKind;
use std::error::Error;
use std::fmt::{Display, Formatter};

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
    DSCOPE_SAMPLE_COUNTS, DSCOPE_SAMPLE_RATES_HZ, DSCOPE_TRIGGER_HOLDOFF_MAX_NS,
    DSCOPE_TRIGGER_LEVEL_CENTER, DSCOPE_TRIGGER_LEVEL_MAX, DSCOPE_TRIGGER_LEVEL_MIN,
    DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX, DSCOPE_TRIGGER_POSITION_CENTER_PERCENT,
    DSCOPE_TRIGGER_POSITION_MAX_PERCENT, DSCOPE_TRIGGER_POSITION_MIN_PERCENT,
    DSCOPE_VERTICAL_OFFSET_CENTER, DSCOPE_VERTICAL_OFFSET_MAX, DSCOPE_VERTICAL_OFFSET_MIN,
    DSCOPE_VOLTS_PER_DIV_MV,
};
pub use dslogic::{
    DsLogicCaptureConfig, DsLogicCaptureSource, DsLogicInputFilter, DsLogicOperationMode,
    DsLogicStopOption,
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
    DsLogicU3Pro16,
    DsCopeU3P100,
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

pub const SUPPORTED_PROFILES: &[DeviceProfile] = &[DSLOGIC_U3PRO16, DSCOPE_U3P100];

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

/// Enumerate the two supported models without opening or claiming them.
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
    let usb_devices = rusb::devices()
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
