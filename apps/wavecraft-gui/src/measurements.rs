use std::collections::VecDeque;
use std::ops::Range;

const LEVEL_HISTOGRAM_BINS: usize = 128;
const LEVEL_HISTOGRAM_SAMPLE_LIMIT: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeMeasurementKind {
    Frequency,
    Period,
    PositiveDuty,
    NegativeDuty,
    PositiveCount,
    RiseTime,
    FallTime,
    PositiveWidth,
    NegativeWidth,
    BurstWidth,
    Amplitude,
    High,
    Low,
    Rms,
    Mean,
    PeakToPeak,
    Maximum,
    Minimum,
    PositiveOvershoot,
    NegativeOvershoot,
}

pub(crate) const SCOPE_MEASUREMENT_KINDS: [ScopeMeasurementKind; 20] = [
    ScopeMeasurementKind::Frequency,
    ScopeMeasurementKind::Period,
    ScopeMeasurementKind::PositiveDuty,
    ScopeMeasurementKind::NegativeDuty,
    ScopeMeasurementKind::PositiveCount,
    ScopeMeasurementKind::RiseTime,
    ScopeMeasurementKind::FallTime,
    ScopeMeasurementKind::PositiveWidth,
    ScopeMeasurementKind::NegativeWidth,
    ScopeMeasurementKind::BurstWidth,
    ScopeMeasurementKind::Amplitude,
    ScopeMeasurementKind::High,
    ScopeMeasurementKind::Low,
    ScopeMeasurementKind::Rms,
    ScopeMeasurementKind::Mean,
    ScopeMeasurementKind::PeakToPeak,
    ScopeMeasurementKind::Maximum,
    ScopeMeasurementKind::Minimum,
    ScopeMeasurementKind::PositiveOvershoot,
    ScopeMeasurementKind::NegativeOvershoot,
];

impl ScopeMeasurementKind {
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Frequency => 0,
            Self::Period => 1,
            Self::PositiveDuty => 2,
            Self::NegativeDuty => 3,
            Self::PositiveCount => 4,
            Self::RiseTime => 5,
            Self::FallTime => 6,
            Self::PositiveWidth => 7,
            Self::NegativeWidth => 8,
            Self::BurstWidth => 9,
            Self::Amplitude => 10,
            Self::High => 11,
            Self::Low => 12,
            Self::Rms => 13,
            Self::Mean => 14,
            Self::PeakToPeak => 15,
            Self::Maximum => 16,
            Self::Minimum => 17,
            Self::PositiveOvershoot => 18,
            Self::NegativeOvershoot => 19,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Frequency => "Freq",
            Self::Period => "Period",
            Self::PositiveDuty => "+Duty",
            Self::NegativeDuty => "−Duty",
            Self::PositiveCount => "+Count",
            Self::RiseTime => "Rise",
            Self::FallTime => "Fall",
            Self::PositiveWidth => "+Width",
            Self::NegativeWidth => "−Width",
            Self::BurstWidth => "Burst W",
            Self::Amplitude => "Ampl",
            Self::High => "High",
            Self::Low => "Low",
            Self::Rms => "RMS",
            Self::Mean => "Mean",
            Self::PeakToPeak => "PK-PK",
            Self::Maximum => "Max",
            Self::Minimum => "Min",
            Self::PositiveOvershoot => "+Over",
            Self::NegativeOvershoot => "−Over",
        }
    }

    fn format(self, value: f64) -> String {
        match self {
            Self::Frequency => format_frequency(value),
            Self::Period
            | Self::RiseTime
            | Self::FallTime
            | Self::PositiveWidth
            | Self::NegativeWidth
            | Self::BurstWidth => format_seconds(value),
            Self::PositiveDuty
            | Self::NegativeDuty
            | Self::PositiveOvershoot
            | Self::NegativeOvershoot => format!("{value:.2}%"),
            Self::PositiveCount => format_count(value),
            Self::Amplitude
            | Self::High
            | Self::Low
            | Self::Rms
            | Self::Mean
            | Self::PeakToPeak
            | Self::Maximum
            | Self::Minimum => format_voltage(value),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeChannelMeasurementSettings {
    pub(crate) enabled: bool,
    pub(crate) selected: [bool; SCOPE_MEASUREMENT_KINDS.len()],
}

impl Default for ScopeChannelMeasurementSettings {
    fn default() -> Self {
        let mut selected = [false; SCOPE_MEASUREMENT_KINDS.len()];
        for kind in [
            ScopeMeasurementKind::Frequency,
            ScopeMeasurementKind::Amplitude,
            ScopeMeasurementKind::Rms,
            ScopeMeasurementKind::PeakToPeak,
        ] {
            selected[kind.index()] = true;
        }
        Self {
            enabled: true,
            selected,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScopeMeasurementSettings {
    pub(crate) channels: [ScopeChannelMeasurementSettings; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScopeMeasurementResults {
    values: [Option<f64>; SCOPE_MEASUREMENT_KINDS.len()],
}

impl Default for ScopeMeasurementResults {
    fn default() -> Self {
        Self {
            values: [None; SCOPE_MEASUREMENT_KINDS.len()],
        }
    }
}

impl ScopeMeasurementResults {
    fn set(&mut self, kind: ScopeMeasurementKind, value: Option<f64>) {
        self.values[kind.index()] = value.filter(|value| value.is_finite());
    }

    pub(crate) fn get(self, kind: ScopeMeasurementKind) -> Option<f64> {
        self.values[kind.index()]
    }

    pub(crate) fn formatted(self, kind: ScopeMeasurementKind) -> String {
        self.get(kind)
            .map(|value| kind.format(value))
            .unwrap_or_else(|| "--".to_owned())
    }
}

#[derive(Default)]
struct IntervalAverage {
    total: f64,
    count: usize,
}

impl IntervalAverage {
    fn add(&mut self, value: f64) {
        if value.is_finite() && value >= 0.0 {
            self.total += value;
            self.count += 1;
        }
    }

    fn samples(&self) -> Option<f64> {
        (self.count > 0).then(|| self.total / self.count as f64)
    }
}

pub(crate) fn calculate_scope_measurements(
    samples: &VecDeque<f32>,
    visible_range: Range<usize>,
    sample_rate_hz: u64,
) -> ScopeMeasurementResults {
    let start = visible_range.start.min(samples.len());
    let end = visible_range.end.min(samples.len()).max(start);
    let mut results = ScopeMeasurementResults::default();
    if start == end {
        return results;
    }

    let mut minimum = f64::INFINITY;
    let mut maximum = f64::NEG_INFINITY;
    let mut sum = 0.0;
    let mut sum_of_squares = 0.0;
    let mut count = 0_usize;
    for sample in samples.iter().skip(start).take(end - start) {
        let sample = f64::from(*sample);
        if !sample.is_finite() {
            continue;
        }
        minimum = minimum.min(sample);
        maximum = maximum.max(sample);
        sum += sample;
        sum_of_squares += sample * sample;
        count += 1;
    }
    if count == 0 {
        return results;
    }

    let mean = sum / count as f64;
    let rms = (sum_of_squares / count as f64).sqrt();
    let peak_to_peak = maximum - minimum;
    results.set(ScopeMeasurementKind::Minimum, Some(minimum));
    results.set(ScopeMeasurementKind::Maximum, Some(maximum));
    results.set(ScopeMeasurementKind::Mean, Some(mean));
    results.set(ScopeMeasurementKind::Rms, Some(rms));
    results.set(ScopeMeasurementKind::PeakToPeak, Some(peak_to_peak));

    let Some((low, high)) = estimate_signal_levels(samples, start..end, minimum, maximum) else {
        return results;
    };
    let amplitude = high - low;
    if amplitude <= f64::EPSILON {
        return results;
    }
    results.set(ScopeMeasurementKind::Low, Some(low));
    results.set(ScopeMeasurementKind::High, Some(high));
    results.set(ScopeMeasurementKind::Amplitude, Some(amplitude));
    results.set(
        ScopeMeasurementKind::PositiveOvershoot,
        Some(((maximum - high) / amplitude).max(0.0) * 100.0),
    );
    results.set(
        ScopeMeasurementKind::NegativeOvershoot,
        Some(((low - minimum) / amplitude).max(0.0) * 100.0),
    );

    if sample_rate_hz == 0 || end - start < 2 {
        return results;
    }

    let low_threshold = low + amplitude * 0.1;
    let midpoint = low + amplitude * 0.5;
    let high_threshold = low + amplitude * 0.9;
    let mut rising_periods = IntervalAverage::default();
    let mut falling_periods = IntervalAverage::default();
    let mut high_widths = IntervalAverage::default();
    let mut low_widths = IntervalAverage::default();
    let mut rise_times = IntervalAverage::default();
    let mut fall_times = IntervalAverage::default();
    let mut previous_rising = None;
    let mut previous_falling = None;
    let mut last_rising = None;
    let mut last_falling = None;
    let mut last_rising_low = None;
    let mut last_falling_high = None;
    let mut first_edge = None::<f64>;
    let mut last_edge = None::<f64>;
    let mut positive_count = 0_usize;
    let mut previous_sample = None::<(usize, f64)>;

    for (index, sample) in samples.iter().enumerate().skip(start).take(end - start) {
        let sample = f64::from(*sample);
        if !sample.is_finite() {
            previous_sample = None;
            continue;
        }
        let Some((previous_index, previous)) = previous_sample else {
            previous_sample = Some((index, sample));
            continue;
        };

        if let Some(position) = rising_crossing(previous_index, previous, sample, low_threshold) {
            last_rising_low = Some(position);
        }
        if let Some(position) = rising_crossing(previous_index, previous, sample, high_threshold) {
            if let Some(low_position) = last_rising_low.take() {
                rise_times.add(position - low_position);
            }
        }
        if let Some(position) = falling_crossing(previous_index, previous, sample, high_threshold) {
            last_falling_high = Some(position);
        }
        if let Some(position) = falling_crossing(previous_index, previous, sample, low_threshold) {
            if let Some(high_position) = last_falling_high.take() {
                fall_times.add(position - high_position);
            }
        }

        if let Some(position) = rising_crossing(previous_index, previous, sample, midpoint) {
            positive_count += 1;
            if let Some(previous_position) = previous_rising.replace(position) {
                rising_periods.add(position - previous_position);
            }
            if let Some(falling_position) = last_falling {
                if falling_position < position {
                    low_widths.add(position - falling_position);
                }
            }
            last_rising = Some(position);
            first_edge.get_or_insert(position);
            last_edge = Some(position);
        }
        if let Some(position) = falling_crossing(previous_index, previous, sample, midpoint) {
            if let Some(previous_position) = previous_falling.replace(position) {
                falling_periods.add(position - previous_position);
            }
            if let Some(rising_position) = last_rising {
                if rising_position < position {
                    high_widths.add(position - rising_position);
                }
            }
            last_falling = Some(position);
            first_edge.get_or_insert(position);
            last_edge = Some(position);
        }
        previous_sample = Some((index, sample));
    }

    let samples_per_period = rising_periods
        .samples()
        .or_else(|| falling_periods.samples());
    let period = samples_per_period.map(|samples| samples / sample_rate_hz as f64);
    results.set(ScopeMeasurementKind::Period, period);
    results.set(
        ScopeMeasurementKind::Frequency,
        period
            .filter(|period| *period > 0.0)
            .map(|period| 1.0 / period),
    );
    results.set(
        ScopeMeasurementKind::PositiveCount,
        Some(positive_count as f64),
    );
    let high_width = high_widths
        .samples()
        .map(|samples| samples / sample_rate_hz as f64);
    let low_width = low_widths
        .samples()
        .map(|samples| samples / sample_rate_hz as f64);
    results.set(ScopeMeasurementKind::PositiveWidth, high_width);
    results.set(ScopeMeasurementKind::NegativeWidth, low_width);
    results.set(
        ScopeMeasurementKind::PositiveDuty,
        high_width
            .zip(period)
            .map(|(width, period)| width / period * 100.0),
    );
    results.set(
        ScopeMeasurementKind::NegativeDuty,
        low_width
            .zip(period)
            .map(|(width, period)| width / period * 100.0),
    );
    results.set(
        ScopeMeasurementKind::RiseTime,
        rise_times
            .samples()
            .map(|samples| samples / sample_rate_hz as f64),
    );
    results.set(
        ScopeMeasurementKind::FallTime,
        fall_times
            .samples()
            .map(|samples| samples / sample_rate_hz as f64),
    );
    results.set(
        ScopeMeasurementKind::BurstWidth,
        first_edge
            .zip(last_edge)
            .filter(|(first, last)| last > first)
            .map(|(first, last)| (last - first) / sample_rate_hz as f64),
    );
    results
}

fn estimate_signal_levels(
    samples: &VecDeque<f32>,
    range: Range<usize>,
    minimum: f64,
    maximum: f64,
) -> Option<(f64, f64)> {
    let span = maximum - minimum;
    if !span.is_finite() || span <= f64::EPSILON {
        return None;
    }
    let step = (range.len() / LEVEL_HISTOGRAM_SAMPLE_LIMIT).max(1);
    let mut histogram = [0_usize; LEVEL_HISTOGRAM_BINS];
    let mut sampled = 0_usize;
    for index in (range.start..range.end).step_by(step) {
        let value = f64::from(samples[index]);
        if !value.is_finite() {
            continue;
        }
        let fraction = ((value - minimum) / span).clamp(0.0, 1.0);
        let bin = (fraction * LEVEL_HISTOGRAM_BINS as f64)
            .floor()
            .min((LEVEL_HISTOGRAM_BINS - 1) as f64) as usize;
        histogram[bin] += 1;
        sampled += 1;
    }
    if sampled < 2 {
        return None;
    }
    let (first_peak, first_count) = histogram
        .iter()
        .copied()
        .enumerate()
        .max_by_key(|(_, count)| *count)?;
    let minimum_peak_separation = LEVEL_HISTOGRAM_BINS / 8;
    let (second_peak, second_count) = histogram
        .iter()
        .copied()
        .enumerate()
        .filter(|(bin, _)| bin.abs_diff(first_peak) >= minimum_peak_separation)
        .max_by_key(|(_, count)| *count)?;
    let uniform_bin_count = sampled as f64 / LEVEL_HISTOGRAM_BINS as f64;
    let peaks_are_distinct = (first_count + second_count) as f64 > uniform_bin_count * 4.0
        && second_count >= (sampled / 10_000).max(2);
    let (low, high) = if peaks_are_distinct {
        let level = |bin: usize| minimum + (bin as f64 + 0.5) / LEVEL_HISTOGRAM_BINS as f64 * span;
        let first = level(first_peak);
        let second = level(second_peak);
        (first.min(second), first.max(second))
    } else {
        (
            histogram_percentile(&histogram, sampled, 0.1, minimum, span),
            histogram_percentile(&histogram, sampled, 0.9, minimum, span),
        )
    };
    (high > low).then_some((low, high))
}

fn histogram_percentile(
    histogram: &[usize; LEVEL_HISTOGRAM_BINS],
    total: usize,
    percentile: f64,
    minimum: f64,
    span: f64,
) -> f64 {
    let target = (percentile * total.saturating_sub(1) as f64).round() as usize;
    let mut cumulative = 0_usize;
    let mut selected_bin = LEVEL_HISTOGRAM_BINS - 1;
    for (bin, count) in histogram.iter().copied().enumerate() {
        cumulative += count;
        if cumulative > target {
            selected_bin = bin;
            break;
        }
    }
    minimum + (selected_bin as f64 + 0.5) / LEVEL_HISTOGRAM_BINS as f64 * span
}

fn rising_crossing(index: usize, previous: f64, current: f64, threshold: f64) -> Option<f64> {
    (previous < threshold && current >= threshold)
        .then(|| interpolated_crossing(index, previous, current, threshold))
}

fn falling_crossing(index: usize, previous: f64, current: f64, threshold: f64) -> Option<f64> {
    (previous > threshold && current <= threshold)
        .then(|| interpolated_crossing(index, previous, current, threshold))
}

fn interpolated_crossing(index: usize, previous: f64, current: f64, threshold: f64) -> f64 {
    let fraction = if current == previous {
        0.0
    } else {
        ((threshold - previous) / (current - previous)).clamp(0.0, 1.0)
    };
    index as f64 + fraction
}

fn format_voltage(value: f64) -> String {
    if value.abs() >= 1.0 {
        format!("{value:.3} V")
    } else if value.abs() >= 1.0e-3 {
        format!("{:.2} mV", value * 1.0e3)
    } else {
        format!("{:.2} µV", value * 1.0e6)
    }
}

fn format_seconds(value: f64) -> String {
    if value >= 1.0 {
        format!("{value:.3} s")
    } else if value >= 1.0e-3 {
        format!("{:.3} ms", value * 1.0e3)
    } else if value >= 1.0e-6 {
        format!("{:.3} µs", value * 1.0e6)
    } else {
        format!("{:.3} ns", value * 1.0e9)
    }
}

fn format_frequency(value: f64) -> String {
    if value >= 1.0e9 {
        format!("{:.3} GHz", value / 1.0e9)
    } else if value >= 1.0e6 {
        format!("{:.3} MHz", value / 1.0e6)
    } else if value >= 1.0e3 {
        format!("{:.3} kHz", value / 1.0e3)
    } else {
        format!("{value:.3} Hz")
    }
}

fn format_count(value: f64) -> String {
    if value >= 1.0e6 {
        format!("{:.3}M", value / 1.0e6)
    } else if value >= 1.0e3 {
        format!("{:.3}K", value / 1.0e3)
    } else {
        format!("{value:.0}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(results: ScopeMeasurementResults, kind: ScopeMeasurementKind) -> f64 {
        results.get(kind).unwrap()
    }

    #[test]
    fn measurement_catalog_matches_the_vendor_application() {
        assert_eq!(SCOPE_MEASUREMENT_KINDS.len(), 20);
        assert_eq!(SCOPE_MEASUREMENT_KINDS[0].label(), "Freq");
        assert_eq!(SCOPE_MEASUREMENT_KINDS[9].label(), "Burst W");
        assert_eq!(SCOPE_MEASUREMENT_KINDS[15].label(), "PK-PK");
        assert_eq!(SCOPE_MEASUREMENT_KINDS[19].label(), "−Over");
    }

    #[test]
    fn each_scope_channel_has_independent_measurement_selection() {
        let mut settings = ScopeMeasurementSettings::default();
        let frequency = ScopeMeasurementKind::Frequency.index();

        settings.channels[0].selected[frequency] = false;
        settings.channels[1].enabled = false;

        assert!(!settings.channels[0].selected[frequency]);
        assert!(settings.channels[0].enabled);
        assert!(settings.channels[1].selected[frequency]);
        assert!(!settings.channels[1].enabled);
    }

    #[test]
    fn square_wave_reports_timing_and_voltage_measurements() {
        let sample_rate_hz = 1_000;
        let samples = (0..500)
            .map(|index| if index % 100 < 40 { 1.0 } else { -1.0 })
            .collect::<VecDeque<_>>();

        let results = calculate_scope_measurements(&samples, 0..samples.len(), sample_rate_hz);

        assert!((value(results, ScopeMeasurementKind::Frequency) - 10.0).abs() < 0.001);
        assert!((value(results, ScopeMeasurementKind::Period) - 0.1).abs() < 0.000_001);
        assert!((value(results, ScopeMeasurementKind::PositiveDuty) - 40.0).abs() < 0.1);
        assert!((value(results, ScopeMeasurementKind::NegativeDuty) - 60.0).abs() < 0.1);
        assert_eq!(value(results, ScopeMeasurementKind::PositiveCount), 4.0);
        assert!((value(results, ScopeMeasurementKind::PositiveWidth) - 0.04).abs() < 0.001);
        assert!((value(results, ScopeMeasurementKind::NegativeWidth) - 0.06).abs() < 0.001);
        assert!((value(results, ScopeMeasurementKind::Amplitude) - 2.0).abs() < 0.02);
        assert!((value(results, ScopeMeasurementKind::PeakToPeak) - 2.0).abs() < 0.001);
        assert!((value(results, ScopeMeasurementKind::Rms) - 1.0).abs() < 0.001);
        assert!((value(results, ScopeMeasurementKind::Mean) + 0.2).abs() < 0.001);
        for kind in SCOPE_MEASUREMENT_KINDS {
            assert!(
                results.get(kind).is_some(),
                "{} was unavailable",
                kind.label()
            );
        }
    }

    #[test]
    fn sine_wave_frequency_uses_interpolated_crossings() {
        let sample_rate_hz = 100_000;
        let samples = (0..1_000)
            .map(|index| {
                (std::f32::consts::TAU * 1_000.0 * index as f32 / sample_rate_hz as f32).sin()
            })
            .collect::<VecDeque<_>>();

        let results = calculate_scope_measurements(&samples, 0..samples.len(), sample_rate_hz);

        assert!((value(results, ScopeMeasurementKind::Frequency) - 1_000.0).abs() < 0.5);
        assert!((value(results, ScopeMeasurementKind::Rms) - 2.0_f64.sqrt() / 2.0).abs() < 0.001);
    }

    #[test]
    fn robust_levels_preserve_overshoot_measurements() {
        let mut samples = (0..1_000)
            .map(|index| if index % 100 < 50 { 1.0 } else { -1.0 })
            .collect::<VecDeque<_>>();
        samples[100] = 1.3;
        samples[200] = -1.2;

        let results = calculate_scope_measurements(&samples, 0..samples.len(), 10_000);

        assert!((value(results, ScopeMeasurementKind::PositiveOvershoot) - 15.0).abs() < 1.0);
        assert!((value(results, ScopeMeasurementKind::NegativeOvershoot) - 10.0).abs() < 1.0);
    }

    #[test]
    fn narrow_pulses_keep_distinct_high_and_low_levels() {
        let samples = (0..10_000)
            .map(|index| if index % 1_000 < 5 { 1.0 } else { 0.0 })
            .collect::<VecDeque<_>>();

        let results = calculate_scope_measurements(&samples, 0..samples.len(), 1_000_000);

        assert!((value(results, ScopeMeasurementKind::Amplitude) - 1.0).abs() < 0.01);
        assert!((value(results, ScopeMeasurementKind::Frequency) - 1_000.0).abs() < 0.1);
        assert!((value(results, ScopeMeasurementKind::PositiveDuty) - 0.5).abs() < 0.05);
    }

    #[test]
    fn constant_signal_keeps_scalar_values_and_rejects_edge_measurements() {
        let samples = VecDeque::from(vec![0.25; 100]);

        let results = calculate_scope_measurements(&samples, 0..samples.len(), 1_000);

        assert_eq!(results.get(ScopeMeasurementKind::Mean), Some(0.25));
        assert_eq!(results.get(ScopeMeasurementKind::Rms), Some(0.25));
        assert_eq!(results.get(ScopeMeasurementKind::PeakToPeak), Some(0.0));
        assert_eq!(results.get(ScopeMeasurementKind::Frequency), None);
        assert_eq!(results.get(ScopeMeasurementKind::Amplitude), None);
    }
}
