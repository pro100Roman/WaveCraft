//! USB discovery and profiles for the initial supported hardware.
//!
//! Discovery uses operating-system inventory APIs and never opens, claims, or
//! modifies a device. Direct libusb transport belongs to the acquisition layer.

use dsview_core::InstrumentKind;
use std::error::Error;
use std::fmt::{Display, Formatter};

pub const DREAMSOURCELAB_VENDOR_ID: u16 = 0x2a0e;
pub const USB_INTERFACE: u8 = 0;
pub const BULK_OUT_ENDPOINT: u8 = 0x02;
pub const BULK_IN_ENDPOINT: u8 = 0x86;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceModel {
    DsLogicU3Pro16,
    DsCopeU3P100,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbSpeedClass {
    HighSpeed,
    SuperSpeed,
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
};

pub const SUPPORTED_PROFILES: &[DeviceProfile] = &[DSLOGIC_U3PRO16, DSCOPE_U3P100];

pub fn profile_for(
    vendor_id: u16,
    product_id: u16,
    speed: UsbSpeedClass,
) -> Option<&'static DeviceProfile> {
    if speed == UsbSpeedClass::Unsupported {
        return None;
    }
    SUPPORTED_PROFILES
        .iter()
        .find(|profile| profile.vendor_id == vendor_id && profile.product_id == product_id)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceLocation {
    /// OS-specific physical topology or sysfs identifier.
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
            let profile = profile_for(raw.vendor_id, raw.product_id, raw.speed)?;
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

#[cfg(target_os = "macos")]
fn discover_raw_devices() -> Result<Vec<RawUsbDevice>, UsbError> {
    use std::process::Command;

    let output = Command::new("system_profiler")
        .args(["SPUSBDataType", "-detailLevel", "mini"])
        .output()
        .map_err(|error| UsbError::new(format!("failed to run system_profiler: {error}")))?;
    if !output.status.success() {
        return Err(UsbError::new(format!(
            "system_profiler failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(parse_macos_usb_inventory(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

#[cfg(target_os = "linux")]
fn discover_raw_devices() -> Result<Vec<RawUsbDevice>, UsbError> {
    use std::fs;
    use std::path::Path;

    let root = Path::new("/sys/bus/usb/devices");
    let entries = fs::read_dir(root)
        .map_err(|error| UsbError::new(format!("failed to read {}: {error}", root.display())))?;
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(vendor_id) = read_hex_file(&path.join("idVendor")) else {
            continue;
        };
        let Some(product_id) = read_hex_file(&path.join("idProduct")) else {
            continue;
        };
        let speed_mbps = fs::read_to_string(path.join("speed"))
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or_default();
        devices.push(RawUsbDevice {
            vendor_id,
            product_id,
            speed: speed_from_mbps(speed_mbps),
            platform_path: entry.file_name().to_string_lossy().into_owned(),
        });
    }
    Ok(devices)
}

#[cfg(target_os = "linux")]
fn read_hex_file(path: &std::path::Path) -> Option<u16> {
    let value = std::fs::read_to_string(path).ok()?;
    u16::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn discover_raw_devices() -> Result<Vec<RawUsbDevice>, UsbError> {
    Err(UsbError::new(
        "USB inventory is not implemented for this operating system",
    ))
}

#[cfg(any(target_os = "linux", test))]
fn speed_from_mbps(speed_mbps: u32) -> UsbSpeedClass {
    if speed_mbps >= 5_000 {
        UsbSpeedClass::SuperSpeed
    } else if speed_mbps >= 480 {
        UsbSpeedClass::HighSpeed
    } else {
        UsbSpeedClass::Unsupported
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos_usb_inventory(inventory: &str) -> Vec<RawUsbDevice> {
    #[derive(Default)]
    struct PendingDevice {
        vendor_id: Option<u16>,
        product_id: Option<u16>,
        speed: Option<UsbSpeedClass>,
        location: Option<String>,
    }

    fn finish(pending: &mut PendingDevice, devices: &mut Vec<RawUsbDevice>) {
        if let (Some(vendor_id), Some(product_id)) = (pending.vendor_id, pending.product_id) {
            devices.push(RawUsbDevice {
                vendor_id,
                product_id,
                speed: pending.speed.unwrap_or(UsbSpeedClass::Unsupported),
                platform_path: pending
                    .location
                    .take()
                    .unwrap_or_else(|| format!("unknown-{vendor_id:04x}-{product_id:04x}")),
            });
        }
        *pending = PendingDevice::default();
    }

    let mut devices = Vec::new();
    let mut pending = PendingDevice::default();
    for line in inventory.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("Product ID:") {
            finish(&mut pending, &mut devices);
            pending.product_id = parse_prefixed_hex(value);
        } else if let Some(value) = line.strip_prefix("Vendor ID:") {
            pending.vendor_id = parse_prefixed_hex(value);
        } else if let Some(value) = line.strip_prefix("Speed:") {
            pending.speed = Some(if value.contains("5 Gb/s") || value.contains("10 Gb/s") {
                UsbSpeedClass::SuperSpeed
            } else if value.contains("480 Mb/s") {
                UsbSpeedClass::HighSpeed
            } else {
                UsbSpeedClass::Unsupported
            });
        } else if let Some(value) = line.strip_prefix("Location ID:") {
            pending.location = value.split('/').next().map(str::trim).map(str::to_owned);
        }
    }
    finish(&mut pending, &mut devices);
    devices
}

#[cfg(any(target_os = "macos", test))]
fn parse_prefixed_hex(value: &str) -> Option<u16> {
    let token = value.split_whitespace().next()?;
    u16::from_str_radix(token.trim_start_matches("0x"), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_requested_models_at_usb_two_and_three_speeds() {
        assert_eq!(
            profile_for(DREAMSOURCELAB_VENDOR_ID, 0x002a, UsbSpeedClass::HighSpeed),
            Some(&DSLOGIC_U3PRO16)
        );
        assert_eq!(
            profile_for(DREAMSOURCELAB_VENDOR_ID, 0x002b, UsbSpeedClass::SuperSpeed),
            Some(&DSCOPE_U3P100)
        );
    }

    #[test]
    fn rejects_unknown_devices_and_slow_connections() {
        assert_eq!(
            profile_for(DREAMSOURCELAB_VENDOR_ID, 0xffff, UsbSpeedClass::SuperSpeed),
            None
        );
        assert_eq!(
            profile_for(DREAMSOURCELAB_VENDOR_ID, 0x002a, UsbSpeedClass::Unsupported),
            None
        );
    }

    #[test]
    fn stable_id_normalizes_platform_path() {
        let location = DeviceLocation {
            platform_path: "0x02100000 / 1".to_owned(),
        };

        assert_eq!(location.stable_id(), "usb-0x02100000---1");
    }

    #[test]
    fn parses_macos_inventory() {
        let inventory = r#"
            DSLogic U3Pro16:
              Product ID: 0x002a
              Vendor ID: 0x2a0e
              Speed: Up to 5 Gb/s
              Location ID: 0x02100000 / 2

            DSCope U3P100:
              Product ID: 0x002b
              Vendor ID: 0x2a0e
              Speed: Up to 480 Mb/s
              Location ID: 0x02200000 / 3
        "#;

        let devices = parse_macos_usb_inventory(inventory);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].product_id, 0x002a);
        assert_eq!(devices[0].speed, UsbSpeedClass::SuperSpeed);
        assert_eq!(devices[1].product_id, 0x002b);
        assert_eq!(devices[1].speed, UsbSpeedClass::HighSpeed);
    }

    #[test]
    fn classifies_supported_link_speeds() {
        assert_eq!(speed_from_mbps(480), UsbSpeedClass::HighSpeed);
        assert_eq!(speed_from_mbps(5_000), UsbSpeedClass::SuperSpeed);
        assert_eq!(speed_from_mbps(12), UsbSpeedClass::Unsupported);
    }
}
