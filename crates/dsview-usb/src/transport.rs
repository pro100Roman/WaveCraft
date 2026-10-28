use crate::{profile_for, topology_path, DiscoveredDevice, UsbError, USB_INTERFACE};
use rusb::{request_type, Device, DeviceHandle, Direction, GlobalContext, Recipient, RequestType};
use std::fmt::{Display, Formatter};
use std::thread;
use std::time::{Duration, Instant};

const CONTROL_WRITE: u8 = 0xb0;
const CONTROL_READ_PREPARE: u8 = 0xb1;
const CONTROL_READ: u8 = 0xb2;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const CONTROL_READ_DELAY: Duration = Duration::from_millis(10);
const BULK_TIMEOUT: Duration = Duration::from_secs(3);
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

const FIRMWARE_VERSION_DESTINATION: u8 = 0;
const HARDWARE_STATUS_DESTINATION: u8 = 2;
const PROGRAM_DESTINATION: u8 = 3;
const LED_DESTINATION: u8 = 5;
const INTERRUPT_READY_DESTINATION: u8 = 6;
const WORD_WIDTH_DESTINATION: u8 = 7;
const BULK_WRITE_DESTINATION: u8 = 10;
const I2C_REGISTER_DESTINATION: u8 = 14;
const I2C_STATUS_DESTINATION: u8 = 15;
const REQUIRED_FIRMWARE_MAJOR: u8 = 2;
const REQUIRED_HDL_VERSION: u8 = 0x0e;
const CONTROL_REGISTER_ZERO: u16 = 0x70;
const HDL_VERSION_OFFSET: usize = 4;
const MAX_CONTROL_WRITE_DATA: usize = 60;
const MAX_BITSTREAM_SIZE: usize = 0x00ff_ffff;
const BITSTREAM_SYNC_OFFSET: usize = 16;
const BITSTREAM_SYNC_WORD: [u8; 4] = [0x55, 0x99, 0xaa, 0x66];

const GPIF_DONE_MASK: u8 = 1 << 7;
const FPGA_DONE_MASK: u8 = 1 << 6;
const FPGA_INITIALIZED_MASK: u8 = 1 << 5;
const FIFO_OVERFLOW_MASK: u8 = 1 << 4;
const PROGRAM_HIGH: u8 = 1 << 2;
const LED_GREEN: u8 = 1;
const LED_RED: u8 = 1 << 1;
const INTERRUPT_READY_HIGH: u8 = 1 << 7;
const WORD_WIDTH_WIDE: u8 = 1;

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
pub struct HdlVersion(pub u8);

impl HdlVersion {
    pub fn is_compatible(self) -> bool {
        self.0 == REQUIRED_HDL_VERSION
    }
}

impl Display for HdlVersion {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "0x{:02x}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HardwareStatus {
    pub raw: u8,
    pub gpif_done: bool,
    pub fpga_configured: bool,
    pub fpga_initialized: bool,
    pub fifo_overflow: bool,
}

impl HardwareStatus {
    fn from_raw(raw: u8) -> Self {
        Self {
            raw,
            gpif_done: raw & GPIF_DONE_MASK != 0,
            fpga_configured: raw & FPGA_DONE_MASK != 0,
            fpga_initialized: raw & FPGA_INITIALIZED_MASK != 0,
            fifo_overflow: raw & FIFO_OVERFLOW_MASK != 0,
        }
    }
}

/// An exclusively claimed connection to one supported instrument.
///
/// Dropping a session releases interface 0. Call [`Self::close`] when release
/// errors need to be reported to the caller.
pub struct UsbDeviceSession {
    device: DiscoveredDevice,
    handle: DeviceHandle<GlobalContext>,
    claimed: bool,
}

impl UsbDeviceSession {
    pub fn open(discovered: &DiscoveredDevice) -> Result<Self, UsbError> {
        let device = find_device(discovered)?;
        let handle = device
            .open()
            .map_err(|error| usb_error(discovered, "open the USB device", error))?;
        handle
            .claim_interface(USB_INTERFACE)
            .map_err(|error| usb_error(discovered, "claim USB interface 0", error))?;

        Ok(Self {
            device: discovered.clone(),
            handle,
            claimed: true,
        })
    }

    pub fn active_configuration(&self) -> Result<rusb::ConfigDescriptor, UsbError> {
        self.handle
            .device()
            .active_config_descriptor()
            .map_err(|error| {
                usb_error(
                    &self.device,
                    "read the active USB configuration descriptor",
                    error,
                )
            })
    }

    pub fn firmware_version(&self) -> Result<FirmwareVersion, UsbError> {
        let mut version = [0_u8; 2];
        self.read_control(FIRMWARE_VERSION_DESTINATION, 0, &mut version)?;
        Ok(FirmwareVersion {
            major: version[0],
            minor: version[1],
        })
    }

    pub fn hardware_status(&self) -> Result<HardwareStatus, UsbError> {
        let mut status = [0_u8; 1];
        self.read_control(HARDWARE_STATUS_DESTINATION, 0, &mut status)?;
        Ok(HardwareStatus::from_raw(status[0]))
    }

    /// Read the HDL version exposed by a configured FPGA.
    pub fn hdl_version(&self) -> Result<HdlVersion, UsbError> {
        let status = self.hardware_status()?;
        if !status.fpga_configured {
            return Err(UsbError::new(format!(
                "cannot read the HDL version from {} before its FPGA is configured",
                self.device.location.stable_id()
            )));
        }

        // Match the original driver: clear CTR0 before reading the I2C status
        // window that contains the HDL version byte.
        self.write_control(I2C_REGISTER_DESTINATION, CONTROL_REGISTER_ZERO, &[0])?;
        let mut status_window = [0_u8; HDL_VERSION_OFFSET + 1];
        self.read_control(I2C_STATUS_DESTINATION, 0, &mut status_window)?;
        Ok(HdlVersion(status_window[HDL_VERSION_OFFSET]))
    }

    /// Configure the instrument FPGA with the raw bitstream selected by its
    /// device profile.
    pub fn configure_fpga(&self, bitstream: &[u8]) -> Result<HardwareStatus, UsbError> {
        validate_bitstream(bitstream)?;
        let result = self.configure_fpga_inner(bitstream);
        if result.is_err() {
            self.recover_after_configuration_failure();
        }
        result
    }

    pub fn close(mut self) -> Result<(), UsbError> {
        self.release()
    }

    fn read_control(&self, destination: u8, offset: u16, data: &mut [u8]) -> Result<(), UsbError> {
        let size = u8::try_from(data.len()).map_err(|_| {
            UsbError::new(format!(
                "control read for {} is too large: {} bytes (maximum 255)",
                self.device.location.stable_id(),
                data.len()
            ))
        })?;
        let header = control_read_header(destination, offset, size);
        let out_request = request_type(Direction::Out, RequestType::Vendor, Recipient::Device);
        let written = self
            .handle
            .write_control(
                out_request,
                CONTROL_READ_PREPARE,
                0,
                0,
                &header,
                CONTROL_TIMEOUT,
            )
            .map_err(|error| usb_error(&self.device, "prepare a control read", error))?;
        if written != header.len() {
            return Err(UsbError::new(format!(
                "short control-read preparation for {}: wrote {written} of {} bytes",
                self.device.location.stable_id(),
                header.len()
            )));
        }

        thread::sleep(CONTROL_READ_DELAY);
        let in_request = request_type(Direction::In, RequestType::Vendor, Recipient::Device);
        let read = self
            .handle
            .read_control(in_request, CONTROL_READ, 0, 0, data, CONTROL_TIMEOUT)
            .map_err(|error| usb_error(&self.device, "complete a control read", error))?;
        if read != data.len() {
            return Err(UsbError::new(format!(
                "short control read for {}: received {read} of {} bytes",
                self.device.location.stable_id(),
                data.len()
            )));
        }
        Ok(())
    }

    fn configure_fpga_inner(&self, bitstream: &[u8]) -> Result<HardwareStatus, UsbError> {
        self.write_control(PROGRAM_DESTINATION, 0, &[!PROGRAM_HIGH])?;
        self.write_control(LED_DESTINATION, 0, &[!(LED_GREEN | LED_RED)])?;
        self.write_control(PROGRAM_DESTINATION, 0, &[PROGRAM_HIGH])?;
        self.wait_for_status("FPGA initialization", |status| status.fpga_initialized)?;

        self.write_control(INTERRUPT_READY_DESTINATION, 0, &[!INTERRUPT_READY_HIGH])?;
        let size = bitstream.len();
        self.write_control(
            BULK_WRITE_DESTINATION,
            0,
            &[size as u8, (size >> 8) as u8, (size >> 16) as u8],
        )?;

        let written = self
            .handle
            .write_bulk(crate::BULK_OUT_ENDPOINT, bitstream, BULK_TIMEOUT)
            .map_err(|error| usb_error(&self.device, "write the FPGA bitstream", error))?;
        if written != bitstream.len() {
            return Err(UsbError::new(format!(
                "short FPGA bitstream write for {}: sent {written} of {} bytes",
                self.device.location.stable_id(),
                bitstream.len()
            )));
        }

        self.write_control(INTERRUPT_READY_DESTINATION, 0, &[INTERRUPT_READY_HIGH])?;
        self.wait_for_status("FPGA bulk transfer", |status| status.gpif_done)?;
        self.write_control(INTERRUPT_READY_DESTINATION, 0, &[!INTERRUPT_READY_HIGH])?;
        let status = self.wait_for_status("FPGA configuration", |status| status.fpga_configured)?;
        self.write_control(LED_DESTINATION, 0, &[LED_GREEN])?;
        self.write_control(WORD_WIDTH_DESTINATION, 0, &[WORD_WIDTH_WIDE])?;
        Ok(status)
    }

    fn write_control(&self, destination: u8, offset: u16, data: &[u8]) -> Result<(), UsbError> {
        if data.len() > MAX_CONTROL_WRITE_DATA {
            return Err(UsbError::new(format!(
                "control write for {} is too large: {} bytes (maximum {MAX_CONTROL_WRITE_DATA})",
                self.device.location.stable_id(),
                data.len()
            )));
        }
        let mut command = Vec::with_capacity(4 + data.len());
        command.extend_from_slice(&control_read_header(destination, offset, data.len() as u8));
        command.extend_from_slice(data);
        let out_request = request_type(Direction::Out, RequestType::Vendor, Recipient::Device);
        let written = self
            .handle
            .write_control(out_request, CONTROL_WRITE, 0, 0, &command, CONTROL_TIMEOUT)
            .map_err(|error| usb_error(&self.device, "send a control write", error))?;
        if written != command.len() {
            return Err(UsbError::new(format!(
                "short control write for {}: sent {written} of {} bytes",
                self.device.location.stable_id(),
                command.len()
            )));
        }
        Ok(())
    }

    fn wait_for_status(
        &self,
        operation: &str,
        ready: impl Fn(HardwareStatus) -> bool,
    ) -> Result<HardwareStatus, UsbError> {
        let deadline = Instant::now() + STATUS_TIMEOUT;
        let mut last_status = HardwareStatus::from_raw(0);
        while Instant::now() < deadline {
            last_status = self.hardware_status()?;
            if ready(last_status) {
                return Ok(last_status);
            }
        }
        Err(UsbError::new(format!(
            "timed out waiting for {operation} on {} (last hardware status 0x{:02x})",
            self.device.location.stable_id(),
            last_status.raw
        )))
    }

    fn recover_after_configuration_failure(&self) {
        let _ = self.write_control(PROGRAM_DESTINATION, 0, &[PROGRAM_HIGH]);
        let _ = self.write_control(INTERRUPT_READY_DESTINATION, 0, &[!INTERRUPT_READY_HIGH]);
        let _ = self.write_control(WORD_WIDTH_DESTINATION, 0, &[WORD_WIDTH_WIDE]);
    }

    fn release(&mut self) -> Result<(), UsbError> {
        if !self.claimed {
            return Ok(());
        }
        self.handle
            .release_interface(USB_INTERFACE)
            .map_err(|error| usb_error(&self.device, "release USB interface 0", error))?;
        self.claimed = false;
        Ok(())
    }
}

impl Drop for UsbDeviceSession {
    fn drop(&mut self) {
        let _ = self.release();
    }
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
                "{} disconnected before it could be opened",
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

fn control_read_header(destination: u8, offset: u16, size: u8) -> [u8; 4] {
    let [offset_low, offset_high] = offset.to_le_bytes();
    [destination, offset_low, offset_high, size]
}

fn validate_bitstream(bitstream: &[u8]) -> Result<(), UsbError> {
    if bitstream.len() > MAX_BITSTREAM_SIZE {
        return Err(UsbError::new(format!(
            "FPGA bitstream is too large: {} bytes (maximum {MAX_BITSTREAM_SIZE})",
            bitstream.len()
        )));
    }
    let sync_end = BITSTREAM_SYNC_OFFSET + BITSTREAM_SYNC_WORD.len();
    if bitstream.len() < sync_end
        || bitstream[..BITSTREAM_SYNC_OFFSET]
            .iter()
            .any(|byte| *byte != 0xff)
        || bitstream[BITSTREAM_SYNC_OFFSET..sync_end] != BITSTREAM_SYNC_WORD
    {
        return Err(UsbError::new(
            "FPGA bitstream does not contain the expected Xilinx sync header",
        ));
    }
    Ok(())
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
    fn decodes_hardware_status_bits() {
        let status = HardwareStatus::from_raw(
            GPIF_DONE_MASK | FPGA_DONE_MASK | FPGA_INITIALIZED_MASK | FIFO_OVERFLOW_MASK,
        );

        assert!(status.gpif_done);
        assert!(status.fpga_configured);
        assert!(status.fpga_initialized);
        assert!(status.fifo_overflow);
    }

    #[test]
    fn leaves_clear_hardware_status_bits_false() {
        let status = HardwareStatus::from_raw(0);

        assert!(!status.gpif_done);
        assert!(!status.fpga_configured);
        assert!(!status.fpga_initialized);
        assert!(!status.fifo_overflow);
    }

    #[test]
    fn checks_firmware_major_version_compatibility() {
        assert!(FirmwareVersion { major: 2, minor: 0 }.is_compatible());
        assert!(!FirmwareVersion { major: 1, minor: 9 }.is_compatible());
        assert!(!FirmwareVersion { major: 3, minor: 0 }.is_compatible());
    }

    #[test]
    fn checks_hdl_version_compatibility() {
        assert!(HdlVersion(0x0e).is_compatible());
        assert!(!HdlVersion(0x0d).is_compatible());
        assert_eq!(HdlVersion(0x0e).to_string(), "0x0e");
    }

    #[test]
    fn accepts_raw_xilinx_bitstream_header() {
        let mut bitstream = vec![0xff; BITSTREAM_SYNC_OFFSET];
        bitstream.extend_from_slice(&BITSTREAM_SYNC_WORD);
        bitstream.extend_from_slice(&[1, 2, 3]);

        validate_bitstream(&bitstream).expect("header should be valid");
    }

    #[test]
    fn rejects_empty_or_malformed_bitstreams() {
        assert!(validate_bitstream(&[]).is_err());
        assert!(validate_bitstream(&[0; 32]).is_err());
    }
}
