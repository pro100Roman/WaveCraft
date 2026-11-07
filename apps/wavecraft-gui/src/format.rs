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
