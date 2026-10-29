use crate::{profile_for, topology_path, DiscoveredDevice, UsbError, USB_INTERFACE};
use rusb::{
    request_type, Device, DeviceHandle, Direction, GlobalContext, Recipient, RequestType,
    UsbContext,
};
use std::ffi::c_void;
use std::fmt::{Display, Formatter};
use std::ptr::NonNull;
use std::thread;
use std::time::{Duration, Instant};

const CONTROL_WRITE: u8 = 0xb0;
const CONTROL_READ_PREPARE: u8 = 0xb1;
const CONTROL_READ: u8 = 0xb2;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const CONTROL_READ_DELAY: Duration = Duration::from_millis(10);
const BULK_TIMEOUT: Duration = Duration::from_secs(3);
const BULK_READ_POLL_TIMEOUT: Duration = Duration::from_millis(50);
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

const FIRMWARE_VERSION_DESTINATION: u8 = 0;
const HARDWARE_STATUS_DESTINATION: u8 = 2;
const PROGRAM_DESTINATION: u8 = 3;
const LED_DESTINATION: u8 = 5;
const INTERRUPT_READY_DESTINATION: u8 = 6;
const WORD_WIDTH_DESTINATION: u8 = 7;
const BULK_WRITE_DESTINATION: u8 = 10;
const NVM_DESTINATION: u8 = 12;
const CAPTURE_START_DESTINATION: u8 = 8;
const CAPTURE_STOP_DESTINATION: u8 = 9;
const I2C_REGISTER_DESTINATION: u8 = 14;
const I2C_STATUS_DESTINATION: u8 = 15;
const I2C_DSO_DESTINATION: u8 = 13;
const DSO_CHANNEL_ZERO_ENABLE_DESTINATION: u8 = 16;
const DSO_CHANNEL_ONE_ENABLE_DESTINATION: u8 = 19;
const REQUIRED_FIRMWARE_MAJOR: u8 = 2;
const REQUIRED_HDL_VERSION: u8 = 0x0e;
const CONTROL_REGISTER_ZERO: u16 = 0x70;
const ANALOG_CONTROL_REGISTER: u16 = 0x48;
const THRESHOLD_REGISTER: u16 = 0x78;
const HDL_VERSION_OFFSET: usize = 4;
const MAX_CONTROL_WRITE_DATA: usize = 60;
const MAX_BITSTREAM_SIZE: usize = 0x00ff_ffff;
const BITSTREAM_SYNC_OFFSET: usize = 16;
const BITSTREAM_SYNC_WORD: [u8; 4] = [0x55, 0x99, 0xaa, 0x66];

const GPIF_DONE_MASK: u8 = 1 << 7;
const FPGA_DONE_MASK: u8 = 1 << 6;
const FPGA_INITIALIZED_MASK: u8 = 1 << 5;
const FIFO_OVERFLOW_MASK: u8 = 1 << 4;
const SYSTEM_CLEARED_MASK: u8 = 1 << 3;
const PROGRAM_HIGH: u8 = 1 << 2;
const LED_GREEN: u8 = 1;
const LED_RED: u8 = 1 << 1;
const INTERRUPT_READY_HIGH: u8 = 1 << 7;
const WORD_WIDTH_WIDE: u8 = 1;
const FORCE_READY: u8 = 1 << 1;

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
    pub system_cleared: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BulkReadOutcome {
    Data(usize),
    Cancelled,
}

pub(crate) enum QueuedBulkReadOutcome {
    Data(Vec<u8>),
    Cancelled,
}

struct QueuedBulkReadState {
    buffer: Vec<u8>,
    completed: bool,
    status: i32,
    actual_length: usize,
}

pub(crate) struct QueuedBulkRead {
    device_id: String,
    transfer: Option<NonNull<rusb::ffi::libusb_transfer>>,
    state: Box<QueuedBulkReadState>,
}

extern "system" fn queued_bulk_read_callback(transfer: *mut rusb::ffi::libusb_transfer) {
    // SAFETY: queue_bulk_read stores a live Box<QueuedBulkReadState> in user_data
    // and does not move or free it until libusb invokes this callback.
    unsafe {
        let state = &mut *((*transfer).user_data as *mut QueuedBulkReadState);
        state.status = (*transfer).status;
        state.actual_length = (*transfer).actual_length.max(0) as usize;
        state.completed = true;
    }
}

impl QueuedBulkRead {
    pub(crate) fn is_complete(&self) -> bool {
        self.state.completed
    }

    pub(crate) fn cancel(&mut self) -> Result<(), UsbError> {
        if self.state.completed {
            return Ok(());
        }
        let transfer = self
            .transfer
            .expect("a queued bulk read owns its libusb transfer");
        // SAFETY: transfer remains allocated until finish or Drop.
        let result = unsafe { rusb::ffi::libusb_cancel_transfer(transfer.as_ptr()) };
        if result == rusb::constants::LIBUSB_SUCCESS
            || result == rusb::constants::LIBUSB_ERROR_NOT_FOUND
        {
            Ok(())
        } else {
            Err(UsbError::new(format!(
                "failed to cancel queued capture read for {} (libusb error {result})",
                self.device_id
            )))
        }
    }

    pub(crate) fn finish(mut self) -> Result<QueuedBulkReadOutcome, UsbError> {
        if !self.state.completed {
            return Err(UsbError::new(format!(
                "queued capture read for {} has not completed",
                self.device_id
            )));
        }
        let transfer = self
            .transfer
            .take()
            .expect("a queued bulk read owns its libusb transfer");
        // SAFETY: the transfer completed and no callback can access it again.
        unsafe { rusb::ffi::libusb_free_transfer(transfer.as_ptr()) };

        match self.state.status {
            rusb::constants::LIBUSB_TRANSFER_COMPLETED => {
                if self.state.actual_length > self.state.buffer.len() {
                    return Err(UsbError::new(format!(
                        "queued capture read for {} exceeded its {}-byte buffer",
                        self.device_id,
                        self.state.buffer.len()
                    )));
                }
                self.state.buffer.truncate(self.state.actual_length);
                Ok(QueuedBulkReadOutcome::Data(std::mem::take(
                    &mut self.state.buffer,
                )))
            }
            rusb::constants::LIBUSB_TRANSFER_CANCELLED => Ok(QueuedBulkReadOutcome::Cancelled),
            status => Err(UsbError::new(format!(
                "queued capture read for {} failed with libusb transfer status {status}",
                self.device_id
            ))),
        }
    }
}

impl Drop for QueuedBulkRead {
    fn drop(&mut self) {
        let Some(transfer) = self.transfer.take() else {
            return;
        };
        if self.state.completed {
            // SAFETY: the completed transfer has no pending callback.
            unsafe { rusb::ffi::libusb_free_transfer(transfer.as_ptr()) };
        } else {
            // A pending libusb callback still references state. Leaking both is
            // safer than freeing callback-owned memory during error unwinding.
            self.transfer = Some(transfer);
            let state = std::mem::replace(
                &mut self.state,
                Box::new(QueuedBulkReadState {
                    buffer: Vec::new(),
                    completed: true,
                    status: rusb::constants::LIBUSB_TRANSFER_CANCELLED,
                    actual_length: 0,
                }),
            );
            let _ = Box::into_raw(state);
            self.transfer = None;
        }
    }
}

impl HardwareStatus {
    fn from_raw(raw: u8) -> Self {
        Self {
            raw,
            gpif_done: raw & GPIF_DONE_MASK != 0,
            fpga_configured: raw & FPGA_DONE_MASK != 0,
            fpga_initialized: raw & FPGA_INITIALIZED_MASK != 0,
            fifo_overflow: raw & FIFO_OVERFLOW_MASK != 0,
            system_cleared: raw & SYSTEM_CLEARED_MASK != 0,
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

    /// Wait for one bulk-IN transfer while polling cancellation at least every
    /// 50 milliseconds. USB timeouts and interrupted system calls are retried.
    pub fn read_bulk_cancellable(
        &self,
        buffer: &mut [u8],
        cancelled: impl FnMut() -> bool,
    ) -> Result<BulkReadOutcome, UsbError> {
        if buffer.is_empty() {
            return Err(UsbError::new(
                "bulk read buffer must contain at least one byte",
            ));
        }
        poll_bulk_read(
            |buffer, timeout| {
                self.handle
                    .read_bulk(crate::BULK_IN_ENDPOINT, buffer, timeout)
            },
            buffer,
            cancelled,
        )
        .map_err(|error| usb_error(&self.device, "read capture data", error))
    }

    pub(crate) fn queue_bulk_read(&self, size: usize) -> Result<QueuedBulkRead, UsbError> {
        if size == 0 || size > i32::MAX as usize {
            return Err(UsbError::new(format!(
                "invalid queued bulk-read size {size} for {}",
                self.device.location.stable_id()
            )));
        }
        // SAFETY: libusb allocates an unsubmitted transfer owned by the wrapper below.
        let transfer = NonNull::new(unsafe { rusb::ffi::libusb_alloc_transfer(0) })
            .ok_or_else(|| UsbError::new("failed to allocate a libusb capture transfer"))?;
        let mut state = Box::new(QueuedBulkReadState {
            buffer: vec![0_u8; size],
            completed: false,
            status: rusb::constants::LIBUSB_TRANSFER_ERROR,
            actual_length: 0,
        });
        // SAFETY: state is boxed at a stable address and remains owned by
        // QueuedBulkRead until the transfer completes and is freed.
        unsafe {
            rusb::ffi::libusb_fill_bulk_transfer(
                transfer.as_ptr(),
                self.handle.as_raw(),
                crate::BULK_IN_ENDPOINT,
                state.buffer.as_mut_ptr(),
                size as i32,
                queued_bulk_read_callback,
                (&mut *state as *mut QueuedBulkReadState).cast::<c_void>(),
                0,
            );
            let result = rusb::ffi::libusb_submit_transfer(transfer.as_ptr());
            if result != rusb::constants::LIBUSB_SUCCESS {
                rusb::ffi::libusb_free_transfer(transfer.as_ptr());
                return Err(UsbError::new(format!(
                    "failed to submit queued capture read for {} (libusb error {result})",
                    self.device.location.stable_id()
                )));
            }
        }
        Ok(QueuedBulkRead {
            device_id: self.device.location.stable_id(),
            transfer: Some(transfer),
            state,
        })
    }

    pub(crate) fn handle_usb_events(&self, timeout: Duration) -> Result<(), UsbError> {
        GlobalContext::default()
            .handle_events(Some(timeout))
            .map_err(|error| usb_error(&self.device, "process queued capture transfers", error))
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

    pub(crate) fn stop_capture(&self) -> Result<(), UsbError> {
        self.write_control(CAPTURE_STOP_DESTINATION, 0, &[])
    }

    pub(crate) fn arm_capture(&self, settings: &[u8]) -> Result<(), UsbError> {
        if settings.is_empty() || settings.len() % 2 != 0 {
            return Err(UsbError::new(
                "capture settings must contain a non-empty number of 16-bit words",
            ));
        }
        let word_count = settings.len() / 2;
        if word_count > MAX_BITSTREAM_SIZE {
            return Err(UsbError::new(format!(
                "capture settings are too large: {word_count} words"
            )));
        }

        self.write_control(
            BULK_WRITE_DESTINATION,
            0,
            &[
                word_count as u8,
                (word_count >> 8) as u8,
                (word_count >> 16) as u8,
            ],
        )?;
        self.wait_for_status("capture engine reset", |status| status.system_cleared)?;
        self.write_bulk_exact(settings, "write capture settings")?;
        self.write_control(INTERRUPT_READY_DESTINATION, 0, &[INTERRUPT_READY_HIGH])?;
        self.wait_for_status("capture settings transfer", |status| status.gpif_done)?;
        Ok(())
    }

    pub(crate) fn start_capture(&self) -> Result<(), UsbError> {
        self.write_control(CAPTURE_START_DESTINATION, 0, &[])
    }

    pub(crate) fn force_capture_ready(&self) -> Result<(), UsbError> {
        self.write_control(
            I2C_REGISTER_DESTINATION,
            CONTROL_REGISTER_ZERO,
            &[FORCE_READY],
        )
    }

    pub(crate) fn capture_progress(&self) -> Result<u32, UsbError> {
        let mut progress = [0_u8; 4];
        self.read_control(I2C_STATUS_DESTINATION, 0, &mut progress)?;
        Ok(u32::from_le_bytes(progress))
    }

    pub(crate) fn initialize_dslogic_frontend(&self) -> Result<(), UsbError> {
        // Match DSLogic's device-open path for the U3Pro16: a 1.0 V input
        // threshold followed by the ADF4360 500 MHz clock configuration.
        self.write_control(I2C_REGISTER_DESTINATION, THRESHOLD_REGISTER, &[0x2e])?;
        self.write_control(
            I2C_REGISTER_DESTINATION,
            ANALOG_CONTROL_REGISTER + 2,
            &[0x01],
        )?;
        for value in [0x01, 0x61, 0x00, 0x30] {
            self.write_control(I2C_REGISTER_DESTINATION, ANALOG_CONTROL_REGISTER, &[value])?;
        }
        for value in [0x01, 0x40, 0xf1, 0x46] {
            self.write_control(I2C_REGISTER_DESTINATION, ANALOG_CONTROL_REGISTER, &[value])?;
        }
        thread::sleep(Duration::from_millis(10));
        for value in [0x01, 0x62, 0x3d, 0x40] {
            self.write_control(I2C_REGISTER_DESTINATION, ANALOG_CONTROL_REGISTER, &[value])?;
        }
        Ok(())
    }

    pub(crate) fn initialize_dscope_frontend(
        &self,
        enabled_channels: [bool; 2],
    ) -> Result<(), UsbError> {
        const ADC_DUAL_CHANNEL: &[(u16, &[u8])] = &[
            (ANALOG_CONTROL_REGISTER + 1, &[0x03, 0x01, 0x00]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x02, 0x01, 0x31]),
            (ANALOG_CONTROL_REGISTER + 1, &[0x01]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x02, 0x02, 0x3a]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x10, 0x10, 0x3b]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x00, 0x00, 0x42]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x34, 0x00, 0x50]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x22, 0x02, 0x11]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x11, 0x00, 0x24]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x00, 0x00, 0x55]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x02, 0x00, 0x33]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x33, 0x00, 0x2b]),
        ];
        const ADC_SINGLE_CHANNEL_ZERO: &[(u16, &[u8])] = &[
            (ANALOG_CONTROL_REGISTER + 1, &[0x03, 0x01, 0x00]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x01, 0x00, 0x31]),
            (ANALOG_CONTROL_REGISTER + 1, &[0x01]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x02, 0x02, 0x3a]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x02, 0x02, 0x3b]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x00, 0x00, 0x42]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x34, 0x00, 0x50]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x22, 0x02, 0x11]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x7f, 0x00, 0x24]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x00, 0x00, 0x55]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x02, 0x00, 0x33]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x00, 0x03, 0x2b]),
        ];
        const ADC_SINGLE_CHANNEL_ONE: &[(u16, &[u8])] = &[
            (ANALOG_CONTROL_REGISTER + 1, &[0x03, 0x01, 0x00]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x01, 0x00, 0x31]),
            (ANALOG_CONTROL_REGISTER + 1, &[0x01]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x10, 0x10, 0x3a]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x10, 0x10, 0x3b]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x00, 0x00, 0x42]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x34, 0x00, 0x50]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x22, 0x02, 0x11]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x00, 0x00, 0x24]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x00, 0x00, 0x55]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x02, 0x00, 0x33]),
            (ANALOG_CONTROL_REGISTER, &[0x00, 0x00, 0x03, 0x2b]),
        ];
        self.write_control(
            I2C_REGISTER_DESTINATION,
            ANALOG_CONTROL_REGISTER + 2,
            &[0x01],
        )?;
        for values in [[0x01, 0x61, 0x00, 0x30], [0x01, 0x40, 0xf1, 0x46]] {
            for value in values {
                self.write_control(I2C_REGISTER_DESTINATION, ANALOG_CONTROL_REGISTER, &[value])?;
            }
        }
        thread::sleep(Duration::from_millis(10));
        for value in [0x01, 0x62, 0x3d, 0x00] {
            self.write_control(I2C_REGISTER_DESTINATION, ANALOG_CONTROL_REGISTER, &[value])?;
        }

        let adc_configuration = match enabled_channels {
            [true, true] => ADC_DUAL_CHANNEL,
            [true, false] => ADC_SINGLE_CHANNEL_ZERO,
            [false, true] => ADC_SINGLE_CHANNEL_ONE,
            [false, false] => {
                return Err(UsbError::new(
                    "cannot initialize the DSCope front-end without an enabled channel",
                ))
            }
        };
        for (register, values) in adc_configuration {
            for value in *values {
                self.write_control(I2C_REGISTER_DESTINATION, *register, &[*value])?;
            }
        }

        // Start from unity. Factory fine-gain values are applied after the
        // channel calibration record has been read.
        for command in dscope_adc_fine_gain_commands([[0; 4]; 2]) {
            for value in command {
                self.write_control(I2C_REGISTER_DESTINATION, ANALOG_CONTROL_REGISTER, &[value])?;
            }
        }
        for register in (ANALOG_CONTROL_REGISTER + 3)..=(ANALOG_CONTROL_REGISTER + 6) {
            self.write_control(I2C_REGISTER_DESTINATION, register, &[0])?;
        }
        Ok(())
    }

    pub(crate) fn configure_dscope_adc_fine_gains(
        &self,
        gain_codes: [[u8; 4]; 2],
    ) -> Result<(), UsbError> {
        for command in dscope_adc_fine_gain_commands(gain_codes) {
            for value in command {
                self.write_control(I2C_REGISTER_DESTINATION, ANALOG_CONTROL_REGISTER, &[value])?;
            }
        }
        Ok(())
    }

    pub(crate) fn write_dso_command(&self, command: u64) -> Result<(), UsbError> {
        self.write_control(I2C_DSO_DESTINATION, 0, &command.to_le_bytes())
    }

    pub(crate) fn set_dscope_bandwidth_limit(
        &self,
        limit: crate::dscope::DsCopeBandwidthLimit,
    ) -> Result<(), UsbError> {
        let pulse = match limit {
            crate::dscope::DsCopeBandwidthLimit::Full => 1 << 6,
            crate::dscope::DsCopeBandwidthLimit::Mhz20 => 1 << 5,
        };
        self.write_control(I2C_REGISTER_DESTINATION, CONTROL_REGISTER_ZERO, &[pulse])
    }

    pub(crate) fn read_dscope_nvm(&self, address: u16, data: &mut [u8]) -> Result<(), UsbError> {
        self.read_control(NVM_DESTINATION, address, data)
    }

    pub(crate) fn write_dscope_nvm(&self, address: u16, data: &[u8]) -> Result<(), UsbError> {
        self.write_control(NVM_DESTINATION, address, data)
    }

    pub(crate) fn set_dscope_nvm_write_enabled(&self, enabled: bool) -> Result<(), UsbError> {
        self.write_control(
            I2C_REGISTER_DESTINATION,
            CONTROL_REGISTER_ZERO,
            &[u8::from(enabled)],
        )
    }

    pub(crate) fn configure_dscope_channels(
        &self,
        enabled_channels: [bool; 2],
    ) -> Result<(), UsbError> {
        let channel_zero = if enabled_channels[0] {
            1 << 7
        } else {
            !(1 << 7)
        };
        let channel_one = if enabled_channels[1] {
            1 << 5
        } else {
            !(1 << 5)
        };
        self.write_control(DSO_CHANNEL_ZERO_ENABLE_DESTINATION, 0, &[channel_zero])?;
        self.write_control(DSO_CHANNEL_ONE_ENABLE_DESTINATION, 0, &[channel_one])
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

        self.write_bulk_exact(bitstream, "write the FPGA bitstream")?;

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

    fn write_bulk_exact(&self, data: &[u8], action: &str) -> Result<(), UsbError> {
        let written = self
            .handle
            .write_bulk(crate::BULK_OUT_ENDPOINT, data, BULK_TIMEOUT)
            .map_err(|error| usb_error(&self.device, action, error))?;
        if written != data.len() {
            return Err(UsbError::new(format!(
                "short bulk write for {}: sent {written} of {} bytes while trying to {action}",
                self.device.location.stable_id(),
                data.len()
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

fn dscope_adc_fine_gain_commands(gain_codes: [[u8; 4]; 2]) -> [[u8; 4]; 4] {
    std::array::from_fn(|branch| {
        let channel = branch / 2;
        let pair = (branch % 2) * 2;
        [
            0,
            gain_codes[channel][pair],
            gain_codes[channel][pair + 1],
            0x34 + branch as u8,
        ]
    })
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

fn poll_bulk_read(
    mut read: impl FnMut(&mut [u8], Duration) -> Result<usize, rusb::Error>,
    buffer: &mut [u8],
    mut cancelled: impl FnMut() -> bool,
) -> Result<BulkReadOutcome, rusb::Error> {
    loop {
        if cancelled() {
            return Ok(BulkReadOutcome::Cancelled);
        }
        match read(buffer, BULK_READ_POLL_TIMEOUT) {
            Ok(bytes_read) => return Ok(BulkReadOutcome::Data(bytes_read)),
            Err(rusb::Error::Timeout | rusb::Error::Interrupted) => {}
            Err(error) => return Err(error),
        }
    }
}

fn usb_error(discovered: &DiscoveredDevice, action: &str, error: rusb::Error) -> UsbError {
    let guidance = match error {
        rusb::Error::Access => "permission denied; check the device access rules",
        rusb::Error::Busy => "the device or interface is already claimed by another process",
        rusb::Error::NoDevice => "the device was disconnected",
        rusb::Error::NotFound => "the requested interface or endpoint was not found",
        rusb::Error::NotSupported => "the host USB backend does not support this operation",
        rusb::Error::Timeout => "the device did not respond before the timeout",
        rusb::Error::Overflow => "the USB transfer exceeded the supplied buffer",
        rusb::Error::Pipe => "the USB endpoint stalled",
        rusb::Error::Other => {
            "the operating system rejected the operation; close other instrument software or reconnect the device"
        }
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
    use std::cell::Cell;
    use std::collections::VecDeque;

    #[test]
    fn encodes_control_read_header_in_protocol_order() {
        assert_eq!(control_read_header(0x12, 0x3456, 7), [0x12, 0x56, 0x34, 7]);
    }

    #[test]
    fn maps_dscope_factory_fine_gains_to_all_adc_branches() {
        assert_eq!(
            dscope_adc_fine_gain_commands([[1, 2, 3, 4], [5, 6, 7, 8]]),
            [
                [0, 1, 2, 0x34],
                [0, 3, 4, 0x35],
                [0, 5, 6, 0x36],
                [0, 7, 8, 0x37],
            ]
        );
    }

    #[test]
    fn decodes_hardware_status_bits() {
        let status = HardwareStatus::from_raw(
            GPIF_DONE_MASK
                | FPGA_DONE_MASK
                | FPGA_INITIALIZED_MASK
                | FIFO_OVERFLOW_MASK
                | SYSTEM_CLEARED_MASK,
        );

        assert!(status.gpif_done);
        assert!(status.fpga_configured);
        assert!(status.fpga_initialized);
        assert!(status.fifo_overflow);
        assert!(status.system_cleared);
    }

    #[test]
    fn leaves_clear_hardware_status_bits_false() {
        let status = HardwareStatus::from_raw(0);

        assert!(!status.gpif_done);
        assert!(!status.fpga_configured);
        assert!(!status.fpga_initialized);
        assert!(!status.fifo_overflow);
        assert!(!status.system_cleared);
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

    #[test]
    fn cancels_bulk_read_without_touching_usb() {
        let attempts = Cell::new(0);
        let mut buffer = [0_u8; 8];

        let outcome = poll_bulk_read(
            |_, _| {
                attempts.set(attempts.get() + 1);
                Ok(0)
            },
            &mut buffer,
            || true,
        )
        .expect("cancellation should succeed");

        assert_eq!(outcome, BulkReadOutcome::Cancelled);
        assert_eq!(attempts.get(), 0);
    }

    #[test]
    fn retries_transient_bulk_read_errors_before_data() {
        let mut results = VecDeque::from([
            Err(rusb::Error::Timeout),
            Err(rusb::Error::Interrupted),
            Ok(3),
        ]);
        let mut buffer = [0_u8; 8];

        let outcome = poll_bulk_read(
            |buffer, timeout| {
                assert_eq!(timeout, BULK_READ_POLL_TIMEOUT);
                let result = results.pop_front().expect("script should have a result");
                if result.is_ok() {
                    buffer[..3].copy_from_slice(&[1, 2, 3]);
                }
                result
            },
            &mut buffer,
            || false,
        )
        .expect("transient errors should be retried");

        assert_eq!(outcome, BulkReadOutcome::Data(3));
        assert_eq!(&buffer[..3], &[1, 2, 3]);
        assert!(results.is_empty());
    }

    #[test]
    fn notices_cancellation_after_bulk_timeout() {
        let cancellation_checks = Cell::new(0);
        let attempts = Cell::new(0);
        let mut buffer = [0_u8; 8];

        let outcome = poll_bulk_read(
            |_, _| {
                attempts.set(attempts.get() + 1);
                Err(rusb::Error::Timeout)
            },
            &mut buffer,
            || {
                cancellation_checks.set(cancellation_checks.get() + 1);
                cancellation_checks.get() >= 2
            },
        )
        .expect("cancellation should succeed");

        assert_eq!(outcome, BulkReadOutcome::Cancelled);
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn propagates_hard_bulk_read_errors() {
        let mut buffer = [0_u8; 8];

        let error = poll_bulk_read(|_, _| Err(rusb::Error::NoDevice), &mut buffer, || false)
            .expect_err("disconnect should fail");

        assert_eq!(error, rusb::Error::NoDevice);
    }
}
