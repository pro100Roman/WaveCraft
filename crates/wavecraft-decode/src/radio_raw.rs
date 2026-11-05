use crate::{
    BitOrder, ByteStatus, DecodeDiagnostic, DecodeError, SampleRange, Severity, TimedBit,
    TimedBitBatch, TimedByte,
};

const MAX_PREAMBLE_BYTES: usize = 256;
const MAX_UNCERTAIN_BITS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawRadioConfig {
    /// Byte repeated to form the acquisition preamble.
    pub preamble_byte: u8,
    /// Number of consecutive preamble bytes required before byte output starts.
    pub preamble_bytes: usize,
    /// Optional big-endian sync word following the repeated preamble.
    pub sync_word: u64,
    /// Number of low-order bytes from `sync_word`, between zero and eight.
    pub sync_word_bytes: u8,
    /// Maximum hard-decision mismatches accepted across preamble and sync.
    pub maximum_preamble_bit_errors: usize,
    pub bit_order: BitOrder,
    /// Minimum accepted hard-decision confidence, in thousandths.
    pub minimum_bit_confidence_milli: u16,
    /// Maximum low-confidence bits retained in one preamble-delimited message.
    pub maximum_uncertain_bits: usize,
}

impl RawRadioConfig {
    pub fn validate(self) -> Result<(), DecodeError> {
        if !(1..=MAX_PREAMBLE_BYTES).contains(&self.preamble_bytes) {
            return Err(DecodeError::new(
                "raw radio preamble length must be between 1 and 256 bytes",
            ));
        }
        if self.sync_word_bytes > 8 {
            return Err(DecodeError::new(
                "raw radio sync word must contain at most 8 bytes",
            ));
        }
        let acquisition_bits = self
            .preamble_bytes
            .saturating_add(usize::from(self.sync_word_bytes))
            .saturating_mul(8);
        if self.maximum_preamble_bit_errors >= acquisition_bits {
            return Err(DecodeError::new(
                "raw radio preamble error limit must be smaller than the acquisition pattern",
            ));
        }
        if self.minimum_bit_confidence_milli > 1_000 {
            return Err(DecodeError::new(
                "raw radio minimum bit confidence must be between 0 and 1",
            ));
        }
        if self.maximum_uncertain_bits > MAX_UNCERTAIN_BITS {
            return Err(DecodeError::new(
                "raw radio maximum uncertain bits must not exceed 1000000",
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
    pub maximum_uncertain_bits: usize,
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
    preamble_run_uncertain_bits: usize,
    preamble_run_start: Option<u64>,
    last_input_start: Option<u64>,
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
            preamble_run_uncertain_bits: 0,
            preamble_run_start: None,
            last_input_start: None,
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
        self.preamble_run_uncertain_bits = 0;
        self.preamble_run_start = None;
        self.last_input_start = None;
    }

    pub fn push(&mut self, batch: &TimedBitBatch) -> Result<RawRadioOutput, DecodeError> {
        batch.validate()?;
        let mut output = RawRadioOutput::default();
        if batch.discontinuity {
            self.flush_partial_byte(&mut output, "input discontinuity");
            self.reset();
        } else if self.last_input_start.is_some_and(|start| {
            batch
                .bits
                .first()
                .is_some_and(|bit| bit.range.start < start)
        }) {
            return Err(DecodeError::new(
                "new raw radio bits are not ordered after the previous input batch",
            ));
        }

        for bit in &batch.bits {
            if self.synchronized {
                self.push_search_bit(*bit);
                if self.config.sync_word_bytes > 0 && self.acquisition_pattern_matches() {
                    // A distinct sync word can safely re-establish the symbol-to-byte
                    // phase after a timing slip. The already-emitted acquisition bytes
                    // remain with the previous raw burst; new payload begins after sync.
                    self.pending_bits.clear();
                    self.search_bits.clear();
                    self.preamble_run_bytes = 0;
                    self.preamble_run_uncertain_bits = 0;
                    self.preamble_run_start = None;
                    output.message_starts.push(RawRadioMessageStart {
                        sample_index: bit.range.end,
                        preamble_bytes: 0,
                        maximum_uncertain_bits: self.config.maximum_uncertain_bits,
                    });
                    continue;
                }
                if let Some(byte) = self.push_synchronized_bit(*bit) {
                    self.observe_synchronized_byte(byte, &mut output.message_starts);
                    output.bytes.push(byte);
                }
            } else {
                self.push_search_bit(*bit);
                if self.acquisition_pattern_matches()
                    && self
                        .search_bits
                        .iter()
                        .filter(|bit| self.is_low_confidence(bit))
                        .count()
                        <= self.config.maximum_uncertain_bits
                {
                    let preamble = pack_bytes(
                        &self.search_bits,
                        self.config.bit_order,
                        self.minimum_bit_confidence(),
                    );
                    let sample_index = preamble
                        .first()
                        .map_or(bit.range.start, |byte| byte.range.start);
                    let preamble_uncertain_bits = preamble
                        .iter()
                        .map(|byte| usize::from(byte.status.low_confidence_bits))
                        .sum();
                    output.message_starts.push(RawRadioMessageStart {
                        sample_index,
                        preamble_bytes: self
                            .config
                            .preamble_bytes
                            .saturating_add(usize::from(self.config.sync_word_bytes)),
                        maximum_uncertain_bits: self.config.maximum_uncertain_bits,
                    });
                    output.bytes.extend(preamble);
                    self.search_bits.clear();
                    self.synchronized = true;
                    self.preamble_run_bytes = self.config.preamble_bytes;
                    self.preamble_run_uncertain_bits = preamble_uncertain_bits;
                    self.preamble_run_start = Some(sample_index);
                }
            }
        }
        if let Some(last) = batch.bits.last() {
            // Source ranges describe every IQ sample that contributed to a bit
            // decision. Filtering and interpolated timing recovery can therefore
            // produce valid adjacent bits whose provenance ranges overlap.
            self.last_input_start = Some(last.range.start);
        }
        output.synchronized = self.synchronized;
        Ok(output)
    }

    pub fn finish(&mut self) -> RawRadioOutput {
        // A capture or squelch-delimited burst may end at any bit position.
        // Dropping fewer than eight remaining bits is therefore an expected
        // boundary condition, not a decode warning.
        let output = RawRadioOutput::default();
        self.reset();
        output
    }

    fn push_synchronized_bit(&mut self, bit: TimedBit) -> Option<TimedByte> {
        self.pending_bits.push(bit);
        if self.pending_bits.len() == 8 {
            let byte = pack_timed_byte(
                &self.pending_bits,
                self.config.bit_order,
                self.minimum_bit_confidence(),
            );
            self.pending_bits.clear();
            Some(byte)
        } else {
            None
        }
    }

    fn push_search_bit(&mut self, bit: TimedBit) {
        self.search_bits.push(bit);
        if self.search_bits.len() > self.preamble_bits.len() {
            self.search_bits.remove(0);
        }
    }

    fn acquisition_pattern_matches(&self) -> bool {
        self.search_bits.len() == self.preamble_bits.len()
            && self
                .search_bits
                .iter()
                .zip(&self.preamble_bits)
                .filter(|(actual, expected)| actual.value != **expected)
                .count()
                <= self.config.maximum_preamble_bit_errors
    }

    fn observe_synchronized_byte(
        &mut self,
        byte: TimedByte,
        message_starts: &mut Vec<RawRadioMessageStart>,
    ) {
        if self.config.sync_word_bytes > 0 {
            return;
        }
        let uncertain_bits = usize::from(byte.status.low_confidence_bits);
        if byte.value != self.config.preamble_byte
            || self
                .preamble_run_uncertain_bits
                .saturating_add(uncertain_bits)
                > self.config.maximum_uncertain_bits
        {
            self.preamble_run_bytes = 0;
            self.preamble_run_uncertain_bits = 0;
            self.preamble_run_start = None;
            return;
        }
        if self.preamble_run_bytes == 0 {
            self.preamble_run_start = Some(byte.range.start);
        }
        self.preamble_run_bytes = self.preamble_run_bytes.saturating_add(1);
        self.preamble_run_uncertain_bits = self
            .preamble_run_uncertain_bits
            .saturating_add(uncertain_bits);
        if self.preamble_run_bytes == self.config.preamble_bytes {
            message_starts.push(RawRadioMessageStart {
                sample_index: self.preamble_run_start.unwrap_or(byte.range.start),
                preamble_bytes: self
                    .config
                    .preamble_bytes
                    .saturating_add(usize::from(self.config.sync_word_bytes)),
                maximum_uncertain_bits: self.config.maximum_uncertain_bits,
            });
        }
    }

    fn minimum_bit_confidence(&self) -> f32 {
        f32::from(self.config.minimum_bit_confidence_milli) / 1_000.0
    }

    fn is_low_confidence(&self, bit: &TimedBit) -> bool {
        bit.confidence < self.minimum_bit_confidence()
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
    let preamble = std::iter::repeat(config.preamble_byte).take(config.preamble_bytes);
    let sync = (0..config.sync_word_bytes).map(|index| {
        let shift = u32::from(config.sync_word_bytes - index - 1) * 8;
        ((config.sync_word >> shift) & 0xff) as u8
    });
    preamble
        .chain(sync)
        .flat_map(|byte| {
            (0..8).map(move |index| {
                let shift = match config.bit_order {
                    BitOrder::MostSignificantFirst => 7 - index,
                    BitOrder::LeastSignificantFirst => index,
                };
                byte & (1 << shift) != 0
            })
        })
        .collect()
}

fn pack_bytes(
    bits: &[TimedBit],
    bit_order: BitOrder,
    minimum_bit_confidence: f32,
) -> Vec<TimedByte> {
    bits.chunks_exact(8)
        .map(|bits| pack_timed_byte(bits, bit_order, minimum_bit_confidence))
        .collect()
}

fn pack_timed_byte(
    bits: &[TimedBit],
    bit_order: BitOrder,
    minimum_bit_confidence: f32,
) -> TimedByte {
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
        status: ByteStatus {
            low_confidence_bits: bits
                .iter()
                .filter(|bit| bit.confidence < minimum_bit_confidence)
                .count() as u8,
            ..ByteStatus::default()
        },
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

    fn overlapping_bits(values: &[bool], first_bit: usize, discontinuity: bool) -> TimedBitBatch {
        TimedBitBatch {
            discontinuity,
            bits: values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let start = (first_bit + index) as u64 * 2;
                    TimedBit {
                        value: *value,
                        confidence: 0.9,
                        range: SampleRange {
                            start,
                            end: start + 20,
                        },
                    }
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
            sync_word: 0,
            sync_word_bytes: 0,
            maximum_preamble_bit_errors: 0,
            bit_order: BitOrder::MostSignificantFirst,
            minimum_bit_confidence_milli: 500,
            maximum_uncertain_bits: 0,
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
                maximum_uncertain_bits: 0,
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
    fn confidence_gate_rejects_an_uncertain_preamble() {
        let input = byte_bits(&[0xaa, 0xaa, 0xaa, 0x12]);
        let mut batch = bits(&input, 0, false);
        batch.bits[3].confidence = 0.25;
        let mut decoder = RawRadioDecoder::new(config()).unwrap();

        let output = decoder.push(&batch).unwrap();

        assert!(!output.synchronized);
        assert!(output.bytes.is_empty());
        assert!(output.message_starts.is_empty());
    }

    #[test]
    fn confidence_gate_counts_allowed_uncertain_bits_in_output_bytes() {
        let input = byte_bits(&[0xaa, 0xaa, 0xaa, 0x12]);
        let mut batch = bits(&input, 0, false);
        batch.bits[3].confidence = 0.25;
        batch.bits[27].confidence = 0.25;
        let mut allowed = config();
        allowed.maximum_uncertain_bits = 2;
        let mut decoder = RawRadioDecoder::new(allowed).unwrap();

        let output = decoder.push(&batch).unwrap();

        assert!(output.synchronized);
        assert_eq!(output.bytes[0].status.low_confidence_bits, 1);
        assert_eq!(output.bytes[3].status.low_confidence_bits, 1);
    }

    #[test]
    fn preamble_hamming_limit_accepts_and_reports_a_hard_decision_error() {
        let mut input = byte_bits(&[0xaa, 0xaa, 0xaa, 0x12]);
        input[5] = !input[5];
        let mut tolerant = config();
        tolerant.maximum_preamble_bit_errors = 1;
        let mut decoder = RawRadioDecoder::new(tolerant).unwrap();

        let output = decoder.push(&bits(&input, 0, false)).unwrap();

        assert!(output.synchronized);
        assert_eq!(output.bytes.len(), 4);
        assert_ne!(output.bytes[0].value, 0xaa);
        assert_eq!(output.bytes[3].value, 0x12);
    }

    #[test]
    fn distinct_sync_word_establishes_the_payload_byte_boundary() {
        let mut synchronized = config();
        synchronized.preamble_bytes = 2;
        synchronized.sync_word = 0x2dd4;
        synchronized.sync_word_bytes = 2;
        let mut input = vec![false, true, true];
        input.extend(byte_bits(&[0xaa, 0xaa, 0x2d, 0xd4, 0x12, 0x34]));
        let mut decoder = RawRadioDecoder::new(synchronized).unwrap();

        let output = decoder.push(&bits(&input, 0, false)).unwrap();

        assert!(output.synchronized);
        assert_eq!(
            output
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xaa, 0xaa, 0x2d, 0xd4, 0x12, 0x34]
        );
        assert_eq!(output.message_starts[0].preamble_bytes, 4);
    }

    #[test]
    fn distinct_sync_word_realigns_after_an_inserted_bit() {
        let mut synchronized = config();
        synchronized.preamble_bytes = 2;
        synchronized.sync_word = 0x2dd4;
        synchronized.sync_word_bytes = 2;
        let mut input = byte_bits(&[0xaa, 0xaa, 0x2d, 0xd4, 0x12]);
        input.push(true);
        input.extend(byte_bits(&[0xaa, 0xaa, 0x2d, 0xd4, 0x34]));
        let mut decoder = RawRadioDecoder::new(synchronized).unwrap();

        let output = decoder.push(&bits(&input, 0, false)).unwrap();

        assert_eq!(output.message_starts.len(), 2);
        assert_eq!(output.message_starts[1].preamble_bytes, 0);
        assert_eq!(output.bytes.last().map(|byte| byte.value), Some(0x34));
    }

    #[test]
    fn accepts_filtered_bit_ranges_that_overlap_across_input_batches() {
        let input = byte_bits(&[0xaa, 0xaa, 0xaa, 0x12]);
        let split = 19;
        let mut decoder = RawRadioDecoder::new(config()).unwrap();

        decoder
            .push(&overlapping_bits(&input[..split], 0, false))
            .unwrap();
        let output = decoder
            .push(&overlapping_bits(&input[split..], split, false))
            .unwrap();

        assert!(output.synchronized);
        assert_eq!(
            output
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xaa, 0xaa, 0xaa, 0x12]
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
                    maximum_uncertain_bits: 0,
                },
                RawRadioMessageStart {
                    sample_index: 180,
                    preamble_bytes: 3,
                    maximum_uncertain_bits: 0,
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
                maximum_uncertain_bits: 0,
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
    fn normal_finish_silently_discards_a_partial_byte() {
        let mut decoder = RawRadioDecoder::new(config()).unwrap();
        let mut input = byte_bits(&[0xaa, 0xaa, 0xaa, 0x12]);
        input.extend([true, false]);
        assert!(decoder.push(&bits(&input, 0, false)).unwrap().synchronized);

        let output = decoder.finish();

        assert!(output.diagnostics.is_empty());
        assert!(!decoder.is_synchronized());
    }

    #[test]
    fn rejects_an_unbounded_preamble() {
        let mut invalid = config();
        invalid.preamble_bytes = 257;
        assert!(RawRadioDecoder::new(invalid).is_err());
    }
}
