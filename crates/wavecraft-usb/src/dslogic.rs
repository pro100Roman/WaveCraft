#[cfg(test)]
use crate::ds_driver_util::read_u16;
use crate::ds_driver_util::{capture_error, combine_results, push_u16, push_u32, read_u32};
use crate::queued_reads::{
    abandon_reads_after_error, require_queued_data, service_usb_events, with_queued_reads,
    QueuedReads,
};
use crate::{
    BulkReadOutcome, DeviceModel, DeviceProfile, DiscoveredDevice, UsbDeviceSession, UsbSpeedClass,
};
use std::time::{Duration, Instant};
use wavecraft_core::{
    CaptureBatch, CaptureContext, CaptureError, CaptureSource, DeviceDescriptor, DeviceId,
    InstrumentKind, LogicBatch,
};

const CHANNEL_COUNT: u8 = 32;
const LEGACY_CHANNEL_COUNT: usize = 16;
pub const DSLOGIC_SAMPLE_ALIGNMENT: u64 = 1_024;
const ATOMIC_SAMPLES: usize = 64;
const BYTES_PER_CHANNEL_BLOCK: usize = 8;
#[cfg(test)]
const TRIGGER_HEADER_BYTES: usize = 1_024;
const TRIGGER_CHECK_ID: u32 = 0x5555_5555;
const SETTINGS_BYTES: usize = 372;
const EXTENDED_SETTINGS_BYTES: usize = 204;
const CAPTURE_READ_BYTES: usize = 128 * 1_024;
const PREQUEUED_DATA_BYTES: usize = 1_024 * 1_024;
// U3Pro16 uses 1,024-byte SuperSpeed packets and 512-byte HighSpeed packets.
// Aligning to 1,024 satisfies both links and prevents a transfer from ending
// in the middle of a USB packet.
const USB_TRANSFER_ALIGNMENT: usize = 1_024;
const SUPER_SPEED_TRANSFER_MILLISECONDS: u64 = 10;
const SUPER_SPEED_QUEUED_TRANSFERS: usize = 4;
const HIGH_SPEED_TRANSFER_MILLISECONDS: u64 = 20;
const HIGH_SPEED_QUEUED_TRANSFERS: usize = 5;
pub const DSLOGIC_MAX_BUFFER_SAMPLE_COUNT: u64 = 134_217_728;
// Match the original 64-bit application's software capture depth. Stream-mode
// data is consumed in batches, so it is not constrained by the FPGA buffer.
pub const DSLOGIC_MAX_STREAM_SAMPLE_COUNT: u64 = 16 * 1_073_741_824;
pub const DSLOGIC_MAX_SAMPLE_COUNT: u64 = DSLOGIC_MAX_STREAM_SAMPLE_COUNT;
const SAMPLE_ALIGNMENT: u64 = DSLOGIC_SAMPLE_ALIGNMENT;
const MAX_SAMPLE_COUNT: u64 = DSLOGIC_MAX_SAMPLE_COUNT;
const CAPTURE_COMPLETION_GRACE: Duration = Duration::from_secs(5);
const PAYLOAD_TRANSFER_GRACE: Duration = Duration::from_secs(5);

const CLOCK_TYPE_BIT: u16 = 1;
const TRIGGER_ENABLE_BIT: u16 = 0;
const CLOCK_EDGE_BIT: u16 = 2;
const RLE_MODE_BIT: u16 = 3;
const HALF_MODE_BIT: u16 = 5;
const QUARTER_MODE_BIT: u16 = 6;
const FILTER_BIT: u16 = 8;
const LOOP_MODE_BIT: u16 = 7;
const SLOW_ACQUISITION_BIT: u16 = 10;
const STREAM_MODE_BIT: u16 = 12;
const INTERNAL_TEST_BIT: u16 = 15;
pub const DSLOGIC_TRIGGER_STAGE_COUNT: usize = 16;

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
    200_000_000,
    250_000_000,
    400_000_000,
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

/// Per-channel match performed by the DSLogic FPGA.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DsLogicTriggerCondition {
    Low,
    High,
    RisingEdge,
    FallingEdge,
    EitherEdge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DsLogicSimpleTrigger {
    conditions: [Option<DsLogicTriggerCondition>; CHANNEL_COUNT as usize],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DsLogicTriggerStage {
    conditions: [Option<DsLogicTriggerCondition>; CHANNEL_COUNT as usize],
}

impl DsLogicTriggerStage {
    pub fn new(channel: u8, condition: DsLogicTriggerCondition) -> Result<Self, CaptureError> {
        Self {
            conditions: [None; CHANNEL_COUNT as usize],
        }
        .with_channel_condition(channel, condition)
    }

    pub fn with_channel_condition(
        mut self,
        channel: u8,
        condition: DsLogicTriggerCondition,
    ) -> Result<Self, CaptureError> {
        let Some(destination) = self.conditions.get_mut(usize::from(channel)) else {
            return Err(CaptureError::new(format!(
                "DSLogic trigger channel must be between 0 and {}; got {channel}",
                CHANNEL_COUNT - 1
            )));
        };
        *destination = Some(condition);
        Ok(self)
    }

    pub fn conditions(self) -> [Option<DsLogicTriggerCondition>; CHANNEL_COUNT as usize] {
        self.conditions
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PackedTriggerStage {
    mask: u32,
    value: u32,
    edge: u32,
}

impl PackedTriggerStage {
    fn from_stage(stage: DsLogicTriggerStage) -> Self {
        let mut packed = Self {
            mask: u32::MAX,
            ..Self::default()
        };
        for (channel, condition) in stage.conditions.into_iter().enumerate() {
            let Some(condition) = condition else {
                continue;
            };
            let bit = 1_u32 << channel;
            match condition {
                DsLogicTriggerCondition::Low => packed.mask &= !bit,
                DsLogicTriggerCondition::High => {
                    packed.mask &= !bit;
                    packed.value |= bit;
                }
                DsLogicTriggerCondition::RisingEdge => {
                    packed.mask &= !bit;
                    packed.value |= bit;
                    packed.edge |= bit;
                }
                DsLogicTriggerCondition::FallingEdge => {
                    packed.mask &= !bit;
                    packed.edge |= bit;
                }
                DsLogicTriggerCondition::EitherEdge => packed.edge |= bit,
            }
        }
        packed
    }

    fn conditions(self) -> [Option<DsLogicTriggerCondition>; CHANNEL_COUNT as usize] {
        std::array::from_fn(|channel| {
            let bit = 1_u32 << channel;
            match (
                self.mask & bit != 0,
                self.value & bit != 0,
                self.edge & bit != 0,
            ) {
                (true, false, false) => None,
                (false, false, false) => Some(DsLogicTriggerCondition::Low),
                (false, true, false) => Some(DsLogicTriggerCondition::High),
                (false, true, true) => Some(DsLogicTriggerCondition::RisingEdge),
                (false, false, true) => Some(DsLogicTriggerCondition::FallingEdge),
                (true, false, true) => Some(DsLogicTriggerCondition::EitherEdge),
                _ => None,
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DsLogicSmartTrigger {
    stages: [PackedTriggerStage; DSLOGIC_TRIGGER_STAGE_COUNT],
    stage_count: u8,
}

impl DsLogicSmartTrigger {
    /// Create an ordered hardware trigger. Every condition in a stage must
    /// match at the same sample; stages must then match in sequence.
    pub fn new(first_stage: DsLogicTriggerStage) -> Self {
        let mut stages = [PackedTriggerStage::default(); DSLOGIC_TRIGGER_STAGE_COUNT];
        stages[0] = PackedTriggerStage::from_stage(first_stage);
        Self {
            stages,
            stage_count: 1,
        }
    }

    pub fn with_stage(mut self, stage: DsLogicTriggerStage) -> Result<Self, CaptureError> {
        let stage_index = usize::from(self.stage_count);
        let Some(destination) = self.stages.get_mut(stage_index) else {
            return Err(CaptureError::new(format!(
                "DSLogic smart trigger supports at most {DSLOGIC_TRIGGER_STAGE_COUNT} stages"
            )));
        };
        *destination = PackedTriggerStage::from_stage(stage);
        self.stage_count += 1;
        Ok(self)
    }

    pub fn stages(self) -> [Option<DsLogicTriggerStage>; DSLOGIC_TRIGGER_STAGE_COUNT] {
        std::array::from_fn(|index| {
            (index < usize::from(self.stage_count)).then(|| DsLogicTriggerStage {
                conditions: self.stages[index].conditions(),
            })
        })
    }

    pub fn stage_count(self) -> u8 {
        self.stage_count
    }
}

impl DsLogicSimpleTrigger {
    /// Create a simple hardware trigger. Additional channel conditions can be
    /// added with [`Self::with_channel_condition`]; all configured conditions
    /// must match at the same sample, matching the original DSView behavior.
    pub fn new(channel: u8, condition: DsLogicTriggerCondition) -> Result<Self, CaptureError> {
        Self {
            conditions: [None; CHANNEL_COUNT as usize],
        }
        .with_channel_condition(channel, condition)
    }

    pub fn with_channel_condition(
        mut self,
        channel: u8,
        condition: DsLogicTriggerCondition,
    ) -> Result<Self, CaptureError> {
        let Some(destination) = self.conditions.get_mut(usize::from(channel)) else {
            return Err(CaptureError::new(format!(
                "DSLogic trigger channel must be between 0 and {}; got {channel}",
                CHANNEL_COUNT - 1
            )));
        };
        *destination = Some(condition);
        Ok(self)
    }

    pub fn conditions(self) -> [Option<DsLogicTriggerCondition>; CHANNEL_COUNT as usize] {
        self.conditions
    }

    pub fn condition_count(self) -> u8 {
        self.conditions
            .iter()
            .filter(|condition| condition.is_some())
            .count() as u8
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DsLogicTriggerConfig {
    Simple(DsLogicSimpleTrigger),
    Smart(DsLogicSmartTrigger),
}

impl DsLogicTriggerConfig {
    fn condition_sets(
        self,
    ) -> [[Option<DsLogicTriggerCondition>; CHANNEL_COUNT as usize]; DSLOGIC_TRIGGER_STAGE_COUNT]
    {
        let mut condition_sets = [[None; CHANNEL_COUNT as usize]; DSLOGIC_TRIGGER_STAGE_COUNT];
        match self {
            Self::Simple(trigger) => condition_sets[0] = trigger.conditions,
            Self::Smart(trigger) => {
                for (destination, stage) in condition_sets
                    .iter_mut()
                    .zip(trigger.stages)
                    .take(usize::from(trigger.stage_count))
                {
                    *destination = stage.conditions();
                }
            }
        }
        condition_sets
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DsLogicCaptureConfig {
    sample_rate_hz: u64,
    sample_count: u64,
    operation_mode: DsLogicOperationMode,
    loop_mode: bool,
    stop_option: DsLogicStopOption,
    threshold_mv: u16,
    input_filter: DsLogicInputFilter,
    rle_enabled: bool,
    external_clock: bool,
    falling_clock_edge: bool,
    trigger: Option<DsLogicTriggerConfig>,
    enabled_channels: [bool; CHANNEL_COUNT as usize],
    device_model: DeviceModel,
    device_channel_count: u8,
}

impl DsLogicCaptureConfig {
    pub fn new(sample_rate_hz: u64, sample_count: u64) -> Result<Self, CaptureError> {
        if !DSLOGIC_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            return Err(CaptureError::new(format!(
                "unsupported DSLogic sample rate {sample_rate_hz} Hz"
            )));
        }
        if !(SAMPLE_ALIGNMENT..=MAX_SAMPLE_COUNT).contains(&sample_count) {
            return Err(CaptureError::new(format!(
                "DSLogic sample count must be between {SAMPLE_ALIGNMENT} and {MAX_SAMPLE_COUNT}"
            )));
        }
        let mut enabled_channels = [false; CHANNEL_COUNT as usize];
        enabled_channels[..LEGACY_CHANNEL_COUNT].fill(true);
        Ok(Self {
            sample_rate_hz,
            sample_count,
            operation_mode: DsLogicOperationMode::Buffer,
            loop_mode: false,
            stop_option: DsLogicStopOption::UploadCapturedData,
            threshold_mv: 1_000,
            input_filter: DsLogicInputFilter::None,
            rle_enabled: false,
            external_clock: false,
            falling_clock_edge: false,
            trigger: None,
            enabled_channels,
            device_model: DeviceModel::DsLogicU3Pro16,
            device_channel_count: LEGACY_CHANNEL_COUNT as u8,
        })
    }

    pub fn with_operation_mode(mut self, operation_mode: DsLogicOperationMode) -> Self {
        self.operation_mode = operation_mode;
        self
    }

    pub fn with_loop_mode(mut self, loop_mode: bool) -> Self {
        self.loop_mode = loop_mode;
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

    pub fn with_trigger(mut self, trigger: Option<DsLogicTriggerConfig>) -> Self {
        self.trigger = trigger;
        self
    }

    pub fn with_enabled_channels<const N: usize>(
        mut self,
        enabled_channels: [bool; N],
    ) -> Result<Self, CaptureError> {
        if N != LEGACY_CHANNEL_COUNT && N != CHANNEL_COUNT as usize {
            return Err(CaptureError::new(format!(
                "DSLogic channel selection must contain 16 or 32 entries; got {N}"
            )));
        }
        if !enabled_channels.iter().any(|enabled| *enabled) {
            return Err(CaptureError::new(
                "DSLogic capture requires at least one enabled channel",
            ));
        }
        self.enabled_channels = [false; CHANNEL_COUNT as usize];
        self.enabled_channels[..N].copy_from_slice(&enabled_channels);
        Ok(self)
    }

    pub fn with_enabled_channels_32(
        self,
        enabled_channels: [bool; CHANNEL_COUNT as usize],
    ) -> Result<Self, CaptureError> {
        self.with_enabled_channels(enabled_channels)
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

    pub fn loop_mode(self) -> bool {
        self.loop_mode
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

    pub fn trigger(self) -> Option<DsLogicTriggerConfig> {
        self.trigger
    }

    pub fn enabled_channels(self) -> [bool; LEGACY_CHANNEL_COUNT] {
        let mut legacy_channels = [false; LEGACY_CHANNEL_COUNT];
        legacy_channels.copy_from_slice(&self.enabled_channels[..LEGACY_CHANNEL_COUNT]);
        legacy_channels
    }

    pub fn enabled_channels_32(self) -> [bool; CHANNEL_COUNT as usize] {
        self.enabled_channels
    }

    pub fn enabled_channel_count(self) -> u8 {
        self.enabled_channels
            .iter()
            .filter(|enabled| **enabled)
            .count() as u8
    }

    pub fn maximum_sample_rate_hz(self, speed: UsbSpeedClass) -> u64 {
        self.maximum_sample_rate_hz_for_model(DeviceModel::DsLogicU3Pro16, speed)
    }

    pub fn maximum_sample_rate_hz_for_model(self, model: DeviceModel, speed: UsbSpeedClass) -> u64 {
        let enabled_count = self.enabled_channel_count();
        let lower_eight_only = self.enabled_channels[8..].iter().all(|enabled| !enabled);
        let lower_four_only = self.enabled_channels[4..].iter().all(|enabled| !enabled);
        let lower_sixteen_only = self.enabled_channels[16..].iter().all(|enabled| !enabled);
        let modern = model.logic_hardware_sample_rate_hz() == 500_000_000;
        match self.operation_mode {
            DsLogicOperationMode::Buffer if modern => {
                if enabled_count <= 8 && lower_eight_only {
                    1_000_000_000
                } else if enabled_count <= 16 && lower_sixteen_only {
                    500_000_000
                } else {
                    250_000_000
                }
            }
            DsLogicOperationMode::Buffer => {
                if matches!(
                    model,
                    DeviceModel::DsLogicU2Basic
                        | DeviceModel::DsLogicU2BasicPgl12
                        | DeviceModel::DsLogicU2BasicPgl12Rev2
                ) {
                    100_000_000
                } else if enabled_count <= 4 && lower_four_only {
                    400_000_000
                } else if enabled_count <= 8 && lower_eight_only {
                    200_000_000
                } else {
                    100_000_000
                }
            }
            DsLogicOperationMode::Stream
                if model.logic_uses_usb3_protocol() && speed >= UsbSpeedClass::SuperSpeed =>
            {
                if enabled_count <= 3 && lower_eight_only {
                    1_000_000_000
                } else if enabled_count <= 6 {
                    500_000_000
                } else if enabled_count <= 12 {
                    250_000_000
                } else if enabled_count <= 16 && lower_sixteen_only {
                    125_000_000
                } else if enabled_count <= 30 {
                    100_000_000
                } else {
                    50_000_000
                }
            }
            DsLogicOperationMode::Stream => {
                if enabled_count <= 3 {
                    100_000_000
                } else if enabled_count <= 6 {
                    50_000_000
                } else if enabled_count <= 12 {
                    25_000_000
                } else if enabled_count <= 16 {
                    20_000_000
                } else {
                    10_000_000
                }
            }
            DsLogicOperationMode::InternalTest => 100_000_000,
        }
    }

    pub fn maximum_sample_count(self) -> u64 {
        self.maximum_sample_count_for_model(DeviceModel::DsLogicU3Pro16)
    }

    pub fn maximum_sample_count_for_model(self, model: DeviceModel) -> u64 {
        match self.operation_mode {
            DsLogicOperationMode::Stream => DSLOGIC_MAX_STREAM_SAMPLE_COUNT,
            DsLogicOperationMode::Buffer | DsLogicOperationMode::InternalTest => model
                .logic_buffer_bits()
                .checked_div(u64::from(self.enabled_channel_count().max(1)))
                .unwrap_or(0)
                .min(DSLOGIC_MAX_BUFFER_SAMPLE_COUNT),
        }
    }

    fn validate_for_device(
        self,
        profile: &DeviceProfile,
        speed: UsbSpeedClass,
    ) -> Result<(), CaptureError> {
        let model = profile.model;
        let channel_count = profile.channel_count;
        if self.enabled_channels[usize::from(channel_count)..]
            .iter()
            .any(|enabled| *enabled)
        {
            return Err(CaptureError::new(format!(
                "{} exposes {channel_count} logic channels",
                profile.display_name
            )));
        }
        if self.operation_mode == DsLogicOperationMode::Buffer && !model.logic_has_buffer() {
            return Err(CaptureError::new(format!(
                "{} supports stream capture only",
                profile.display_name
            )));
        }
        if self.loop_mode && self.operation_mode != DsLogicOperationMode::Stream {
            return Err(CaptureError::new(
                "DSLogic loop mode requires stream operation mode",
            ));
        }
        if self.loop_mode && self.trigger.is_some() {
            return Err(CaptureError::new(
                "DSLogic hardware triggering is not supported in loop mode",
            ));
        }
        if self.operation_mode == DsLogicOperationMode::InternalTest && self.trigger.is_some() {
            return Err(CaptureError::new(
                "DSLogic hardware triggering is not supported in internal-test mode",
            ));
        }
        if matches!(self.trigger, Some(DsLogicTriggerConfig::Smart(_)))
            && self.operation_mode != DsLogicOperationMode::Buffer
        {
            return Err(CaptureError::new(
                "DSLogic smart triggering requires buffer operation mode",
            ));
        }
        if let Some(trigger) = self.trigger {
            for conditions in trigger.condition_sets() {
                for (channel, condition) in conditions.iter().enumerate() {
                    if condition.is_none() {
                        continue;
                    }
                    if channel >= usize::from(channel_count) {
                        return Err(CaptureError::new(format!(
                            "{} exposes {channel_count} logic channels; trigger channel {channel} is unavailable",
                            profile.display_name
                        )));
                    }
                    if !self.enabled_channels[channel] {
                        return Err(CaptureError::new(format!(
                            "DSLogic trigger channel {channel} must be enabled"
                        )));
                    }
                }
            }
        }
        let maximum = self.maximum_sample_rate_hz_for_model(model, speed);
        if self.sample_rate_hz > maximum {
            return Err(CaptureError::new(format!(
                "DSLogic sample rate {} Hz exceeds the {} Hz maximum for {} enabled channel(s) in {:?} mode",
                self.sample_rate_hz,
                maximum,
                self.enabled_channel_count(),
                self.operation_mode
            )));
        }
        let maximum_sample_count = self.maximum_sample_count_for_model(model);
        if self.sample_count > maximum_sample_count {
            return Err(CaptureError::new(format!(
                "DSLogic sample count {} exceeds the {}-sample maximum in {:?} mode",
                self.sample_count, maximum_sample_count, self.operation_mode
            )));
        }
        Ok(())
    }

    #[cfg(test)]
    fn validate_for_speed(self, speed: UsbSpeedClass) -> Result<(), CaptureError> {
        self.validate_for_device(&crate::DSLOGIC_U3PRO16, speed)
    }

    fn armed_sample_count(self) -> u64 {
        self.sample_count.div_ceil(SAMPLE_ALIGNMENT) * SAMPLE_ALIGNMENT
    }

    fn encode_settings(self) -> Vec<u8> {
        let model = self.device_model;
        let hardware_sample_rate_hz = model.logic_hardware_sample_rate_hz();
        let predivider = model.logic_predivider();
        let divider = hardware_sample_rate_hz.div_ceil(self.sample_rate_hz) as u32;
        let divider_high = if divider >= predivider {
            predivider - 1
        } else {
            divider - 1
        } << 8;
        let divided = divider.div_ceil(predivider);
        let armed_samples = self.armed_sample_count();
        let capture_count = armed_samples >> 4;

        let operation_mode = match self.operation_mode {
            DsLogicOperationMode::Buffer => 0,
            DsLogicOperationMode::Stream => 1 << STREAM_MODE_BIT,
            DsLogicOperationMode::InternalTest => 1 << INTERNAL_TEST_BIT,
        };
        // The vendor FPGA needs its slow-acquisition path when the packed
        // wire rate falls below one 1,024-byte USB packet per millisecond.
        // Without this bit, low-rate captures arm but never produce bulk data.
        let bytes_per_millisecond =
            self.sample_rate_hz as u128 * u128::from(self.enabled_channel_count()) / 8 / 1_000;
        let mode = operation_mode
            | (u16::from(self.trigger.is_some()) << TRIGGER_ENABLE_BIT)
            | (u16::from(self.external_clock) << CLOCK_TYPE_BIT)
            | (u16::from(self.falling_clock_edge) << CLOCK_EDGE_BIT)
            | (u16::from(self.rle_enabled) << RLE_MODE_BIT)
            | (u16::from(self.loop_mode) << LOOP_MODE_BIT)
            | (u16::from(
                self.sample_rate_hz
                    == if hardware_sample_rate_hz == 500_000_000 {
                        500_000_000
                    } else {
                        200_000_000
                    },
            ) << HALF_MODE_BIT)
            | (u16::from(
                self.sample_rate_hz
                    == if hardware_sample_rate_hz == 500_000_000 {
                        1_000_000_000
                    } else {
                        400_000_000
                    },
            ) << QUARTER_MODE_BIT)
            | (u16::from(self.input_filter == DsLogicInputFilter::OneSampleClock) << FILTER_BIT)
            | (u16::from(bytes_per_millisecond < USB_TRANSFER_ALIGNMENT as u128)
                << SLOW_ACQUISITION_BIT);
        let enabled_channel_mask_low = self.enabled_channels[..16]
            .iter()
            .enumerate()
            .fold(0_u16, |mask, (channel, enabled)| {
                mask | (u16::from(*enabled) << channel)
            });
        let enabled_channel_mask_high = self.enabled_channels[16..]
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
        push_u16(
            &mut bytes,
            (u16::from(self.enabled_channel_count()) << 8) | u16::from(self.trigger_last_stage()),
        );
        push_u16(&mut bytes, 0x0802);
        push_u16(&mut bytes, armed_samples as u16);
        push_u16(&mut bytes, (armed_samples >> 16) as u16);
        push_u16(&mut bytes, 0x0a02);
        push_u16(&mut bytes, enabled_channel_mask_low);
        push_u16(&mut bytes, enabled_channel_mask_high);
        push_u16(&mut bytes, 0x0c01);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0x40a0);
        let trigger = self.trigger_words(0);
        push_trigger_stages_u16(&mut bytes, trigger.map(|words| words.mask));
        push_trigger_stages_u16(&mut bytes, [u16::MAX; DSLOGIC_TRIGGER_STAGE_COUNT]);
        push_trigger_stages_u16(&mut bytes, trigger.map(|words| words.value));
        push_trigger_stages_u16(&mut bytes, [0; DSLOGIC_TRIGGER_STAGE_COUNT]);
        push_trigger_stages_u16(&mut bytes, trigger.map(|words| words.edge));
        push_trigger_stages_u16(&mut bytes, [0; DSLOGIC_TRIGGER_STAGE_COUNT]);
        push_trigger_stages_u16(&mut bytes, [2; DSLOGIC_TRIGGER_STAGE_COUNT]);
        push_trigger_stages_u16(&mut bytes, [2; DSLOGIC_TRIGGER_STAGE_COUNT]);
        for _ in 0..DSLOGIC_TRIGGER_STAGE_COUNT {
            push_u32(&mut bytes, 0);
        }
        push_u32(&mut bytes, 0xfa5a_fa5a);

        debug_assert_eq!(bytes.len(), SETTINGS_BYTES);
        bytes
    }

    fn encode_extended_settings(self) -> Option<Vec<u8>> {
        let model = self.device_model;
        if model != DeviceModel::DsLogicU3Pro32 {
            return None;
        }
        let mut bytes = Vec::with_capacity(EXTENDED_SETTINGS_BYTES);
        push_u32(&mut bytes, 0xf5a5_f5a5);
        push_u16(&mut bytes, 0x6060);
        let trigger = self.trigger_words(16);
        push_trigger_stages_u16(&mut bytes, trigger.map(|words| words.mask));
        push_trigger_stages_u16(&mut bytes, [u16::MAX; DSLOGIC_TRIGGER_STAGE_COUNT]);
        push_trigger_stages_u16(&mut bytes, trigger.map(|words| words.value));
        push_trigger_stages_u16(&mut bytes, [0; DSLOGIC_TRIGGER_STAGE_COUNT]);
        push_trigger_stages_u16(&mut bytes, trigger.map(|words| words.edge));
        push_trigger_stages_u16(&mut bytes, [0; DSLOGIC_TRIGGER_STAGE_COUNT]);
        push_u16(&mut bytes, u16::MAX);
        push_u32(&mut bytes, 0xfa5a_fa5a);
        debug_assert_eq!(bytes.len(), EXTENDED_SETTINGS_BYTES);
        Some(bytes)
    }

    fn trigger_last_stage(self) -> u8 {
        match self.trigger {
            Some(DsLogicTriggerConfig::Smart(trigger)) => trigger.stage_count.saturating_sub(1),
            _ => 0,
        }
    }

    fn trigger_words(self, first_channel: u8) -> [TriggerWords; DSLOGIC_TRIGGER_STAGE_COUNT] {
        let mut stages = [TriggerWords::default(); DSLOGIC_TRIGGER_STAGE_COUNT];
        let Some(trigger) = self.trigger else {
            return stages;
        };
        for (stage_index, conditions) in trigger.condition_sets().into_iter().enumerate() {
            stages[stage_index] = self.trigger_stage_words(conditions, first_channel);
        }
        stages
    }

    fn trigger_stage_words(
        self,
        conditions: [Option<DsLogicTriggerCondition>; CHANNEL_COUNT as usize],
        first_channel: u8,
    ) -> TriggerWords {
        let mut mask = u16::MAX;
        let mut value = 0;
        let mut edge = 0;
        for channel in first_channel..first_channel + 16 {
            let Some(condition) = conditions[usize::from(channel)] else {
                continue;
            };
            let bit = 1_u16 << (channel - first_channel);
            match condition {
                DsLogicTriggerCondition::Low => mask &= !bit,
                DsLogicTriggerCondition::High => {
                    mask &= !bit;
                    value |= bit;
                }
                DsLogicTriggerCondition::RisingEdge => {
                    mask &= !bit;
                    value |= bit;
                    edge |= bit;
                }
                DsLogicTriggerCondition::FallingEdge => {
                    mask &= !bit;
                    edge |= bit;
                }
                DsLogicTriggerCondition::EitherEdge => edge |= bit,
            }
        }
        TriggerWords {
            mask: self.replicate_high_rate_trigger_word(mask),
            value: self.replicate_high_rate_trigger_word(value),
            edge: self.replicate_high_rate_trigger_word(edge),
        }
    }

    fn replicate_high_rate_trigger_word(self, value: u16) -> u16 {
        let quarter_mode = self.sample_rate_hz
            == if self.device_model.logic_hardware_sample_rate_hz() == 500_000_000 {
                1_000_000_000
            } else {
                400_000_000
            };
        let half_mode = self.sample_rate_hz
            == if self.device_model.logic_hardware_sample_rate_hz() == 500_000_000 {
                500_000_000
            } else {
                200_000_000
            };
        if !self.device_model.logic_uses_adf4360() && quarter_mode {
            let low = value & 0x000f;
            low | (low << 4) | (low << 8) | (low << 12)
        } else if (!self.device_model.logic_uses_adf4360() && half_mode)
            || (self.device_model.logic_uses_adf4360() && quarter_mode)
        {
            let low = value & 0x00ff;
            low | (low << 8)
        } else {
            value
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TriggerWords {
    mask: u16,
    value: u16,
    edge: u16,
}

impl Default for TriggerWords {
    fn default() -> Self {
        Self {
            mask: u16::MAX,
            value: 0,
            edge: 0,
        }
    }
}

fn push_trigger_stages_u16(bytes: &mut Vec<u8>, stages: [u16; DSLOGIC_TRIGGER_STAGE_COUNT]) {
    for stage in stages {
        push_u16(bytes, stage);
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
        mut config: DsLogicCaptureConfig,
    ) -> Result<Self, CaptureError> {
        if !device.profile.model.is_logic_analyzer() || !device.profile.model.supports_capture() {
            return Err(CaptureError::new(format!(
                "{} is not a supported DSLogic logic analyzer",
                device.location.stable_id()
            )));
        }
        config.validate_for_device(device.profile, device.speed)?;
        config.device_model = device.profile.model;
        config.device_channel_count = device.profile.channel_count;
        let descriptor = DeviceDescriptor {
            id: DeviceId::new(device.location.stable_id()),
            name: device.profile.display_name.to_owned(),
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
        let capture_result = run_capture(
            &session,
            self.device.profile.model,
            self.config,
            self.device.speed,
            &context,
        );
        let close_result = session.close().map_err(capture_error);
        combine_results(capture_result, close_result, "release the USB interface")
    }
}

fn run_capture(
    session: &UsbDeviceSession,
    model: DeviceModel,
    config: DsLogicCaptureConfig,
    speed: UsbSpeedClass,
    context: &CaptureContext,
) -> Result<(), CaptureError> {
    validate_device_ready(session, model, config)?;
    session.stop_capture().map_err(capture_error)?;
    let capture_result = (|| {
        let settings = config.encode_settings();
        let extended_settings = config.encode_extended_settings();
        session
            .arm_capture_extended(&settings, extended_settings.as_deref())
            .map_err(capture_error)?;
        if config.operation_mode == DsLogicOperationMode::Stream {
            if config.loop_mode {
                return read_loop_stream_capture(
                    session,
                    config,
                    speed,
                    model.logic_trigger_header_bytes(),
                    context,
                );
            }
            return read_stream_capture(
                session,
                config,
                speed,
                model.logic_trigger_header_bytes(),
                context,
            );
        }
        let Some(prequeued) =
            read_prequeued_capture(session, config, model.logic_trigger_header_bytes(), context)?
        else {
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

/// Queues one payload read that is expected to hold `valid_bytes` of capture
/// data, rounded up to the USB transfer alignment.
fn queue_stream_payload_read(
    session: &UsbDeviceSession,
    reads: &mut QueuedReads<usize>,
    valid_bytes: usize,
    config: DsLogicCaptureConfig,
) -> Result<(), CaptureError> {
    let usb_request_bytes = aligned_transfer_bytes(valid_bytes, config);
    reads.push_payload(
        session
            .queue_bulk_read(usb_request_bytes)
            .map_err(capture_error)?,
        valid_bytes,
    );
    Ok(())
}

fn read_stream_capture(
    session: &UsbDeviceSession,
    config: DsLogicCaptureConfig,
    speed: UsbSpeedClass,
    trigger_header_bytes: usize,
    context: &CaptureContext,
) -> Result<(), CaptureError> {
    with_queued_reads(session, "cancel queued stream reads", |reads| {
        read_stream_capture_inner(session, config, speed, trigger_header_bytes, context, reads)
    })
}

fn read_loop_stream_capture(
    session: &UsbDeviceSession,
    config: DsLogicCaptureConfig,
    speed: UsbSpeedClass,
    trigger_header_bytes: usize,
    context: &CaptureContext,
) -> Result<(), CaptureError> {
    with_queued_reads(session, "cancel queued loop-stream reads", |reads| {
        read_loop_stream_capture_inner(session, config, speed, trigger_header_bytes, context, reads)
    })
}

fn read_loop_stream_capture_inner(
    session: &UsbDeviceSession,
    config: DsLogicCaptureConfig,
    speed: UsbSpeedClass,
    trigger_header_bytes: usize,
    context: &CaptureContext,
    reads: &mut QueuedReads<usize>,
) -> Result<(), CaptureError> {
    let (transfer_bytes, queued_transfer_count) = stream_transfer_plan(speed, config);
    reads.set_header(
        session
            .queue_bulk_read(trigger_header_bytes)
            .map_err(capture_error)?,
    );
    while reads.payload_count() < queued_transfer_count {
        queue_stream_payload_read(session, reads, transfer_bytes, config)?;
    }

    context.synchronize_start()?;
    let capture_origin = context.elapsed();
    session.start_capture().map_err(capture_error)?;

    let mut header_complete = false;
    let mut sample_offset = 0_u64;
    let mut sequence = 0_u64;
    loop {
        if context.is_cancelled() {
            return Ok(());
        }

        if let Some(header) = reads.take_completed_header("DSLogic loop-stream trigger-header")? {
            TriggerPosition::decode(&header, trigger_header_bytes)?;
            header_complete = true;
        }

        let mut completed_payloads = Vec::with_capacity(queued_transfer_count);
        if header_complete {
            while let Some((payload, _)) =
                reads.take_completed_payload("DSLogic loop-stream payload")?
            {
                if payload.is_empty() {
                    return Err(CaptureError::new(
                        "the DSLogic loop-stream endpoint returned an empty transfer",
                    ));
                }

                // Keep the host-side queue full before decoding or publishing
                // the completed payload. Those operations can block long
                // enough for a continuously streaming device to overflow if no
                // read is pending.
                queue_stream_payload_read(session, reads, transfer_bytes, config)?;
                completed_payloads.push(payload);
            }
        }

        for payload in completed_payloads {
            emit_wire_batch(
                &payload,
                config,
                capture_origin,
                context,
                &mut sequence,
                &mut sample_offset,
            )?;
        }

        service_usb_events(session)?;
    }
}

fn read_stream_capture_inner(
    session: &UsbDeviceSession,
    config: DsLogicCaptureConfig,
    speed: UsbSpeedClass,
    trigger_header_bytes: usize,
    context: &CaptureContext,
    reads: &mut QueuedReads<usize>,
) -> Result<(), CaptureError> {
    let total_bytes = usize::try_from(config.sample_count & !(SAMPLE_ALIGNMENT - 1))
        .ok()
        .and_then(|samples| samples.checked_mul(config.enabled_channel_count() as usize))
        .map(|bits| bits / 8)
        .ok_or_else(|| CaptureError::new("requested DSLogic stream payload is too large"))?;
    let (transfer_bytes, queued_transfer_count) = stream_transfer_plan(speed, config);
    reads.set_header(
        session
            .queue_bulk_read(trigger_header_bytes)
            .map_err(capture_error)?,
    );

    let mut reserved_bytes = 0_usize;
    while reads.payload_count() < queued_transfer_count && reserved_bytes < total_bytes {
        let valid_bytes = transfer_bytes.min(total_bytes - reserved_bytes);
        queue_stream_payload_read(session, reads, valid_bytes, config)?;
        reserved_bytes += valid_bytes;
    }

    context.synchronize_start()?;
    let capture_origin = context.elapsed();
    session.start_capture().map_err(capture_error)?;

    let capture_time =
        Duration::from_secs_f64(config.sample_count as f64 / config.sample_rate_hz as f64);
    // An armed hardware trigger may legitimately wait indefinitely. Callers
    // retain cancellation/timeout control through CaptureContext.
    let deadline = config
        .trigger
        .is_none()
        .then(|| Instant::now() + capture_time + CAPTURE_COMPLETION_GRACE);
    let mut header = None;
    let mut received_bytes = 0_usize;
    let mut sample_offset = 0_u64;
    let mut sequence = 0_u64;

    while header.is_none() || received_bytes < total_bytes {
        if context.is_cancelled() {
            return Ok(());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(capture_timeout_error(session, "stream capture transfers"));
        }

        if let Some(data) = reads.take_completed_header("DSLogic stream trigger-header")? {
            header = Some(data);
        }

        while let Some((mut payload, valid_bytes)) =
            reads.take_completed_payload("DSLogic stream payload")?
        {
            reserved_bytes -= valid_bytes;
            if payload.is_empty() {
                return Err(CaptureError::new(
                    "the DSLogic stream endpoint returned an empty transfer",
                ));
            }
            // The FPGA pads the final wire block to a complete USB packet.
            // Keep only the logical capture bytes reserved for this transfer.
            payload.truncate(valid_bytes);

            let received_after_payload = received_bytes + payload.len();
            let unreserved_bytes = total_bytes - received_after_payload - reserved_bytes;
            if unreserved_bytes > 0 {
                let valid_bytes = transfer_bytes.min(unreserved_bytes);
                queue_stream_payload_read(session, reads, valid_bytes, config)?;
                reserved_bytes += valid_bytes;
            }

            emit_wire_batch(
                &payload,
                config,
                capture_origin,
                context,
                &mut sequence,
                &mut sample_offset,
            )?;
            received_bytes = received_after_payload;
        }

        if header.is_none() || received_bytes < total_bytes {
            service_usb_events(session)?;
        }
    }

    let header = header.ok_or_else(|| {
        CaptureError::new("DSLogic stream ended before its trigger header completed")
    })?;
    TriggerPosition::decode(&header, trigger_header_bytes)?;
    Ok(())
}

fn stream_transfer_plan(speed: UsbSpeedClass, config: DsLogicCaptureConfig) -> (usize, usize) {
    let (transfer_milliseconds, queued_transfer_count) = if speed >= UsbSpeedClass::SuperSpeed {
        (
            SUPER_SPEED_TRANSFER_MILLISECONDS,
            SUPER_SPEED_QUEUED_TRANSFERS,
        )
    } else {
        (
            HIGH_SPEED_TRANSFER_MILLISECONDS,
            HIGH_SPEED_QUEUED_TRANSFERS,
        )
    };
    let bytes_per_second =
        config.sample_rate_hz as u128 * u128::from(config.enabled_channel_count()) / 8;
    let target = (bytes_per_second * u128::from(transfer_milliseconds) / 1_000)
        .min(i32::MAX as u128) as usize;
    (
        aligned_transfer_bytes(target, config),
        queued_transfer_count,
    )
}

fn validate_device_ready(
    session: &UsbDeviceSession,
    model: DeviceModel,
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
        .initialize_dslogic_frontend(model, config.threshold_mv)
        .map_err(capture_error)?;
    if model.requires_security_handshake() {
        session
            .initialize_dslogic_security()
            .map_err(capture_error)?;
    }
    Ok(())
}

fn read_capture(
    session: &UsbDeviceSession,
    config: DsLogicCaptureConfig,
    prequeued: PrequeuedCapture,
    context: &CaptureContext,
) -> Result<(), CaptureError> {
    let trigger = TriggerPosition::decode(&prequeued.header, prequeued.header.len())?;
    let available_samples = captured_sample_count(config, trigger)?;
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
    let read_buffer_bytes = aligned_transfer_bytes(CAPTURE_READ_BYTES, config);
    let mut wire_buffer = vec![0_u8; read_buffer_bytes];
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
    let mut bytes_remaining = total_bytes - prequeued.payload.len();
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

fn aligned_transfer_bytes(target: usize, config: DsLogicCaptureConfig) -> usize {
    let cross_block_bytes = config.enabled_channel_count() as usize * BYTES_PER_CHANNEL_BLOCK;
    let alignment = least_common_multiple(cross_block_bytes, USB_TRANSFER_ALIGNMENT);
    target.max(1).div_ceil(alignment) * alignment
}

fn least_common_multiple(left: usize, right: usize) -> usize {
    left / greatest_common_divisor(left, right) * right
}

fn greatest_common_divisor(mut left: usize, mut right: usize) -> usize {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn captured_sample_count(
    config: DsLogicCaptureConfig,
    trigger: TriggerPosition,
) -> Result<u64, CaptureError> {
    if config.operation_mode == DsLogicOperationMode::Stream {
        // The vendor driver deliberately ignores the trigger header's
        // remaining-sample count in stream mode. Stream transfers can already
        // contain a full USB buffer while that counter still describes only a
        // small completed portion of the current hardware capture window.
        return Ok(config.sample_count & !(SAMPLE_ALIGNMENT - 1));
    }
    trigger.available_samples(config.sample_count)
}

struct PrequeuedCapture {
    header: Vec<u8>,
    payload: Vec<u8>,
    capture_origin: Duration,
    upload_after_stop: bool,
}

fn read_prequeued_capture(
    session: &UsbDeviceSession,
    config: DsLogicCaptureConfig,
    trigger_header_bytes: usize,
    context: &CaptureContext,
) -> Result<Option<PrequeuedCapture>, CaptureError> {
    let expected_payload_bytes = usize::try_from(config.sample_count & !(SAMPLE_ALIGNMENT - 1))
        .ok()
        .and_then(|samples| samples.checked_mul(config.enabled_channel_count() as usize))
        .map(|bits| bits / 8)
        .ok_or_else(|| CaptureError::new("requested DSLogic payload is too large"))?;
    let prequeued_data_bytes = aligned_transfer_bytes(PREQUEUED_DATA_BYTES, config);
    let mut header = vec![0_u8; trigger_header_bytes];
    let mut payload = vec![0_u8; expected_payload_bytes.min(prequeued_data_bytes)];
    let capture_time =
        Duration::from_secs_f64(config.sample_count as f64 / config.sample_rate_hz as f64);
    // Before a hardware trigger occurs there is no meaningful capture
    // completion deadline. Keep servicing USB so cancellation stays prompt.
    let deadline = config
        .trigger
        .is_none()
        .then(|| Instant::now() + capture_time + CAPTURE_COMPLETION_GRACE);
    let mut header_read = session
        .queue_bulk_read(header.len())
        .map_err(capture_error)?;
    let mut payload_read = match session.queue_bulk_read(payload.len()) {
        Ok(read) => read,
        Err(error) => {
            return Err(abandon_reads_after_error(
                session,
                capture_error(error),
                vec![header_read],
            ));
        }
    };

    context.synchronize_start()?;
    let capture_origin = context.elapsed();
    if let Err(error) = session.start_capture() {
        return Err(abandon_reads_after_error(
            session,
            capture_error(error),
            vec![header_read, payload_read],
        ));
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
        } else if !cancelled
            && timeout_error.is_none()
            && deadline.is_some_and(|deadline| Instant::now() >= deadline)
        {
            timeout_error = Some(capture_timeout_error(session, "queued capture transfers"));
            let _ = session.force_capture_ready();
            header_read.cancel().map_err(capture_error)?;
            payload_read.cancel().map_err(capture_error)?;
        }
        service_usb_events(session)?;
    }

    let header_result = header_read.finish().map_err(capture_error);
    let payload_result = payload_read.finish().map_err(capture_error);
    if cancelled {
        return Ok(None);
    }
    if let Some(error) = timeout_error {
        return Err(error);
    }
    let header_data = require_queued_data(header_result?, "DSLogic trigger-header")?;
    if header_data.len() != trigger_header_bytes {
        return Err(CaptureError::new(format!(
            "short DSLogic trigger header: received {} of {trigger_header_bytes} bytes",
            header_data.len(),
        )));
    }
    header.copy_from_slice(&header_data);
    payload = require_queued_data(payload_result?, "DSLogic payload")?;
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
            channel_count: config.device_channel_count,
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
    let enabled_channel_indices = enabled_channels
        .iter()
        .enumerate()
        .filter_map(|(channel, enabled)| enabled.then_some(channel))
        .collect::<Vec<_>>();
    let enabled_channel_count = enabled_channel_indices.len();
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
            let mut word_bytes = [0_u8; BYTES_PER_CHANNEL_BLOCK];
            word_bytes.copy_from_slice(word);
            channels[channel] = u64::from_le_bytes(word_bytes);
        }
        for bit in 0..ATOMIC_SAMPLES {
            let mut sample = 0_u32;
            for channel in &enabled_channel_indices {
                sample |= (((channels[*channel] >> bit) & 1) as u32) << channel;
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
    fn decode(header: &[u8], expected_bytes: usize) -> Result<Self, CaptureError> {
        if header.len() != expected_bytes {
            return Err(CaptureError::new(format!(
                "DSLogic trigger header has {} bytes; expected {expected_bytes}",
                header.len(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_u2pro16_as_a_capture_source() {
        let device = DiscoveredDevice {
            profile: &crate::DSLOGIC_U2PRO16,
            location: crate::DeviceLocation {
                platform_path: "test-u2pro16".to_owned(),
            },
            speed: UsbSpeedClass::HighSpeed,
        };
        let config =
            DsLogicCaptureConfig::new(1_000_000, 1_000_000).expect("configuration should be valid");

        let source = DsLogicCaptureSource::new(device, config)
            .expect("U2Pro16 should use the DSLogic capture driver");

        assert_eq!(source.descriptor().name, "DSLogic U2Pro16");
        assert_eq!(source.descriptor().kind, InstrumentKind::LogicAnalyzer);
    }

    #[test]
    fn every_dslogic_profile_accepts_a_safe_stream_configuration() {
        for profile in crate::SUPPORTED_PROFILES
            .iter()
            .filter(|profile| profile.model.is_logic_analyzer())
        {
            let device = DiscoveredDevice {
                profile,
                location: crate::DeviceLocation {
                    platform_path: format!("test-{:04x}", profile.product_id),
                },
                speed: profile.minimum_speed,
            };
            let config = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
                .expect("configuration should be valid")
                .with_operation_mode(DsLogicOperationMode::Stream);
            let source = DsLogicCaptureSource::new(device, config)
                .unwrap_or_else(|error| panic!("{}: {error}", profile.display_name));
            assert_eq!(source.descriptor().name, profile.display_name);
        }
    }

    #[test]
    fn u3pro32_encodes_upper_channels_and_extended_trigger_settings() {
        let mut channels = [false; CHANNEL_COUNT as usize];
        channels[31] = true;
        let mut config = DsLogicCaptureConfig::new(100_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_enabled_channels_32(channels)
            .expect("channel selection should be valid");
        config.device_model = DeviceModel::DsLogicU3Pro32;
        config.device_channel_count = 32;
        config = config.with_trigger(Some(DsLogicTriggerConfig::Simple(
            DsLogicSimpleTrigger::new(31, DsLogicTriggerCondition::High).unwrap(),
        )));

        let settings = config.encode_settings();
        assert_eq!(read_u16(&settings, 38), 0);
        assert_eq!(read_u16(&settings, 40), 1 << 15);
        let extended = config
            .encode_extended_settings()
            .expect("U3Pro32 needs extended settings");
        assert_eq!(extended.len(), EXTENDED_SETTINGS_BYTES);
        assert_eq!(read_u16(&extended, 6), 0x7fff);
        assert_eq!(read_u16(&extended, 70), 0x8000);

        let samples = decode_cross_samples(&1_u64.to_le_bytes(), channels)
            .expect("upper-channel data should decode");
        assert_eq!(samples[0], 1_u32 << 31);
    }

    #[test]
    fn legacy_logic_limits_and_headers_follow_the_upstream_profiles() {
        let all_channels = DsLogicCaptureConfig::new(100_000_000, 1_000_000)
            .expect("configuration should be valid");
        assert_eq!(
            all_channels.maximum_sample_rate_hz_for_model(
                DeviceModel::DsLogicPro,
                UsbSpeedClass::HighSpeed
            ),
            100_000_000
        );
        assert_eq!(DeviceModel::DsLogicPro.logic_trigger_header_bytes(), 512);
        assert_eq!(
            DeviceModel::DsLogicU3Pro16.logic_trigger_header_bytes(),
            1_024
        );

        let basic_device = DiscoveredDevice {
            profile: &crate::DSLOGIC_BASIC,
            location: crate::DeviceLocation {
                platform_path: "test-basic".to_owned(),
            },
            speed: UsbSpeedClass::HighSpeed,
        };
        assert!(DsLogicCaptureSource::new(basic_device.clone(), all_channels).is_err());
        let safe_stream = DsLogicCaptureConfig::new(20_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::Stream);
        assert!(DsLogicCaptureSource::new(basic_device, safe_stream).is_ok());
    }

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
    fn stream_capture_depth_is_independent_of_hardware_buffer_depth() {
        let stream = DsLogicCaptureConfig::new(1_000_000_000, 10_000_000_000)
            .expect("stream capture count should fit the software depth")
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_enabled_channels([
                true, true, true, false, false, false, false, false, false, false, false, false,
                false, false, false, false,
            ])
            .expect("three channels should be valid");
        assert!(stream.validate_for_speed(UsbSpeedClass::SuperSpeed).is_ok());

        let buffer = stream.with_operation_mode(DsLogicOperationMode::Buffer);
        assert!(buffer
            .validate_for_speed(UsbSpeedClass::SuperSpeed)
            .is_err());
    }

    #[test]
    fn transfer_buffers_align_to_usb_packets_and_enabled_channel_blocks() {
        for enabled_channel_count in 1..=CHANNEL_COUNT as usize {
            let mut enabled_channels = [false; CHANNEL_COUNT as usize];
            enabled_channels[..enabled_channel_count].fill(true);
            let config = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
                .expect("configuration should be valid")
                .with_operation_mode(DsLogicOperationMode::Stream)
                .with_enabled_channels(enabled_channels)
                .expect("channel selection should be valid");

            let bytes = aligned_transfer_bytes(PREQUEUED_DATA_BYTES, config);
            assert_eq!(bytes % USB_TRANSFER_ALIGNMENT, 0);
            assert_eq!(bytes % (enabled_channel_count * BYTES_PER_CHANNEL_BLOCK), 0);
        }
    }

    #[test]
    fn stream_transfer_plan_matches_the_vendor_queue_depth() {
        let mut three_channels = [false; CHANNEL_COUNT as usize];
        three_channels[..3].fill(true);
        let super_speed = DsLogicCaptureConfig::new(1_000_000_000, 1_000_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_enabled_channels(three_channels)
            .expect("three channels should be valid");
        let (transfer_bytes, transfer_count) =
            stream_transfer_plan(UsbSpeedClass::SuperSpeed, super_speed);
        assert_eq!(transfer_count, 4);
        assert!(transfer_bytes >= 3_750_000);
        assert_eq!(transfer_bytes % USB_TRANSFER_ALIGNMENT, 0);
        assert_eq!(transfer_bytes % (3 * BYTES_PER_CHANNEL_BLOCK), 0);

        let high_speed = DsLogicCaptureConfig::new(20_000_000, 20_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::Stream);
        let (transfer_bytes, transfer_count) =
            stream_transfer_plan(UsbSpeedClass::HighSpeed, high_speed);
        assert_eq!(transfer_count, 5);
        assert!(transfer_bytes >= 800_000);
        assert_eq!(transfer_bytes % USB_TRANSFER_ALIGNMENT, 0);
    }

    #[test]
    fn stream_tail_buffer_is_rounded_up_to_a_complete_usb_packet() {
        let mut three_channels = [false; CHANNEL_COUNT as usize];
        three_channels[..3].fill(true);
        let config = DsLogicCaptureConfig::new(1_000_000_000, 1_000_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_enabled_channels(three_channels)
            .expect("three channels should be valid");
        let valid_bytes = 1_219_968;
        let usb_request_bytes = aligned_transfer_bytes(valid_bytes, config);

        assert!(usb_request_bytes >= valid_bytes);
        assert_eq!(usb_request_bytes % USB_TRANSFER_ALIGNMENT, 0);
        assert_eq!(usb_request_bytes % (3 * BYTES_PER_CHANNEL_BLOCK), 0);
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
    fn encodes_simple_rising_edge_trigger_in_fpga_settings() {
        let trigger = DsLogicSimpleTrigger::new(3, DsLogicTriggerCondition::RisingEdge)
            .expect("trigger should be valid");
        let config = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_trigger(Some(DsLogicTriggerConfig::Simple(trigger)));
        let settings = config.encode_settings();

        assert_eq!(
            config.trigger(),
            Some(DsLogicTriggerConfig::Simple(trigger))
        );
        assert_eq!(read_u16(&settings, 6), 1 << TRIGGER_ENABLE_BIT);
        assert_eq!(read_u16(&settings, 48), u16::MAX ^ (1 << 3));
        assert_eq!(read_u16(&settings, 80), u16::MAX);
        assert_eq!(read_u16(&settings, 112), 1 << 3);
        assert_eq!(read_u16(&settings, 144), 0);
        assert_eq!(read_u16(&settings, 176), 1 << 3);
        assert_eq!(read_u16(&settings, 208), 0);
        assert_eq!(read_u16(&settings, 240), 2);
        assert_eq!(read_u16(&settings, 50), u16::MAX);
    }

    #[test]
    fn combines_simple_channel_conditions_at_one_sample() {
        let trigger = DsLogicSimpleTrigger::new(0, DsLogicTriggerCondition::RisingEdge)
            .unwrap()
            .with_channel_condition(1, DsLogicTriggerCondition::High)
            .unwrap()
            .with_channel_condition(2, DsLogicTriggerCondition::Low)
            .unwrap();
        let settings = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .unwrap()
            .with_trigger(Some(DsLogicTriggerConfig::Simple(trigger)))
            .encode_settings();

        assert_eq!(trigger.condition_count(), 3);
        assert_eq!(read_u16(&settings, 48), u16::MAX ^ 0b111);
        assert_eq!(read_u16(&settings, 112), 0b011);
        assert_eq!(read_u16(&settings, 176), 0b001);
    }

    #[test]
    fn encodes_ordered_smart_trigger_stages() {
        let first_stage = DsLogicTriggerStage::new(0, DsLogicTriggerCondition::RisingEdge)
            .unwrap()
            .with_channel_condition(1, DsLogicTriggerCondition::High)
            .unwrap();
        let second_stage =
            DsLogicTriggerStage::new(2, DsLogicTriggerCondition::FallingEdge).unwrap();
        let trigger = DsLogicSmartTrigger::new(first_stage)
            .with_stage(second_stage)
            .unwrap();
        let settings = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .unwrap()
            .with_trigger(Some(DsLogicTriggerConfig::Smart(trigger)))
            .encode_settings();

        assert_eq!(trigger.stage_count(), 2);
        assert_eq!(read_u16(&settings, 28), 0x1001);
        assert_eq!(read_u16(&settings, 48), u16::MAX ^ 0b11);
        assert_eq!(read_u16(&settings, 50), u16::MAX ^ 0b100);
        assert_eq!(read_u16(&settings, 112), 0b11);
        assert_eq!(read_u16(&settings, 114), 0);
        assert_eq!(read_u16(&settings, 176), 0b01);
        assert_eq!(read_u16(&settings, 178), 0b100);
    }

    #[test]
    fn smart_trigger_requires_buffer_mode() {
        let trigger = DsLogicSmartTrigger::new(
            DsLogicTriggerStage::new(0, DsLogicTriggerCondition::High).unwrap(),
        );
        let config = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .unwrap()
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_trigger(Some(DsLogicTriggerConfig::Smart(trigger)));

        assert!(config
            .validate_for_speed(UsbSpeedClass::SuperSpeed)
            .is_err());
    }

    #[test]
    fn encodes_all_simple_trigger_conditions() {
        let cases = [
            (DsLogicTriggerCondition::Low, u16::MAX ^ 1, 0, 0),
            (DsLogicTriggerCondition::High, u16::MAX ^ 1, 1, 0),
            (DsLogicTriggerCondition::RisingEdge, u16::MAX ^ 1, 1, 1),
            (DsLogicTriggerCondition::FallingEdge, u16::MAX ^ 1, 0, 1),
            (DsLogicTriggerCondition::EitherEdge, u16::MAX, 0, 1),
        ];

        for (condition, mask, value, edge) in cases {
            let trigger = DsLogicSimpleTrigger::new(0, condition).unwrap();
            let settings = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
                .unwrap()
                .with_trigger(Some(DsLogicTriggerConfig::Simple(trigger)))
                .encode_settings();
            assert_eq!(read_u16(&settings, 48), mask, "{condition:?}");
            assert_eq!(read_u16(&settings, 112), value, "{condition:?}");
            assert_eq!(read_u16(&settings, 176), edge, "{condition:?}");
        }
    }

    #[test]
    fn replicates_trigger_bits_for_gigasample_capture() {
        let mut enabled_channels = [false; CHANNEL_COUNT as usize];
        enabled_channels[3] = true;
        let trigger = DsLogicSimpleTrigger::new(3, DsLogicTriggerCondition::RisingEdge).unwrap();
        let settings = DsLogicCaptureConfig::new(1_000_000_000, 1_000_000)
            .unwrap()
            .with_enabled_channels(enabled_channels)
            .unwrap()
            .with_trigger(Some(DsLogicTriggerConfig::Simple(trigger)))
            .encode_settings();

        assert_eq!(read_u16(&settings, 48), 0xf7f7);
        assert_eq!(read_u16(&settings, 112), 0x0808);
        assert_eq!(read_u16(&settings, 176), 0x0808);
    }

    #[test]
    fn validates_simple_trigger_channel() {
        assert!(DsLogicSimpleTrigger::new(32, DsLogicTriggerCondition::High).is_err());

        let mut enabled_channels = [false; CHANNEL_COUNT as usize];
        enabled_channels[0] = true;
        let trigger = DsLogicSimpleTrigger::new(1, DsLogicTriggerCondition::High).unwrap();
        let config = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .unwrap()
            .with_enabled_channels(enabled_channels)
            .unwrap()
            .with_trigger(Some(DsLogicTriggerConfig::Simple(trigger)));
        assert!(config
            .validate_for_speed(UsbSpeedClass::SuperSpeed)
            .is_err());
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
    fn encodes_slow_acquisition_for_sub_packet_wire_rates() {
        let mut one_channel = [false; CHANNEL_COUNT as usize];
        one_channel[0] = true;

        let slow = DsLogicCaptureConfig::new(5_000_000, 5_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_enabled_channels(one_channel)
            .expect("one channel should be valid");
        assert_eq!(
            read_u16(&slow.encode_settings(), 6),
            (1 << STREAM_MODE_BIT) | (1 << SLOW_ACQUISITION_BIT)
        );

        let normal = DsLogicCaptureConfig::new(10_000_000, 10_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_enabled_channels(one_channel)
            .expect("one channel should be valid");
        assert_eq!(read_u16(&normal.encode_settings(), 6), 1 << STREAM_MODE_BIT);
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
    fn encodes_loop_mode_for_continuous_stream_capture() {
        let config = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_loop_mode(true);

        assert!(config.loop_mode());
        assert_eq!(
            read_u16(&config.encode_settings(), 6),
            (1 << STREAM_MODE_BIT) | (1 << LOOP_MODE_BIT)
        );
        assert!(config.validate_for_speed(UsbSpeedClass::SuperSpeed).is_ok());

        let invalid = config.with_operation_mode(DsLogicOperationMode::Buffer);
        assert!(invalid
            .validate_for_speed(UsbSpeedClass::SuperSpeed)
            .is_err());
    }

    #[test]
    fn loop_stream_plan_keeps_multiple_aligned_reads_in_flight() {
        let mut two_channels = [false; CHANNEL_COUNT as usize];
        two_channels[..2].fill(true);
        let config = DsLogicCaptureConfig::new(100_000_000, 1_000_000)
            .expect("configuration should be valid")
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_loop_mode(true)
            .with_enabled_channels(two_channels)
            .expect("channel selection should be valid");
        let (transfer_bytes, transfer_count) =
            stream_transfer_plan(UsbSpeedClass::HighSpeed, config);

        assert_eq!(transfer_bytes, 500_736);
        assert_eq!(transfer_count, 5);
        assert_eq!(transfer_bytes % USB_TRANSFER_ALIGNMENT, 0);
        assert_eq!(
            transfer_bytes % (config.enabled_channel_count() as usize * BYTES_PER_CHANNEL_BLOCK),
            0
        );
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

        let trigger =
            TriggerPosition::decode(&header, TRIGGER_HEADER_BYTES).expect("header should decode");

        assert_eq!(trigger.remaining_samples, 64);
        assert_eq!(trigger.available_samples(1_000_000).unwrap(), 999_424);
    }

    #[test]
    fn stream_capture_ignores_partial_trigger_remaining_count() {
        let trigger = TriggerPosition {
            remaining_samples: 996_928,
        };
        let buffer_config = DsLogicCaptureConfig::new(1_000_000, 1_000_000)
            .expect("buffer configuration should be valid");
        let stream_config = buffer_config.with_operation_mode(DsLogicOperationMode::Stream);

        assert_eq!(
            captured_sample_count(buffer_config, trigger).unwrap(),
            3_072
        );
        assert_eq!(
            captured_sample_count(stream_config, trigger).unwrap(),
            999_424
        );
        assert_eq!(
            captured_sample_count(stream_config, trigger).unwrap()
                * u64::from(stream_config.enabled_channel_count())
                / 8,
            1_998_848
        );
    }

    #[test]
    fn rejects_malformed_or_empty_trigger_results() {
        let header = [0_u8; TRIGGER_HEADER_BYTES];
        assert!(TriggerPosition::decode(&header, TRIGGER_HEADER_BYTES).is_err());

        let trigger = TriggerPosition {
            remaining_samples: 1_000_000,
        };
        assert!(trigger.available_samples(1_000_000).is_err());
    }
}
