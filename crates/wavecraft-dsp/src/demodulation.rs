use crate::{ChannelizedBatch, DspError};
use std::ops::Range;

pub const MAX_SOFT_SYMBOL_ALPHABET_SIZE: u16 = 256;

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
        (sum > 0.0).then(|| self.likelihoods.iter().copied().fold(0.0_f32, f32::max) / sum)
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
    Analog(TimedAnalogBatch),
}

pub trait Demodulator: Send {
    fn reset(&mut self);

    fn push(&mut self, input: &ChannelizedBatch) -> Result<DemodulatedBatch, DspError>;
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
        assert_eq!(symbol.confidence(), Some(0.5));
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
