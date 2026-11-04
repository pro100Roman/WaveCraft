use crate::{
    ChannelizedBatch, DemodulatedBatch, Demodulator, DspError, TimedComplexSample, TimedSoftSymbol,
    TimedSoftSymbolBatch,
};
use std::collections::VecDeque;
use std::f64::consts::PI;
use std::f64::consts::TAU;
use std::ops::Range;

const GFSK_FILTER_SPAN_SYMBOLS: u64 = 4;
const MAX_GFSK_FILTER_TAPS: usize = 4_095;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FskPolarity {
    Normal,
    Inverted,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FskModulation {
    Fsk,
    Gfsk { bt: f64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FskConfig {
    pub symbol_rate_hz: u64,
    /// Expected positive or negative tone displacement from channel center.
    pub deviation_hz: u64,
    pub modulation: FskModulation,
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
        if let FskModulation::Gfsk { bt } = self.modulation {
            if !bt.is_finite() || bt <= 0.0 {
                return Err(DspError::new(
                    "GFSK BT must be finite and greater than zero",
                ));
            }
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

impl From<TimedSoftBit> for TimedSoftSymbol {
    fn from(bit: TimedSoftBit) -> Self {
        let one_likelihood = (bit.likelihood.clamp(-1.0, 1.0) + 1.0) * 0.5;
        Self {
            likelihoods: vec![1.0 - one_likelihood, one_likelihood],
            source_range: bit.source_range,
        }
    }
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
    gaussian_filter: Vec<f64>,
    gaussian_history: VecDeque<DiscriminatorSample>,
}

#[derive(Clone, Debug)]
struct DiscriminatorSample {
    likelihood: f64,
    effective_source_index: u64,
    source_range: Range<u64>,
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
            gaussian_filter: Vec::new(),
            gaussian_history: VecDeque::new(),
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
        self.gaussian_filter.clear();
        self.gaussian_history.clear();
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
        if matches!(self.config.modulation, FskModulation::Gfsk { .. })
            && self.gaussian_filter.is_empty()
        {
            self.gaussian_filter = design_gaussian_matched_filter(
                batch.sample_rate_hz,
                self.config.symbol_rate_hz,
                self.config.modulation,
            )?;
            self.gaussian_history = VecDeque::with_capacity(self.gaussian_filter.len());
        }

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
            let discriminator = DiscriminatorSample {
                likelihood: polarity * frequency_hz / self.config.deviation_hz as f64,
                effective_source_index: effective_index,
                source_range: previous.source_range.start..sample.source_range.end,
            };
            let Some(discriminator) = self.filter_discriminator(discriminator) else {
                continue;
            };
            let (symbol_number, symbol_remainder) = symbol_position(
                discriminator.effective_source_index,
                batch.segment_start_sample_index,
                batch.source_sample_rate_hz,
                self.config.symbol_rate_hz,
            )?;
            let mut current_symbol_number = self.current_symbol_number.unwrap_or(symbol_number);
            if self.likelihood_count == 0 && symbol_number > current_symbol_number + 1 {
                current_symbol_number = symbol_number;
                self.current_symbol_number = Some(symbol_number);
                self.current_symbol_complete = symbol_remainder == 0;
            }
            self.likelihood_sum += discriminator.likelihood;
            self.likelihood_count += 1;
            extend_range(&mut self.symbol_range, discriminator.source_range);
            if symbol_number > current_symbol_number {
                if symbol_number != current_symbol_number + 1 {
                    self.reset();
                    return Err(DspError::new(
                        "channelized sample spacing skipped an FSK symbol boundary",
                    ));
                }
                if self.current_symbol_complete && self.likelihood_count != 0 {
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
                self.current_symbol_complete = symbol_number == current_symbol_number + 1;
            }
        }
        Ok(bits)
    }

    fn filter_discriminator(&mut self, sample: DiscriminatorSample) -> Option<DiscriminatorSample> {
        if matches!(self.config.modulation, FskModulation::Fsk) {
            return Some(sample);
        }

        self.gaussian_history.push_back(sample);
        if self.gaussian_history.len() < self.gaussian_filter.len() {
            return None;
        }
        let likelihood = self
            .gaussian_history
            .iter()
            .zip(&self.gaussian_filter)
            .map(|(sample, coefficient)| sample.likelihood * coefficient)
            .sum();
        let midpoint = self.gaussian_filter.len() / 2;
        let effective_source_index = self.gaussian_history.get(midpoint)?.effective_source_index;
        let source_range = self.gaussian_history.front()?.source_range.start
            ..self.gaussian_history.back()?.source_range.end;
        self.gaussian_history.pop_front();
        Some(DiscriminatorSample {
            likelihood,
            effective_source_index,
            source_range,
        })
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
        if matches!(self.config.modulation, FskModulation::Gfsk { .. }) {
            gaussian_filter_tap_count(batch.sample_rate_hz, self.config.symbol_rate_hz)?;
        }
        Ok(())
    }
}

impl Demodulator for FskDemodulator {
    fn reset(&mut self) {
        FskDemodulator::reset(self);
    }

    fn push(&mut self, input: &ChannelizedBatch) -> Result<DemodulatedBatch, DspError> {
        let symbols = FskDemodulator::push(self, input)?
            .into_iter()
            .map(TimedSoftSymbol::from)
            .collect();
        Ok(DemodulatedBatch::SoftSymbols(TimedSoftSymbolBatch {
            alphabet_size: 2,
            discontinuity: input.discontinuity,
            symbols,
        }))
    }
}

fn gaussian_filter_tap_count(sample_rate_hz: u64, symbol_rate_hz: u64) -> Result<usize, DspError> {
    let span_samples = u128::from(sample_rate_hz)
        .saturating_mul(u128::from(GFSK_FILTER_SPAN_SYMBOLS))
        .div_ceil(u128::from(symbol_rate_hz));
    let mut tap_count = usize::try_from(span_samples)
        .map_err(|_| DspError::new("GFSK matched filter is too large"))?
        .saturating_add(1);
    if tap_count % 2 == 0 {
        tap_count = tap_count.saturating_add(1);
    }
    if tap_count > MAX_GFSK_FILTER_TAPS {
        return Err(DspError::new(
            "GFSK matched filter exceeds 4095 taps; reduce the channel sample rate",
        ));
    }
    Ok(tap_count)
}

fn design_gaussian_matched_filter(
    sample_rate_hz: u64,
    symbol_rate_hz: u64,
    modulation: FskModulation,
) -> Result<Vec<f64>, DspError> {
    let FskModulation::Gfsk { bt } = modulation else {
        return Ok(Vec::new());
    };
    let tap_count = gaussian_filter_tap_count(sample_rate_hz, symbol_rate_hz)?;
    let samples_per_symbol = sample_rate_hz as f64 / symbol_rate_hz as f64;
    let midpoint = (tap_count / 2) as isize;
    let mut taps = (0..tap_count)
        .map(|index| {
            let time_symbols = (index as isize - midpoint) as f64 / samples_per_symbol;
            (-2.0 * (PI * bt * time_symbols).powi(2) / 2.0_f64.ln()).exp()
        })
        .collect::<Vec<_>>();
    let sum = taps.iter().sum::<f64>();
    for tap in &mut taps {
        *tap /= sum;
    }
    Ok(taps)
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

    fn gfsk_samples(bits: &[bool], first_sample_index: u64, bt: f64) -> Vec<TimedComplexSample> {
        const SAMPLE_RATE_HZ: u64 = 8_000;
        const SYMBOL_RATE_HZ: u64 = 1_000;
        const DEVIATION_HZ: f64 = 1_000.0;
        const SAMPLES_PER_SYMBOL: usize = (SAMPLE_RATE_HZ / SYMBOL_RATE_HZ) as usize;
        let levels = bits
            .iter()
            .flat_map(|bit| {
                std::iter::repeat(if *bit { 1.0 } else { -1.0 }).take(SAMPLES_PER_SYMBOL)
            })
            .collect::<Vec<_>>();
        let taps = design_gaussian_matched_filter(
            SAMPLE_RATE_HZ,
            SYMBOL_RATE_HZ,
            FskModulation::Gfsk { bt },
        )
        .unwrap();
        let midpoint = (taps.len() / 2) as isize;
        let shaped = (0..levels.len())
            .map(|sample_index| {
                taps.iter()
                    .enumerate()
                    .map(|(tap_index, tap)| {
                        let level_index = (sample_index as isize + tap_index as isize - midpoint)
                            .clamp(0, levels.len() as isize - 1)
                            as usize;
                        levels[level_index] * tap
                    })
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();

        let mut phase: f64 = 0.0;
        let mut samples = Vec::with_capacity(shaped.len() + 1);
        for sample_index in 0..=shaped.len() {
            let index = first_sample_index + sample_index as u64;
            samples.push(TimedComplexSample {
                i: phase.cos() as f32,
                q: phase.sin() as f32,
                source_range: index..index + 1,
            });
            if let Some(level) = shaped.get(sample_index) {
                phase += TAU * DEVIATION_HZ * level / SAMPLE_RATE_HZ as f64;
            }
        }
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
            modulation: FskModulation::Fsk,
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

    #[test]
    fn gfsk_matched_filter_decodes_shaped_symbols_across_batches() {
        let expected = [
            true, true, true, false, true, false, false, true, true, false, true, false, false,
            false,
        ];
        let samples = gfsk_samples(&expected, 0, 0.5);
        let make_demodulator = || {
            FskDemodulator::new(FskConfig {
                symbol_rate_hz: 1_000,
                deviation_hz: 1_000,
                modulation: FskModulation::Gfsk { bt: 0.5 },
                polarity: FskPolarity::Normal,
            })
            .unwrap()
        };

        let mut whole = make_demodulator();
        let whole_bits = whole.push(&batch(samples.clone(), true, 0)).unwrap();

        let mut chunked = make_demodulator();
        let chunked_bits = [0..13, 13..41, 41..76, 76..samples.len()]
            .into_iter()
            .enumerate()
            .flat_map(|(index, range)| {
                chunked
                    .push(&batch(samples[range].to_vec(), index == 0, 0))
                    .unwrap()
            })
            .collect::<Vec<_>>();

        assert_eq!(chunked_bits, whole_bits);
        assert_eq!(
            whole_bits
                .iter()
                .map(|bit| bit.likelihood.is_sign_positive())
                .collect::<Vec<_>>(),
            expected[3..expected.len() - 2]
        );
        assert!(whole_bits.iter().all(|bit| bit.likelihood.abs() > 0.35));
    }

    #[test]
    fn rejects_invalid_gfsk_bt() {
        for bt in [0.0, -0.5, f64::NAN] {
            assert!(FskDemodulator::new(FskConfig {
                symbol_rate_hz: 1_000,
                deviation_hz: 1_000,
                modulation: FskModulation::Gfsk { bt },
                polarity: FskPolarity::Normal,
            })
            .is_err());
        }
    }

    #[test]
    fn generic_demodulator_contract_exposes_binary_soft_symbols() {
        let expected = [true, false, true];
        let mut demodulator: Box<dyn Demodulator> = Box::new(demodulator());
        let output = demodulator
            .push(&batch(fsk_samples(&expected, 0), true, 0))
            .unwrap();
        let DemodulatedBatch::SoftSymbols(symbols) = output else {
            panic!("binary FSK must emit soft symbols");
        };

        assert!(symbols.validate().is_ok());
        assert_eq!(symbols.alphabet_size, 2);
        assert!(symbols.discontinuity);
        assert_eq!(
            symbols
                .symbols
                .iter()
                .map(TimedSoftSymbol::decision)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(0), Some(1)]
        );
    }
}
