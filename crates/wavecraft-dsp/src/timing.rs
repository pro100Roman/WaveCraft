use crate::DspError;
use std::ops::Range;

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
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn estimated_symbol_rate_hz(&self) -> Option<f64> {
        (self.locked && self.period_samples > 0.0)
            .then(|| self.sample_rate_hz.unwrap_or_default() as f64 / self.period_samples)
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

            if crossed_threshold(previous.value, sample.value) {
                let denominator = f64::from(previous.value.abs() + sample.value.abs());
                let fraction = if denominator > f64::EPSILON {
                    f64::from(previous.value.abs()) / denominator
                } else {
                    0.5
                };
                self.observe_transition(current_sample_index - 1.0 + fraction);
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
                }
                self.next_symbol_center_sample = Some(center + self.period_samples);
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
        if sample_rate_hz == 0 {
            return Err(DspError::new("timing-recovery sample rate must be nonzero"));
        }
        if self.config.symbol_rate_hz.saturating_mul(2) > sample_rate_hz {
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
        Ok(())
    }

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

        if !self.locked {
            self.period_samples = measured_period;
            self.next_symbol_center_sample = Some(transition_sample + self.period_samples * 0.5);
            self.locked = true;
        } else {
            let predicted_transition =
                previous_transition + symbol_periods as f64 * self.period_samples;
            let phase_error = transition_sample - predicted_transition;
            let rate_gain = self.config.loop_bandwidth * self.config.loop_bandwidth * 0.25;
            self.period_samples += rate_gain * (measured_period - self.period_samples);
            self.period_samples = self.period_samples.clamp(minimum_period, maximum_period);
            if let Some(next_center) = &mut self.next_symbol_center_sample {
                *next_center += self.config.loop_bandwidth * phase_error;
            }
        }
        self.last_transition_sample = Some(transition_sample);
    }
}

fn crossed_threshold(previous: f32, current: f32) -> bool {
    (previous < 0.0 && current >= 0.0) || (previous >= 0.0 && current < 0.0)
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
            .push(&samples[..24], SAMPLE_RATE_HZ, true)
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
}
