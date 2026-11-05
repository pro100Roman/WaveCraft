use crate::DspError;
use std::collections::VecDeque;
use std::ops::Range;

const TRANSITION_HYSTERESIS: f32 = 0.12;
const ACQUISITION_TRANSITIONS: usize = 4;
const MAX_ACQUISITION_INTERVALS: usize = 8;
const MAX_LOCKED_PHASE_ERROR_SYMBOLS: f64 = 0.38;
const BAD_TRANSITIONS_BEFORE_UNLOCK: u8 = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct TimingRecoveryConfig {
    pub symbol_rate_hz: u64,
    /// Dimensionless phase-loop gain in the range `(0, 1]`.
    pub loop_bandwidth: f64,
    /// Maximum symbol-clock error relative to the configured rate.
    pub max_clock_error_ppm: u32,
}

impl TimingRecoveryConfig {
    pub fn validate(&self) -> Result<(), DspError> {
        if self.symbol_rate_hz == 0 {
            return Err(DspError::new("symbol timing rate must be nonzero"));
        }
        if !self.loop_bandwidth.is_finite()
            || self.loop_bandwidth <= 0.0
            || self.loop_bandwidth > 1.0
        {
            return Err(DspError::new(
                "symbol timing loop bandwidth must be finite and in (0, 1]",
            ));
        }
        if self.max_clock_error_ppm > 250_000 {
            return Err(DspError::new(
                "symbol timing clock-error limit must not exceed 250000 ppm",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedScalarSample {
    pub value: f32,
    pub source_range: Range<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecoveredSymbol {
    pub value: f32,
    pub source_range: Range<u64>,
}

/// Streaming Mueller-and-Muller clock recovery for a real decision metric.
///
/// This follows GNU Radio's binary GFSK path: interpolate one soft sample,
/// calculate the decision-directed M&M error, update the clock period and
/// fractional phase, then advance to the next symbol.
pub struct MuellerMullerClockRecovery {
    config: TimingRecoveryConfig,
    sample_rate_hz: Option<u64>,
    nominal_period_samples: f64,
    period_samples: f64,
    fractional_phase: f64,
    pending_advance: usize,
    samples: VecDeque<TimedScalarSample>,
    last_output: Option<f32>,
    output_count: usize,
    error_average: f32,
    last_input_start: Option<u64>,
}

impl MuellerMullerClockRecovery {
    pub fn new(config: TimingRecoveryConfig) -> Result<Self, DspError> {
        config.validate()?;
        Ok(Self {
            config,
            sample_rate_hz: None,
            nominal_period_samples: 0.0,
            period_samples: 0.0,
            fractional_phase: 0.5,
            pending_advance: 0,
            samples: VecDeque::new(),
            last_output: None,
            output_count: 0,
            error_average: 1.0,
            last_input_start: None,
        })
    }

    pub fn reset(&mut self) {
        self.sample_rate_hz = None;
        self.nominal_period_samples = 0.0;
        self.period_samples = 0.0;
        self.fractional_phase = 0.5;
        self.pending_advance = 0;
        self.samples.clear();
        self.last_output = None;
        self.output_count = 0;
        self.error_average = 1.0;
        self.last_input_start = None;
    }

    pub fn is_locked(&self) -> bool {
        self.output_count >= 4
    }

    pub fn estimated_symbol_rate_hz(&self) -> Option<f64> {
        (self.output_count >= 2 && self.period_samples > 0.0)
            .then(|| self.sample_rate_hz.unwrap_or_default() as f64 / self.period_samples)
    }

    pub fn lock_quality(&self) -> f32 {
        let acquisition = (self.output_count as f32 / 8.0).clamp(0.0, 1.0);
        acquisition * (1.0 - self.error_average.clamp(0.0, 1.0))
    }

    pub fn push(
        &mut self,
        input: &[TimedScalarSample],
        sample_rate_hz: u64,
        discontinuity: bool,
    ) -> Result<Vec<RecoveredSymbol>, DspError> {
        validate_timing_input(input, sample_rate_hz, self.config.symbol_rate_hz)?;
        if discontinuity
            || self
                .sample_rate_hz
                .is_some_and(|rate| rate != sample_rate_hz)
        {
            self.reset();
        }
        if input.first().is_some_and(|sample| {
            self.last_input_start
                .is_some_and(|start| sample.source_range.start < start)
        }) {
            self.reset();
            return Err(DspError::new(
                "M&M timing-recovery samples are not ordered by source range",
            ));
        }
        if self.sample_rate_hz.is_none() {
            self.sample_rate_hz = Some(sample_rate_hz);
            self.nominal_period_samples = sample_rate_hz as f64 / self.config.symbol_rate_hz as f64;
            self.period_samples = self.nominal_period_samples;
        }
        self.samples.extend(input.iter().cloned());
        if let Some(last) = input.last() {
            self.last_input_start = Some(last.source_range.start);
        }

        self.discard_pending_advance();
        let gain_mu = mueller_muller_phase_gain(self.config.loop_bandwidth);
        let gain_omega = 0.25 * gain_mu * gain_mu;
        let relative_limit = f64::from(self.config.max_clock_error_ppm) / 1_000_000.0;
        let period_limit = self.nominal_period_samples * relative_limit;
        let minimum_period = self.nominal_period_samples - period_limit;
        let maximum_period = self.nominal_period_samples + period_limit;
        let mut recovered = Vec::new();

        while self.pending_advance == 0 && self.samples.len() >= 2 {
            let first = &self.samples[0];
            let second = &self.samples[1];
            let fraction = self.fractional_phase as f32;
            let output = first.value + (second.value - first.value) * fraction;
            let error = self.last_output.map_or(0.0, |previous| {
                binary_slice(previous) * output - binary_slice(output) * previous
            });
            self.last_output = Some(output);
            self.output_count = self.output_count.saturating_add(1);
            self.error_average = self.error_average * 0.9 + error.abs().min(1.0) * 0.1;
            recovered.push(RecoveredSymbol {
                value: output,
                source_range: first.source_range.start..second.source_range.end,
            });

            self.period_samples = (self.period_samples + gain_omega * f64::from(error))
                .clamp(minimum_period, maximum_period);
            let next_phase =
                self.fractional_phase + self.period_samples + gain_mu * f64::from(error);
            let advance = next_phase.floor().max(1.0) as usize;
            self.fractional_phase = next_phase - advance as f64;
            let drained = advance.min(self.samples.len());
            self.samples.drain(..drained);
            self.pending_advance = advance - drained;
            self.discard_pending_advance();
        }
        Ok(recovered)
    }

    fn discard_pending_advance(&mut self) {
        let discard = self.pending_advance.min(self.samples.len());
        self.samples.drain(..discard);
        self.pending_advance -= discard;
    }
}

fn binary_slice(value: f32) -> f32 {
    if value >= 0.0 {
        1.0
    } else {
        -1.0
    }
}

fn mueller_muller_phase_gain(loop_bandwidth: f64) -> f64 {
    // GNU Radio's GFSK defaults derive loop bandwidth from gain_mu and
    // gain_omega = 0.25 * gain_mu^2. Invert that relation so the existing
    // WaveCraft loop-bandwidth setting drives the same M&M update law.
    let combined_gain = 2.0 * (1.0 - (-loop_bandwidth).exp());
    2.0 * ((1.0 + combined_gain).sqrt() - 1.0)
}

/// Streaming transition-aided timing recovery for a real decision metric.
///
/// The loop acquires after two transitions, interpolates decisions at symbol
/// centers, and tracks bounded symbol-clock error. It is suitable for binary
/// FSK/GFSK and amplitude decision streams. Complex-valued PSK/QAM timing can
/// use a separate detector behind the same timed-symbol contracts.
pub struct SymbolTimingRecovery {
    config: TimingRecoveryConfig,
    sample_rate_hz: Option<u64>,
    nominal_period_samples: f64,
    period_samples: f64,
    transition_reference_sample: Option<f64>,
    observed_symbol_periods: u64,
    last_transition_sample: Option<f64>,
    next_symbol_center_sample: Option<f64>,
    previous_sample: Option<TimedScalarSample>,
    next_input_sample: u64,
    locked: bool,
    slicer_level: Option<bool>,
    acquisition_intervals: VecDeque<f64>,
    bad_transition_count: u8,
    lock_quality: f32,
    pending_crossing_sample: Option<f64>,
    acquisition_samples: Vec<TimedScalarSample>,
}

impl SymbolTimingRecovery {
    pub fn new(config: TimingRecoveryConfig) -> Result<Self, DspError> {
        config.validate()?;
        Ok(Self {
            config,
            sample_rate_hz: None,
            nominal_period_samples: 0.0,
            period_samples: 0.0,
            transition_reference_sample: None,
            observed_symbol_periods: 0,
            last_transition_sample: None,
            next_symbol_center_sample: None,
            previous_sample: None,
            next_input_sample: 0,
            locked: false,
            slicer_level: None,
            acquisition_intervals: VecDeque::with_capacity(MAX_ACQUISITION_INTERVALS),
            bad_transition_count: 0,
            lock_quality: 0.0,
            pending_crossing_sample: None,
            acquisition_samples: Vec::new(),
        })
    }

    pub fn config(&self) -> &TimingRecoveryConfig {
        &self.config
    }

    pub fn reset(&mut self) {
        self.sample_rate_hz = None;
        self.nominal_period_samples = 0.0;
        self.period_samples = 0.0;
        self.transition_reference_sample = None;
        self.observed_symbol_periods = 0;
        self.last_transition_sample = None;
        self.next_symbol_center_sample = None;
        self.previous_sample = None;
        self.next_input_sample = 0;
        self.locked = false;
        self.slicer_level = None;
        self.acquisition_intervals.clear();
        self.bad_transition_count = 0;
        self.lock_quality = 0.0;
        self.pending_crossing_sample = None;
        self.acquisition_samples.clear();
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn estimated_symbol_rate_hz(&self) -> Option<f64> {
        (self.locked && self.period_samples > 0.0)
            .then(|| self.sample_rate_hz.unwrap_or_default() as f64 / self.period_samples)
    }

    /// Normalized confidence in the current timing lock.
    pub fn lock_quality(&self) -> f32 {
        self.lock_quality
    }

    pub fn push(
        &mut self,
        samples: &[TimedScalarSample],
        sample_rate_hz: u64,
        discontinuity: bool,
    ) -> Result<Vec<RecoveredSymbol>, DspError> {
        self.validate_input(samples, sample_rate_hz)?;
        if discontinuity
            || self
                .sample_rate_hz
                .is_some_and(|rate| rate != sample_rate_hz)
        {
            self.reset();
        }
        if self.sample_rate_hz.is_none() {
            self.sample_rate_hz = Some(sample_rate_hz);
            self.nominal_period_samples = sample_rate_hz as f64 / self.config.symbol_rate_hz as f64;
            self.period_samples = self.nominal_period_samples;
        }

        let mut recovered = Vec::new();
        for sample in samples {
            let current_sample_index = self.next_input_sample as f64;
            self.next_input_sample = self.next_input_sample.saturating_add(1);
            if !self.locked {
                self.acquisition_samples.push(sample.clone());
            }
            let Some(previous) = self.previous_sample.take() else {
                self.previous_sample = Some(sample.clone());
                continue;
            };
            if sample.source_range.start < previous.source_range.start {
                self.reset();
                return Err(DspError::new(
                    "timing-recovery samples are not ordered by source range",
                ));
            }

            if let Some(crossing) =
                zero_crossing_time(previous.value, sample.value, current_sample_index - 1.0)
            {
                match self.slicer_level {
                    Some(false) if previous.value <= 0.0 && sample.value > 0.0 => {
                        self.pending_crossing_sample = Some(crossing);
                    }
                    Some(false) if previous.value > 0.0 && sample.value <= 0.0 => {
                        self.pending_crossing_sample = None;
                    }
                    Some(true) if previous.value >= 0.0 && sample.value < 0.0 => {
                        self.pending_crossing_sample = Some(crossing);
                    }
                    Some(true) if previous.value < 0.0 && sample.value >= 0.0 => {
                        self.pending_crossing_sample = None;
                    }
                    _ => {}
                }
            }
            if self.observe_slicer_level(sample.value) {
                let denominator = f64::from(previous.value.abs() + sample.value.abs());
                let fallback = current_sample_index - 1.0
                    + if denominator > f64::EPSILON {
                        f64::from(previous.value.abs()) / denominator
                    } else {
                        0.5
                    };
                let transition_sample = self.pending_crossing_sample.take().unwrap_or(fallback);
                self.observe_transition(transition_sample);
            }

            while let Some(center) = self.next_symbol_center_sample {
                if center > current_sample_index {
                    break;
                }
                let previous_sample_index = current_sample_index - 1.0;
                if center >= previous_sample_index {
                    let fraction = (center - previous_sample_index).clamp(0.0, 1.0) as f32;
                    recovered.push(RecoveredSymbol {
                        value: previous.value + (sample.value - previous.value) * fraction,
                        source_range: previous.source_range.start..sample.source_range.end,
                    });
                } else if let Some(symbol) =
                    interpolate_acquisition_sample(&self.acquisition_samples, center)
                {
                    recovered.push(symbol);
                }
                self.next_symbol_center_sample = Some(center + self.period_samples);
            }
            if self.locked {
                self.acquisition_samples.clear();
            }
            self.previous_sample = Some(sample.clone());
        }
        Ok(recovered)
    }

    fn validate_input(
        &self,
        samples: &[TimedScalarSample],
        sample_rate_hz: u64,
    ) -> Result<(), DspError> {
        validate_timing_input(samples, sample_rate_hz, self.config.symbol_rate_hz)
    }
}

fn validate_timing_input(
    samples: &[TimedScalarSample],
    sample_rate_hz: u64,
    symbol_rate_hz: u64,
) -> Result<(), DspError> {
    if sample_rate_hz == 0 {
        return Err(DspError::new("timing-recovery sample rate must be nonzero"));
    }
    if symbol_rate_hz.saturating_mul(2) > sample_rate_hz {
        return Err(DspError::new(
            "symbol timing requires at least two samples per symbol",
        ));
    }
    for sample in samples {
        if !sample.value.is_finite() {
            return Err(DspError::new(
                "timing-recovery sample contains a non-finite value",
            ));
        }
        if sample.source_range.is_empty() {
            return Err(DspError::new(
                "timing-recovery sample has an empty source range",
            ));
        }
    }
    if samples
        .windows(2)
        .any(|samples| samples[0].source_range.start > samples[1].source_range.start)
    {
        return Err(DspError::new(
            "timing-recovery samples are not ordered by source range",
        ));
    }
    Ok(())
}

impl SymbolTimingRecovery {
    fn observe_transition(&mut self, transition_sample: f64) {
        let Some(previous_transition) = self.last_transition_sample else {
            self.transition_reference_sample = Some(transition_sample);
            self.last_transition_sample = Some(transition_sample);
            return;
        };
        let elapsed = transition_sample - previous_transition;
        if elapsed <= 0.0 {
            return;
        }
        let symbol_periods = (elapsed / self.period_samples).round().max(1.0) as u64;
        let interval_period = elapsed / symbol_periods as f64;
        if !self.locked {
            let error_ppm = f64::from(self.config.max_clock_error_ppm) / 1_000_000.0;
            let minimum_period = self.nominal_period_samples * (1.0 - error_ppm);
            let maximum_period = self.nominal_period_samples * (1.0 + error_ppm);
            if (minimum_period..=maximum_period).contains(&interval_period) {
                self.acquisition_intervals.push_back(interval_period);
                while self.acquisition_intervals.len() > MAX_ACQUISITION_INTERVALS {
                    self.acquisition_intervals.pop_front();
                }
            } else {
                self.acquisition_intervals.clear();
                self.transition_reference_sample = Some(transition_sample);
                self.observed_symbol_periods = 0;
            }
        }
        self.observed_symbol_periods = self.observed_symbol_periods.saturating_add(symbol_periods);
        let reference_transition = self
            .transition_reference_sample
            .unwrap_or(previous_transition);
        let measured_period =
            (transition_sample - reference_transition) / self.observed_symbol_periods as f64;
        let error_ppm = f64::from(self.config.max_clock_error_ppm) / 1_000_000.0;
        let minimum_period = self.nominal_period_samples * (1.0 - error_ppm);
        let maximum_period = self.nominal_period_samples * (1.0 + error_ppm);
        let measured_period = measured_period.clamp(minimum_period, maximum_period);

        if !self.locked && self.acquisition_intervals.len() >= ACQUISITION_TRANSITIONS - 1 {
            self.period_samples = measured_period;
            self.next_symbol_center_sample =
                Some((reference_transition - self.period_samples * 0.5).max(0.0));
            self.locked = true;
            self.bad_transition_count = 0;
            self.lock_quality = 0.5;
        } else {
            if !self.locked {
                self.last_transition_sample = Some(transition_sample);
                return;
            }
            let predicted_transition =
                previous_transition + symbol_periods as f64 * self.period_samples;
            let phase_error = transition_sample - predicted_transition;
            let normalized_error = phase_error.abs() / self.period_samples.max(f64::EPSILON);
            if normalized_error > MAX_LOCKED_PHASE_ERROR_SYMBOLS {
                self.bad_transition_count = self.bad_transition_count.saturating_add(1);
                self.lock_quality = (self.lock_quality - 0.2).max(0.0);
                if self.bad_transition_count >= BAD_TRANSITIONS_BEFORE_UNLOCK {
                    self.reacquire_from(transition_sample);
                    return;
                }
            } else {
                self.bad_transition_count = self.bad_transition_count.saturating_sub(1);
                let observation_quality =
                    (1.0 - normalized_error / MAX_LOCKED_PHASE_ERROR_SYMBOLS) as f32;
                self.lock_quality =
                    (self.lock_quality * 0.9 + observation_quality * 0.1).clamp(0.0, 1.0);
            }
            let rate_gain = self.config.loop_bandwidth * self.config.loop_bandwidth * 0.25;
            self.period_samples += rate_gain * (measured_period - self.period_samples);
            self.period_samples = self.period_samples.clamp(minimum_period, maximum_period);
            if let Some(next_center) = &mut self.next_symbol_center_sample {
                *next_center += self.config.loop_bandwidth * phase_error;
            }
        }
        self.last_transition_sample = Some(transition_sample);
    }

    fn observe_slicer_level(&mut self, value: f32) -> bool {
        let next_level = match self.slicer_level {
            None if value >= TRANSITION_HYSTERESIS => Some(true),
            None if value <= -TRANSITION_HYSTERESIS => Some(false),
            Some(false) if value >= TRANSITION_HYSTERESIS => Some(true),
            Some(true) if value <= -TRANSITION_HYSTERESIS => Some(false),
            level => level,
        };
        let transitioned =
            self.slicer_level.is_some() && next_level.is_some() && self.slicer_level != next_level;
        self.slicer_level = next_level;
        transitioned
    }

    fn reacquire_from(&mut self, transition_sample: f64) {
        self.locked = false;
        self.next_symbol_center_sample = None;
        self.transition_reference_sample = Some(transition_sample);
        self.last_transition_sample = Some(transition_sample);
        self.observed_symbol_periods = 0;
        self.acquisition_intervals.clear();
        self.bad_transition_count = 0;
        self.lock_quality = 0.0;
        self.pending_crossing_sample = None;
        self.period_samples = self.nominal_period_samples;
    }
}

fn interpolate_acquisition_sample(
    samples: &[TimedScalarSample],
    center: f64,
) -> Option<RecoveredSymbol> {
    let lower_index = center.floor().max(0.0) as usize;
    let upper_index = lower_index.saturating_add(1);
    let lower = samples.get(lower_index)?;
    let upper = samples.get(upper_index)?;
    let fraction = (center - lower_index as f64).clamp(0.0, 1.0) as f32;
    Some(RecoveredSymbol {
        value: lower.value + (upper.value - lower.value) * fraction,
        source_range: lower.source_range.start..upper.source_range.end,
    })
}

fn zero_crossing_time(previous: f32, current: f32, previous_sample_index: f64) -> Option<f64> {
    let crossed = (previous <= 0.0 && current > 0.0) || (previous >= 0.0 && current < 0.0);
    if !crossed {
        return None;
    }
    let denominator = f64::from(previous.abs() + current.abs());
    let fraction = if denominator > f64::EPSILON {
        f64::from(previous.abs()) / denominator
    } else {
        0.5
    };
    Some(previous_sample_index + fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE_HZ: u64 = 8_000;
    const CONFIGURED_SYMBOL_RATE_HZ: u64 = 1_000;

    fn alternating_samples(
        symbol_count: usize,
        actual_symbol_rate_hz: f64,
        phase_samples: f64,
        first_source_sample: u64,
    ) -> Vec<TimedScalarSample> {
        let period = SAMPLE_RATE_HZ as f64 / actual_symbol_rate_hz;
        let sample_count = (symbol_count as f64 * period).ceil() as usize;
        (0..sample_count)
            .map(|index| {
                let symbol = ((index as f64 + phase_samples) / period).floor() as usize;
                TimedScalarSample {
                    value: if symbol % 2 == 0 { 1.0 } else { -1.0 },
                    source_range: first_source_sample + index as u64
                        ..first_source_sample + index as u64 + 1,
                }
            })
            .collect()
    }

    fn recovery() -> SymbolTimingRecovery {
        SymbolTimingRecovery::new(TimingRecoveryConfig {
            symbol_rate_hz: CONFIGURED_SYMBOL_RATE_HZ,
            loop_bandwidth: 0.2,
            max_clock_error_ppm: 10_000,
        })
        .unwrap()
    }

    fn mueller_muller_recovery() -> MuellerMullerClockRecovery {
        MuellerMullerClockRecovery::new(TimingRecoveryConfig {
            symbol_rate_hz: CONFIGURED_SYMBOL_RATE_HZ,
            loop_bandwidth: 0.1,
            max_clock_error_ppm: 10_000,
        })
        .unwrap()
    }

    #[test]
    fn mueller_muller_recovery_is_chunk_independent_with_arbitrary_phase() {
        let samples = alternating_samples(120, 1_004.0, 2.7, 10_000);
        let mut whole = mueller_muller_recovery();
        let whole_symbols = whole.push(&samples, SAMPLE_RATE_HZ, true).unwrap();

        let mut chunked = mueller_muller_recovery();
        let mut chunked_symbols = Vec::new();
        for (index, range) in [0..7, 7..83, 83..314, 314..samples.len()]
            .into_iter()
            .enumerate()
        {
            chunked_symbols.extend(
                chunked
                    .push(&samples[range], SAMPLE_RATE_HZ, index == 0)
                    .unwrap(),
            );
        }

        assert_eq!(chunked_symbols, whole_symbols);
        assert!(whole.is_locked());
        assert!(whole_symbols.len() >= 115);
        assert!(whole_symbols
            .windows(2)
            .all(|symbols| symbols[0].value.is_sign_positive()
                != symbols[1].value.is_sign_positive()));
        assert!((whole.estimated_symbol_rate_hz().unwrap() - 1_004.0).abs() < 5.0);
    }

    #[test]
    fn locks_with_arbitrary_phase_and_tracks_symbol_clock_error() {
        let actual_symbol_rate_hz = 1_004.0;
        let samples = alternating_samples(120, actual_symbol_rate_hz, 2.7, 10_000);
        let mut recovery = recovery();
        let symbols = recovery.push(&samples, SAMPLE_RATE_HZ, true).unwrap();

        assert!(recovery.is_locked());
        assert!(symbols.len() > 110);
        assert!(symbols.iter().all(|symbol| symbol.value.abs() > 0.9));
        assert!(symbols
            .windows(2)
            .all(|symbols| symbols[0].value.is_sign_positive()
                != symbols[1].value.is_sign_positive()));
        assert!((recovery.estimated_symbol_rate_hz().unwrap() - actual_symbol_rate_hz).abs() < 1.0);
    }

    #[test]
    fn recovery_is_independent_of_batch_boundaries() {
        let samples = alternating_samples(80, 997.0, 5.1, 20_000);
        let mut whole = recovery();
        let whole_symbols = whole.push(&samples, SAMPLE_RATE_HZ, true).unwrap();

        let mut chunked = recovery();
        let mut chunked_symbols = Vec::new();
        for (index, range) in [0..17, 17..103, 103..287, 287..samples.len()]
            .into_iter()
            .enumerate()
        {
            chunked_symbols.extend(
                chunked
                    .push(&samples[range], SAMPLE_RATE_HZ, index == 0)
                    .unwrap(),
            );
        }

        assert_eq!(chunked_symbols, whole_symbols);
        assert_eq!(
            chunked.estimated_symbol_rate_hz(),
            whole.estimated_symbol_rate_hz()
        );
    }

    #[test]
    fn discontinuity_discards_timing_lock_and_partial_state() {
        let samples = alternating_samples(20, 1_000.0, 1.0, 0);
        let mut recovery = recovery();
        assert!(!recovery
            .push(&samples[..40], SAMPLE_RATE_HZ, true)
            .unwrap()
            .is_empty());
        assert!(recovery.is_locked());

        let restarted = alternating_samples(20, 1_000.0, 3.0, 1_000);
        let output = recovery
            .push(&restarted[..8], SAMPLE_RATE_HZ, true)
            .unwrap();
        assert!(output.is_empty());
        assert!(!recovery.is_locked());
    }

    #[test]
    fn hysteresis_ignores_small_noise_crossings() {
        let samples = (0..128)
            .map(|index| TimedScalarSample {
                value: if index % 2 == 0 { 0.05 } else { -0.05 },
                source_range: index..index + 1,
            })
            .collect::<Vec<_>>();
        let mut recovery = recovery();

        assert!(recovery
            .push(&samples, SAMPLE_RATE_HZ, true)
            .unwrap()
            .is_empty());
        assert!(!recovery.is_locked());
        assert_eq!(recovery.lock_quality(), 0.0);
    }
}
