use crate::{
    profile_for, topology_path, DiscoveredDevice, UsbError, BULK_IN_ENDPOINT, BULK_OUT_ENDPOINT,
    USB_INTERFACE,
};
use rusb::{
    request_type, Device, DeviceHandle, Direction, GlobalContext, Recipient, RequestType,
    TransferType,
};
use std::fmt::{Display, Formatter};
use std::thread;
use std::time::Duration;

const CONTROL_READ_PREPARE: u8 = 0xb1;
const CONTROL_READ: u8 = 0xb2;
const FIRMWARE_VERSION_DESTINATION: u8 = 0;
const REQUIRED_FIRMWARE_MAJOR: u8 = 2;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const CONTROL_READ_DELAY: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareVersion {
    pub major: u8,
    pub minor: u8,
}

impl FirmwareVersion {
    pub fn is_compatible(self) -> bool {
        self.major == REQUIRED_FIRMWARE_MAJOR
    }
}

impl Display for FirmwareVersion {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbEndpointInfo {
    pub address: u8,
    pub max_packet_size: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceProbe {
    pub device: DiscoveredDevice,
    pub configuration: u8,
    pub interface: u8,
    pub alternate_setting: u8,
    pub bulk_out: UsbEndpointInfo,
    pub bulk_in: UsbEndpointInfo,
    pub firmware_version: FirmwareVersion,
}

/// Open one previously discovered device, validate its USB layout, read its
/// firmware version, and prove that its acquisition interface can be claimed.
/// No firmware, FPGA, register, or acquisition writes are performed.
pub fn probe_device(discovered: &DiscoveredDevice) -> Result<DeviceProbe, UsbError> {
    let device = find_device(discovered)?;
    let configuration = device.active_config_descriptor().map_err(|error| {
        usb_error(
            discovered,
            "read the active USB configuration descriptor",
            error,
        )
    })?;
    let (alternate_setting, bulk_out, bulk_in) = validate_layout(discovered, &configuration)?;
    let handle = device
        .open()
        .map_err(|error| usb_error(discovered, "open the USB device", error))?;

    handle
        .claim_interface(USB_INTERFACE)
        .map_err(|error| usb_error(discovered, "claim USB interface 0", error))?;

    let firmware_result = read_firmware_version(&handle)
        .map_err(|error| usb_error(discovered, "read the firmware version", error));
    let release_result = handle
        .release_interface(USB_INTERFACE)
        .map_err(|error| usb_error(discovered, "release USB interface 0", error));
    let firmware_version = match (firmware_result, release_result) {
        (Ok(version), Ok(())) => version,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(probe_error), Err(release_error)) => {
            return Err(UsbError::new(format!(
                "{probe_error}; additionally failed to release the interface: {release_error}"
            )))
        }
    };

    Ok(DeviceProbe {
        device: discovered.clone(),
        configuration: configuration.number(),
        interface: USB_INTERFACE,
        alternate_setting,
        bulk_out,
        bulk_in,
        firmware_version,
    })
}

fn find_device(discovered: &DiscoveredDevice) -> Result<Device<GlobalContext>, UsbError> {
    let devices = rusb::devices().map_err(|error| {
        UsbError::new(format!(
            "failed to enumerate USB devices while locating {}: {error}",
            discovered.location.stable_id()
        ))
    })?;

    devices
        .iter()
        .find(|device| matches_discovered_device(device, discovered))
        .ok_or_else(|| {
            UsbError::new(format!(
                "{} disconnected before it could be probed",
                discovered.location.stable_id()
            ))
        })
}

fn matches_discovered_device(
    device: &Device<GlobalContext>,
    discovered: &DiscoveredDevice,
) -> bool {
    let Ok(descriptor) = device.device_descriptor() else {
        return false;
    };
    if profile_for(descriptor.vendor_id(), descriptor.product_id()) != Some(discovered.profile) {
        return false;
    }

    let ports = device.port_numbers().unwrap_or_default();
    topology_path(device.bus_number(), device.address(), &ports)
        == discovered.location.platform_path
}

fn validate_layout(
    discovered: &DiscoveredDevice,
    configuration: &rusb::ConfigDescriptor,
) -> Result<(u8, UsbEndpointInfo, UsbEndpointInfo), UsbError> {
    let interface = configuration
        .interfaces()
        .find(|interface| interface.number() == USB_INTERFACE)
        .ok_or_else(|| {
            UsbError::new(format!(
                "{} has no USB interface {} in active configuration {}",
                discovered.location.stable_id(),
                USB_INTERFACE,
                configuration.number()
            ))
        })?;
    let descriptor = interface
        .descriptors()
        .find(|descriptor| descriptor.setting_number() == 0)
        .ok_or_else(|| {
            UsbError::new(format!(
                "{} has no default alternate setting for USB interface {}",
                discovered.location.stable_id(),
                USB_INTERFACE
            ))
        })?;
    let endpoints = descriptor
        .endpoint_descriptors()
        .map(|endpoint| EndpointShape {
            address: endpoint.address(),
            transfer_type: endpoint.transfer_type(),
            max_packet_size: endpoint.max_packet_size(),
        })
        .collect::<Vec<_>>();
    let (bulk_out, bulk_in) = required_bulk_endpoints(&endpoints).map_err(|message| {
        UsbError::new(format!(
            "{} has an incompatible USB layout: {message}",
            discovered.location.stable_id()
        ))
    })?;

    Ok((descriptor.setting_number(), bulk_out, bulk_in))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EndpointShape {
    address: u8,
    transfer_type: TransferType,
    max_packet_size: u16,
}

fn required_bulk_endpoints(
    endpoints: &[EndpointShape],
) -> Result<(UsbEndpointInfo, UsbEndpointInfo), String> {
    let find = |address| {
        endpoints
            .iter()
            .find(|endpoint| {
                endpoint.address == address && endpoint.transfer_type == TransferType::Bulk
            })
            .map(|endpoint| UsbEndpointInfo {
                address: endpoint.address,
                max_packet_size: endpoint.max_packet_size,
            })
    };
    let bulk_out = find(BULK_OUT_ENDPOINT).ok_or_else(|| {
        format!("missing bulk OUT endpoint 0x{BULK_OUT_ENDPOINT:02x} on interface {USB_INTERFACE}")
    })?;
    let bulk_in = find(BULK_IN_ENDPOINT).ok_or_else(|| {
        format!("missing bulk IN endpoint 0x{BULK_IN_ENDPOINT:02x} on interface {USB_INTERFACE}")
    })?;
    Ok((bulk_out, bulk_in))
}

fn read_firmware_version(
    handle: &DeviceHandle<GlobalContext>,
) -> Result<FirmwareVersion, rusb::Error> {
    let header = control_read_header(FIRMWARE_VERSION_DESTINATION, 0, 2);
    let out_request = request_type(Direction::Out, RequestType::Vendor, Recipient::Device);
    let written = handle.write_control(
        out_request,
        CONTROL_READ_PREPARE,
        0,
        0,
        &header,
        CONTROL_TIMEOUT,
    )?;
    if written != header.len() {
        return Err(rusb::Error::Other);
    }

    thread::sleep(CONTROL_READ_DELAY);
    let mut version = [0_u8; 2];
    let in_request = request_type(Direction::In, RequestType::Vendor, Recipient::Device);
    let read = handle.read_control(
        in_request,
        CONTROL_READ,
        0,
        0,
        &mut version,
        CONTROL_TIMEOUT,
    )?;
    if read != version.len() {
        return Err(rusb::Error::Other);
    }

    Ok(FirmwareVersion {
        major: version[0],
        minor: version[1],
    })
}

fn control_read_header(destination: u8, offset: u16, size: u8) -> [u8; 4] {
    let [offset_low, offset_high] = offset.to_le_bytes();
    [destination, offset_low, offset_high, size]
}

fn usb_error(discovered: &DiscoveredDevice, action: &str, error: rusb::Error) -> UsbError {
    let guidance = match error {
        rusb::Error::Access => "permission denied; check the device access rules",
        rusb::Error::Busy => "the device or interface is already claimed by another process",
        rusb::Error::NoDevice => "the device was disconnected",
        rusb::Error::NotFound => "the requested interface or endpoint was not found",
        rusb::Error::NotSupported => "the host USB backend does not support this operation",
        rusb::Error::Timeout => "the device did not respond before the timeout",
        _ => "USB communication failed",
    };
    UsbError::new(format!(
        "failed to {action} for {}: {guidance} ({error})",
        discovered.location.stable_id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_control_read_header_in_protocol_order() {
        assert_eq!(control_read_header(0x12, 0x3456, 7), [0x12, 0x56, 0x34, 7]);
    }

    #[test]
    fn accepts_required_bulk_endpoints() {
        let endpoints = [
            EndpointShape {
                address: BULK_OUT_ENDPOINT,
                transfer_type: TransferType::Bulk,
                max_packet_size: 512,
            },
            EndpointShape {
                address: BULK_IN_ENDPOINT,
                transfer_type: TransferType::Bulk,
                max_packet_size: 1024,
            },
        ];

        let (bulk_out, bulk_in) =
            required_bulk_endpoints(&endpoints).expect("layout should be supported");
        assert_eq!(bulk_out.max_packet_size, 512);
        assert_eq!(bulk_in.max_packet_size, 1024);
    }

    #[test]
    fn rejects_wrong_endpoint_transfer_type() {
        let endpoints = [
            EndpointShape {
                address: BULK_OUT_ENDPOINT,
                transfer_type: TransferType::Interrupt,
                max_packet_size: 512,
            },
            EndpointShape {
                address: BULK_IN_ENDPOINT,
                transfer_type: TransferType::Bulk,
                max_packet_size: 1024,
            },
        ];

        let error = required_bulk_endpoints(&endpoints).expect_err("layout should be rejected");
        assert!(error.contains("bulk OUT endpoint"));
    }

    #[test]
    fn checks_firmware_major_version_compatibility() {
        assert!(FirmwareVersion { major: 2, minor: 0 }.is_compatible());
        assert!(!FirmwareVersion { major: 1, minor: 9 }.is_compatible());
        assert!(!FirmwareVersion { major: 3, minor: 0 }.is_compatible());
    }
}
