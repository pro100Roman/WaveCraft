//! Logic analyzer capture settings: sample rate and duration, triggers, the
//! per-view session settings, and per-channel display defaults.

use crate::{
    format_duration, AcquisitionMode, Color32, DsLogicCaptureConfig, DsLogicInputFilter,
    DsLogicOperationMode, DsLogicSimpleTrigger, DsLogicSmartTrigger, DsLogicStopOption,
    DsLogicTriggerCondition, DsLogicTriggerConfig, DsLogicTriggerStage, Duration,
    LogicMarkerSettings, ProtocolAnalyzerSettings, UsbSpeedClass, WaveformViewport,
    DEFAULT_LOGIC_CHANNEL_MARKER_OPACITY, DSLOGIC_SAMPLE_ALIGNMENT, DSLOGIC_TRIGGER_STAGE_COUNT,
    LOGIC_CHANNEL_COUNT, ORANGE, SAMPLE_COUNT, SAMPLE_RATE_HZ, TIMEBASE_STEPS_SECONDS,
};

pub(crate) fn empty_logic_channel_names() -> [String; LOGIC_CHANNEL_COUNT] {
    std::array::from_fn(|_| String::new())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LogicTriggerMode {
    Simple,
    Smart,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LogicCaptureSettings {
    pub(crate) sample_rate_hz: u64,
    pub(crate) sample_count: u64,
    pub(crate) enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
    pub(crate) maximum_heights: [u8; LOGIC_CHANNEL_COUNT],
    pub(crate) operation_mode: DsLogicOperationMode,
    pub(crate) stop_option: DsLogicStopOption,
    pub(crate) threshold_voltage: f32,
    pub(crate) input_filter: DsLogicInputFilter,
    pub(crate) rle_enabled: bool,
    pub(crate) external_clock: bool,
    pub(crate) falling_clock_edge: bool,
    pub(crate) trigger_enabled: bool,
    pub(crate) trigger_mode: LogicTriggerMode,
    pub(crate) trigger_conditions: [Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT],
    pub(crate) smart_trigger_stage_count: u8,
    pub(crate) smart_trigger_conditions:
        [[Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT]; DSLOGIC_TRIGGER_STAGE_COUNT],
}

impl Default for LogicCaptureSettings {
    fn default() -> Self {
        let mut trigger_conditions = [None; LOGIC_CHANNEL_COUNT];
        trigger_conditions[0] = Some(DsLogicTriggerCondition::RisingEdge);
        let mut smart_trigger_conditions =
            [[None; LOGIC_CHANNEL_COUNT]; DSLOGIC_TRIGGER_STAGE_COUNT];
        smart_trigger_conditions[0][0] = Some(DsLogicTriggerCondition::RisingEdge);
        smart_trigger_conditions[1][0] = Some(DsLogicTriggerCondition::FallingEdge);
        Self {
            sample_rate_hz: SAMPLE_RATE_HZ,
            sample_count: SAMPLE_COUNT,
            enabled_channels: [true; LOGIC_CHANNEL_COUNT],
            maximum_heights: [1; LOGIC_CHANNEL_COUNT],
            operation_mode: DsLogicOperationMode::Stream,
            stop_option: DsLogicStopOption::UploadCapturedData,
            threshold_voltage: 1.0,
            input_filter: DsLogicInputFilter::None,
            rle_enabled: false,
            external_clock: false,
            falling_clock_edge: false,
            trigger_enabled: false,
            trigger_mode: LogicTriggerMode::Simple,
            trigger_conditions,
            smart_trigger_stage_count: 2,
            smart_trigger_conditions,
        }
    }
}

impl LogicCaptureSettings {
    pub(crate) fn capture_config(self) -> Result<DsLogicCaptureConfig, String> {
        let stream_mode = self.operation_mode == DsLogicOperationMode::Stream;
        let internal_test = self.operation_mode == DsLogicOperationMode::InternalTest;
        DsLogicCaptureConfig::new(
            if internal_test {
                100_000_000
            } else {
                self.sample_rate_hz
            },
            self.sample_count,
        )
        .map_err(|error| error.to_string())?
        .with_threshold_voltage(self.threshold_voltage)
        .map_err(|error| error.to_string())
        .map(|config| {
            config
                .with_operation_mode(self.operation_mode)
                .with_stop_option(if stream_mode {
                    DsLogicStopOption::StopImmediately
                } else {
                    self.stop_option
                })
                .with_input_filter(self.input_filter)
                .with_rle_enabled(self.rle_enabled && !stream_mode && !internal_test)
                .with_external_clock(self.external_clock)
                .with_falling_clock_edge(self.falling_clock_edge)
        })
        .and_then(|config| {
            let config = config
                .with_enabled_channels(if internal_test {
                    [true; LOGIC_CHANNEL_COUNT]
                } else {
                    self.enabled_channels
                })
                .map_err(|error| error.to_string())?;
            if !self.trigger_enabled {
                return Ok(config);
            }
            if internal_test {
                return Err("Logic triggering is unavailable in internal-test mode".to_owned());
            }
            let trigger = match self.trigger_mode {
                LogicTriggerMode::Simple => DsLogicTriggerConfig::Simple(simple_logic_trigger(
                    self.trigger_conditions,
                    self.enabled_channels,
                )?),
                LogicTriggerMode::Smart => {
                    if self.operation_mode != DsLogicOperationMode::Buffer {
                        return Err(
                            "Smart logic triggering requires buffered capture mode".to_owned()
                        );
                    }
                    let stage_count = usize::from(self.smart_trigger_stage_count)
                        .clamp(2, DSLOGIC_TRIGGER_STAGE_COUNT);
                    let mut trigger = DsLogicSmartTrigger::new(logic_trigger_stage(
                        self.smart_trigger_conditions[0],
                        self.enabled_channels,
                        0,
                    )?);
                    for stage_index in 1..stage_count {
                        trigger = trigger
                            .with_stage(logic_trigger_stage(
                                self.smart_trigger_conditions[stage_index],
                                self.enabled_channels,
                                stage_index,
                            )?)
                            .map_err(|error| error.to_string())?;
                    }
                    DsLogicTriggerConfig::Smart(trigger)
                }
            };
            Ok(config.with_trigger(Some(trigger)))
        })
    }

    pub(crate) fn maximum_sample_rate_hz(self, speed: UsbSpeedClass) -> u64 {
        let mut enabled_channels = self.enabled_channels;
        if !enabled_channels.iter().any(|enabled| *enabled) {
            enabled_channels[0] = true;
        }
        default_logic_capture_config()
            .and_then(|config| {
                config
                    .with_operation_mode(self.operation_mode)
                    .with_enabled_channels(enabled_channels)
                    .ok()
            })
            .map_or(SAMPLE_RATE_HZ, |config| {
                config.maximum_sample_rate_hz(speed)
            })
    }

    pub(crate) fn maximum_sample_count(self) -> u64 {
        default_logic_capture_config().map_or(SAMPLE_COUNT, |config| {
            config
                .with_operation_mode(self.operation_mode)
                .maximum_sample_count()
        })
    }
}

/// The capture configuration the logic limits are derived from. The default
/// rate and count are valid constants, so `None` is not expected; callers fall
/// back to the default rate and count instead of panicking if it ever happens.
pub(crate) fn default_logic_capture_config() -> Option<DsLogicCaptureConfig> {
    DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT).ok()
}

pub(crate) fn configured_logic_trigger_conditions(
    conditions: [Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT],
    enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
    stage_index: Option<usize>,
) -> Result<Vec<(usize, DsLogicTriggerCondition)>, String> {
    let configured = conditions
        .into_iter()
        .enumerate()
        .filter_map(|(channel, condition)| condition.map(|condition| (channel, condition)))
        .collect::<Vec<_>>();
    if configured.is_empty() {
        return Err(match stage_index {
            Some(stage) => format!(
                "Configure at least one condition in smart trigger stage {}",
                stage + 1
            ),
            None => "Configure at least one logic trigger condition".to_owned(),
        });
    }
    if let Some((channel, _)) = configured
        .iter()
        .find(|(channel, _)| !enabled_channels[*channel])
    {
        return Err(format!("Logic trigger channel {channel} must be enabled"));
    }
    Ok(configured)
}

pub(crate) fn simple_logic_trigger(
    conditions: [Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT],
    enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
) -> Result<DsLogicSimpleTrigger, String> {
    let mut configured =
        configured_logic_trigger_conditions(conditions, enabled_channels, None)?.into_iter();
    let Some((first_channel, first_condition)) = configured.next() else {
        return Err("Configure at least one logic trigger condition".to_owned());
    };
    let mut trigger = DsLogicSimpleTrigger::new(first_channel as u8, first_condition)
        .map_err(|error| error.to_string())?;
    for (channel, condition) in configured {
        trigger = trigger
            .with_channel_condition(channel as u8, condition)
            .map_err(|error| error.to_string())?;
    }
    Ok(trigger)
}

pub(crate) fn logic_trigger_stage(
    conditions: [Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT],
    enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
    stage_index: usize,
) -> Result<DsLogicTriggerStage, String> {
    let mut configured =
        configured_logic_trigger_conditions(conditions, enabled_channels, Some(stage_index))?
            .into_iter();
    let Some((first_channel, first_condition)) = configured.next() else {
        return Err(format!(
            "Configure at least one condition in smart trigger stage {}",
            stage_index + 1
        ));
    };
    let mut stage = DsLogicTriggerStage::new(first_channel as u8, first_condition)
        .map_err(|error| error.to_string())?;
    for (channel, condition) in configured {
        stage = stage
            .with_channel_condition(channel as u8, condition)
            .map_err(|error| error.to_string())?;
    }
    Ok(stage)
}

pub(crate) fn logic_trigger_condition_label(condition: DsLogicTriggerCondition) -> &'static str {
    match condition {
        DsLogicTriggerCondition::Low => "Low",
        DsLogicTriggerCondition::High => "High",
        DsLogicTriggerCondition::RisingEdge => "Rising edge",
        DsLogicTriggerCondition::FallingEdge => "Falling edge",
        DsLogicTriggerCondition::EitherEdge => "Either edge",
    }
}

pub(crate) fn logic_trigger_condition_key(condition: DsLogicTriggerCondition) -> &'static str {
    match condition {
        DsLogicTriggerCondition::Low => "low",
        DsLogicTriggerCondition::High => "high",
        DsLogicTriggerCondition::RisingEdge => "rising_edge",
        DsLogicTriggerCondition::FallingEdge => "falling_edge",
        DsLogicTriggerCondition::EitherEdge => "either_edge",
    }
}

pub(crate) fn parse_logic_trigger_condition(value: &str) -> Option<DsLogicTriggerCondition> {
    match value {
        "low" => Some(DsLogicTriggerCondition::Low),
        "high" => Some(DsLogicTriggerCondition::High),
        "rising_edge" => Some(DsLogicTriggerCondition::RisingEdge),
        "falling_edge" => Some(DsLogicTriggerCondition::FallingEdge),
        "either_edge" => Some(DsLogicTriggerCondition::EitherEdge),
        _ => None,
    }
}

pub(crate) fn aligned_logic_sample_count(
    duration_seconds: f64,
    sample_rate_hz: u64,
    maximum_sample_count: u64,
) -> Option<u64> {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 || sample_rate_hz == 0 {
        return None;
    }
    let requested = (duration_seconds * sample_rate_hz as f64).ceil();
    if requested < DSLOGIC_SAMPLE_ALIGNMENT as f64 || requested > u64::MAX as f64 {
        return None;
    }
    let aligned = (requested as u64).div_ceil(DSLOGIC_SAMPLE_ALIGNMENT) * DSLOGIC_SAMPLE_ALIGNMENT;
    (aligned <= maximum_sample_count).then_some(aligned)
}

pub(crate) fn logic_capture_duration_options(settings: LogicCaptureSettings) -> Vec<(f64, u64)> {
    let mut options = Vec::new();
    let maximum_sample_count = settings.maximum_sample_count();
    for duration_seconds in TIMEBASE_STEPS_SECONDS {
        let Some(sample_count) = aligned_logic_sample_count(
            duration_seconds,
            settings.sample_rate_hz,
            maximum_sample_count,
        ) else {
            continue;
        };
        if options
            .last()
            .is_some_and(|(_, previous_count)| *previous_count == sample_count)
        {
            continue;
        }
        options.push((duration_seconds, sample_count));
    }
    options
}

pub(crate) fn logic_capture_duration_seconds(settings: LogicCaptureSettings) -> f64 {
    logic_capture_duration_options(settings)
        .into_iter()
        .find_map(|(duration, sample_count)| {
            (sample_count == settings.sample_count).then_some(duration)
        })
        .unwrap_or(settings.sample_count as f64 / settings.sample_rate_hz as f64)
}

pub(crate) fn logic_capture_duration_label(settings: LogicCaptureSettings) -> String {
    format_duration(Duration::from_secs_f64(logic_capture_duration_seconds(
        settings,
    )))
}

pub(crate) fn set_logic_sample_rate_preserving_duration(
    settings: &mut LogicCaptureSettings,
    sample_rate_hz: u64,
) {
    if settings.sample_rate_hz == sample_rate_hz {
        return;
    }
    let duration_seconds = logic_capture_duration_seconds(*settings);
    settings.sample_rate_hz = sample_rate_hz;
    set_logic_capture_duration(settings, duration_seconds);
}

pub(crate) fn set_logic_operation_mode_preserving_duration(
    settings: &mut LogicCaptureSettings,
    operation_mode: DsLogicOperationMode,
) {
    if settings.operation_mode == operation_mode {
        return;
    }
    let duration_seconds = logic_capture_duration_seconds(*settings);
    settings.operation_mode = operation_mode;
    if operation_mode == DsLogicOperationMode::InternalTest {
        settings.sample_rate_hz = 100_000_000;
    }
    set_logic_capture_duration(settings, duration_seconds);
}

pub(crate) fn set_logic_capture_duration(
    settings: &mut LogicCaptureSettings,
    duration_seconds: f64,
) {
    let maximum_sample_count = settings.maximum_sample_count();
    settings.sample_count = aligned_logic_sample_count(
        duration_seconds,
        settings.sample_rate_hz,
        maximum_sample_count,
    )
    .unwrap_or_else(|| {
        if duration_seconds * (settings.sample_rate_hz as f64) < DSLOGIC_SAMPLE_ALIGNMENT as f64 {
            DSLOGIC_SAMPLE_ALIGNMENT
        } else {
            maximum_sample_count / DSLOGIC_SAMPLE_ALIGNMENT * DSLOGIC_SAMPLE_ALIGNMENT
        }
    });
}

pub(crate) fn default_logic_channel_colors() -> [Color32; LOGIC_CHANNEL_COUNT] {
    [
        Color32::from_rgb(90, 220, 145),
        Color32::from_rgb(255, 204, 74),
        Color32::from_rgb(74, 210, 230),
        Color32::from_rgb(238, 112, 214),
        ORANGE,
        Color32::from_rgb(99, 179, 255),
        Color32::from_rgb(180, 230, 80),
        Color32::from_rgb(255, 120, 160),
        Color32::from_rgb(70, 210, 185),
        Color32::from_rgb(245, 177, 66),
        Color32::from_rgb(174, 130, 255),
        Color32::from_rgb(100, 205, 255),
        Color32::from_rgb(245, 100, 100),
        Color32::from_rgb(105, 235, 190),
        Color32::from_rgb(225, 215, 90),
        Color32::from_rgb(195, 155, 255),
    ]
}

pub(crate) fn default_logic_channel_marker_opacities() -> [u8; LOGIC_CHANNEL_COUNT] {
    [DEFAULT_LOGIC_CHANNEL_MARKER_OPACITY; LOGIC_CHANNEL_COUNT]
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LogicChannelSettingsDraft {
    pub(crate) enabled: bool,
    pub(crate) maximum_height: u8,
    pub(crate) name: String,
    pub(crate) color: Color32,
    pub(crate) marker_opacity: u8,
}

pub(crate) fn default_logic_channel_settings_drafts(
) -> [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT] {
    let colors = default_logic_channel_colors();
    std::array::from_fn(|channel| LogicChannelSettingsDraft {
        enabled: true,
        maximum_height: 1,
        name: String::new(),
        color: colors[channel],
        marker_opacity: DEFAULT_LOGIC_CHANNEL_MARKER_OPACITY,
    })
}

#[derive(Clone)]
pub(crate) struct LogicSessionSettings {
    pub(crate) device_id: Option<String>,
    pub(crate) capture: LogicCaptureSettings,
    pub(crate) acquisition_mode: AcquisitionMode,
    pub(crate) repetitive_interval_seconds: f32,
    pub(crate) channel_colors: [Color32; LOGIC_CHANNEL_COUNT],
    pub(crate) channel_marker_opacities: [u8; LOGIC_CHANNEL_COUNT],
    pub(crate) channel_settings_drafts: [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT],
    pub(crate) smart_timing_enabled: bool,
    pub(crate) marker_settings: LogicMarkerSettings,
    pub(crate) protocol_analyzer: ProtocolAnalyzerSettings,
    pub(crate) channel_names: [String; LOGIC_CHANNEL_COUNT],
    pub(crate) viewport: WaveformViewport,
}

impl Default for LogicSessionSettings {
    fn default() -> Self {
        Self {
            device_id: None,
            capture: LogicCaptureSettings::default(),
            acquisition_mode: AcquisitionMode::Single,
            repetitive_interval_seconds: 1.0,
            channel_colors: default_logic_channel_colors(),
            channel_marker_opacities: default_logic_channel_marker_opacities(),
            channel_settings_drafts: default_logic_channel_settings_drafts(),
            smart_timing_enabled: true,
            marker_settings: LogicMarkerSettings::default(),
            protocol_analyzer: ProtocolAnalyzerSettings::default(),
            channel_names: empty_logic_channel_names(),
            viewport: WaveformViewport::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    #[test]
    fn default_logic_capture_config_is_valid() {
        // The limit helpers fall back to the default rate and count when this
        // configuration cannot be built, so it must stay buildable.
        assert!(default_logic_capture_config().is_some());
    }

    #[test]
    fn logic_capture_defaults_to_stream_mode() {
        assert_eq!(
            LogicCaptureSettings::default().operation_mode,
            DsLogicOperationMode::Stream
        );
    }

    #[test]
    fn logic_capture_menu_offers_five_milliseconds_at_125_megahertz() {
        let option = logic_capture_duration_options(LogicCaptureSettings {
            sample_rate_hz: 125_000_000,
            ..LogicCaptureSettings::default()
        })
        .into_iter()
        .find(|(duration, _)| (*duration - 0.005).abs() < f64::EPSILON)
        .expect("5 ms should be available");

        assert_eq!(option.1, 625_664);
        let settings = LogicCaptureSettings {
            sample_rate_hz: 125_000_000,
            sample_count: option.1,
            ..LogicCaptureSettings::default()
        };
        assert_eq!(logic_capture_duration_label(settings), "5.00 ms");
    }

    #[test]
    fn changing_logic_sample_rate_preserves_capture_duration() {
        let mut settings = LogicCaptureSettings {
            sample_rate_hz: 125_000_000,
            sample_count: 625_664,
            ..LogicCaptureSettings::default()
        };

        set_logic_sample_rate_preserving_duration(&mut settings, 250_000_000);

        assert_eq!(settings.sample_rate_hz, 250_000_000);
        assert_eq!(settings.sample_count, 1_250_304);
        assert_eq!(logic_capture_duration_label(settings), "5.00 ms");
    }

    #[test]
    fn stream_duration_choices_do_not_shrink_at_high_sample_rates() {
        let settings = LogicCaptureSettings {
            sample_rate_hz: 1_000_000_000,
            enabled_channels: [
                true, true, true, false, false, false, false, false, false, false, false, false,
                false, false, false, false,
            ],
            ..LogicCaptureSettings::default()
        };

        let ten_seconds = logic_capture_duration_options(settings)
            .into_iter()
            .find(|(duration, _)| (*duration - 10.0).abs() < f64::EPSILON)
            .expect("10 s should remain available in stream mode");

        assert_eq!(ten_seconds.1, 10_000_000_000);
    }

    #[test]
    fn switching_to_buffer_mode_clamps_capture_to_hardware_depth() {
        let mut settings = LogicCaptureSettings {
            sample_rate_hz: 125_000_000,
            sample_count: 1_250_000_128,
            ..LogicCaptureSettings::default()
        };

        set_logic_operation_mode_preserving_duration(&mut settings, DsLogicOperationMode::Buffer);

        assert_eq!(settings.sample_count, 134_217_728);
    }

    #[test]
    fn logic_settings_map_every_device_option() {
        let mut enabled_channels = [false; LOGIC_CHANNEL_COUNT];
        enabled_channels[0] = true;
        enabled_channels[3] = true;
        let mut trigger_conditions = [None; LOGIC_CHANNEL_COUNT];
        trigger_conditions[0] = Some(DsLogicTriggerCondition::RisingEdge);
        trigger_conditions[3] = Some(DsLogicTriggerCondition::FallingEdge);
        let settings = LogicCaptureSettings {
            sample_rate_hz: 250_000_000,
            sample_count: 625_664,
            enabled_channels,
            maximum_heights: [4; LOGIC_CHANNEL_COUNT],
            operation_mode: DsLogicOperationMode::Buffer,
            stop_option: DsLogicStopOption::StopImmediately,
            threshold_voltage: 2.5,
            input_filter: DsLogicInputFilter::OneSampleClock,
            rle_enabled: true,
            external_clock: true,
            falling_clock_edge: true,
            trigger_enabled: true,
            trigger_conditions,
            ..LogicCaptureSettings::default()
        };

        let config = settings
            .capture_config()
            .expect("logic settings should be valid");

        assert_eq!(config.operation_mode(), settings.operation_mode);
        assert_eq!(config.sample_rate_hz(), settings.sample_rate_hz);
        assert_eq!(config.sample_count(), settings.sample_count);
        assert_eq!(config.enabled_channels(), settings.enabled_channels);
        assert_eq!(config.stop_option(), settings.stop_option);
        assert_eq!(config.threshold_voltage(), settings.threshold_voltage);
        assert_eq!(config.input_filter(), settings.input_filter);
        assert!(config.rle_enabled());
        assert!(config.external_clock());
        assert!(config.falling_clock_edge());
        assert_eq!(
            config.trigger(),
            Some(DsLogicTriggerConfig::Simple(
                DsLogicSimpleTrigger::new(0, DsLogicTriggerCondition::RisingEdge)
                    .unwrap()
                    .with_channel_condition(3, DsLogicTriggerCondition::FallingEdge)
                    .unwrap()
            ))
        );
    }

    #[test]
    fn smart_logic_settings_build_ordered_hardware_stages() {
        let mut settings = LogicCaptureSettings {
            operation_mode: DsLogicOperationMode::Buffer,
            trigger_enabled: true,
            trigger_mode: LogicTriggerMode::Smart,
            smart_trigger_stage_count: 2,
            ..LogicCaptureSettings::default()
        };
        settings.smart_trigger_conditions =
            [[None; LOGIC_CHANNEL_COUNT]; DSLOGIC_TRIGGER_STAGE_COUNT];
        settings.smart_trigger_conditions[0][0] = Some(DsLogicTriggerCondition::RisingEdge);
        settings.smart_trigger_conditions[0][1] = Some(DsLogicTriggerCondition::High);
        settings.smart_trigger_conditions[1][2] = Some(DsLogicTriggerCondition::FallingEdge);

        let config = settings
            .capture_config()
            .expect("smart trigger settings should be valid");

        let first_stage = DsLogicTriggerStage::new(0, DsLogicTriggerCondition::RisingEdge)
            .unwrap()
            .with_channel_condition(1, DsLogicTriggerCondition::High)
            .unwrap();
        let second_stage =
            DsLogicTriggerStage::new(2, DsLogicTriggerCondition::FallingEdge).unwrap();
        assert_eq!(
            config.trigger(),
            Some(DsLogicTriggerConfig::Smart(
                DsLogicSmartTrigger::new(first_stage)
                    .with_stage(second_stage)
                    .unwrap()
            ))
        );
    }

    #[test]
    fn stream_mode_ignores_buffer_only_options() {
        let settings = LogicCaptureSettings {
            operation_mode: DsLogicOperationMode::Stream,
            stop_option: DsLogicStopOption::UploadCapturedData,
            rle_enabled: true,
            ..LogicCaptureSettings::default()
        };

        let config = settings
            .capture_config()
            .expect("logic settings should be valid");

        assert_eq!(config.stop_option(), DsLogicStopOption::StopImmediately);
        assert!(!config.rle_enabled());
    }

    #[test]
    fn every_logic_channel_has_a_distinct_default_color() {
        let colors = default_logic_channel_colors();
        for (index, color) in colors.iter().enumerate() {
            assert!(!colors[..index].contains(color));
        }
    }
}
