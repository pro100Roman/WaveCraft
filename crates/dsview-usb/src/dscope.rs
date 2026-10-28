use crate::transport::QueuedBulkReadOutcome;
use crate::{DeviceModel, DiscoveredDevice, UsbDeviceSession, UsbError, DSCOPE_U3P100};
use dsview_core::{
    CaptureBatch, CaptureContext, CaptureError, CaptureSource, DeviceDescriptor, DeviceId,
    InstrumentKind, ScopeBatch,
};
use std::time::{Duration, Instant};

const CHANNEL_COUNT: usize = 2;
const SAMPLE_RATE_HZ: u64 = 1_000_000;
const HARDWARE_SAMPLE_RATE_HZ: u64 = 1_000_000_000;
const SAMPLE_ALIGNMENT: u64 = 1_024;
const MAX_SAMPLE_COUNT: u64 = 1_000_000;
const SETTINGS_BYTES: usize = 372;
const TRIGGER_HEADER_BYTES: usize = 1_024;
const MEASUREMENT_FOOTER_BYTES: usize = 1_024;
const TRIGGER_CHECK_ID: u32 = 0x5555_5555;
const DSO_PACKET_ID: u16 = 0xa500;
const SCOPE_BATCH_SAMPLES: usize = 65_536;
const CAPTURE_COMPLETION_GRACE: Duration = Duration::from_secs(5);
const VOLTAGE_RANGE: f32 = 10.0;
const ADC_REFERENCE_SPAN: f32 = 235.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DsCopeCaptureConfig {
    sample_rate_hz: u64,
    sample_count: u64,
}

impl DsCopeCaptureConfig {
    pub fn new(sample_rate_hz: u64, sample_count: u64) -> Result<Self, CaptureError> {
        if sample_rate_hz != SAMPLE_RATE_HZ {
            return Err(CaptureError::new(format!(
                "the initial DSCope U3P100 driver supports only {SAMPLE_RATE_HZ} Hz"
            )));
        }
        if !(SAMPLE_ALIGNMENT..=MAX_SAMPLE_COUNT).contains(&sample_count) {
            return Err(CaptureError::new(format!(
                "DSCope U3P100 sample count must be between {SAMPLE_ALIGNMENT} and {MAX_SAMPLE_COUNT}"
            )));
        }
        Ok(Self {
            sample_rate_hz,
            sample_count,
        })
    }

    pub fn sample_rate_hz(self) -> u64 {
        self.sample_rate_hz
    }

    pub fn sample_count(self) -> u64 {
        self.sample_count
    }

    fn armed_sample_count(self) -> u64 {
        self.sample_count.div_ceil(SAMPLE_ALIGNMENT) * SAMPLE_ALIGNMENT
    }

    fn divider(self) -> u32 {
        HARDWARE_SAMPLE_RATE_HZ.div_ceil(self.sample_rate_hz * CHANNEL_COUNT as u64) as u32
    }

    fn encode_settings(self) -> Vec<u8> {
        let armed_samples = self.armed_sample_count();
        let capture_count = armed_samples >> 4;
        let divider = self.divider();
        let mut bytes = Vec::with_capacity(SETTINGS_BYTES);
        push_u32(&mut bytes, 0xf5a5_f5a5);
        push_u16(&mut bytes, 0x0001);
        push_u16(&mut bytes, 1 << 4);
        push_u16(&mut bytes, 0x0102);
        push_u16(&mut bytes, divider as u16);
        push_u16(&mut bytes, (divider >> 16) as u16);
        push_u16(&mut bytes, 0x0302);
        push_u16(&mut bytes, capture_count as u16);
        push_u16(&mut bytes, (capture_count >> 16) as u16);
        push_u16(&mut bytes, 0x0502);
        push_u16(&mut bytes, 64);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0x0701);
        push_u16(&mut bytes, (CHANNEL_COUNT as u16) << 8);
        push_u16(&mut bytes, 0x0802);
        push_u16(&mut bytes, self.sample_count as u16);
        push_u16(&mut bytes, (self.sample_count >> 16) as u16);
        push_u16(&mut bytes, 0x0a02);
        push_u16(&mut bytes, 0b11);
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

pub struct DsCopeCaptureSource {
    device: DiscoveredDevice,
    config: DsCopeCaptureConfig,
    descriptor: DeviceDescriptor,
}

impl DsCopeCaptureSource {
    pub fn new(
        device: DiscoveredDevice,
        config: DsCopeCaptureConfig,
    ) -> Result<Self, CaptureError> {
        if device.profile.model != DeviceModel::DsCopeU3P100 {
            return Err(CaptureError::new(format!(
                "{} is not a DSCope U3P100 oscilloscope",
                device.location.stable_id()
            )));
        }
        let descriptor = DeviceDescriptor {
            id: DeviceId::new(device.location.stable_id()),
            name: DSCOPE_U3P100.display_name.to_owned(),
            kind: InstrumentKind::Oscilloscope,
        };
        Ok(Self {
            device,
            config,
            descriptor,
        })
    }
}

impl CaptureSource for DsCopeCaptureSource {
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
    config: DsCopeCaptureConfig,
    context: &CaptureContext,
) -> Result<(), CaptureError> {
    validate_device_ready(session)?;
    session.stop_capture().map_err(capture_error)?;
    let capture_result = (|| {
        session
            .initialize_dscope_frontend()
            .map_err(capture_error)?;
        session.enable_dscope_channels().map_err(capture_error)?;
        configure_scope(session, config)?;
        session
            .arm_capture(&config.encode_settings())
            .map_err(capture_error)?;
        configure_scope_post_arm(session, config)?;
        let Some(capture) = read_prequeued_capture(session, config, context)? else {
            return Ok(());
        };
        decode_capture(capture, config, context)
    })();

    if context.is_cancelled() || capture_result.is_err() {
        let _ = session.force_capture_ready();
    }
    let stop_result = session.stop_capture().map_err(capture_error);
    combine_results(capture_result, stop_result, "stop the capture engine")
}

fn validate_device_ready(session: &UsbDeviceSession) -> Result<(), CaptureError> {
    let firmware = session.firmware_version().map_err(capture_error)?;
    if !firmware.is_compatible() {
        return Err(CaptureError::new(format!(
            "firmware {firmware} is incompatible; major version 2 is required"
        )));
    }
    let status = session.hardware_status().map_err(capture_error)?;
    if !status.fpga_configured {
        return Err(CaptureError::new(
            "the DSCope FPGA is not configured; prepare the device before capture",
        ));
    }
    let hdl = session.hdl_version().map_err(capture_error)?;
    if !hdl.is_compatible() {
        return Err(CaptureError::new(format!(
            "HDL {hdl} is incompatible; version 0x0e is required"
        )));
    }
    Ok(())
}

fn configure_scope(
    session: &UsbDeviceSession,
    config: DsCopeCaptureConfig,
) -> Result<(), CaptureError> {
    for command in dso_commands(config) {
        session.write_dso_command(command).map_err(capture_error)?;
    }
    Ok(())
}

fn configure_scope_post_arm(
    session: &UsbDeviceSession,
    config: DsCopeCaptureConfig,
) -> Result<(), CaptureError> {
    let commands = dso_commands(config);
    for index in [8, 3, 4, 5, 6] {
        session
            .write_dso_command(commands[index])
            .map_err(capture_error)?;
    }
    Ok(())
}

fn dso_commands(config: DsCopeCaptureConfig) -> [u64; 14] {
    let nominal_offset = (128_u64 << 32) + (128_u64 * 780 / 255) + 60;
    [
        0xa5a5_a500,
        0x0f00,
        0x0f80,
        0x0006_9d08,
        0x0006_9d88,
        0x10 + (nominal_offset << 8),
        0x90 + (nominal_offset << 8),
        0x18 + (u64::from(config.divider()) << 8),
        0x20,
        0x58,
        0x28,
        0x30,
        0x0080_8038,
        0x0840,
    ]
}

struct RawScopeCapture {
    trigger_header: Vec<u8>,
    payload: Vec<u8>,
    capture_origin: Duration,
}

fn read_prequeued_capture(
    session: &UsbDeviceSession,
    config: DsCopeCaptureConfig,
    context: &CaptureContext,
) -> Result<Option<RawScopeCapture>, CaptureError> {
    let payload_bytes = usize::try_from(config.armed_sample_count())
        .ok()
        .and_then(|samples| samples.checked_mul(CHANNEL_COUNT))
        .and_then(|bytes| bytes.checked_add(MEASUREMENT_FOOTER_BYTES))
        .ok_or_else(|| CaptureError::new("requested DSCope payload is too large"))?;
    let mut header_read = session
        .queue_bulk_read(TRIGGER_HEADER_BYTES)
        .map_err(capture_error)?;
    let mut payload_read = match session.queue_bulk_read(payload_bytes) {
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

    let capture_origin = context.elapsed();
    if let Err(error) = session.start_capture() {
        let _ = header_read.cancel();
        let _ = payload_read.cancel();
        drain_reads(session, &header_read, &payload_read)?;
        let _ = header_read.finish();
        let _ = payload_read.finish();
        return Err(capture_error(error));
    }

    let capture_time =
        Duration::from_secs_f64(config.sample_count as f64 / config.sample_rate_hz as f64);
    let deadline = Instant::now() + capture_time + CAPTURE_COMPLETION_GRACE;
    let mut cancelled = false;
    let mut timeout_error = None;
    while !header_read.is_complete() || !payload_read.is_complete() {
        if !cancelled && timeout_error.is_none() && context.is_cancelled() {
            cancelled = true;
            let _ = session.force_capture_ready();
            header_read.cancel().map_err(capture_error)?;
            payload_read.cancel().map_err(capture_error)?;
        } else if !cancelled && timeout_error.is_none() && Instant::now() >= deadline {
            timeout_error = Some(capture_timeout_error(session));
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
    let trigger_header = require_queued_data(header_result?, "trigger header")?;
    if trigger_header.len() != TRIGGER_HEADER_BYTES {
        return Err(CaptureError::new(format!(
            "short DSCope trigger header: received {} of {TRIGGER_HEADER_BYTES} bytes",
            trigger_header.len()
        )));
    }
    let payload = require_queued_data(payload_result?, "capture payload")?;
    if payload.len() != payload_bytes {
        return Err(CaptureError::new(format!(
            "short DSCope payload: received {} of {payload_bytes} bytes",
            payload.len()
        )));
    }
    Ok(Some(RawScopeCapture {
        trigger_header,
        payload,
        capture_origin,
    }))
}

fn drain_reads(
    session: &UsbDeviceSession,
    header_read: &crate::transport::QueuedBulkRead,
    payload_read: &crate::transport::QueuedBulkRead,
) -> Result<(), CaptureError> {
    while !header_read.is_complete() || !payload_read.is_complete() {
        session
            .handle_usb_events(Duration::from_millis(5))
            .map_err(capture_error)?;
    }
    Ok(())
}

fn require_queued_data(
    outcome: QueuedBulkReadOutcome,
    phase: &str,
) -> Result<Vec<u8>, CaptureError> {
    match outcome {
        QueuedBulkReadOutcome::Data(data) => Ok(data),
        QueuedBulkReadOutcome::Cancelled => Err(CaptureError::new(format!(
            "DSCope {phase} transfer was unexpectedly cancelled"
        ))),
    }
}

fn decode_capture(
    capture: RawScopeCapture,
    config: DsCopeCaptureConfig,
    context: &CaptureContext,
) -> Result<(), CaptureError> {
    validate_trigger_header(&capture.trigger_header)?;
    let measurement_offset = usize::try_from(config.armed_sample_count())
        .expect("validated sample count fits usize")
        * CHANNEL_COUNT;
    let measurement = ScopeMeasurement::decode(&capture.payload[measurement_offset..], config)?;
    let sample_count = usize::try_from(measurement.sample_count)
        .expect("32-bit measurement sample count fits usize");
    let data_start = usize::from(measurement.trigger_offset) * CHANNEL_COUNT;
    let data_bytes = sample_count
        .checked_mul(CHANNEL_COUNT)
        .and_then(|bytes| data_start.checked_add(bytes))
        .ok_or_else(|| CaptureError::new("DSCope sample window is too large"))?;
    if data_bytes > measurement_offset {
        return Err(CaptureError::new(format!(
            "DSCope sample window ends at byte {data_bytes}, beyond measurement footer at {measurement_offset}"
        )));
    }

    let samples = &capture.payload[data_start..data_bytes];
    for (sequence, chunk) in samples
        .chunks(SCOPE_BATCH_SAMPLES * CHANNEL_COUNT)
        .enumerate()
    {
        let frame_sample_offset = sequence * SCOPE_BATCH_SAMPLES;
        let mut channels = vec![
            Vec::with_capacity(chunk.len() / CHANNEL_COUNT),
            Vec::with_capacity(chunk.len() / CHANNEL_COUNT),
        ];
        for pair in chunk.chunks_exact(CHANNEL_COUNT) {
            channels[0].push(raw_to_volts(pair[0], measurement.hardware_offsets[0]));
            channels[1].push(raw_to_volts(pair[1], measurement.hardware_offsets[1]));
        }
        let captured_at = capture.capture_origin
            + Duration::from_secs_f64(frame_sample_offset as f64 / config.sample_rate_hz as f64);
        context.emit(
            sequence as u64,
            captured_at,
            CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: config.sample_rate_hz,
                channels,
            }),
        )?;
    }
    Ok(())
}

fn validate_trigger_header(header: &[u8]) -> Result<(), CaptureError> {
    if header.len() != TRIGGER_HEADER_BYTES || read_u32(header, 0) != TRIGGER_CHECK_ID {
        return Err(CaptureError::new("invalid DSCope trigger header"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeMeasurement {
    sample_count: u32,
    trigger_offset: u8,
    hardware_offsets: [u8; CHANNEL_COUNT],
}

impl ScopeMeasurement {
    fn decode(footer: &[u8], config: DsCopeCaptureConfig) -> Result<Self, CaptureError> {
        if footer.len() != MEASUREMENT_FOOTER_BYTES {
            return Err(CaptureError::new(format!(
                "DSCope measurement footer has {} bytes; expected {MEASUREMENT_FOOTER_BYTES}",
                footer.len()
            )));
        }
        if read_u16(footer, 0) != DSO_PACKET_ID {
            return Err(CaptureError::new("invalid DSCope measurement packet ID"));
        }
        let sample_count = read_u32(footer, 4) & 0x0fff_ffff;
        let divider = read_u32(footer, 8) & 0x00ff_ffff;
        if divider != config.divider() || sample_count == 0 {
            return Err(CaptureError::new(format!(
                "invalid DSCope measurement metadata: divider {divider}, samples {sample_count}"
            )));
        }
        Ok(Self {
            sample_count: sample_count.min(config.sample_count as u32),
            trigger_offset: footer[11] & 0x07,
            hardware_offsets: [footer[102], footer[166]],
        })
    }
}

fn raw_to_volts(raw: u8, hardware_offset: u8) -> f32 {
    (f32::from(hardware_offset) - f32::from(raw)) * VOLTAGE_RANGE / ADC_REFERENCE_SPAN
}

fn capture_timeout_error(session: &UsbDeviceSession) -> CaptureError {
    let status = session.hardware_status();
    let progress = session.capture_progress();
    match (status, progress) {
        (Ok(status), Ok(progress)) => CaptureError::new(format!(
            "timed out waiting for DSCope capture data (hardware status 0x{:02x}, capture progress 0x{progress:08x})",
            status.raw
        )),
        (Ok(status), Err(error)) => CaptureError::new(format!(
            "timed out waiting for DSCope capture data (hardware status 0x{:02x}; progress read failed: {error})",
            status.raw
        )),
        (Err(error), _) => CaptureError::new(format!(
            "timed out waiting for DSCope capture data; status read failed: {error}"
        )),
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
    fn validates_conservative_scope_configuration() {
        assert!(DsCopeCaptureConfig::new(SAMPLE_RATE_HZ, MAX_SAMPLE_COUNT).is_ok());
        assert!(DsCopeCaptureConfig::new(2_000_000, MAX_SAMPLE_COUNT).is_err());
        assert!(DsCopeCaptureConfig::new(SAMPLE_RATE_HZ, 1).is_err());
    }

    #[test]
    fn encodes_dso_mode_and_capture_counts() {
        let config = DsCopeCaptureConfig::new(SAMPLE_RATE_HZ, MAX_SAMPLE_COUNT).unwrap();
        let settings = config.encode_settings();

        assert_eq!(settings.len(), SETTINGS_BYTES);
        assert_eq!(read_u32(&settings, 0), 0xf5a5_f5a5);
        assert_eq!(read_u16(&settings, 6), 1 << 4);
        assert_eq!(read_u16(&settings, 10), 500);
        assert_eq!(read_u16(&settings, 28), 0x0200);
        assert_eq!(read_u16(&settings, 32), MAX_SAMPLE_COUNT as u16);
        assert_eq!(read_u16(&settings, 38), 0b11);
        assert_eq!(read_u32(&settings, 368), 0xfa5a_fa5a);
    }

    #[test]
    fn generates_nominal_two_channel_dso_commands() {
        let config = DsCopeCaptureConfig::new(SAMPLE_RATE_HZ, MAX_SAMPLE_COUNT).unwrap();
        let commands = dso_commands(config);

        assert_eq!(commands[0], 0xa5a5_a500);
        assert_eq!(commands[1], 0x0f00);
        assert_eq!(commands[2], 0x0f80);
        assert_eq!(commands[3], 0x0006_9d08);
        assert_eq!(commands[4], 0x0006_9d88);
        assert_eq!(commands[7], 0x0001_f418);
        assert_eq!(commands[12], 0x0080_8038);
    }

    #[test]
    fn parses_measurement_footer_and_converts_voltage() {
        let config = DsCopeCaptureConfig::new(SAMPLE_RATE_HZ, MAX_SAMPLE_COUNT).unwrap();
        let mut footer = [0_u8; MEASUREMENT_FOOTER_BYTES];
        footer[0..2].copy_from_slice(&DSO_PACKET_ID.to_le_bytes());
        footer[4..8].copy_from_slice(&100_u32.to_le_bytes());
        footer[8..12].copy_from_slice(&config.divider().to_le_bytes());
        footer[11] = 3;
        footer[102] = 128;
        footer[166] = 130;

        let measurement = ScopeMeasurement::decode(&footer, config).unwrap();

        assert_eq!(measurement.sample_count, 100);
        assert_eq!(measurement.trigger_offset, 3);
        assert_eq!(measurement.hardware_offsets, [128, 130]);
        assert_eq!(raw_to_volts(128, 128), 0.0);
        assert!(raw_to_volts(100, 128) > 1.19);
    }
}
