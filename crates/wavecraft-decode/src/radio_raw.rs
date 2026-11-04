use crate::{
    BitOrder, ByteStatus, DecodeDiagnostic, DecodeError, SampleRange, Severity, TimedBit,
    TimedBitBatch, TimedByte,
};

const MAX_PREAMBLE_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawRadioConfig {
    /// Byte repeated to form the acquisition preamble.
    pub preamble_byte: u8,
    /// Number of consecutive preamble bytes required before byte output starts.
    pub preamble_bytes: usize,
    pub bit_order: BitOrder,
}

impl RawRadioConfig {
    pub fn validate(self) -> Result<(), DecodeError> {
        if !(1..=MAX_PREAMBLE_BYTES).contains(&self.preamble_bytes) {
            return Err(DecodeError::new(
                "raw radio preamble length must be between 1 and 256 bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawRadioOutput {
    /// Preamble bytes are included, followed by every byte decoded after lock.
    pub bytes: Vec<TimedByte>,
    /// Preamble-delimited message starts in source-sample coordinates.
    pub message_starts: Vec<RawRadioMessageStart>,
    pub diagnostics: Vec<DecodeDiagnostic>,
    pub synchronized: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawRadioMessageStart {
    pub sample_index: u64,
    pub preamble_bytes: usize,
}

/// Streaming raw-byte decoder that derives byte alignment from a repeated-byte preamble.
///
/// Once synchronized, output remains a continuous byte stream until a discontinuity or
/// explicit reset. Packet boundaries require a separate framing stage.
pub struct RawRadioDecoder {
    config: RawRadioConfig,
    preamble_bits: Vec<bool>,
    search_bits: Vec<TimedBit>,
    pending_bits: Vec<TimedBit>,
    synchronized: bool,
    preamble_run_bytes: usize,
    preamble_run_start: Option<u64>,
    last_input_end: Option<u64>,
}

impl RawRadioDecoder {
    pub fn new(config: RawRadioConfig) -> Result<Self, DecodeError> {
        config.validate()?;
        let preamble_bits = repeated_byte_bits(config);
        Ok(Self {
            search_bits: Vec::with_capacity(preamble_bits.len()),
            pending_bits: Vec::with_capacity(8),
            preamble_bits,
            config,
            synchronized: false,
            preamble_run_bytes: 0,
            preamble_run_start: None,
            last_input_end: None,
        })
    }

    pub fn config(&self) -> RawRadioConfig {
        self.config
    }

    pub fn is_synchronized(&self) -> bool {
        self.synchronized
    }

    pub fn reset(&mut self) {
        self.search_bits.clear();
        self.pending_bits.clear();
        self.synchronized = false;
        self.preamble_run_bytes = 0;
        self.preamble_run_start = None;
        self.last_input_end = None;
    }

    pub fn push(&mut self, batch: &TimedBitBatch) -> Result<RawRadioOutput, DecodeError> {
        batch.validate()?;
        let mut output = RawRadioOutput::default();
        if batch.discontinuity {
            self.flush_partial_byte(&mut output, "input discontinuity");
            self.reset();
        } else if self
            .last_input_end
            .is_some_and(|end| batch.bits.first().is_some_and(|bit| bit.range.start < end))
        {
            return Err(DecodeError::new(
                "new raw radio bits overlap the previous input batch",
            ));
        }

        for bit in &batch.bits {
            if self.synchronized {
                if let Some(byte) = self.push_synchronized_bit(*bit) {
                    self.observe_synchronized_byte(byte, &mut output.message_starts);
                    output.bytes.push(byte);
                }
            } else {
                self.search_bits.push(*bit);
                if self.search_bits.len() > self.preamble_bits.len() {
                    self.search_bits.remove(0);
                }
                if self.search_bits.len() == self.preamble_bits.len()
                    && self
                        .search_bits
                        .iter()
                        .zip(&self.preamble_bits)
                        .all(|(actual, expected)| actual.value == *expected)
                {
                    let preamble = pack_bytes(&self.search_bits, self.config.bit_order);
                    let sample_index = preamble
                        .first()
                        .map_or(bit.range.start, |byte| byte.range.start);
                    output.message_starts.push(RawRadioMessageStart {
                        sample_index,
                        preamble_bytes: self.config.preamble_bytes,
                    });
                    output.bytes.extend(preamble);
                    self.search_bits.clear();
                    self.synchronized = true;
                    self.preamble_run_bytes = self.config.preamble_bytes;
                    self.preamble_run_start = Some(sample_index);
                }
            }
        }
        if let Some(last) = batch.bits.last() {
            self.last_input_end = Some(last.range.end);
        }
        output.synchronized = self.synchronized;
        Ok(output)
    }

    pub fn finish(&mut self) -> RawRadioOutput {
        let mut output = RawRadioOutput::default();
        self.flush_partial_byte(&mut output, "end of stream");
        self.reset();
        output
    }

    fn push_synchronized_bit(&mut self, bit: TimedBit) -> Option<TimedByte> {
        self.pending_bits.push(bit);
        if self.pending_bits.len() == 8 {
            let byte = pack_timed_byte(&self.pending_bits, self.config.bit_order);
            self.pending_bits.clear();
            Some(byte)
        } else {
            None
        }
    }

    fn observe_synchronized_byte(
        &mut self,
        byte: TimedByte,
        message_starts: &mut Vec<RawRadioMessageStart>,
    ) {
        if byte.value != self.config.preamble_byte {
            self.preamble_run_bytes = 0;
            self.preamble_run_start = None;
            return;
        }
        if self.preamble_run_bytes == 0 {
            self.preamble_run_start = Some(byte.range.start);
        }
        self.preamble_run_bytes = self.preamble_run_bytes.saturating_add(1);
        if self.preamble_run_bytes == self.config.preamble_bytes {
            message_starts.push(RawRadioMessageStart {
                sample_index: self.preamble_run_start.unwrap_or(byte.range.start),
                preamble_bytes: self.config.preamble_bytes,
            });
        }
    }

    fn flush_partial_byte(&mut self, output: &mut RawRadioOutput, reason: &str) {
        if self.synchronized && !self.pending_bits.is_empty() {
            output.diagnostics.push(DecodeDiagnostic {
                code: "radio.raw.incomplete_byte".to_owned(),
                message: format!(
                    "discarded {} trailing raw radio bit(s) at {reason}",
                    self.pending_bits.len()
                ),
                severity: Severity::Warning,
                range: bit_range(&self.pending_bits),
            });
        }
    }
}

fn repeated_byte_bits(config: RawRadioConfig) -> Vec<bool> {
    (0..config.preamble_bytes)
        .flat_map(|_| {
            (0..8).map(move |index| {
                let shift = match config.bit_order {
                    BitOrder::MostSignificantFirst => 7 - index,
                    BitOrder::LeastSignificantFirst => index,
                };
                config.preamble_byte & (1 << shift) != 0
            })
        })
        .collect()
}

fn pack_bytes(bits: &[TimedBit], bit_order: BitOrder) -> Vec<TimedByte> {
    bits.chunks_exact(8)
        .map(|bits| pack_timed_byte(bits, bit_order))
        .collect()
}

fn pack_timed_byte(bits: &[TimedBit], bit_order: BitOrder) -> TimedByte {
    let value = bits.iter().enumerate().fold(0_u8, |value, (index, bit)| {
        let shift = match bit_order {
            BitOrder::MostSignificantFirst => 7 - index,
            BitOrder::LeastSignificantFirst => index,
        };
        value | (u8::from(bit.value) << shift)
    });
    TimedByte {
        value,
        range: SampleRange {
            start: bits[0].range.start,
            end: bits[7].range.end,
        },
        status: ByteStatus::default(),
    }
}

fn bit_range(bits: &[TimedBit]) -> Option<SampleRange> {
    Some(SampleRange {
        start: bits.first()?.range.start,
        end: bits.last()?.range.end,
    })
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
                    confidence: 0.9,
                    range: SampleRange {
                        start: first_sample + index as u64 * 2,
                        end: first_sample + index as u64 * 2 + 2,
                    },
                })
                .collect(),
        }
    }

    fn byte_bits(values: &[u8]) -> Vec<bool> {
        values
            .iter()
            .flat_map(|value| (0..8).map(move |shift| value & (1 << (7 - shift)) != 0))
            .collect()
    }

    fn config() -> RawRadioConfig {
        RawRadioConfig {
            preamble_byte: 0xaa,
            preamble_bytes: 3,
            bit_order: BitOrder::MostSignificantFirst,
        }
    }

    #[test]
    fn finds_unaligned_preamble_and_includes_it_in_raw_output() {
        let mut decoder = RawRadioDecoder::new(config()).unwrap();
        let mut input = vec![false, false, false];
        input.extend(byte_bits(&[0xaa, 0xaa, 0xaa, 0x12, 0x34]));

        let output = decoder.push(&bits(&input, 10, false)).unwrap();

        assert!(output.synchronized);
        assert_eq!(
            output
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xaa, 0xaa, 0xaa, 0x12, 0x34]
        );
        assert_eq!(output.bytes[0].range, SampleRange { start: 16, end: 32 });
        assert_eq!(
            output.message_starts,
            [RawRadioMessageStart {
                sample_index: 16,
                preamble_bytes: 3,
            }]
        );
    }

    #[test]
    fn acquisition_and_byte_output_are_chunk_independent() {
        let input = byte_bits(&[0x01, 0xaa, 0xaa, 0xaa, 0x12, 0x34, 0x56]);
        let mut decoder = RawRadioDecoder::new(config()).unwrap();
        let mut decoded = Vec::new();
        let mut offset = 0;
        for end in [5, 19, 33, input.len()] {
            decoded.extend(
                decoder
                    .push(&bits(&input[offset..end], offset as u64 * 2, false))
                    .unwrap()
                    .bytes,
            );
            offset = end;
        }

        assert_eq!(
            decoded.iter().map(|byte| byte.value).collect::<Vec<_>>(),
            [0xaa, 0xaa, 0xaa, 0x12, 0x34, 0x56]
        );
    }

    #[test]
    fn repeated_preamble_starts_a_new_raw_message() {
        let mut decoder = RawRadioDecoder::new(config()).unwrap();
        let input = byte_bits(&[0xaa, 0xaa, 0xaa, 0x12, 0x34, 0xaa, 0xaa, 0xaa, 0x56]);

        let output = decoder.push(&bits(&input, 100, false)).unwrap();

        assert_eq!(
            output.message_starts,
            [
                RawRadioMessageStart {
                    sample_index: 100,
                    preamble_bytes: 3,
                },
                RawRadioMessageStart {
                    sample_index: 180,
                    preamble_bytes: 3,
                },
            ]
        );
    }

    #[test]
    fn repeated_preamble_boundary_survives_input_chunks() {
        let mut decoder = RawRadioDecoder::new(config()).unwrap();
        let first = byte_bits(&[0xaa, 0xaa, 0xaa, 0x12, 0xaa]);
        let second = byte_bits(&[0xaa, 0xaa, 0x56]);
        let first_end = first.len() as u64 * 2;
        decoder.push(&bits(&first, 0, false)).unwrap();

        let output = decoder.push(&bits(&second, first_end, false)).unwrap();

        assert_eq!(
            output.message_starts,
            [RawRadioMessageStart {
                sample_index: 64,
                preamble_bytes: 3,
            }]
        );
    }

    #[test]
    fn discontinuity_discards_partial_byte_and_requires_a_new_preamble() {
        let mut decoder = RawRadioDecoder::new(config()).unwrap();
        let mut first = byte_bits(&[0xaa, 0xaa, 0xaa, 0x12]);
        first.extend([true, false, true]);
        assert!(decoder.push(&bits(&first, 0, false)).unwrap().synchronized);

        let gap = decoder
            .push(&bits(&byte_bits(&[0x34]), 1_000, true))
            .unwrap();
        assert!(!gap.synchronized);
        assert_eq!(gap.bytes, []);
        assert_eq!(gap.diagnostics.len(), 1);

        let reacquired = decoder
            .push(&bits(&byte_bits(&[0xaa, 0xaa, 0xaa, 0x56]), 2_000, false))
            .unwrap();
        assert_eq!(
            reacquired
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xaa, 0xaa, 0xaa, 0x56]
        );
    }

    #[test]
    fn rejects_an_unbounded_preamble() {
        let mut invalid = config();
        invalid.preamble_bytes = 257;
        assert!(RawRadioDecoder::new(invalid).is_err());
    }
}
