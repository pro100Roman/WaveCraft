use crate::{
    DiscoveredDevice, FirmwareVersion, HardwareStatus, HdlVersion, UsbDeviceSession, UsbError,
};
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FpgaLoad {
    AlreadyConfigured(HardwareStatus),
    Configured {
        bytes: usize,
        status: HardwareStatus,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedDevice {
    pub firmware: FirmwareVersion,
    pub hdl: HdlVersion,
    pub fpga: FpgaLoad,
}

pub fn load_device_fpga(
    device: &DiscoveredDevice,
    bitstream_path: &Path,
) -> Result<FpgaLoad, UsbError> {
    with_device_session(device, |session| {
        require_compatible_firmware(session)?;
        ensure_fpga_configured(session, bitstream_path)
    })
}

pub fn prepare_device(
    device: &DiscoveredDevice,
    bitstream_path: &Path,
) -> Result<PreparedDevice, UsbError> {
    with_device_session(device, |session| {
        let firmware = require_compatible_firmware(session)?;
        let fpga = ensure_fpga_configured(session, bitstream_path)?;
        let hdl = session.hdl_version()?;
        if !hdl.is_compatible() {
            return Err(UsbError::new(format!(
                "HDL {hdl} is incompatible; version 0x0e is required"
            )));
        }
        Ok(PreparedDevice {
            firmware,
            hdl,
            fpga,
        })
    })
}

fn require_compatible_firmware(session: &UsbDeviceSession) -> Result<FirmwareVersion, UsbError> {
    let firmware = session.firmware_version()?;
    if !firmware.is_compatible() {
        return Err(UsbError::new(format!(
            "firmware {firmware} is incompatible; major version 2 is required"
        )));
    }
    Ok(firmware)
}

fn ensure_fpga_configured(
    session: &UsbDeviceSession,
    bitstream_path: &Path,
) -> Result<FpgaLoad, UsbError> {
    let current_status = session.hardware_status()?;
    if current_status.fpga_configured {
        return Ok(FpgaLoad::AlreadyConfigured(current_status));
    }
    let bitstream = std::fs::read(bitstream_path).map_err(|error| {
        UsbError::new(format!(
            "failed to read FPGA bitstream {}: {error}",
            bitstream_path.display()
        ))
    })?;
    let status = session.configure_fpga(&bitstream)?;
    Ok(FpgaLoad::Configured {
        bytes: bitstream.len(),
        status,
    })
}

fn with_device_session<T>(
    device: &DiscoveredDevice,
    operation: impl FnOnce(&UsbDeviceSession) -> Result<T, UsbError>,
) -> Result<T, UsbError> {
    let session = UsbDeviceSession::open(device)?;
    let operation_result = operation(&session);
    let release_result = session.close();
    match (operation_result, release_result) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(release_error)) => Err(UsbError::new(format!(
            "{operation_error}; additionally failed to release the interface: {release_error}"
        ))),
    }
}
