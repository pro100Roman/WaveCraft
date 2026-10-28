use crate::transport::QueuedBulkReadOutcome;
use crate::{DeviceModel, DiscoveredDevice, UsbDeviceSession, UsbError, DSCOPE_U3P100};
use dsview_core::{
    CaptureBatch, CaptureContext, CaptureError, CaptureSource, DeviceDescriptor, DeviceId,
    InstrumentKind, ScopeBatch,
};
use std::time::{Duration, Instant};

const CHANNEL_COUNT: usize = 2;
const HARDWARE_SAMPLE_RATE_HZ: u64 = 1_000_000_000;
const SAMPLE_ALIGNMENT: u64 = 1_024;
const MAX_TOTAL_SAMPLE_COUNT: u64 = 2_000_000;
const SETTINGS_BYTES: usize = 372;
const TRIGGER_HEADER_BYTES: usize = 1_024;
const MEASUREMENT_FOOTER_BYTES: usize = 1_024;
const TRIGGER_CHECK_ID: u32 = 0x5555_5555;
const DSO_PACKET_ID: u16 = 0xa500;
const SCOPE_BATCH_SAMPLES: usize = 65_536;
const CAPTURE_COMPLETION_GRACE: Duration = Duration::from_secs(5);
const ADC_REFERENCE_SPAN: f32 = 235.0;
const CALIBRATION_ZERO_BYTES: usize = 30;
const CALIBRATION_VGA_BYTES: usize = 17;
const CALIBRATION_FLASH_PAGE_STRIDE: u16 = 8;
const CALIBRATION_ID_BASE: u8 = 0x40;
const OFFSET_PWM_SPAN: u64 = 780;

pub const DSCOPE_VERTICAL_OFFSET_MIN: u16 = 10;
pub const DSCOPE_VERTICAL_OFFSET_CENTER: u16 = 128;
pub const DSCOPE_VERTICAL_OFFSET_MAX: u16 = 245;

pub const DSCOPE_SAMPLE_RATES_HZ: [u64; 17] = [
    100_000,
    200_000,
    400_000,
    500_000,
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
pub const DSCOPE_SAMPLE_COUNTS: [u64; 7] = [
    10_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 2_000_000,
];
pub const DSCOPE_VOLTS_PER_DIV_MV: [u16; 8] = [10, 20, 50, 100, 200, 500, 1_000, 2_000];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DsCopeCoupling {
    Dc,
    Ac,
}

impl DsCopeCoupling {
    fn command_bit(self) -> u64 {
        match self {
            Self::Dc => 0x100,
            Self::Ac => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DsCopeChannelConfig {
    enabled: bool,
    volts_per_div_mv: u16,
    coupling: DsCopeCoupling,
    probe_attenuation: u8,
    vertical_offset: u16,
}

impl Default for DsCopeChannelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            volts_per_div_mv: 1_000,
            coupling: DsCopeCoupling::Dc,
            probe_attenuation: 1,
            vertical_offset: DSCOPE_VERTICAL_OFFSET_CENTER,
        }
    }
}

impl DsCopeChannelConfig {
    pub fn new(
        volts_per_div_mv: u16,
        coupling: DsCopeCoupling,
        probe_attenuation: u8,
    ) -> Result<Self, CaptureError> {
        if !DSCOPE_VOLTS_PER_DIV_MV.contains(&volts_per_div_mv) {
            return Err(CaptureError::new(format!(
                "unsupported DSCope volts/div value: {volts_per_div_mv} mV"
            )));
        }
        if !matches!(probe_attenuation, 1 | 10) {
            return Err(CaptureError::new(format!(
                "unsupported DSCope probe attenuation: {probe_attenuation}x"
            )));
        }
        Ok(Self {
            enabled: true,
            volts_per_div_mv,
            coupling,
            probe_attenuation,
            vertical_offset: DSCOPE_VERTICAL_OFFSET_CENTER,
        })
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_vertical_offset(mut self, vertical_offset: u16) -> Result<Self, CaptureError> {
        if !(DSCOPE_VERTICAL_OFFSET_MIN..=DSCOPE_VERTICAL_OFFSET_MAX).contains(&vertical_offset) {
            return Err(CaptureError::new(format!(
                "DSCope vertical offset must be between {DSCOPE_VERTICAL_OFFSET_MIN} and {DSCOPE_VERTICAL_OFFSET_MAX}; got {vertical_offset}"
            )));
        }
        self.vertical_offset = vertical_offset;
        Ok(self)
    }

    pub fn enabled(self) -> bool {
        self.enabled
    }

    pub fn volts_per_div_mv(self) -> u16 {
        self.volts_per_div_mv
    }

    pub fn coupling(self) -> DsCopeCoupling {
        self.coupling
    }

    pub fn probe_attenuation(self) -> u8 {
        self.probe_attenuation
    }

    pub fn vertical_offset(self) -> u16 {
        self.vertical_offset
    }

    fn calibration_index(self) -> usize {
        DSCOPE_VOLTS_PER_DIV_MV
            .iter()
            .position(|candidate| *candidate == self.volts_per_div_mv)
            .expect("validated volts/div value is present")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DsCopeCaptureConfig {
    sample_rate_hz: u64,
    sample_count: u64,
    channels: [DsCopeChannelConfig; CHANNEL_COUNT],
}

impl DsCopeCaptureConfig {
    pub fn new(sample_rate_hz: u64, sample_count: u64) -> Result<Self, CaptureError> {
        Self::with_channel_configs(
            sample_rate_hz,
            sample_count,
            [DsCopeChannelConfig::default(); CHANNEL_COUNT],
        )
    }

    pub fn with_channel_configs(
        sample_rate_hz: u64,
        sample_count: u64,
        channels: [DsCopeChannelConfig; CHANNEL_COUNT],
    ) -> Result<Self, CaptureError> {
        if !DSCOPE_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            return Err(CaptureError::new(format!(
                "unsupported DSCope U3P100 sample rate: {sample_rate_hz} Hz"
            )));
        }
        let enabled_channel_count = channels.iter().filter(|channel| channel.enabled).count();
        if enabled_channel_count == 0 {
            return Err(CaptureError::new(
                "DSCope U3P100 capture requires at least one enabled channel",
            ));
        }
        let maximum_rate_hz = HARDWARE_SAMPLE_RATE_HZ / enabled_channel_count as u64;
        if sample_rate_hz > maximum_rate_hz {
            return Err(CaptureError::new(format!(
                "DSCope U3P100 supports at most {maximum_rate_hz} Hz with {enabled_channel_count} enabled channel(s)"
            )));
        }
        let maximum_sample_count = MAX_TOTAL_SAMPLE_COUNT / enabled_channel_count as u64;
        if !(SAMPLE_ALIGNMENT..=maximum_sample_count).contains(&sample_count) {
            return Err(CaptureError::new(format!(
                "DSCope U3P100 sample count must be between {SAMPLE_ALIGNMENT} and {maximum_sample_count} with {enabled_channel_count} enabled channel(s)"
            )));
        }
        Ok(Self {
            sample_rate_hz,
            sample_count,
            channels,
        })
    }

    pub fn sample_rate_hz(self) -> u64 {
        self.sample_rate_hz
    }

    pub fn sample_count(self) -> u64 {
        self.sample_count
    }

    pub fn channels(self) -> [DsCopeChannelConfig; CHANNEL_COUNT] {
        self.channels
    }

    pub fn enabled_channel_count(self) -> usize {
        self.channels
            .iter()
            .filter(|channel| channel.enabled)
            .count()
    }

    fn enabled_channel_indices(self) -> impl Iterator<Item = usize> {
        self.channels
            .into_iter()
            .enumerate()
            .filter_map(|(index, channel)| channel.enabled.then_some(index))
    }

    fn armed_sample_count(self) -> u64 {
        self.sample_count.div_ceil(SAMPLE_ALIGNMENT) * SAMPLE_ALIGNMENT
    }

    fn divider(self) -> u32 {
        HARDWARE_SAMPLE_RATE_HZ.div_ceil(self.sample_rate_hz * self.enabled_channel_count() as u64)
            as u32
    }

    fn encode_settings(self) -> Vec<u8> {
        let armed_samples = self.armed_sample_count();
        let channel_count = self.enabled_channel_count() as u64;
        let channel_ratio = CHANNEL_COUNT as u64 / channel_count;
        let capture_count = (armed_samples / channel_ratio) >> 4;
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
        push_u16(&mut bytes, (channel_count as u16) << 8);
        push_u16(&mut bytes, 0x0802);
        let dso_count = self.sample_count / channel_ratio;
        push_u16(&mut bytes, dso_count as u16);
        push_u16(&mut bytes, (dso_count >> 16) as u16);
        push_u16(&mut bytes, 0x0a02);
        let channel_mask = self
            .channels
            .iter()
            .enumerate()
            .fold(0_u16, |mask, (index, channel)| {
                mask | (u16::from(channel.enabled) << index)
            });
        push_u16(&mut bytes, channel_mask);
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

pub fn apply_dscope_configuration(
    device: &DiscoveredDevice,
    config: DsCopeCaptureConfig,
) -> Result<(), CaptureError> {
    if device.profile.model != DeviceModel::DsCopeU3P100 {
        return Err(CaptureError::new(format!(
            "{} is not a DSCope U3P100 oscilloscope",
            device.location.stable_id()
        )));
    }
    let session = UsbDeviceSession::open(device).map_err(capture_error)?;
    let configuration_result = (|| {
        validate_device_ready(&session)?;
        session.stop_capture().map_err(capture_error)?;
        configure_frontend(&session, config).map(|_| ())
    })();
    let close_result = session.close().map_err(capture_error);
    combine_results(
        configuration_result,
        close_result,
        "release the USB interface",
    )
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
        let calibration = configure_frontend(session, config)?;
        session
            .arm_capture(&config.encode_settings())
            .map_err(capture_error)?;
        configure_scope_post_arm(session, config, calibration)?;
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

fn configure_frontend(
    session: &UsbDeviceSession,
    config: DsCopeCaptureConfig,
) -> Result<DsCopeCalibration, CaptureError> {
    let enabled_channels = config.channels.map(|channel| channel.enabled);
    session
        .initialize_dscope_frontend(enabled_channels)
        .map_err(capture_error)?;
    let calibration = DsCopeCalibration::load(session, config)?;
    session
        .configure_dscope_channels(enabled_channels)
        .map_err(capture_error)?;
    configure_scope(session, config, calibration)?;
    Ok(calibration)
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
    calibration: DsCopeCalibration,
) -> Result<(), CaptureError> {
    for command in dso_commands(config, calibration) {
        session.write_dso_command(command).map_err(capture_error)?;
    }
    Ok(())
}

fn configure_scope_post_arm(
    session: &UsbDeviceSession,
    config: DsCopeCaptureConfig,
    calibration: DsCopeCalibration,
) -> Result<(), CaptureError> {
    let commands = dso_commands(config, calibration);
    for index in [8, 3, 4, 5, 6] {
        session
            .write_dso_command(commands[index])
            .map_err(capture_error)?;
    }
    Ok(())
}

fn dso_commands(config: DsCopeCaptureConfig, calibration: DsCopeCalibration) -> [u64; 14] {
    let channel_zero = calibration.channels[0];
    let channel_one = calibration.channels[1];
    [
        0xa5a5_a500,
        coupling_command(config, 0, config.channels[0].coupling),
        coupling_command(config, 1, config.channels[1].coupling),
        0x08 + u64::from(channel_zero.vga_gain),
        0x88 + u64::from(channel_one.vga_gain),
        offset_command(
            0x10,
            config.channels[0].vertical_offset,
            channel_zero.preoffset,
        ),
        offset_command(
            0x90,
            config.channels[1].vertical_offset,
            channel_one.preoffset,
        ),
        0x18 + (u64::from(config.divider()) << 8),
        0x20,
        0x58,
        0x28,
        0x30,
        0x0080_8038,
        0x0840,
    ]
}

fn coupling_command(config: DsCopeCaptureConfig, channel: usize, coupling: DsCopeCoupling) -> u64 {
    let command = if config.enabled_channel_count() == CHANNEL_COUNT {
        0x0e00
    } else if config.channels[0].enabled {
        0x1600
    } else {
        0x1a00
    };
    command + ((channel as u64) << 7) + coupling.command_bit()
}

fn offset_command(command: u64, vertical_offset: u16, preoffset: u16) -> u64 {
    let vertical_offset = u64::from(vertical_offset);
    let offset =
        (vertical_offset << 32) + (vertical_offset * OFFSET_PWM_SPAN / 255) + u64::from(preoffset);
    command + (offset << 8)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DsCopeChannelCalibration {
    preoffset: u16,
    vga_gain: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DsCopeCalibration {
    channels: [DsCopeChannelCalibration; CHANNEL_COUNT],
}

impl DsCopeCalibration {
    fn load(session: &UsbDeviceSession, config: DsCopeCaptureConfig) -> Result<Self, CaptureError> {
        let mut channels = [DsCopeChannelCalibration {
            preoffset: 0,
            vga_gain: 0,
        }; CHANNEL_COUNT];
        for (channel, calibration) in channels.iter_mut().enumerate() {
            let page = u16::try_from(channel).expect("channel index fits u16")
                * CALIBRATION_FLASH_PAGE_STRIDE;
            let mut zero_record = [0_u8; CALIBRATION_ZERO_BYTES];
            let mut vga_record = [0_u8; CALIBRATION_VGA_BYTES];
            session
                .read_dscope_nvm(page, &mut zero_record)
                .map_err(capture_error)?;
            session
                .read_dscope_nvm(page + 1, &mut vga_record)
                .map_err(capture_error)?;
            *calibration = parse_channel_calibration(
                channel,
                config.channels[channel].calibration_index(),
                &zero_record,
                &vga_record,
            )?;
        }
        Ok(Self { channels })
    }

    #[cfg(test)]
    fn nominal() -> Self {
        Self {
            channels: [DsCopeChannelCalibration {
                preoffset: 60,
                vga_gain: 0x0006_9d00,
            }; CHANNEL_COUNT],
        }
    }
}

fn parse_channel_calibration(
    channel: usize,
    vdiv_index: usize,
    zero_record: &[u8; CALIBRATION_ZERO_BYTES],
    vga_record: &[u8; CALIBRATION_VGA_BYTES],
) -> Result<DsCopeChannelCalibration, CaptureError> {
    let record_stride = CALIBRATION_ZERO_BYTES + CALIBRATION_VGA_BYTES;
    let calibration_id = CALIBRATION_ID_BASE
        .checked_add(
            u8::try_from(channel * record_stride)
                .map_err(|_| CaptureError::new("DSCope calibration channel is out of range"))?,
        )
        .ok_or_else(|| CaptureError::new("DSCope calibration identity overflow"))?;
    let vga_id = calibration_id + CALIBRATION_ZERO_BYTES as u8;
    if zero_record[0] != calibration_id || vga_record[0] != vga_id {
        return Err(CaptureError::new(format!(
            "invalid DSCope channel {channel} factory calibration identity: zero 0x{:02x}, VGA 0x{:02x}; expected 0x{calibration_id:02x} and 0x{vga_id:02x}",
            zero_record[0], vga_record[0]
        )));
    }

    if vdiv_index >= DSCOPE_VOLTS_PER_DIV_MV.len() {
        return Err(CaptureError::new(format!(
            "DSCope volts/div calibration index {vdiv_index} is out of range"
        )));
    }
    let table_offset = 1 + vdiv_index * 2;
    let preoffset = read_u16(zero_record, table_offset);
    let vga_gain = u32::from(read_u16(vga_record, table_offset)) << 8;
    if preoffset > 1_023 || vga_gain == 0 || vga_gain > 0x001f_ff00 {
        return Err(CaptureError::new(format!(
            "invalid DSCope channel {channel} {} mV/div calibration: preoffset {preoffset}, VGA gain 0x{vga_gain:08x}",
            DSCOPE_VOLTS_PER_DIV_MV[vdiv_index]
        )));
    }
    Ok(DsCopeChannelCalibration {
        preoffset,
        vga_gain,
    })
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
        .and_then(|samples| samples.checked_mul(config.enabled_channel_count()))
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

    context.synchronize_start()?;
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
    let enabled_channel_count = config.enabled_channel_count();
    let enabled_channel_indices = config.enabled_channel_indices().collect::<Vec<_>>();
    let measurement_offset = usize::try_from(config.armed_sample_count())
        .expect("validated sample count fits usize")
        * enabled_channel_count;
    let measurement = ScopeMeasurement::decode(&capture.payload[measurement_offset..], config)?;
    let sample_count = usize::try_from(measurement.sample_count)
        .expect("32-bit measurement sample count fits usize");
    // The hardware trigger offset is expressed in two-byte ADC words even in
    // single-channel interleaved mode.
    let data_start = usize::from(measurement.trigger_offset) * CHANNEL_COUNT;
    let data_bytes = sample_count
        .checked_mul(enabled_channel_count)
        .and_then(|bytes| data_start.checked_add(bytes))
        .ok_or_else(|| CaptureError::new("DSCope sample window is too large"))?;
    if data_bytes > measurement_offset {
        return Err(CaptureError::new(format!(
            "DSCope sample window ends at byte {data_bytes}, beyond measurement footer at {measurement_offset}"
        )));
    }

    let samples = &capture.payload[data_start..data_bytes];
    for (sequence, chunk) in samples
        .chunks(SCOPE_BATCH_SAMPLES * enabled_channel_count)
        .enumerate()
    {
        let frame_sample_offset = sequence * SCOPE_BATCH_SAMPLES;
        let samples_per_channel = chunk.len() / enabled_channel_count;
        let mut channels = vec![Vec::with_capacity(samples_per_channel); enabled_channel_count];
        for wire_samples in chunk.chunks_exact(enabled_channel_count) {
            for (wire_index, physical_channel) in
                enabled_channel_indices.iter().copied().enumerate()
            {
                channels[wire_index].push(raw_to_volts(
                    wire_samples[wire_index],
                    measurement.hardware_offsets[physical_channel],
                    config.channels[physical_channel],
                ));
            }
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
        let valid_length = read_u32(footer, 4) & 0x0fff_ffff;
        let divider = read_u32(footer, 8) & 0x00ff_ffff;
        if divider != config.divider() || valid_length == 0 {
            return Err(CaptureError::new(format!(
                "invalid DSCope measurement metadata: divider {divider}, valid length {valid_length}"
            )));
        }
        let channel_ratio = CHANNEL_COUNT / config.enabled_channel_count();
        let sample_count = valid_length
            .checked_mul(channel_ratio as u32)
            .ok_or_else(|| CaptureError::new("DSCope valid sample length overflowed"))?;
        Ok(Self {
            sample_count: sample_count.min(config.sample_count as u32),
            trigger_offset: footer[11] & 0x07,
            hardware_offsets: [footer[102], footer[166]],
        })
    }
}

fn raw_to_volts(raw: u8, hardware_offset: u8, channel: DsCopeChannelConfig) -> f32 {
    let input_span = f32::from(channel.volts_per_div_mv) / 1_000.0 * 10.0;
    (f32::from(hardware_offset) - f32::from(raw)) * input_span / ADC_REFERENCE_SPAN
        * f32::from(channel.probe_attenuation)
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

    const MAX_DUAL_CHANNEL_SAMPLE_COUNT: u64 = MAX_TOTAL_SAMPLE_COUNT / 2;

    #[test]
    fn validates_sample_rate_against_enabled_channel_count() {
        assert!(DsCopeCaptureConfig::new(1_000_000, MAX_DUAL_CHANNEL_SAMPLE_COUNT).is_ok());
        assert!(DsCopeCaptureConfig::new(500_000_000, MAX_DUAL_CHANNEL_SAMPLE_COUNT).is_ok());
        assert!(DsCopeCaptureConfig::new(1_000_000_000, MAX_DUAL_CHANNEL_SAMPLE_COUNT).is_err());
        assert!(DsCopeCaptureConfig::new(1_000_000, MAX_TOTAL_SAMPLE_COUNT).is_err());
        let single_channel = [
            DsCopeChannelConfig::default(),
            DsCopeChannelConfig::default().with_enabled(false),
        ];
        assert!(DsCopeCaptureConfig::with_channel_configs(
            1_000_000_000,
            MAX_TOTAL_SAMPLE_COUNT,
            single_channel,
        )
        .is_ok());
        assert!(DsCopeCaptureConfig::new(3_000_000, MAX_DUAL_CHANNEL_SAMPLE_COUNT).is_err());
        assert!(DsCopeCaptureConfig::new(1_000_000, 1).is_err());
        assert!(DsCopeCaptureConfig::with_channel_configs(
            1_000_000,
            MAX_TOTAL_SAMPLE_COUNT,
            [
                DsCopeChannelConfig::default().with_enabled(false),
                DsCopeChannelConfig::default().with_enabled(false),
            ],
        )
        .is_err());
    }

    #[test]
    fn encodes_dso_mode_and_capture_counts() {
        let config = DsCopeCaptureConfig::new(1_000_000, MAX_DUAL_CHANNEL_SAMPLE_COUNT).unwrap();
        let settings = config.encode_settings();

        assert_eq!(settings.len(), SETTINGS_BYTES);
        assert_eq!(read_u32(&settings, 0), 0xf5a5_f5a5);
        assert_eq!(read_u16(&settings, 6), 1 << 4);
        assert_eq!(read_u16(&settings, 10), 500);
        assert_eq!(read_u16(&settings, 28), 0x0200);
        assert_eq!(
            read_u16(&settings, 32),
            MAX_DUAL_CHANNEL_SAMPLE_COUNT as u16
        );
        assert_eq!(read_u16(&settings, 38), 0b11);
        assert_eq!(read_u32(&settings, 368), 0xfa5a_fa5a);
    }

    #[test]
    fn generates_nominal_two_channel_dso_commands() {
        let config = DsCopeCaptureConfig::new(1_000_000, MAX_DUAL_CHANNEL_SAMPLE_COUNT).unwrap();
        let commands = dso_commands(config, DsCopeCalibration::nominal());

        assert_eq!(commands[0], 0xa5a5_a500);
        assert_eq!(commands[1], 0x0f00);
        assert_eq!(commands[2], 0x0f80);
        assert_eq!(commands[3], 0x0006_9d08);
        assert_eq!(commands[4], 0x0006_9d88);
        assert_eq!(commands[7], 0x0001_f418);
        assert_eq!(commands[12], 0x0080_8038);
    }

    #[test]
    fn encodes_one_channel_gigasample_capture() {
        let channels = [
            DsCopeChannelConfig::default().with_enabled(false),
            DsCopeChannelConfig::default(),
        ];
        let config = DsCopeCaptureConfig::with_channel_configs(
            1_000_000_000,
            MAX_TOTAL_SAMPLE_COUNT,
            channels,
        )
        .unwrap();
        let settings = config.encode_settings();
        let commands = dso_commands(config, DsCopeCalibration::nominal());

        assert_eq!(config.enabled_channel_count(), 1);
        assert_eq!(config.divider(), 1);
        assert_eq!(read_u16(&settings, 10), 1);
        assert_eq!(read_u16(&settings, 28), 0x0100);
        assert_eq!(read_u16(&settings, 38), 0b10);
        assert_eq!(commands[1], 0x1b00);
        assert_eq!(commands[2], 0x1b80);
        assert_eq!(commands[7], 0x0118);
    }

    #[test]
    fn applies_each_channels_factory_gain_and_offset() {
        let config = DsCopeCaptureConfig::new(1_000_000, MAX_DUAL_CHANNEL_SAMPLE_COUNT).unwrap();
        let calibration = DsCopeCalibration {
            channels: [
                DsCopeChannelCalibration {
                    preoffset: 71,
                    vga_gain: 0x0006_a100,
                },
                DsCopeChannelCalibration {
                    preoffset: 83,
                    vga_gain: 0x0006_9700,
                },
            ],
        };

        let commands = dso_commands(config, calibration);

        assert_eq!(commands[3], 0x0006_a108);
        assert_eq!(commands[4], 0x0006_9788);
        assert_eq!(
            commands[5],
            offset_command(0x10, DSCOPE_VERTICAL_OFFSET_CENTER, 71)
        );
        assert_eq!(
            commands[6],
            offset_command(0x90, DSCOPE_VERTICAL_OFFSET_CENTER, 83)
        );
    }

    #[test]
    fn applies_each_channels_configured_vertical_offset() {
        let channels = [
            DsCopeChannelConfig::default()
                .with_vertical_offset(DSCOPE_VERTICAL_OFFSET_MIN)
                .unwrap(),
            DsCopeChannelConfig::default()
                .with_vertical_offset(DSCOPE_VERTICAL_OFFSET_MAX)
                .unwrap(),
        ];
        let config = DsCopeCaptureConfig::with_channel_configs(
            1_000_000,
            MAX_DUAL_CHANNEL_SAMPLE_COUNT,
            channels,
        )
        .unwrap();
        let calibration = DsCopeCalibration {
            channels: [
                DsCopeChannelCalibration {
                    preoffset: 71,
                    vga_gain: 0x0006_a100,
                },
                DsCopeChannelCalibration {
                    preoffset: 83,
                    vga_gain: 0x0006_9700,
                },
            ],
        };

        let commands = dso_commands(config, calibration);

        assert_eq!(commands[5], 0x0000_0a00_0000_6510);
        assert_eq!(commands[6], 0x0000_f500_0003_4090);
    }

    #[test]
    fn rejects_vertical_offsets_outside_the_hardware_range() {
        assert!(DsCopeChannelConfig::default()
            .with_vertical_offset(DSCOPE_VERTICAL_OFFSET_MIN - 1)
            .is_err());
        assert!(DsCopeChannelConfig::default()
            .with_vertical_offset(DSCOPE_VERTICAL_OFFSET_MAX + 1)
            .is_err());
    }

    #[test]
    fn parses_factory_calibration_for_one_volt_range() {
        let mut zero_record = [0_u8; CALIBRATION_ZERO_BYTES];
        let mut vga_record = [0_u8; CALIBRATION_VGA_BYTES];
        zero_record[0] = 0x6f;
        vga_record[0] = 0x8d;
        zero_record[13..15].copy_from_slice(&73_u16.to_le_bytes());
        vga_record[13..15].copy_from_slice(&0x06a1_u16.to_le_bytes());

        let calibration = parse_channel_calibration(1, 6, &zero_record, &vga_record).unwrap();

        assert_eq!(calibration.preoffset, 73);
        assert_eq!(calibration.vga_gain, 0x0006_a100);
    }

    #[test]
    fn rejects_mismatched_factory_calibration_records() {
        let zero_record = [0xff_u8; CALIBRATION_ZERO_BYTES];
        let vga_record = [0xff_u8; CALIBRATION_VGA_BYTES];

        let error = parse_channel_calibration(0, 6, &zero_record, &vga_record).unwrap_err();

        assert!(error.to_string().contains("factory calibration identity"));
    }

    #[test]
    fn parses_measurement_footer_and_converts_voltage() {
        let config = DsCopeCaptureConfig::new(1_000_000, MAX_DUAL_CHANNEL_SAMPLE_COUNT).unwrap();
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
        let channel = DsCopeChannelConfig::default();
        assert_eq!(raw_to_volts(128, 128, channel), 0.0);
        assert!(raw_to_volts(100, 128, channel) > 1.19);
    }

    #[test]
    fn expands_single_channel_measurement_valid_length() {
        let channels = [
            DsCopeChannelConfig::default().with_enabled(false),
            DsCopeChannelConfig::default(),
        ];
        let config =
            DsCopeCaptureConfig::with_channel_configs(1_000_000_000, 10_000, channels).unwrap();
        let mut footer = [0_u8; MEASUREMENT_FOOTER_BYTES];
        footer[0..2].copy_from_slice(&DSO_PACKET_ID.to_le_bytes());
        footer[4..8].copy_from_slice(&100_u32.to_le_bytes());
        footer[8..12].copy_from_slice(&config.divider().to_le_bytes());

        let measurement = ScopeMeasurement::decode(&footer, config).unwrap();

        assert_eq!(measurement.sample_count, 200);
    }

    #[test]
    fn applies_channel_coupling_scale_and_probe_attenuation() {
        let channels = [
            DsCopeChannelConfig::new(100, DsCopeCoupling::Ac, 10).unwrap(),
            DsCopeChannelConfig::new(2_000, DsCopeCoupling::Dc, 1).unwrap(),
        ];
        let config =
            DsCopeCaptureConfig::with_channel_configs(2_000_000, 250_000, channels).unwrap();
        let commands = dso_commands(config, DsCopeCalibration::nominal());

        assert_eq!(commands[1], 0x0e00);
        assert_eq!(commands[2], 0x0f80);
        assert_eq!(config.divider(), 250);
        assert_eq!(config.channels(), channels);
        assert!(raw_to_volts(100, 128, channels[0]) > 1.19);
        assert!(raw_to_volts(100, 128, channels[1]) > 2.38);
    }
}
