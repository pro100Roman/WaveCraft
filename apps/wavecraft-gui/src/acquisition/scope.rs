//! Oscilloscope capture settings: channels, trigger, math and FFT functions, the
//! per-view session settings, and the calibration editor state.

use crate::{
    default_scope_measurement_window_positions, empty_scope_channel_names, scope_channel_label,
    Color32, DsCopeBandwidthLimit, DsCopeCalibrationValues, DsCopeCaptureConfig,
    DsCopeChannelConfig, DsCopeCoupling, DsCopeOperationMode, DsCopeTriggerConfig,
    DsCopeTriggerSlope, DsCopeTriggerSource, ScopeFftViewport, ScopeMarkerSettings,
    ScopeMeasurementCache, ScopeMeasurementSettings, ScopeQuickTimeMeasurement, Vec2,
    WaveformViewport, DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS, DSCOPE_SAMPLE_RATES_HZ,
    DSCOPE_VERTICAL_OFFSET_CENTER, SAMPLE_COUNT, SAMPLE_RATE_HZ, SCOPE_COLORS,
    SCOPE_HORIZONTAL_DIVISIONS,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeChannelSettings {
    pub(crate) enabled: bool,
    pub(crate) volts_per_div_mv: u16,
    pub(crate) coupling: DsCopeCoupling,
    pub(crate) probe_attenuation: u8,
    pub(crate) vertical_offset: u16,
}

impl Default for ScopeChannelSettings {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TriggerHoldoffUnit {
    Microseconds,
    Milliseconds,
    Seconds,
}

impl TriggerHoldoffUnit {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Microseconds => "µs",
            Self::Milliseconds => "ms",
            Self::Seconds => "s",
        }
    }

    pub(crate) fn nanoseconds(self) -> f64 {
        match self {
            Self::Microseconds => 1_000.0,
            Self::Milliseconds => 1_000_000.0,
            Self::Seconds => 1_000_000_000.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeTriggerSettings {
    pub(crate) position_percent: u8,
    pub(crate) source: DsCopeTriggerSource,
    pub(crate) auto_channel: u8,
    pub(crate) slope: DsCopeTriggerSlope,
    pub(crate) holdoff_ns: u64,
    pub(crate) holdoff_unit: TriggerHoldoffUnit,
    pub(crate) noise_sensitivity: u8,
    pub(crate) levels: [u8; 2],
}

impl Default for ScopeTriggerSettings {
    fn default() -> Self {
        Self::from_capture_config(DsCopeTriggerConfig::default())
    }
}

impl ScopeTriggerSettings {
    pub(crate) fn capture_config(self) -> Result<DsCopeTriggerConfig, String> {
        DsCopeTriggerConfig::new(self.source, self.slope)
            .with_position_percent(self.position_percent)
            .and_then(|trigger| trigger.with_auto_channel(self.auto_channel))
            .and_then(|trigger| trigger.with_holdoff_ns(self.holdoff_ns))
            .and_then(|trigger| trigger.with_noise_sensitivity(self.noise_sensitivity))
            .and_then(|trigger| trigger.with_levels(self.levels))
            .map_err(|error| error.to_string())
    }

    pub(crate) fn from_capture_config(config: DsCopeTriggerConfig) -> Self {
        let holdoff_unit = if config.holdoff_ns() >= 1_000_000_000 {
            TriggerHoldoffUnit::Seconds
        } else if config.holdoff_ns() >= 1_000_000 {
            TriggerHoldoffUnit::Milliseconds
        } else {
            TriggerHoldoffUnit::Microseconds
        };
        Self {
            position_percent: config.position_percent(),
            source: config.source(),
            auto_channel: config.auto_channel(),
            slope: config.slope(),
            holdoff_ns: config.holdoff_ns(),
            holdoff_unit,
            noise_sensitivity: config.noise_sensitivity(),
            levels: config.levels(),
        }
    }

    pub(crate) fn source_is_available(self, channels: [bool; 2]) -> bool {
        match self.source {
            DsCopeTriggerSource::Auto => channels[usize::from(self.auto_channel)],
            DsCopeTriggerSource::ChannelZero => channels[0],
            DsCopeTriggerSource::ChannelOne => channels[1],
            DsCopeTriggerSource::ChannelZeroAndOne => channels[0] && channels[1],
            DsCopeTriggerSource::ChannelZeroOrOne => channels[0] || channels[1],
        }
    }

    pub(crate) fn clamp_to_channels(&mut self, channels: [bool; 2]) {
        if !channels[usize::from(self.auto_channel)] {
            self.auto_channel = if channels[0] { 0 } else { 1 };
        }
        if !self.source_is_available(channels) {
            self.source = DsCopeTriggerSource::Auto;
        }
    }

    pub(crate) fn hardware_differs_from(self, other: Self) -> bool {
        self.position_percent != other.position_percent
            || self.source != other.source
            || self.auto_channel != other.auto_channel
            || self.slope != other.slope
            || self.holdoff_ns != other.holdoff_ns
            || self.noise_sensitivity != other.noise_sensitivity
            || self.levels != other.levels
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeMathOperation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl ScopeMathOperation {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Multiply => "Multiply",
            Self::Divide => "Divide",
        }
    }

    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "−",
            Self::Multiply => "×",
            Self::Divide => "÷",
        }
    }

    pub(crate) fn apply(self, left: f32, right: f32) -> Option<f32> {
        let value = match self {
            Self::Add => left + right,
            Self::Subtract => left - right,
            Self::Multiply => left * right,
            Self::Divide if right.abs() > f32::EPSILON => left / right,
            Self::Divide => return None,
        };
        value.is_finite().then_some(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeMathSettings {
    pub(crate) enabled: bool,
    pub(crate) operation: ScopeMathOperation,
    pub(crate) left_channel: usize,
    pub(crate) right_channel: usize,
}

impl Default for ScopeMathSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            operation: ScopeMathOperation::Add,
            left_channel: 0,
            right_channel: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeFftWindow {
    Rectangular,
    Hann,
    Hamming,
    Blackman,
    FlatTop,
}

impl ScopeFftWindow {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Rectangular => "Rectangular",
            Self::Hann => "Hann",
            Self::Hamming => "Hamming",
            Self::Blackman => "Blackman",
            Self::FlatTop => "Flat Top",
        }
    }

    pub(crate) fn coefficient(self, index: usize, length: usize) -> f64 {
        if self == Self::Rectangular || length <= 1 {
            return 1.0;
        }
        let phase = std::f64::consts::TAU * index as f64 / (length - 1) as f64;
        match self {
            Self::Rectangular => 1.0,
            Self::Hann => 0.5 * (1.0 - phase.cos()),
            Self::Hamming => 0.54 - 0.46 * phase.cos(),
            Self::Blackman => 0.426_59 - 0.496_56 * phase.cos() + 0.076_849 * (2.0 * phase).cos(),
            Self::FlatTop => {
                1.0 - 1.93 * phase.cos() + 1.29 * (2.0 * phase).cos() - 0.388 * (3.0 * phase).cos()
                    + 0.028 * (4.0 * phase).cos()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeFftSettings {
    pub(crate) enabled: bool,
    pub(crate) channel: usize,
    pub(crate) length: usize,
    pub(crate) window: ScopeFftWindow,
}

impl Default for ScopeFftSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: 0,
            length: 16_384,
            window: ScopeFftWindow::Hann,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeLissajousSettings {
    pub(crate) enabled: bool,
    pub(crate) x_channel: usize,
    pub(crate) y_channel: usize,
}

impl Default for ScopeLissajousSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            x_channel: 0,
            y_channel: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScopeFunctionSettings {
    pub(crate) math: ScopeMathSettings,
    pub(crate) fft: ScopeFftSettings,
    pub(crate) lissajous: ScopeLissajousSettings,
    pub(crate) measurements: ScopeMeasurementSettings,
}

impl ScopeFunctionSettings {
    pub(crate) fn clamp_to_channels(&mut self, channels: [bool; 2]) {
        let fallback = if channels[0] { 0 } else { 1 };
        if !channels[self.math.left_channel] {
            self.math.left_channel = fallback;
        }
        if !channels[self.math.right_channel] {
            self.math.right_channel = fallback;
        }
        if !channels[self.fft.channel] {
            self.fft.channel = fallback;
        }
        if !channels[self.lissajous.x_channel] {
            self.lissajous.x_channel = fallback;
        }
        if !channels[self.lissajous.y_channel] {
            self.lissajous.y_channel = fallback;
        }
        if channels.into_iter().filter(|enabled| *enabled).count() < 2 {
            self.lissajous.enabled = false;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeCaptureSettings {
    pub(crate) sample_rate_hz: u64,
    pub(crate) sample_count: u64,
    pub(crate) channels: [ScopeChannelSettings; 2],
    pub(crate) trigger: ScopeTriggerSettings,
    pub(crate) operation_mode: DsCopeOperationMode,
    pub(crate) bandwidth_limit: DsCopeBandwidthLimit,
}

impl Default for ScopeCaptureSettings {
    fn default() -> Self {
        Self {
            sample_rate_hz: SAMPLE_RATE_HZ,
            sample_count: SAMPLE_COUNT,
            channels: [ScopeChannelSettings::default(); 2],
            trigger: ScopeTriggerSettings::default(),
            operation_mode: DsCopeOperationMode::Normal,
            bandwidth_limit: DsCopeBandwidthLimit::Full,
        }
    }
}

impl ScopeCaptureSettings {
    pub(crate) fn capture_config(self) -> Result<DsCopeCaptureConfig, String> {
        let mut channels = [DsCopeChannelConfig::default(); 2];
        for (destination, source) in channels.iter_mut().zip(self.channels) {
            // Keep the analog frontend centered. `ScopeChannelSettings` keeps
            // the independently movable display offset used by the plot.
            *destination = DsCopeChannelConfig::new(
                source.volts_per_div_mv,
                source.coupling,
                source.probe_attenuation,
            )
            .map_err(|error| error.to_string())?
            .with_enabled(source.enabled);
        }
        DsCopeCaptureConfig::with_channel_configs(self.sample_rate_hz, self.sample_count, channels)
            .map_err(|error| error.to_string())?
            .with_trigger_config(self.trigger.capture_config()?)
            .map_err(|error| error.to_string())
            .map(|config| config.with_options(self.operation_mode, self.bandwidth_limit))
    }

    pub(crate) fn enabled_channel_count(self) -> usize {
        self.channels
            .iter()
            .filter(|channel| channel.enabled)
            .count()
    }

    pub(crate) fn maximum_sample_rate_hz(self) -> u64 {
        if self.enabled_channel_count() == 1 {
            1_000_000_000
        } else {
            500_000_000
        }
    }

    pub(crate) fn maximum_sample_count(self) -> u64 {
        if self.enabled_channel_count() == 1 {
            2_000_000
        } else {
            1_000_000
        }
    }

    pub(crate) fn sample_rate_for_timebase(self, time_per_division_seconds: f64) -> u64 {
        if !time_per_division_seconds.is_finite() || time_per_division_seconds <= 0.0 {
            return self.sample_rate_hz;
        }
        let requested_rate = self.sample_count as f64
            / (time_per_division_seconds * SCOPE_HORIZONTAL_DIVISIONS as f64);
        let maximum_rate = self.maximum_sample_rate_hz();
        DSCOPE_SAMPLE_RATES_HZ
            .iter()
            .rev()
            .copied()
            .find(|rate| *rate <= maximum_rate && *rate as f64 <= requested_rate * (1.0 + 1.0e-9))
            .unwrap_or(DSCOPE_SAMPLE_RATES_HZ[0])
    }

    pub(crate) fn clamp_to_channel_limits(&mut self) {
        self.sample_rate_hz = self.sample_rate_hz.min(self.maximum_sample_rate_hz());
        self.sample_count = self.sample_count.min(self.maximum_sample_count());
        self.trigger
            .clamp_to_channels(self.channels.map(|channel| channel.enabled));
    }

    pub(crate) fn frontend_differs_from(self, other: Self) -> bool {
        self.sample_rate_hz != other.sample_rate_hz
            || self.operation_mode != other.operation_mode
            || self.bandwidth_limit != other.bandwidth_limit
            || self
                .channels
                .iter()
                .zip(other.channels)
                .any(|(left, right)| {
                    left.enabled != right.enabled
                        || left.volts_per_div_mv != right.volts_per_div_mv
                        || left.coupling != right.coupling
                })
            || self.trigger.hardware_differs_from(other.trigger)
    }

    pub(crate) fn runtime_frontend_differs_from(self, other: Self) -> bool {
        self.channels
            .iter()
            .zip(other.channels)
            .any(|(left, right)| {
                left.enabled != right.enabled || left.volts_per_div_mv != right.volts_per_div_mv
            })
    }

    pub(crate) fn from_capture_config(config: DsCopeCaptureConfig) -> Self {
        Self {
            sample_rate_hz: config.sample_rate_hz(),
            sample_count: config.sample_count(),
            channels: config.channels().map(|channel| ScopeChannelSettings {
                enabled: channel.enabled(),
                volts_per_div_mv: channel.volts_per_div_mv(),
                coupling: channel.coupling(),
                probe_attenuation: channel.probe_attenuation(),
                vertical_offset: channel.vertical_offset(),
            }),
            trigger: ScopeTriggerSettings::from_capture_config(config.trigger()),
            operation_mode: config.operation_mode(),
            bandwidth_limit: config.bandwidth_limit(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeCalibrationAction {
    Auto,
    Manual,
}

pub(crate) struct ManualCalibrationEditor {
    pub(crate) values: DsCopeCalibrationValues,
    pub(crate) range: usize,
}

#[derive(Clone)]
pub(crate) struct ScopeSessionSettings {
    pub(crate) device_id: Option<String>,
    pub(crate) capture: ScopeCaptureSettings,
    pub(crate) auto_sample_rate: bool,
    pub(crate) smart_timing_enabled: bool,
    pub(crate) function_settings: ScopeFunctionSettings,
    pub(crate) marker_settings: ScopeMarkerSettings,
    pub(crate) quick_time_measurement: ScopeQuickTimeMeasurement,
    pub(crate) channel_colors: [Color32; 2],
    pub(crate) channel_names: [String; 2],
    pub(crate) viewport: WaveformViewport,
    pub(crate) fft_viewport: ScopeFftViewport,
    pub(crate) measurement_caches: [ScopeMeasurementCache; 2],
    pub(crate) measurement_window_positions: [Vec2; 2],
    pub(crate) time_per_division_seconds: Option<f64>,
}

impl Default for ScopeSessionSettings {
    fn default() -> Self {
        Self {
            device_id: None,
            capture: ScopeCaptureSettings::default(),
            auto_sample_rate: true,
            smart_timing_enabled: false,
            function_settings: ScopeFunctionSettings::default(),
            marker_settings: ScopeMarkerSettings::default(),
            quick_time_measurement: ScopeQuickTimeMeasurement::default(),
            channel_colors: SCOPE_COLORS,
            channel_names: empty_scope_channel_names(),
            viewport: WaveformViewport::default(),
            fft_viewport: ScopeFftViewport::default(),
            measurement_caches: [ScopeMeasurementCache::default(); 2],
            measurement_window_positions: default_scope_measurement_window_positions(),
            time_per_division_seconds: Some(DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS),
        }
    }
}

pub(crate) fn coupling_label(coupling: DsCopeCoupling) -> &'static str {
    match coupling {
        DsCopeCoupling::Dc => "DC",
        DsCopeCoupling::Ac => "AC",
    }
}

pub(crate) fn trigger_source_label(
    source: DsCopeTriggerSource,
    channel_names: &[String; 2],
) -> String {
    match source {
        DsCopeTriggerSource::Auto => "Auto".to_owned(),
        DsCopeTriggerSource::ChannelZero => scope_channel_label(0, channel_names),
        DsCopeTriggerSource::ChannelOne => scope_channel_label(1, channel_names),
        DsCopeTriggerSource::ChannelZeroAndOne => format!(
            "{} AND {}",
            scope_channel_label(0, channel_names),
            scope_channel_label(1, channel_names)
        ),
        DsCopeTriggerSource::ChannelZeroOrOne => format!(
            "{} OR {}",
            scope_channel_label(0, channel_names),
            scope_channel_label(1, channel_names)
        ),
    }
}

pub(crate) fn trigger_slope_label(slope: DsCopeTriggerSlope) -> &'static str {
    match slope {
        DsCopeTriggerSlope::Rising => "Rising edge",
        DsCopeTriggerSlope::Falling => "Falling edge",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    #[test]
    fn scope_math_operations_use_captured_channel_values() {
        assert_eq!(ScopeMathOperation::Add.apply(6.0, 2.0), Some(8.0));
        assert_eq!(ScopeMathOperation::Subtract.apply(6.0, 2.0), Some(4.0));
        assert_eq!(ScopeMathOperation::Multiply.apply(6.0, 2.0), Some(12.0));
        assert_eq!(ScopeMathOperation::Divide.apply(6.0, 2.0), Some(3.0));
        assert_eq!(ScopeMathOperation::Divide.apply(6.0, 0.0), None);
    }

    #[test]
    fn scope_timebase_selects_the_vendor_floor_sample_rate() {
        let mut settings = ScopeCaptureSettings::default();

        assert_eq!(settings.sample_rate_for_timebase(1.0e-3), 100_000_000);
        assert_eq!(settings.sample_rate_for_timebase(500.0e-6), 125_000_000);
        assert_eq!(settings.sample_rate_for_timebase(200.0e-6), 500_000_000);
        assert_eq!(settings.sample_rate_for_timebase(10.0), 10_000);
        assert_eq!(
            settings.sample_rate_for_timebase(f64::NAN),
            settings.sample_rate_hz
        );

        settings.channels[1].enabled = false;
        settings.sample_count = 2_000_000;
        assert_eq!(settings.sample_rate_for_timebase(200.0e-6), 1_000_000_000);
    }

    #[test]
    fn scope_limits_follow_the_enabled_channel_count() {
        let mut settings = ScopeCaptureSettings::default();
        assert_eq!(settings.maximum_sample_rate_hz(), 500_000_000);
        assert_eq!(settings.maximum_sample_count(), 1_000_000);

        settings.channels[1].enabled = false;
        settings.sample_rate_hz = 1_000_000_000;
        settings.sample_count = 2_000_000;

        assert_eq!(settings.maximum_sample_rate_hz(), 1_000_000_000);
        assert_eq!(settings.maximum_sample_count(), 2_000_000);
        assert!(settings.capture_config().is_ok());
    }

    #[test]
    fn scope_frontend_change_detection_ignores_display_conversion_and_buffer() {
        let original = ScopeCaptureSettings::default();
        let mut changed = original;
        changed.sample_count = 250_000;
        changed.channels[0].probe_attenuation = 10;
        assert!(!changed.frontend_differs_from(original));

        changed = original;
        changed.channels[1].enabled = false;
        assert!(changed.frontend_differs_from(original));
        assert!(changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.channels[0].volts_per_div_mv = 100;
        assert!(changed.frontend_differs_from(original));
        assert!(changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.channels[0].vertical_offset = 64;
        assert!(!changed.frontend_differs_from(original));
        assert!(!changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.channels[0].coupling = DsCopeCoupling::Ac;
        assert!(changed.frontend_differs_from(original));
        assert!(!changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.trigger.holdoff_unit = TriggerHoldoffUnit::Seconds;
        assert!(!changed.frontend_differs_from(original));

        changed.trigger.slope = DsCopeTriggerSlope::Falling;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.trigger.levels[0] = 64;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.operation_mode = DsCopeOperationMode::InternalTest;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.bandwidth_limit = DsCopeBandwidthLimit::Mhz20;
        assert!(changed.frontend_differs_from(original));
    }

    #[test]
    fn scope_trigger_source_tracks_enabled_channels() {
        let mut trigger = ScopeTriggerSettings {
            source: DsCopeTriggerSource::ChannelOne,
            auto_channel: 1,
            ..ScopeTriggerSettings::default()
        };

        trigger.clamp_to_channels([true, false]);

        assert_eq!(trigger.source, DsCopeTriggerSource::Auto);
        assert_eq!(trigger.auto_channel, 0);
        assert!(trigger.capture_config().is_ok());
    }
}
