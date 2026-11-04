use crate::{DecodeDiagnostic, DecodeError, SampleRange, Severity};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedBit {
    pub value: bool,
    /// Normalized hard-decision confidence in the range `[0, 1]`.
    pub confidence: f32,
    pub range: SampleRange,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedBitBatch {
    pub discontinuity: bool,
    pub bits: Vec<TimedBit>,
}

impl TimedBitBatch {
    pub fn validate(&self) -> Result<(), DecodeError> {
        for bit in &self.bits {
            if !bit.confidence.is_finite() || !(0.0..=1.0).contains(&bit.confidence) {
                return Err(DecodeError::new(
                    "timed-bit confidence must be finite and between zero and one",
                ));
            }
            if bit.range.start >= bit.range.end {
                return Err(DecodeError::new("timed bit has an empty sample range"));
            }
        }
        if self
            .bits
            .windows(2)
            .any(|bits| bits[0].range.start > bits[1].range.start)
        {
            return Err(DecodeError::new(
                "timed bits are not ordered by sample range",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BitPolarity {
    Normal,
    Inverted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineCode {
    Nrz,
    Nrzi {
        /// Whether a transition represents one (`true`) or zero (`false`).
        transition_is_one: bool,
        /// Physical level immediately before the first input chip.
        initial_level: bool,
    },
    Manchester {
        /// Whether a low-to-high chip pair represents one.
        low_to_high_is_one: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BitTransformConfig {
    pub polarity: BitPolarity,
    pub line_code: LineCode,
}

impl Default for BitTransformConfig {
    fn default() -> Self {
        Self {
            polarity: BitPolarity::Normal,
            line_code: LineCode::Nrz,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BitTransformOutput {
    pub bits: Vec<TimedBit>,
    pub diagnostics: Vec<DecodeDiagnostic>,
}

/// Streaming polarity and line-code transform with at most one retained chip.
pub struct BitTransformer {
    config: BitTransformConfig,
    previous_nrzi_chip: Option<TimedBit>,
    pending_manchester_chip: Option<TimedBit>,
    last_input_end: Option<u64>,
}

impl BitTransformer {
    pub fn new(config: BitTransformConfig) -> Self {
        Self {
            config,
            previous_nrzi_chip: None,
            pending_manchester_chip: None,
            last_input_end: None,
        }
    }

    pub fn config(&self) -> BitTransformConfig {
        self.config
    }

    pub fn reset(&mut self) {
        self.previous_nrzi_chip = None;
        self.pending_manchester_chip = None;
        self.last_input_end = None;
    }

    pub fn push(&mut self, batch: &TimedBitBatch) -> Result<BitTransformOutput, DecodeError> {
        batch.validate()?;
        let mut output = BitTransformOutput::default();
        if batch.discontinuity {
            if let Some(pending) = self.pending_manchester_chip.take() {
                output.diagnostics.push(line_code_diagnostic(
                    "radio.line_code.incomplete_manchester",
                    "discarded an incomplete Manchester chip pair at an input discontinuity",
                    Some(pending.range),
                ));
            }
            self.reset();
        } else if self
            .last_input_end
            .is_some_and(|end| batch.bits.first().is_some_and(|bit| bit.range.start < end))
        {
            return Err(DecodeError::new(
                "new timed bits overlap the previous input batch",
            ));
        }

        for bit in &batch.bits {
            let mut chip = *bit;
            if self.config.polarity == BitPolarity::Inverted {
                chip.value = !chip.value;
            }
            match self.config.line_code {
                LineCode::Nrz => output.bits.push(chip),
                LineCode::Nrzi {
                    transition_is_one,
                    initial_level,
                } => {
                    let previous = self.previous_nrzi_chip.unwrap_or(TimedBit {
                        value: initial_level,
                        confidence: 1.0,
                        range: chip.range,
                    });
                    let transition = chip.value != previous.value;
                    output.bits.push(TimedBit {
                        value: transition == transition_is_one,
                        confidence: chip.confidence.min(previous.confidence),
                        range: if self.previous_nrzi_chip.is_some() {
                            covering_range(previous.range, chip.range)
                        } else {
                            chip.range
                        },
                    });
                    self.previous_nrzi_chip = Some(chip);
                }
                LineCode::Manchester { low_to_high_is_one } => {
                    let Some(first) = self.pending_manchester_chip.take() else {
                        self.pending_manchester_chip = Some(chip);
                        continue;
                    };
                    let range = covering_range(first.range, chip.range);
                    if first.value == chip.value {
                        output.diagnostics.push(line_code_diagnostic(
                            "radio.line_code.invalid_manchester",
                            "Manchester chip pair does not contain a transition",
                            Some(range),
                        ));
                        continue;
                    }
                    let low_to_high = !first.value && chip.value;
                    output.bits.push(TimedBit {
                        value: low_to_high == low_to_high_is_one,
                        confidence: first.confidence.min(chip.confidence),
                        range,
                    });
                }
            }
        }
        if let Some(last) = batch.bits.last() {
            self.last_input_end = Some(last.range.end);
        }
        Ok(output)
    }

    pub fn finish(&mut self) -> BitTransformOutput {
        let mut output = BitTransformOutput::default();
        if let Some(pending) = self.pending_manchester_chip.take() {
            output.diagnostics.push(line_code_diagnostic(
                "radio.line_code.incomplete_manchester",
                "discarded an incomplete Manchester chip pair at end of stream",
                Some(pending.range),
            ));
        }
        self.previous_nrzi_chip = None;
        self.last_input_end = None;
        output
    }
}

fn covering_range(first: SampleRange, last: SampleRange) -> SampleRange {
    SampleRange {
        start: first.start.min(last.start),
        end: first.end.max(last.end),
    }
}

fn line_code_diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    range: Option<SampleRange>,
) -> DecodeDiagnostic {
    DecodeDiagnostic {
        code: code.into(),
        message: message.into(),
        severity: Severity::Warning,
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(values: &[bool], first_sample: u64, discontinuity: bool) -> TimedBitBatch {
        TimedBitBatch {
            discontinuity,
            bits: values
                .iter()
                .enumerate()
                .map(|(index, value)| TimedBit {
                    value: *value,
                    confidence: 0.8,
                    range: SampleRange {
                        start: first_sample + index as u64 * 10,
                        end: first_sample + index as u64 * 10 + 8,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn nrz_applies_polarity_without_losing_timing_or_confidence() {
        let mut transformer = BitTransformer::new(BitTransformConfig {
            polarity: BitPolarity::Inverted,
            line_code: LineCode::Nrz,
        });
        let output = transformer
            .push(&bits(&[false, true, true], 100, false))
            .unwrap();

        assert_eq!(
            output.bits.iter().map(|bit| bit.value).collect::<Vec<_>>(),
            [true, false, false]
        );
        assert_eq!(
            output.bits[0].range,
            SampleRange {
                start: 100,
                end: 108
            }
        );
        assert_eq!(output.bits[0].confidence, 0.8);
    }

    #[test]
    fn nrzi_state_survives_chunks_and_resets_at_a_gap() {
        let config = BitTransformConfig {
            polarity: BitPolarity::Normal,
            line_code: LineCode::Nrzi {
                transition_is_one: true,
                initial_level: false,
            },
        };
        let mut chunked = BitTransformer::new(config);
        let first = chunked.push(&bits(&[false, true], 0, false)).unwrap();
        let second = chunked.push(&bits(&[true, false], 20, false)).unwrap();
        let reset = chunked.push(&bits(&[true], 100, true)).unwrap();

        assert_eq!(
            first
                .bits
                .into_iter()
                .chain(second.bits)
                .map(|bit| bit.value)
                .collect::<Vec<_>>(),
            [false, true, false, true]
        );
        assert_eq!(
            reset.bits.iter().map(|bit| bit.value).collect::<Vec<_>>(),
            [true]
        );
    }

    #[test]
    fn manchester_decodes_across_chunks_and_reports_invalid_pairs() {
        let mut transformer = BitTransformer::new(BitTransformConfig {
            polarity: BitPolarity::Normal,
            line_code: LineCode::Manchester {
                low_to_high_is_one: true,
            },
        });
        assert!(transformer
            .push(&bits(&[false], 0, false))
            .unwrap()
            .bits
            .is_empty());
        let valid = transformer
            .push(&bits(&[true, true, false], 10, false))
            .unwrap();
        let invalid = transformer.push(&bits(&[true, true], 40, false)).unwrap();

        assert_eq!(
            valid.bits.iter().map(|bit| bit.value).collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(valid.bits[0].range, SampleRange { start: 0, end: 18 });
        assert!(invalid.bits.is_empty());
        assert_eq!(
            invalid.diagnostics[0].code,
            "radio.line_code.invalid_manchester"
        );
    }

    #[test]
    fn finish_reports_a_partial_manchester_pair() {
        let mut transformer = BitTransformer::new(BitTransformConfig {
            polarity: BitPolarity::Normal,
            line_code: LineCode::Manchester {
                low_to_high_is_one: false,
            },
        });
        transformer.push(&bits(&[true], 0, false)).unwrap();

        let output = transformer.finish();

        assert_eq!(
            output.diagnostics[0].code,
            "radio.line_code.incomplete_manchester"
        );
    }
}
