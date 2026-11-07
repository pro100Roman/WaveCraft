//! Presentation-only value formatting.

use crate::Duration;

pub(crate) fn format_sample_count(sample_count: u64) -> String {
    if sample_count >= 1_000_000 {
        format!("{}M samples", sample_count / 1_000_000)
    } else if sample_count >= 1_000 {
        format!("{}k samples", sample_count / 1_000)
    } else {
        format!("{sample_count} samples")
    }
}

pub(crate) fn format_input_scale(volts_per_div_mv: u16) -> String {
    if volts_per_div_mv >= 1_000 {
        format!("{} V/div", volts_per_div_mv / 1_000)
    } else {
        format!("{volts_per_div_mv} mV/div")
    }
}

pub(crate) fn format_trigger_voltage(value: f32) -> String {
    if value.abs() >= 1.0 {
        format!("{value:.2} V")
    } else if value.abs() >= 0.1 {
        format!("{:.0} mV", value * 1_000.0)
    } else {
        format!("{:.2} mV", value * 1_000.0)
    }
}

pub(crate) fn format_sample_rate(rate_hz: u64) -> String {
    if rate_hz >= 1_000_000_000 {
        format!("{:.2} GSa/s", rate_hz as f64 / 1_000_000_000.0)
    } else if rate_hz >= 1_000_000 {
        format!("{:.2} MSa/s", rate_hz as f64 / 1_000_000.0)
    } else if rate_hz >= 1_000 {
        format!("{:.2} kSa/s", rate_hz as f64 / 1_000.0)
    } else {
        format!("{rate_hz} Sa/s")
    }
}

pub(crate) fn format_frequency(frequency_hz: f64) -> String {
    if frequency_hz >= 1.0e9 {
        format!("{:.2} GHz", frequency_hz / 1.0e9)
    } else if frequency_hz >= 1.0e6 {
        format!("{:.2} MHz", frequency_hz / 1.0e6)
    } else if frequency_hz >= 1.0e3 {
        format!("{:.2} kHz", frequency_hz / 1.0e3)
    } else {
        format!("{frequency_hz:.1} Hz")
    }
}

pub(crate) fn format_duration(duration: Duration) -> String {
    if duration >= Duration::from_secs(1) {
        format!("{:.2} s", duration.as_secs_f64())
    } else if duration >= Duration::from_millis(1) {
        format!("{:.2} ms", duration.as_secs_f64() * 1_000.0)
    } else if duration >= Duration::from_micros(1) {
        format!("{:.1} µs", duration.as_secs_f64() * 1_000_000.0)
    } else {
        format!("{:.1} ns", duration.as_secs_f64() * 1_000_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    #[test]
    fn scope_settings_map_common_and_per_channel_configuration() {
        let settings = ScopeCaptureSettings {
            sample_rate_hz: 2_000_000,
            sample_count: 250_000,
            channels: [
                ScopeChannelSettings {
                    enabled: true,
                    volts_per_div_mv: 100,
                    coupling: DsCopeCoupling::Ac,
                    probe_attenuation: 100,
                    vertical_offset: 64,
                },
                ScopeChannelSettings {
                    enabled: false,
                    volts_per_div_mv: 2_000,
                    coupling: DsCopeCoupling::Dc,
                    probe_attenuation: 1,
                    vertical_offset: 192,
                },
            ],
            trigger: ScopeTriggerSettings {
                position_percent: 75,
                source: DsCopeTriggerSource::ChannelZero,
                auto_channel: 0,
                slope: DsCopeTriggerSlope::Falling,
                holdoff_ns: 250_000,
                holdoff_unit: TriggerHoldoffUnit::Microseconds,
                noise_sensitivity: 12,
                levels: [64, 192],
            },
            operation_mode: DsCopeOperationMode::InternalTest,
            bandwidth_limit: DsCopeBandwidthLimit::Mhz20,
        };

        let config = settings.capture_config().unwrap();
        assert_eq!(config.sample_rate_hz(), 2_000_000);
        assert_eq!(config.sample_count(), 250_000);
        assert_eq!(config.channels()[0].volts_per_div_mv(), 100);
        assert_eq!(config.channels()[0].coupling(), DsCopeCoupling::Ac);
        assert_eq!(config.channels()[0].probe_attenuation(), 100);
        assert_eq!(
            config.channels()[0].vertical_offset(),
            DSCOPE_VERTICAL_OFFSET_CENTER
        );
        assert_eq!(config.channels()[1].volts_per_div_mv(), 2_000);
        assert_eq!(config.channels()[1].coupling(), DsCopeCoupling::Dc);
        assert_eq!(config.channels()[1].probe_attenuation(), 1);
        assert_eq!(
            config.channels()[1].vertical_offset(),
            DSCOPE_VERTICAL_OFFSET_CENTER
        );
        assert!(!config.channels()[1].enabled());
        assert_eq!(config.trigger().position_percent(), 75);
        assert_eq!(config.trigger().source(), DsCopeTriggerSource::ChannelZero);
        assert_eq!(config.trigger().slope(), DsCopeTriggerSlope::Falling);
        assert_eq!(config.trigger().holdoff_ns(), 250_000);
        assert_eq!(config.trigger().noise_sensitivity(), 12);
        assert_eq!(config.trigger().levels(), [64, 192]);
        assert_eq!(config.operation_mode(), DsCopeOperationMode::InternalTest);
        assert_eq!(config.bandwidth_limit(), DsCopeBandwidthLimit::Mhz20);
        let mut hardware_settings = settings;
        for channel in &mut hardware_settings.channels {
            channel.vertical_offset = DSCOPE_VERTICAL_OFFSET_CENTER;
        }
        assert_eq!(
            ScopeCaptureSettings::from_capture_config(config),
            hardware_settings
        );
        assert_eq!(format_sample_rate(1_000_000), "1.00 MSa/s");
        assert_eq!(format_sample_rate(1_000_000_000), "1.00 GSa/s");
    }
}
