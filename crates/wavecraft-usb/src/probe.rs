use crate::{
    DiscoveredDevice, FirmwareVersion, HardwareStatus, UsbDeviceSession, UsbError,
    BULK_IN_ENDPOINT, BULK_OUT_ENDPOINT, USB_INTERFACE,
};
use rusb::TransferType;

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
    pub hardware_status: HardwareStatus,
}

/// Open one previously discovered device, validate its USB layout, read its
/// firmware version, and prove that its acquisition interface can be claimed.
/// No firmware, FPGA, register, or acquisition writes are performed.
pub fn probe_device(discovered: &DiscoveredDevice) -> Result<DeviceProbe, UsbError> {
    let session = UsbDeviceSession::open(discovered)?;
    let probe_result = (|| {
        let configuration = session.active_configuration()?;
        let (alternate_setting, bulk_out, bulk_in) = validate_layout(discovered, &configuration)?;
        let firmware_version = session.firmware_version()?;
        let hardware_status = session.hardware_status()?;

        Ok(DeviceProbe {
            device: discovered.clone(),
            configuration: configuration.number(),
            interface: USB_INTERFACE,
            alternate_setting,
            bulk_out,
            bulk_in,
            firmware_version,
            hardware_status,
        })
    })();
    let release_result = session.close();
    match (probe_result, release_result) {
        (Ok(probe), Ok(())) => Ok(probe),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(probe_error), Err(release_error)) => Err(UsbError::new(format!(
            "{probe_error}; additionally failed to release the interface: {release_error}"
        ))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
