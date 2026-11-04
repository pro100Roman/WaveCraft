use crate::{ChannelizedBatch, DspError, TimedComplexSample};
use std::f64::consts::TAU;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FskPolarity {
    Normal,
    Inverted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FskConfig {
    pub symbol_rate_hz: u64,
    /// Expected positive or negative tone displacement from channel center.
    pub deviation_hz: u64,
    pub polarity: FskPolarity,
}

impl FskConfig {
    pub fn validate(&self) -> Result<(), DspError> {
        if self.symbol_rate_hz == 0 {
            return Err(DspError::new("FSK symbol rate must be nonzero"));
        }
        if self.deviation_hz == 0 {
            return Err(DspError::new("FSK deviation must be nonzero"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedSoftBit {
    /// Negative values represent zero, positive values represent one, and the
    /// magnitude up to one expresses decision confidence.
    pub likelihood: f32,
    pub source_range: Range<u64>,
}

/// Incremental binary-FSK discriminator with fixed source-time symbol timing.
///
/// The first partial symbol after a discontinuity is discarded. Adaptive clock
/// recovery and GFSK matched filtering are deliberately separate later stages.
pub struct FskDemodulator {
    config: FskConfig,
    previous_sample: Option<TimedComplexSample>,
    source_sample_rate_hz: Option<u64>,
    sample_rate_hz: Option<u64>,
    center_frequency_hz: Option<u64>,
    segment_start_sample_index: Option<u64>,
    current_symbol_number: Option<u128>,
    current_symbol_complete: bool,
    likelihood_sum: f64,
    likelihood_count: u64,
    symbol_range: Option<Range<u64>>,
}

impl FskDemodulator {
    pub fn new(config: FskConfig) -> Result<Self, DspError> {
        config.validate()?;
        Ok(Self {
            config,
            previous_sample: None,
            source_sample_rate_hz: None,
            sample_rate_hz: None,
            center_frequency_hz: None,
            segment_start_sample_index: None,
            current_symbol_number: None,
            current_symbol_complete: false,
            likelihood_sum: 0.0,
            likelihood_count: 0,
            symbol_range: None,
        })
    }

    pub fn config(&self) -> &FskConfig {
        &self.config
    }

    pub fn reset(&mut self) {
        self.previous_sample = None;
        self.source_sample_rate_hz = None;
        self.sample_rate_hz = None;
        self.center_frequency_hz = None;
        self.segment_start_sample_index = None;
        self.current_symbol_number = None;
        self.current_symbol_complete = false;
        self.likelihood_sum = 0.0;
        self.likelihood_count = 0;
        self.symbol_range = None;
    }

    pub fn push(&mut self, batch: &ChannelizedBatch) -> Result<Vec<TimedSoftBit>, DspError> {
        self.validate_batch(batch)?;
        if batch.discontinuity
            || self
                .source_sample_rate_hz
                .is_some_and(|rate| rate != batch.source_sample_rate_hz)
            || self
                .sample_rate_hz
                .is_some_and(|rate| rate != batch.sample_rate_hz)
            || self
                .center_frequency_hz
                .is_some_and(|frequency| frequency != batch.center_frequency_hz)
            || self
                .segment_start_sample_index
                .is_some_and(|start| start != batch.segment_start_sample_index)
        {
            self.reset();
        }
        self.source_sample_rate_hz = Some(batch.source_sample_rate_hz);
        self.sample_rate_hz = Some(batch.sample_rate_hz);
        self.center_frequency_hz = Some(batch.center_frequency_hz);
        self.segment_start_sample_index = Some(batch.segment_start_sample_index);

        let mut bits = Vec::new();
        for sample in &batch.samples {
            validate_sample(sample)?;
            let effective_index = effective_source_index(sample, batch)?;
            let Some(previous) = self.previous_sample.replace(sample.clone()) else {
                let (number, remainder) = symbol_position(
                    effective_index,
                    batch.segment_start_sample_index,
                    batch.source_sample_rate_hz,
                    self.config.symbol_rate_hz,
                )?;
                self.current_symbol_number = Some(number);
                self.current_symbol_complete = remainder == 0;
                continue;
            };
            if sample.source_range.start < previous.source_range.start {
                self.reset();
                return Err(DspError::new(
                    "channelized samples are not ordered by source range",
                ));
            }

            let real = f64::from(sample.i) * f64::from(previous.i)
                + f64::from(sample.q) * f64::from(previous.q);
            let imaginary = f64::from(sample.q) * f64::from(previous.i)
                - f64::from(sample.i) * f64::from(previous.q);
            let frequency_hz = imaginary.atan2(real) * batch.sample_rate_hz as f64 / TAU;
            let polarity = match self.config.polarity {
                FskPolarity::Normal => 1.0,
                FskPolarity::Inverted => -1.0,
            };
            self.likelihood_sum += polarity * frequency_hz / self.config.deviation_hz as f64;
            self.likelihood_count += 1;
            extend_range(
                &mut self.symbol_range,
                previous.source_range.start..sample.source_range.end,
            );
            let (symbol_number, _) = symbol_position(
                effective_index,
                batch.segment_start_sample_index,
                batch.source_sample_rate_hz,
                self.config.symbol_rate_hz,
            )?;
            let current_symbol_number = self.current_symbol_number.unwrap_or(symbol_number);
            if symbol_number > current_symbol_number {
                if symbol_number != current_symbol_number + 1 {
                    self.reset();
                    return Err(DspError::new(
                        "channelized sample spacing skipped an FSK symbol boundary",
                    ));
                }
                if self.current_symbol_complete {
                    let likelihood =
                        (self.likelihood_sum / self.likelihood_count as f64).clamp(-1.0, 1.0);
                    if let Some(source_range) = self.symbol_range.take() {
                        bits.push(TimedSoftBit {
                            likelihood: likelihood as f32,
                            source_range,
                        });
                    }
                } else {
                    self.symbol_range = None;
                }
                self.likelihood_sum = 0.0;
                self.likelihood_count = 0;
                self.current_symbol_number = Some(symbol_number);
                self.current_symbol_complete = true;
            }
        }
        Ok(bits)
    }

    fn validate_batch(&self, batch: &ChannelizedBatch) -> Result<(), DspError> {
        if batch.sample_rate_hz == 0 {
            return Err(DspError::new("channelized sample rate must be nonzero"));
        }
        if batch.source_sample_rate_hz == 0 {
            return Err(DspError::new("source IQ sample rate must be nonzero"));
        }
        if self.config.symbol_rate_hz.saturating_mul(2) > batch.sample_rate_hz {
            return Err(DspError::new(
                "FSK decoding requires at least two samples per symbol",
            ));
        }
        if self.config.deviation_hz.saturating_mul(2) >= batch.sample_rate_hz {
            return Err(DspError::new(
                "FSK deviation must remain below the channel Nyquist frequency",
            ));
        }
        Ok(())
    }
}

fn effective_source_index(
    sample: &TimedComplexSample,
    batch: &ChannelizedBatch,
) -> Result<u64, DspError> {
    let index = sample
        .source_range
        .start
        .saturating_add(batch.filter_delay_input_samples);
    if index >= sample.source_range.end {
        return Err(DspError::new(
            "channelized filter delay falls outside the contributing source range",
        ));
    }
    Ok(index)
}

fn symbol_position(
    source_index: u64,
    segment_start: u64,
    source_sample_rate_hz: u64,
    symbol_rate_hz: u64,
) -> Result<(u128, u128), DspError> {
    let offset = source_index
        .checked_sub(segment_start)
        .ok_or_else(|| DspError::new("channelized sample precedes its tuning segment"))?;
    let position = u128::from(offset) * u128::from(symbol_rate_hz);
    let denominator = u128::from(source_sample_rate_hz);
    Ok((position / denominator, position % denominator))
}

fn validate_sample(sample: &TimedComplexSample) -> Result<(), DspError> {
    if !sample.i.is_finite() || !sample.q.is_finite() {
        return Err(DspError::new(
            "channelized sample contains a non-finite value",
        ));
    }
    if sample.source_range.is_empty() {
        return Err(DspError::new(
            "channelized sample has an empty source range",
        ));
    }
    Ok(())
}

fn extend_range(current: &mut Option<Range<u64>>, next: Range<u64>) {
    match current {
        Some(range) => {
            range.start = range.start.min(next.start);
            range.end = range.end.max(next.end);
        }
        None => *current = Some(next),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fsk_samples(bits: &[bool], first_sample_index: u64) -> Vec<TimedComplexSample> {
        const SAMPLE_RATE_HZ: f64 = 8_000.0;
        const DEVIATION_HZ: f64 = 1_000.0;
        const SAMPLES_PER_SYMBOL: usize = 8;
        let mut phase: f64 = 0.0;
        let mut samples = Vec::new();
        for bit in bits {
            let frequency = if *bit { DEVIATION_HZ } else { -DEVIATION_HZ };
            for _ in 0..SAMPLES_PER_SYMBOL {
                let index = first_sample_index + samples.len() as u64;
                samples.push(TimedComplexSample {
                    i: phase.cos() as f32,
                    q: phase.sin() as f32,
                    source_range: index..index + 1,
                });
                phase += TAU * frequency / SAMPLE_RATE_HZ;
            }
        }
        let index = first_sample_index + samples.len() as u64;
        samples.push(TimedComplexSample {
            i: phase.cos() as f32,
            q: phase.sin() as f32,
            source_range: index..index + 1,
        });
        samples
    }

    fn batch(
        samples: Vec<TimedComplexSample>,
        discontinuity: bool,
        segment_start_sample_index: u64,
    ) -> ChannelizedBatch {
        ChannelizedBatch {
            source_sample_rate_hz: 8_000,
            sample_rate_hz: 8_000,
            center_frequency_hz: 100_000_000,
            segment_start_sample_index,
            filter_delay_input_samples: 0,
            discontinuity,
            samples,
        }
    }

    fn demodulator() -> FskDemodulator {
        FskDemodulator::new(FskConfig {
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            polarity: FskPolarity::Normal,
        })
        .unwrap()
    }

    #[test]
    fn demodulates_binary_fsk_into_timed_soft_bits() {
        let expected = [true, false, true, true, false];
        let mut demodulator = demodulator();
        let bits = demodulator
            .push(&batch(fsk_samples(&expected, 0), true, 0))
            .unwrap();

        assert_eq!(bits.len(), expected.len());
        assert_eq!(
            bits.iter()
                .map(|bit| bit.likelihood.is_sign_positive())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(bits.iter().all(|bit| bit.likelihood.abs() > 0.99));
        assert_eq!(bits[0].source_range, 0..9);
    }

    #[test]
    fn demodulation_is_independent_of_channelized_batch_boundaries() {
        let expected = [true, false, false, true];
        let samples = fsk_samples(&expected, 0);
        let mut whole = demodulator();
        let whole_bits = whole.push(&batch(samples.clone(), true, 0)).unwrap();

        let mut chunked = demodulator();
        let first = chunked
            .push(&batch(samples[..7].to_vec(), true, 0))
            .unwrap();
        let second = chunked
            .push(&batch(samples[7..19].to_vec(), false, 0))
            .unwrap();
        let third = chunked
            .push(&batch(samples[19..].to_vec(), false, 0))
            .unwrap();
        let chunked_bits = first
            .into_iter()
            .chain(second)
            .chain(third)
            .collect::<Vec<_>>();

        assert_eq!(chunked_bits, whole_bits);
    }

    #[test]
    fn discontinuity_drops_a_partial_symbol() {
        let samples = fsk_samples(&[true], 0);
        let mut demodulator = demodulator();
        assert!(demodulator
            .push(&batch(samples[..4].to_vec(), true, 0))
            .unwrap()
            .is_empty());

        let reset_samples = fsk_samples(&[false], 100);
        let bits = demodulator.push(&batch(reset_samples, true, 100)).unwrap();
        assert_eq!(bits.len(), 1);
        assert!(bits[0].likelihood.is_sign_negative());
        assert!(bits[0].source_range.start >= 100);
    }

    #[test]
    fn inverted_polarity_reverses_the_hard_decision() {
        let mut demodulator = FskDemodulator::new(FskConfig {
            polarity: FskPolarity::Inverted,
            ..demodulator().config().clone()
        })
        .unwrap();
        let bits = demodulator
            .push(&batch(fsk_samples(&[true], 0), true, 0))
            .unwrap();
        assert!(bits[0].likelihood.is_sign_negative());
    }
}
