use crate::transport::QueuedBulkReadOutcome;
use crate::{
    BulkReadOutcome, DeviceModel, DiscoveredDevice, UsbDeviceSession, UsbError, UsbSpeedClass,
    DSLOGIC_U3PRO16,
};
use dsview_core::{
    CaptureBatch, CaptureContext, CaptureError, CaptureSource, DeviceDescriptor, DeviceId,
    InstrumentKind, LogicBatch,
};
use std::time::{Duration, Instant};

const CHANNEL_COUNT: u8 = 16;
const HARDWARE_SAMPLE_RATE_HZ: u64 = 500_000_000;
const PRE_DIVIDER: u32 = 5;
const SAMPLE_ALIGNMENT: u64 = 1_024;
const ATOMIC_SAMPLES: usize = 64;
const BYTES_PER_CHANNEL_BLOCK: usize = 8;
const TRIGGER_HEADER_BYTES: usize = 1_024;
const TRIGGER_CHECK_ID: u32 = 0x5555_5555;
const SETTINGS_BYTES: usize = 372;
const CAPTURE_READ_BYTES: usize = 128 * 1_024;
const PREQUEUED_DATA_BYTES: usize = 1_024 * 1_024;
const MAX_SAMPLE_COUNT: u64 = 134_217_728;
const CAPTURE_COMPLETION_GRACE: Duration = Duration::from_secs(5);
const PAYLOAD_TRANSFER_GRACE: Duration = Duration::from_secs(5);

const CLOCK_TYPE_BIT: u16 = 1;
const CLOCK_EDGE_BIT: u16 = 2;
const RLE_MODE_BIT: u16 = 3;
const HALF_MODE_BIT: u16 = 5;
const QUARTER_MODE_BIT: u16 = 6;
const FILTER_BIT: u16 = 8;
const STREAM_MODE_BIT: u16 = 12;
const INTERNAL_TEST_BIT: u16 = 15;

pub const DSLOGIC_SAMPLE_RATES_HZ: &[u64] = &[
    1_000_000,
    2_000_000,
    4_000_000,
    5_000_000,
    10_000_000,
    20_000_000,
    25_000_000,
    50_000_000,
    100_000_000,
    125_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DsLogicOperationMode {
    Buffer,
    Stream,
    InternalTest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DsLogicStopOption {
    StopImmediately,
    UploadCapturedData,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DsLogicInputFilter {
    None,
    OneSampleClock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DsLogicCaptureConfig {
    sample_rate_hz: u64,
    sample_count: u64,
    operation_mode: DsLogicOperationMode,
    stop_option: DsLogicStopOption,
    threshold_mv: u16,
    input_filter: DsLogicInputFilter,
    rle_enabled: bool,
    external_clock: bool,
    falling_clock_edge: bool,
    enabled_channels: [bool; CHANNEL_COUNT as usize],
}

impl DsLogicCaptureConfig {
    pub fn new(sample_rate_hz: u64, sample_count: u64) -> Result<Self, CaptureError> {
        if !DSLOGIC_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            return Err(CaptureError::new(format!(
                "unsupported DSLogic U3Pro16 sample rate {sample_rate_hz} Hz"
            )));
        }
        if !(SAMPLE_ALIGNMENT..=MAX_SAMPLE_COUNT).contains(&sample_count) {
            return Err(CaptureError::new(format!(
                "DSLogic U3Pro16 sample count must be between {SAMPLE_ALIGNMENT} and {MAX_SAMPLE_COUNT}"
            )));
        }
        Ok(Self {
            sample_rate_hz,
            sample_count,
            operation_mode: DsLogicOperationMode::Buffer,
            stop_option: DsLogicStopOption::UploadCapturedData,
            threshold_mv: 1_000,
            input_filter: DsLogicInputFilter::None,
            rle_enabled: false,
            external_clock: false,
            falling_clock_edge: false,
            enabled_channels: [true; CHANNEL_COUNT as usize],
        })
    }

    pub fn with_operation_mode(mut self, operation_mode: DsLogicOperationMode) -> Self {
        self.operation_mode = operation_mode;
        self
    }

    pub fn with_stop_option(mut self, stop_option: DsLogicStopOption) -> Self {
        self.stop_option = stop_option;
        self
    }

    pub fn with_threshold_voltage(mut self, volts: f32) -> Result<Self, CaptureError> {
        if !volts.is_finite() || !(0.0..=5.0).contains(&volts) {
            return Err(CaptureError::new(
                "DSLogic threshold voltage must be between 0.0 V and 5.0 V",
            ));
        }
        self.threshold_mv = (volts * 1_000.0).round() as u16;
        Ok(self)
    }

    pub fn with_input_filter(mut self, input_filter: DsLogicInputFilter) -> Self {
        self.input_filter = input_filter;
        self
    }

    pub fn with_rle_enabled(mut self, enabled: bool) -> Self {
        self.rle_enabled = enabled;
        self
    }

    pub fn with_external_clock(mut self, enabled: bool) -> Self {
        self.external_clock = enabled;
        self
    }

    pub fn with_falling_clock_edge(mut self, enabled: bool) -> Self {
        self.falling_clock_edge = enabled;
        self
    }

    pub fn with_enabled_channels(
        mut self,
        enabled_channels: [bool; CHANNEL_COUNT as usize],
    ) -> Result<Self, CaptureError> {
        if !enabled_channels.iter().any(|enabled| *enabled) {
            return Err(CaptureError::new(
                "DSLogic capture requires at least one enabled channel",
            ));
        }
        self.enabled_channels = enabled_channels;
        Ok(self)
    }

    pub fn sample_rate_hz(self) -> u64 {
        self.sample_rate_hz
    }

    pub fn sample_count(self) -> u64 {
        self.sample_count
    }

    pub fn operation_mode(self) -> DsLogicOperationMode {
        self.operation_mode
    }

    pub fn stop_option(self) -> DsLogicStopOption {
        self.stop_option
    }

    pub fn threshold_voltage(self) -> f32 {
        f32::from(self.threshold_mv) / 1_000.0
    }

    pub fn input_filter(self) -> DsLogicInputFilter {
        self.input_filter
    }

    pub fn rle_enabled(self) -> bool {
        self.rle_enabled
    }

    pub fn external_clock(self) -> bool {
        self.external_clock
    }

    pub fn falling_clock_edge(self) -> bool {
        self.falling_clock_edge
    }

    pub fn enabled_channels(self) -> [bool; CHANNEL_COUNT as usize] {
        self.enabled_channels
    }

    pub fn enabled_channel_count(self) -> u8 {
        self.enabled_channels
            .iter()
            .filter(|enabled| **enabled)
            .count() as u8
    }

    pub fn maximum_sample_rate_hz(self, speed: UsbSpeedClass) -> u64 {
        let enabled_count = self.enabled_channel_count();
        let lower_eight_only = self.enabled_channels[8..].iter().all(|enabled| !enabled);
        match self.operation_mode {
            DsLogicOperationMode::Buffer => {
                if enabled_count <= 8 && lower_eight_only {
                    1_000_000_000
                } else {
                    500_000_000
                }
            }
            DsLogicOperationMode::Stream if speed >= UsbSpeedClass::SuperSpeed => {
                if enabled_count <= 3 && lower_eight_only {
                    1_000_000_000
                } else if enabled_count <= 6 {
                    500_000_000
                } else if enabled_count <= 12 {
                    250_000_000
                } else {
                    125_000_000
                }
            }
            DsLogicOperationMode::Stream => {
                if enabled_count <= 3 {
                    100_000_000
                } else if enabled_count <= 6 {
                    50_000_000
                } else if enabled_count <= 12 {
                    25_000_000
                } else {
                    20_000_000
                }
            }
            DsLogicOperationMode::InternalTest => 100_000_000,
        }
    }

    fn validate_for_speed(self, speed: UsbSpeedClass) -> Result<(), CaptureError> {
        let maximum = self.maximum_sample_rate_hz(speed);
        if self.sample_rate_hz > maximum {
            return Err(CaptureError::new(format!(
                "DSLogic sample rate {} Hz exceeds the {} Hz maximum for {} enabled channel(s) in {:?} mode",
                self.sample_rate_hz,
                maximum,
                self.enabled_channel_count(),
                self.operation_mode
            )));
        }
        Ok(())
    }

    fn armed_sample_count(self) -> u64 {
        self.sample_count.div_ceil(SAMPLE_ALIGNMENT) * SAMPLE_ALIGNMENT
    }

    fn encode_settings(self) -> Vec<u8> {
        let divider = HARDWARE_SAMPLE_RATE_HZ.div_ceil(self.sample_rate_hz) as u32;
        let divider_high = if divider >= PRE_DIVIDER {
            PRE_DIVIDER - 1
        } else {
            divider - 1
        } << 8;
        let divided = divider.div_ceil(PRE_DIVIDER);
        let armed_samples = self.armed_sample_count();
        let capture_count = armed_samples >> 4;

        let operation_mode = match self.operation_mode {
            DsLogicOperationMode::Buffer => 0,
            DsLogicOperationMode::Stream => 1 << STREAM_MODE_BIT,
            DsLogicOperationMode::InternalTest => 1 << INTERNAL_TEST_BIT,
        };
        let mode = operation_mode
            | (u16::from(self.external_clock) << CLOCK_TYPE_BIT)
            | (u16::from(self.falling_clock_edge) << CLOCK_EDGE_BIT)
            | (u16::from(self.rle_enabled) << RLE_MODE_BIT)
            | (u16::from(self.sample_rate_hz == 500_000_000) << HALF_MODE_BIT)
            | (u16::from(self.sample_rate_hz == 1_000_000_000) << QUARTER_MODE_BIT)
            | (u16::from(self.input_filter == DsLogicInputFilter::OneSampleClock) << FILTER_BIT);
        let enabled_channel_mask = self
            .enabled_channels
            .iter()
            .enumerate()
            .fold(0_u16, |mask, (channel, enabled)| {
                mask | (u16::from(*enabled) << channel)
            });

        let mut bytes = Vec::with_capacity(SETTINGS_BYTES);
        push_u32(&mut bytes, 0xf5a5_f5a5);
        push_u16(&mut bytes, 0x0001);
        push_u16(&mut bytes, mode);
        push_u16(&mut bytes, 0x0102);
        push_u16(&mut bytes, divided as u16);
        push_u16(&mut bytes, divider_high as u16 + (divided >> 16) as u16);
        push_u16(&mut bytes, 0x0302);
        push_u16(&mut bytes, capture_count as u16);
        push_u16(&mut bytes, (capture_count >> 16) as u16);
        push_u16(&mut bytes, 0x0502);
        push_u16(&mut bytes, ATOMIC_SAMPLES as u16);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0x0701);
        push_u16(&mut bytes, u16::from(self.enabled_channel_count()) << 8);
        push_u16(&mut bytes, 0x0802);
        push_u16(&mut bytes, armed_samples as u16);
        push_u16(&mut bytes, (armed_samples >> 16) as u16);
        push_u16(&mut bytes, 0x0a02);
        push_u16(&mut bytes, enabled_channel_mask);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0x0c01);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0x40a0);

        for _ in 0..2 {
            for _ in 0..16 {
                push_u16(&mut bytes, u16::MAX);
            }
        }
        for _ in 0..4 {
            for _ in 0..16 {
                push_u16(&mut bytes, 0);
            }
        }
        for _ in 0..2 {
            for _ in 0..16 {
                push_u16(&mut bytes, 2);
            }
        }
        for _ in 0..16 {
            push_u32(&mut bytes, 0);
        }
        push_u32(&mut bytes, 0xfa5a_fa5a);

        debug_assert_eq!(bytes.len(), SETTINGS_BYTES);
        bytes
    }
}

pub struct DsLogicCaptureSource {
    device: DiscoveredDevice,
    config: DsLogicCaptureConfig,
    descriptor: DeviceDescriptor,
}

impl DsLogicCaptureSource {
    pub fn new(
        device: DiscoveredDevice,
        config: DsLogicCaptureConfig,
    ) -> Result<Self, CaptureError> {
        if device.profile.model != DeviceModel::DsLogicU3Pro16 {
            return Err(CaptureError::new(format!(
                "{} is not a DSLogic U3Pro16 logic analyzer",
                device.location.stable_id()
            )));
        }
        config.validate_for_speed(device.speed)?;
        let descriptor = DeviceDescriptor {
            id: DeviceId::new(device.location.stable_id()),
            name: DSLOGIC_U3PRO16.display_name.to_owned(),
            kind: InstrumentKind::LogicAnalyzer,
        };
        Ok(Self {
            device,
            config,
            descriptor,
        })
    }
}

impl CaptureSource for DsLogicCaptureSource {
    fn descriptor(&self) -> DeviceDescriptor {
        self.descriptor.clone()
    }

    fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
        let session = UsbDeviceSession::open(&self.device).map_err(capture_error)?;
        let capture_result = run_capture(&session, self.config, &context);
        let close_result = session.close().map_err(capture_error);
        combine_results(capture_result, close_result, "release the USB interface")
    }
}

fn run_capture(
    session: &UsbDeviceSession,
    config: DsLogicCaptureConfig,
    context: &CaptureContext,
) -> Result<(), CaptureError> {
    validate_device_ready(session, config)?;
    session.stop_capture().map_err(capture_error)?;
    let capture_result = (|| {
        session
            .arm_capture(&config.encode_settings())
            .map_err(capture_error)?;
        let Some(prequeued) = read_prequeued_capture(session, config, context)? else {
            return Ok(());
        };
        read_capture(session, config, prequeued, context)
    })();

    if context.is_cancelled() || capture_result.is_err() {
        let _ = session.force_capture_ready();
    }
    let stop_result = session.stop_capture().map_err(capture_error);
    combine_results(capture_result, stop_result, "stop the capture engine")
}

fn validate_device_ready(
    session: &UsbDeviceSession,
    config: DsLogicCaptureConfig,
) -> Result<(), CaptureError> {
    let firmware = session.firmware_version().map_err(capture_error)?;
    if !firmware.is_compatible() {
        return Err(CaptureError::new(format!(
            "firmware {firmware} is incompatible; major version 2 is required"
        )));
    }
    let status = session.hardware_status().map_err(capture_error)?;
    if !status.fpga_configured {
        return Err(CaptureError::new(
            "the DSLogic FPGA is not configured; prepare the device before capture",
        ));
    }
    let hdl = session.hdl_version().map_err(capture_error)?;
    if !hdl.is_compatible() {
        return Err(CaptureError::new(format!(
            "HDL {hdl} is incompatible; version 0x0e is required"
        )));
    }
    session
        .initialize_dslogic_frontend(config.threshold_mv)
        .map_err(capture_error)?;
    Ok(())
}

fn read_capture(
    session: &UsbDeviceSession,
    config: DsLogicCaptureConfig,
    prequeued: PrequeuedCapture,
    context: &CaptureContext,
) -> Result<(), CaptureError> {
    let trigger = TriggerPosition::decode(&prequeued.header)?;
    let available_samples = trigger.available_samples(config.sample_count)?;
    let total_bytes = usize::try_from(available_samples)
        .ok()
        .and_then(|samples| samples.checked_mul(config.enabled_channel_count() as usize))
        .map(|bits| bits / 8)
        .ok_or_else(|| CaptureError::new("captured DSLogic payload is too large"))?;

    if prequeued.payload.len() > total_bytes {
        return Err(CaptureError::new(format!(
            "DSLogic returned {} prequeued payload bytes; expected at most {total_bytes}",
            prequeued.payload.len()
        )));
    }
    let cross_block_bytes = config.enabled_channel_count() as usize * BYTES_PER_CHANNEL_BLOCK;
    let read_buffer_bytes = CAPTURE_READ_BYTES / cross_block_bytes * cross_block_bytes;
    let mut wire_buffer = vec![0_u8; read_buffer_bytes];
    let mut bytes_remaining = total_bytes - prequeued.payload.len();
    let mut sample_offset = 0_u64;
    let mut sequence = 0_u64;
    if !prequeued.payload.is_empty() {
        emit_wire_batch(
            &prequeued.payload,
            config,
            prequeued.capture_origin,
            context,
            &mut sequence,
            &mut sample_offset,
        )?;
    }
    let conservative_transfer_time = Duration::from_secs_f64(total_bytes as f64 / 10_000_000.0);
    let payload_deadline = Instant::now() + conservative_transfer_time + PAYLOAD_TRANSFER_GRACE;
    while bytes_remaining > 0 {
        let bytes_to_read = bytes_remaining.min(wire_buffer.len());
        match read_exact_cancellable(
            session,
            &mut wire_buffer[..bytes_to_read],
            context,
            payload_deadline,
            !prequeued.upload_after_stop,
        )? {
            ReadOutcome::Complete => {}
            ReadOutcome::Cancelled => return Ok(()),
            ReadOutcome::TimedOut => {
                return Err(capture_timeout_error(session, "capture payload"));
            }
        }
        emit_wire_batch(
            &wire_buffer[..bytes_to_read],
            config,
            prequeued.capture_origin,
            context,
            &mut sequence,
            &mut sample_offset,
        )?;
        bytes_remaining -= bytes_to_read;
    }
    Ok(())
}

struct PrequeuedCapture {
    header: [u8; TRIGGER_HEADER_BYTES],
    payload: Vec<u8>,
    capture_origin: Duration,
    upload_after_stop: bool,
}

fn read_prequeued_capture(
    session: &UsbDeviceSession,
    config: DsLogicCaptureConfig,
    context: &CaptureContext,
) -> Result<Option<PrequeuedCapture>, CaptureError> {
    let expected_payload_bytes = usize::try_from(config.sample_count & !(SAMPLE_ALIGNMENT - 1))
        .ok()
        .and_then(|samples| samples.checked_mul(config.enabled_channel_count() as usize))
        .map(|bits| bits / 8)
        .ok_or_else(|| CaptureError::new("requested DSLogic payload is too large"))?;
    let cross_block_bytes = config.enabled_channel_count() as usize * BYTES_PER_CHANNEL_BLOCK;
    let prequeued_data_bytes = PREQUEUED_DATA_BYTES / cross_block_bytes * cross_block_bytes;
    let mut header = [0_u8; TRIGGER_HEADER_BYTES];
    let mut payload = vec![0_u8; expected_payload_bytes.min(prequeued_data_bytes)];
    let capture_time =
        Duration::from_secs_f64(config.sample_count as f64 / config.sample_rate_hz as f64);
    let deadline = Instant::now() + capture_time + CAPTURE_COMPLETION_GRACE;
    let mut header_read = session
        .queue_bulk_read(header.len())
        .map_err(capture_error)?;
    let mut payload_read = match session.queue_bulk_read(payload.len()) {
        Ok(read) => read,
        Err(error) => {
            header_read.cancel().map_err(capture_error)?;
            while !header_read.is_complete() {
                session
                    .handle_usb_events(Duration::from_millis(5))
                    .map_err(capture_error)?;
            }
            let _ = header_read.finish();
            return Err(capture_error(error));
        }
    };

    context.synchronize_start()?;
    let capture_origin = context.elapsed();
    if let Err(error) = session.start_capture() {
        let _ = header_read.cancel();
        let _ = payload_read.cancel();
        while !header_read.is_complete() || !payload_read.is_complete() {
            session
                .handle_usb_events(Duration::from_millis(5))
                .map_err(capture_error)?;
        }
        let _ = header_read.finish();
        let _ = payload_read.finish();
        return Err(capture_error(error));
    }

    let mut cancelled = false;
    let mut upload_after_stop = false;
    let mut timeout_error = None;
    while !header_read.is_complete() || !payload_read.is_complete() {
        if !cancelled && !upload_after_stop && timeout_error.is_none() && context.is_cancelled() {
            match config.stop_option {
                DsLogicStopOption::StopImmediately => {
                    cancelled = true;
                    let _ = session.force_capture_ready();
                    header_read.cancel().map_err(capture_error)?;
                    payload_read.cancel().map_err(capture_error)?;
                }
                DsLogicStopOption::UploadCapturedData => {
                    upload_after_stop = true;
                    session.force_capture_stop().map_err(capture_error)?;
                }
            }
        } else if !cancelled && timeout_error.is_none() && Instant::now() >= deadline {
            timeout_error = Some(capture_timeout_error(session, "queued capture transfers"));
            let _ = session.force_capture_ready();
            header_read.cancel().map_err(capture_error)?;
            payload_read.cancel().map_err(capture_error)?;
        }
        session
            .handle_usb_events(Duration::from_millis(5))
            .map_err(capture_error)?;
    }

    let header_result = header_read.finish().map_err(capture_error);
    let payload_result = payload_read.finish().map_err(capture_error);
    if cancelled {
        return Ok(None);
    }
    if let Some(error) = timeout_error {
        return Err(error);
    }
    let header_data = match header_result? {
        QueuedBulkReadOutcome::Data(data) => data,
        QueuedBulkReadOutcome::Cancelled => {
            return Err(CaptureError::new(
                "DSLogic trigger-header transfer was unexpectedly cancelled",
            ));
        }
    };
    if header_data.len() != TRIGGER_HEADER_BYTES {
        return Err(CaptureError::new(format!(
            "short DSLogic trigger header: received {} of {TRIGGER_HEADER_BYTES} bytes",
            header_data.len()
        )));
    }
    header.copy_from_slice(&header_data);
    payload = match payload_result? {
        QueuedBulkReadOutcome::Data(data) => data,
        QueuedBulkReadOutcome::Cancelled => {
            return Err(CaptureError::new(
                "DSLogic payload transfer was unexpectedly cancelled",
            ));
        }
    };
    Ok(Some(PrequeuedCapture {
        header,
        payload,
        capture_origin,
        upload_after_stop,
    }))
}

fn emit_wire_batch(
    bytes: &[u8],
    config: DsLogicCaptureConfig,
    capture_origin: Duration,
    context: &CaptureContext,
    sequence: &mut u64,
    sample_offset: &mut u64,
) -> Result<(), CaptureError> {
    let samples = decode_cross_samples(bytes, config.enabled_channels)?;
    let captured_at = capture_origin
        + Duration::from_secs_f64(*sample_offset as f64 / config.sample_rate_hz as f64);
    *sample_offset += samples.len() as u64;
    context.emit(
        *sequence,
        captured_at,
        CaptureBatch::Logic(LogicBatch {
            sample_rate_hz: config.sample_rate_hz,
            channel_count: CHANNEL_COUNT,
            samples,
        }),
    )?;
    *sequence += 1;
    Ok(())
}

fn read_exact_cancellable(
    session: &UsbDeviceSession,
    mut buffer: &mut [u8],
    context: &CaptureContext,
    deadline: Instant,
    honor_cancellation: bool,
) -> Result<ReadOutcome, CaptureError> {
    while !buffer.is_empty() {
        match session
            .read_bulk_cancellable(buffer, || {
                (honor_cancellation && context.is_cancelled()) || Instant::now() >= deadline
            })
            .map_err(capture_error)?
        {
            BulkReadOutcome::Data(0) => {
                return Err(CaptureError::new(
                    "the DSLogic bulk endpoint returned an empty transfer",
                ));
            }
            BulkReadOutcome::Data(bytes_read) => {
                let (_, remaining) = buffer.split_at_mut(bytes_read);
                buffer = remaining;
            }
            BulkReadOutcome::Cancelled if honor_cancellation && context.is_cancelled() => {
                return Ok(ReadOutcome::Cancelled);
            }
            BulkReadOutcome::Cancelled => return Ok(ReadOutcome::TimedOut),
        }
    }
    Ok(ReadOutcome::Complete)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadOutcome {
    Complete,
    Cancelled,
    TimedOut,
}

fn capture_timeout_error(session: &UsbDeviceSession, phase: &str) -> CaptureError {
    let status = session.hardware_status();
    let progress = session.capture_progress();
    match (status, progress) {
        (Ok(status), Ok(progress)) => CaptureError::new(format!(
            "timed out waiting for the DSLogic {phase} (hardware status 0x{:02x}, capture progress 0x{progress:08x})",
            status.raw
        )),
        (Ok(status), Err(progress_error)) => CaptureError::new(format!(
            "timed out waiting for the DSLogic {phase} (hardware status 0x{:02x}; failed to read capture progress: {progress_error})",
            status.raw
        )),
        (Err(status_error), Ok(progress)) => CaptureError::new(format!(
            "timed out waiting for the DSLogic {phase} (capture progress 0x{progress:08x}; failed to read hardware status: {status_error})"
        )),
        (Err(status_error), Err(progress_error)) => CaptureError::new(format!(
            "timed out waiting for the DSLogic {phase}; failed to read hardware status ({status_error}) or capture progress ({progress_error})"
        )),
    }
}

fn decode_cross_samples(
    bytes: &[u8],
    enabled_channels: [bool; CHANNEL_COUNT as usize],
) -> Result<Vec<u32>, CaptureError> {
    let enabled_channel_count = enabled_channels.iter().filter(|enabled| **enabled).count();
    if enabled_channel_count == 0 {
        return Err(CaptureError::new(
            "cannot decode DSLogic data without an enabled channel",
        ));
    }
    let cross_block_bytes = enabled_channel_count * BYTES_PER_CHANNEL_BLOCK;
    if bytes.len() % cross_block_bytes != 0 {
        return Err(CaptureError::new(format!(
            "DSLogic payload length {} is not aligned to {cross_block_bytes} bytes",
            bytes.len()
        )));
    }

    let mut samples = Vec::with_capacity(bytes.len() * 8 / enabled_channel_count);
    for block in bytes.chunks_exact(cross_block_bytes) {
        let mut channels = [0_u64; CHANNEL_COUNT as usize];
        for (channel, word) in enabled_channels
            .iter()
            .enumerate()
            .filter_map(|(channel, enabled)| enabled.then_some(channel))
            .zip(block.chunks_exact(BYTES_PER_CHANNEL_BLOCK))
        {
            channels[channel] = u64::from_le_bytes(word.try_into().expect("chunk size is fixed"));
        }
        for bit in 0..ATOMIC_SAMPLES {
            let mut sample = 0_u32;
            for (channel, bits) in channels.iter().enumerate() {
                sample |= (((bits >> bit) & 1) as u32) << channel;
            }
            samples.push(sample);
        }
    }
    Ok(samples)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TriggerPosition {
    remaining_samples: u64,
}

impl TriggerPosition {
    fn decode(header: &[u8]) -> Result<Self, CaptureError> {
        if header.len() != TRIGGER_HEADER_BYTES {
            return Err(CaptureError::new(format!(
                "DSLogic trigger header has {} bytes; expected {TRIGGER_HEADER_BYTES}",
                header.len()
            )));
        }
        if read_u32(header, 0) != TRIGGER_CHECK_ID {
            return Err(CaptureError::new("invalid DSLogic trigger header check ID"));
        }
        let remaining_low = u64::from(read_u32(header, 12));
        let remaining_high = u64::from(read_u32(header, 16));
        Ok(Self {
            remaining_samples: (remaining_high << 32) | remaining_low,
        })
    }

    fn available_samples(self, requested_samples: u64) -> Result<u64, CaptureError> {
        if self.remaining_samples >= requested_samples {
            return Err(CaptureError::new(format!(
                "DSLogic reported no captured samples ({} of {requested_samples} remain)",
                self.remaining_samples
            )));
        }
        Ok((requested_samples - self.remaining_samples) & !(SAMPLE_ALIGNMENT - 1))
    }
}

fn combine_results(
    primary: Result<(), CaptureError>,
    cleanup: Result<(), CaptureError>,
    cleanup_action: &str,
) -> Result<(), CaptureError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(CaptureError::new(format!(
            "{primary}; additionally failed to {cleanup_action}: {cleanup}"
        ))),
    }
}

fn capture_error(error: UsbError) -> CaptureError {
    CaptureError::new(error.to_string())
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("fixed field size"),
    )
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("fixed field size"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_finite_capture_configuration() {
        assert!(DsLogicCaptureConfig::new(1_000_000, 1_000_000).is_ok());
        assert!(DsLogicCaptureConfig::new(3_000_000, 1_000_000).is_err());
        assert!(DsLogicCaptureConfig::new(1_000_000, 0).is_err());
        assert!(DsLogicCaptureConfig::new(1_000_000, MAX_SAMPLE_COUNT + 1).is_err());
        assert!(DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .expect("base configuration should be valid")
            .with_threshold_voltage(f32::NAN)
            .is_err());
        assert!(DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .expect("base configuration should be valid")
            .with_threshold_voltage(5.1)
            .is_err());
    }

    #[test]
    fn encodes_original_packed_fpga_settings_layout() {
        let settings = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .expect("configuration should be valid")
            .encode_settings();

        assert_eq!(settings.len(), SETTINGS_BYTES);
        assert_eq!(read_u32(&settings, 0), 0xf5a5_f5a5);
        assert_eq!(read_u16(&settings, 4), 0x0001);
        assert_eq!(read_u16(&settings, 6), 0);
        assert_eq!(read_u16(&settings, 8), 0x0102);
        assert_eq!(read_u16(&settings, 10), 100);
        assert_eq!(read_u16(&settings, 12), 0x0400);
        assert_eq!(read_u16(&settings, 14), 0x0302);
        assert_eq!(read_u16(&settings, 26), 0x0701);
        assert_eq!(read_u16(&settings, 28), 0x1000);
        assert_eq!(read_u16(&settings, 38), u16::MAX);
        assert_eq!(read_u16(&settings, 46), 0x40a0);
        assert_eq!(read_u16(&settings, 48), u16::MAX);
        assert_eq!(read_u16(&settings, 304), 0);
        assert_eq!(read_u32(&settings, 368), 0xfa5a_fa5a);
    }

    #[test]
    fn encodes_dslogic_device_options_in_mode_word() {
        let settings = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_stop_option(DsLogicStopOption::StopImmediately)
            .with_threshold_voltage(2.5)
            .expect("threshold should be valid")
            .with_input_filter(DsLogicInputFilter::OneSampleClock)
            .with_rle_enabled(true)
            .with_external_clock(true)
            .with_falling_clock_edge(true);

        assert_eq!(
            read_u16(&settings.encode_settings(), 6),
            (1 << STREAM_MODE_BIT)
                | (1 << FILTER_BIT)
                | (1 << RLE_MODE_BIT)
                | (1 << CLOCK_TYPE_BIT)
                | (1 << CLOCK_EDGE_BIT)
        );
        assert_eq!(settings.stop_option(), DsLogicStopOption::StopImmediately);
        assert_eq!(settings.threshold_voltage(), 2.5);
    }

    #[test]
    fn encodes_internal_test_operation_mode() {
        let settings = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::InternalTest)
            .encode_settings();

        assert_eq!(read_u16(&settings, 6), 1 << INTERNAL_TEST_BIT);
    }

    #[test]
    fn encodes_enabled_channel_count_mask_and_high_rate_modes() {
        let mut enabled_channels = [false; CHANNEL_COUNT as usize];
        enabled_channels[2] = true;
        enabled_channels[9] = true;
        let settings = DsLogicCaptureConfig::new(500_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_enabled_channels(enabled_channels)
            .expect("channel selection should be valid")
            .encode_settings();

        assert_eq!(read_u16(&settings, 6), 1 << HALF_MODE_BIT);
        assert_eq!(read_u16(&settings, 28), 2 << 8);
        assert_eq!(read_u16(&settings, 38), (1 << 2) | (1 << 9));

        let gigahertz = DsLogicCaptureConfig::new(1_000_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_enabled_channels([
                true, true, true, true, true, true, true, true, false, false, false, false, false,
                false, false, false,
            ])
            .expect("channel selection should be valid")
            .encode_settings();
        assert_eq!(read_u16(&gigahertz, 6), 1 << QUARTER_MODE_BIT);
    }

    #[test]
    fn channel_selection_matches_original_u3pro16_rate_limits() {
        let all_channels = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::Stream);
        assert_eq!(
            all_channels.maximum_sample_rate_hz(UsbSpeedClass::SuperSpeed),
            125_000_000
        );
        assert_eq!(
            all_channels.maximum_sample_rate_hz(UsbSpeedClass::HighSpeed),
            20_000_000
        );

        let mut six_channels = [false; CHANNEL_COUNT as usize];
        six_channels[..6].fill(true);
        let six_channels = all_channels
            .with_enabled_channels(six_channels)
            .expect("channel selection should be valid");
        assert_eq!(
            six_channels.maximum_sample_rate_hz(UsbSpeedClass::SuperSpeed),
            500_000_000
        );

        let mut three_channels = [false; CHANNEL_COUNT as usize];
        three_channels[..3].fill(true);
        let three_channels = all_channels
            .with_enabled_channels(three_channels)
            .expect("channel selection should be valid");
        assert_eq!(
            three_channels.maximum_sample_rate_hz(UsbSpeedClass::SuperSpeed),
            1_000_000_000
        );

        let lower_eight = [
            true, true, true, true, true, true, true, true, false, false, false, false, false,
            false, false, false,
        ];
        let buffer = all_channels
            .with_operation_mode(DsLogicOperationMode::Buffer)
            .with_enabled_channels(lower_eight)
            .expect("channel selection should be valid");
        assert_eq!(
            buffer.maximum_sample_rate_hz(UsbSpeedClass::HighSpeed),
            1_000_000_000
        );
    }

    #[test]
    fn rejects_an_empty_channel_selection() {
        assert!(DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_enabled_channels([false; CHANNEL_COUNT as usize])
            .is_err());
    }

    #[test]
    fn transposes_cross_channel_bitmaps_into_sample_words() {
        let mut bytes = [0_u8; CHANNEL_COUNT as usize * BYTES_PER_CHANNEL_BLOCK];
        bytes[0..8].copy_from_slice(&(1_u64 | (1_u64 << 63)).to_le_bytes());
        bytes[7 * 8..8 * 8].copy_from_slice(&u64::MAX.to_le_bytes());
        bytes[15 * 8..16 * 8].copy_from_slice(&(1_u64 << 1).to_le_bytes());

        let samples = decode_cross_samples(&bytes, [true; CHANNEL_COUNT as usize])
            .expect("payload should decode");

        assert_eq!(samples.len(), 64);
        assert_eq!(samples[0], (1 << 0) | (1 << 7));
        assert_eq!(samples[1], (1 << 7) | (1 << 15));
        assert_eq!(samples[2], 1 << 7);
        assert_eq!(samples[63], (1 << 0) | (1 << 7));
    }

    #[test]
    fn maps_sparse_wire_channels_back_to_physical_channel_bits() {
        let mut enabled_channels = [false; CHANNEL_COUNT as usize];
        enabled_channels[2] = true;
        enabled_channels[9] = true;
        let mut bytes = [0_u8; 2 * BYTES_PER_CHANNEL_BLOCK];
        bytes[0..8].copy_from_slice(&1_u64.to_le_bytes());
        bytes[8..16].copy_from_slice(&(1_u64 << 1).to_le_bytes());

        let samples =
            decode_cross_samples(&bytes, enabled_channels).expect("payload should decode");

        assert_eq!(samples.len(), ATOMIC_SAMPLES);
        assert_eq!(samples[0], 1 << 2);
        assert_eq!(samples[1], 1 << 9);
        assert!(samples[2..].iter().all(|sample| *sample == 0));
    }

    #[test]
    fn parses_trigger_header_and_aligns_available_samples() {
        let mut header = [0_u8; TRIGGER_HEADER_BYTES];
        header[0..4].copy_from_slice(&TRIGGER_CHECK_ID.to_le_bytes());
        header[12..16].copy_from_slice(&64_u32.to_le_bytes());

        let trigger = TriggerPosition::decode(&header).expect("header should decode");

        assert_eq!(trigger.remaining_samples, 64);
        assert_eq!(trigger.available_samples(1_000_000).unwrap(), 999_424);
    }

    #[test]
    fn rejects_malformed_or_empty_trigger_results() {
        let header = [0_u8; TRIGGER_HEADER_BYTES];
        assert!(TriggerPosition::decode(&header).is_err());

        let trigger = TriggerPosition {
            remaining_samples: 1_000_000,
        };
        assert!(trigger.available_samples(1_000_000).is_err());
    }
}
