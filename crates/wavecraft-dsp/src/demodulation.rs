use crate::{ChannelizedBatch, DspError};
use std::collections::HashSet;
use std::ops::Range;

pub const MAX_SOFT_SYMBOL_ALPHABET_SIZE: u16 = 256;
pub const MAX_SPARSE_SYMBOL_ALPHABET_SIZE: u32 = 1_048_576;
pub const MAX_SPARSE_SYMBOL_ALTERNATIVES: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct TimedSoftSymbol {
    /// Relative likelihood for each symbol in alphabet order.
    pub likelihoods: Vec<f32>,
    /// Half-open range in the original IQ source-sample domain.
    pub source_range: Range<u64>,
}

impl TimedSoftSymbol {
    pub fn validate(&self, alphabet_size: u16) -> Result<(), DspError> {
        if !(2..=MAX_SOFT_SYMBOL_ALPHABET_SIZE).contains(&alphabet_size) {
            return Err(DspError::new(
                "soft-symbol alphabet size must be between 2 and 256",
            ));
        }
        if self.likelihoods.len() != usize::from(alphabet_size) {
            return Err(DspError::new(
                "soft-symbol likelihood count does not match its alphabet",
            ));
        }
        if self.source_range.is_empty() {
            return Err(DspError::new("soft symbol has an empty source range"));
        }
        if self
            .likelihoods
            .iter()
            .any(|likelihood| !likelihood.is_finite() || *likelihood < 0.0)
        {
            return Err(DspError::new(
                "soft-symbol likelihoods must be finite and nonnegative",
            ));
        }
        if self.likelihoods.iter().sum::<f32>() <= 0.0 {
            return Err(DspError::new(
                "soft symbol must assign positive likelihood to at least one symbol",
            ));
        }
        Ok(())
    }

    pub fn decision(&self) -> Option<usize> {
        self.likelihoods
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(index, _)| index)
    }

    pub fn confidence(&self) -> Option<f32> {
        let sum = self.likelihoods.iter().sum::<f32>();
        (sum > 0.0).then(|| {
            let (best, second_best) = self.likelihoods.iter().copied().fold(
                (0.0_f32, 0.0_f32),
                |(best, second_best), likelihood| {
                    if likelihood >= best {
                        (likelihood, best)
                    } else if likelihood > second_best {
                        (best, likelihood)
                    } else {
                        (best, second_best)
                    }
                },
            );
            (best - second_best) / sum
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedSoftSymbolBatch {
    pub alphabet_size: u16,
    pub discontinuity: bool,
    pub symbols: Vec<TimedSoftSymbol>,
}

impl TimedSoftSymbolBatch {
    pub fn validate(&self) -> Result<(), DspError> {
        if !(2..=MAX_SOFT_SYMBOL_ALPHABET_SIZE).contains(&self.alphabet_size) {
            return Err(DspError::new(
                "soft-symbol alphabet size must be between 2 and 256",
            ));
        }
        for symbol in &self.symbols {
            symbol.validate(self.alphabet_size)?;
        }
        if self
            .symbols
            .windows(2)
            .any(|symbols| symbols[0].source_range.start > symbols[1].source_range.start)
        {
            return Err(DspError::new(
                "soft symbols are not ordered by source range",
            ));
        }
        Ok(())
    }
}

/// One bounded alternative to a sparse hard symbol decision.
///
/// Sparse decisions avoid allocating one likelihood per alphabet entry for
/// modulation families such as LoRa, whose SF12 alphabet contains 4,096
/// possible symbols.
#[derive(Clone, Debug, PartialEq)]
pub struct SparseSymbolAlternative {
    pub value: u32,
    pub relative_likelihood: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedSparseSymbol {
    pub value: u32,
    /// Normalized confidence in the selected value, from zero to one.
    pub confidence: f32,
    /// Optional runner-up decisions, bounded independently of alphabet size.
    pub alternatives: Vec<SparseSymbolAlternative>,
    /// Marks the first recovered payload symbol after physical-layer sync.
    pub frame_start: bool,
    /// Half-open range in the original IQ source-sample domain.
    pub source_range: Range<u64>,
}

impl TimedSparseSymbol {
    pub fn validate(&self, alphabet_size: u32) -> Result<(), DspError> {
        validate_sparse_alphabet_size(alphabet_size)?;
        if self.value >= alphabet_size {
            return Err(DspError::new(
                "sparse symbol decision lies outside its alphabet",
            ));
        }
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err(DspError::new(
                "sparse symbol confidence must be finite and between zero and one",
            ));
        }
        if self.source_range.is_empty() {
            return Err(DspError::new("sparse symbol has an empty source range"));
        }
        if self.alternatives.len() > MAX_SPARSE_SYMBOL_ALTERNATIVES {
            return Err(DspError::new(format!(
                "sparse symbol may retain at most {MAX_SPARSE_SYMBOL_ALTERNATIVES} alternatives"
            )));
        }
        let mut values = HashSet::with_capacity(self.alternatives.len() + 1);
        values.insert(self.value);
        for alternative in &self.alternatives {
            if alternative.value >= alphabet_size {
                return Err(DspError::new(
                    "sparse symbol alternative lies outside its alphabet",
                ));
            }
            if !alternative.relative_likelihood.is_finite() || alternative.relative_likelihood < 0.0
            {
                return Err(DspError::new(
                    "sparse symbol alternative likelihood must be finite and nonnegative",
                ));
            }
            if !values.insert(alternative.value) {
                return Err(DspError::new(
                    "sparse symbol decisions and alternatives must be unique",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedSparseSymbolBatch {
    pub alphabet_size: u32,
    pub discontinuity: bool,
    pub symbols: Vec<TimedSparseSymbol>,
}

impl TimedSparseSymbolBatch {
    pub fn validate(&self) -> Result<(), DspError> {
        validate_sparse_alphabet_size(self.alphabet_size)?;
        for symbol in &self.symbols {
            symbol.validate(self.alphabet_size)?;
        }
        if self
            .symbols
            .windows(2)
            .any(|symbols| symbols[0].source_range.start > symbols[1].source_range.start)
        {
            return Err(DspError::new(
                "sparse symbols are not ordered by source range",
            ));
        }
        Ok(())
    }
}

fn validate_sparse_alphabet_size(alphabet_size: u32) -> Result<(), DspError> {
    if !(2..=MAX_SPARSE_SYMBOL_ALPHABET_SIZE).contains(&alphabet_size) {
        return Err(DspError::new(format!(
            "sparse-symbol alphabet size must be between 2 and {MAX_SPARSE_SYMBOL_ALPHABET_SIZE}"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedAnalogSample {
    pub value: f32,
    /// Half-open range in the original IQ source-sample domain.
    pub source_range: Range<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedAnalogBatch {
    pub sample_rate_hz: u64,
    pub discontinuity: bool,
    pub samples: Vec<TimedAnalogSample>,
}

impl TimedAnalogBatch {
    pub fn validate(&self) -> Result<(), DspError> {
        if self.sample_rate_hz == 0 {
            return Err(DspError::new(
                "demodulated analog sample rate must be nonzero",
            ));
        }
        for sample in &self.samples {
            if !sample.value.is_finite() {
                return Err(DspError::new(
                    "demodulated analog sample contains a non-finite value",
                ));
            }
            if sample.source_range.is_empty() {
                return Err(DspError::new(
                    "demodulated analog sample has an empty source range",
                ));
            }
        }
        if self
            .samples
            .windows(2)
            .any(|samples| samples[0].source_range.start > samples[1].source_range.start)
        {
            return Err(DspError::new(
                "demodulated analog samples are not ordered by source range",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DemodulatedBatch {
    SoftSymbols(TimedSoftSymbolBatch),
    SparseSymbols(TimedSparseSymbolBatch),
    Analog(TimedAnalogBatch),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DemodulatorTelemetry {
    pub timing_locked: bool,
    pub timing_lock_quality: f32,
    pub estimated_symbol_rate_hz: Option<f64>,
    pub estimated_frequency_offset_hz: Option<f64>,
    pub estimated_tone_separation_hz: Option<f64>,
}

pub trait Demodulator: Send {
    fn reset(&mut self);

    fn push(&mut self, input: &ChannelizedBatch) -> Result<DemodulatedBatch, DspError>;

    fn telemetry(&self) -> DemodulatorTelemetry {
        DemodulatorTelemetry::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_symbol_reports_decision_and_normalized_confidence() {
        let symbol = TimedSoftSymbol {
            likelihoods: vec![1.0, 3.0, 2.0],
            source_range: 10..20,
        };

        assert!(symbol.validate(3).is_ok());
        assert_eq!(symbol.decision(), Some(1));
        assert_eq!(symbol.confidence(), Some(1.0 / 6.0));

        let ambiguous = TimedSoftSymbol {
            likelihoods: vec![0.5, 0.5],
            source_range: 20..30,
        };
        assert_eq!(ambiguous.confidence(), Some(0.0));
    }

    #[test]
    fn soft_symbol_batch_rejects_invalid_or_unordered_symbols() {
        let invalid = TimedSoftSymbolBatch {
            alphabet_size: 2,
            discontinuity: false,
            symbols: vec![
                TimedSoftSymbol {
                    likelihoods: vec![0.25, 0.75],
                    source_range: 20..30,
                },
                TimedSoftSymbol {
                    likelihoods: vec![0.75, 0.25],
                    source_range: 10..20,
                },
            ],
        };

        assert!(invalid.validate().is_err());
    }

    #[test]
    fn sparse_symbols_support_large_alphabets_with_bounded_alternatives() {
        let batch = TimedSparseSymbolBatch {
            alphabet_size: 4_096,
            discontinuity: false,
            symbols: vec![TimedSparseSymbol {
                value: 3_000,
                confidence: 0.8,
                alternatives: vec![SparseSymbolAlternative {
                    value: 3_001,
                    relative_likelihood: 0.2,
                }],
                frame_start: true,
                source_range: 100..200,
            }],
        };

        assert!(batch.validate().is_ok());
    }

    #[test]
    fn sparse_symbols_reject_duplicate_or_out_of_alphabet_values() {
        let duplicate = TimedSparseSymbol {
            value: 7,
            confidence: 0.5,
            alternatives: vec![SparseSymbolAlternative {
                value: 7,
                relative_likelihood: 0.5,
            }],
            frame_start: false,
            source_range: 0..1,
        };
        assert!(duplicate.validate(128).is_err());

        let outside = TimedSparseSymbol {
            value: 128,
            confidence: 0.5,
            alternatives: Vec::new(),
            frame_start: false,
            source_range: 0..1,
        };
        assert!(outside.validate(128).is_err());
    }

    #[test]
    fn analog_batch_rejects_nonfinite_samples() {
        let invalid = TimedAnalogBatch {
            sample_rate_hz: 48_000,
            discontinuity: false,
            samples: vec![TimedAnalogSample {
                value: f32::NAN,
                source_range: 0..1,
            }],
        };

        assert!(invalid.validate().is_err());
    }
}
