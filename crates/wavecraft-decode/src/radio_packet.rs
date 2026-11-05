use crate::{
    AdditiveWhitening, AdditiveWhiteningConfig, ByteStatus, DecodeDiagnostic, DecodeError, Endian,
    SampleRange, Severity, TimedBit, TimedBitBatch, TimedByte,
};

const MAX_SYNC_WORD_BITS: usize = 256;
const MAX_SYNC_SEARCH_BITS: usize = 1_048_576;
const MAX_PACKET_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BitOrder {
    MostSignificantFirst,
    LeastSignificantFirst,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncWordConfig {
    pub bits: Vec<bool>,
    pub max_bit_errors: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioPacketLength {
    /// Collects post-sync bytes until the input stream ends or becomes discontinuous.
    UntilEndOfStream,
    Fixed {
        packet_bytes: usize,
    },
    LengthField {
        offset_bytes: usize,
        width_bytes: usize,
        endian: Endian,
        /// Signed value added to the decoded field to obtain total packet bytes.
        adjustment: i64,
        max_packet_bytes: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RadioPacketFramerConfig {
    /// Sync is detected and consumed; packet bytes begin with the following bit.
    pub sync: SyncWordConfig,
    /// Optional additive dewhitening applied only after sync is accepted.
    pub whitening: Option<AdditiveWhiteningConfig>,
    pub bit_order: BitOrder,
    pub length: RadioPacketLength,
    /// Maximum noise bits searched before old search state is discarded.
    pub max_search_bits: usize,
}

impl RadioPacketFramerConfig {
    pub fn validate(&self) -> Result<(), DecodeError> {
        if self.sync.bits.is_empty() || self.sync.bits.len() > MAX_SYNC_WORD_BITS {
            return Err(DecodeError::new(
                "radio sync word must contain between 1 and 256 bits",
            ));
        }
        if self.sync.max_bit_errors >= self.sync.bits.len() {
            return Err(DecodeError::new(
                "radio sync error limit must be smaller than the sync word",
            ));
        }
        if self.max_search_bits < self.sync.bits.len()
            || self.max_search_bits > MAX_SYNC_SEARCH_BITS
        {
            return Err(DecodeError::new(
                "radio sync search limit must fit the sync word and not exceed 1048576 bits",
            ));
        }
        if let Some(whitening) = self.whitening {
            whitening.validate()?;
        }
        match self.length {
            RadioPacketLength::UntilEndOfStream => {}
            RadioPacketLength::Fixed { packet_bytes } => {
                validate_packet_limit(packet_bytes)?;
            }
            RadioPacketLength::LengthField {
                offset_bytes,
                width_bytes,
                max_packet_bytes,
                ..
            } => {
                if !(1..=8).contains(&width_bytes) {
                    return Err(DecodeError::new(
                        "radio packet length field width must be between 1 and 8 bytes",
                    ));
                }
                validate_packet_limit(max_packet_bytes)?;
                if offset_bytes.saturating_add(width_bytes) > max_packet_bytes {
                    return Err(DecodeError::new(
                        "radio packet length field lies outside the maximum packet",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RadioPacket {
    pub bytes: Vec<TimedByte>,
    pub range: SampleRange,
    pub confidence: f32,
    pub diagnostics: Vec<DecodeDiagnostic>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RadioPacketFramingOutput {
    pub packets: Vec<RadioPacket>,
    pub diagnostics: Vec<DecodeDiagnostic>,
}

/// Streaming, bounded sync detector and radio packet framer.
pub struct RadioPacketFramer {
    config: RadioPacketFramerConfig,
    search_bits: Vec<TimedBit>,
    packet_bits: Vec<TimedBit>,
    packet_target_bits: Option<usize>,
    sync_range: Option<SampleRange>,
    sync_bit_errors: usize,
    whitening: Option<AdditiveWhitening>,
    last_input_end: Option<u64>,
}

impl RadioPacketFramer {
    pub fn new(config: RadioPacketFramerConfig) -> Result<Self, DecodeError> {
        config.validate()?;
        let whitening = config.whitening.map(AdditiveWhitening::new).transpose()?;
        Ok(Self {
            search_bits: Vec::with_capacity(config.max_search_bits),
            packet_bits: Vec::new(),
            packet_target_bits: fixed_target_bits(config.length),
            sync_range: None,
            sync_bit_errors: 0,
            whitening,
            last_input_end: None,
            config,
        })
    }

    pub fn config(&self) -> &RadioPacketFramerConfig {
        &self.config
    }

    pub fn buffered_bit_count(&self) -> usize {
        self.search_bits.len() + self.packet_bits.len()
    }

    pub fn reset(&mut self) {
        self.search_bits.clear();
        self.packet_bits.clear();
        self.packet_target_bits = fixed_target_bits(self.config.length);
        self.sync_range = None;
        self.sync_bit_errors = 0;
        if let Some(whitening) = &mut self.whitening {
            whitening.reset();
        }
        self.last_input_end = None;
    }

    pub fn push(&mut self, batch: &TimedBitBatch) -> Result<RadioPacketFramingOutput, DecodeError> {
        batch.validate()?;
        let mut output = RadioPacketFramingOutput::default();
        if batch.discontinuity {
            self.flush_partial_packet(&mut output, "input discontinuity");
            self.reset();
        } else if self
            .last_input_end
            .is_some_and(|end| batch.bits.first().is_some_and(|bit| bit.range.start < end))
        {
            return Err(DecodeError::new(
                "new radio packet bits overlap the previous input batch",
            ));
        }

        let mut discarded_search_bits = 0_usize;
        for bit in &batch.bits {
            if self.sync_range.is_none() {
                self.search_bits.push(*bit);
                if let Some(bit_errors) = self.matches_sync_suffix() {
                    let sync_start = self.search_bits.len() - self.config.sync.bits.len();
                    let first = self.search_bits[sync_start];
                    let last = *self.search_bits.last().unwrap_or(&first);
                    self.sync_range = Some(covering_range(first.range, last.range));
                    self.sync_bit_errors = bit_errors;
                    self.search_bits.clear();
                    self.packet_bits.clear();
                    self.packet_target_bits = fixed_target_bits(self.config.length);
                    if let Some(whitening) = &mut self.whitening {
                        whitening.reset();
                    }
                    continue;
                }
                if self.search_bits.len() > self.config.max_search_bits {
                    let retained = self.config.sync.bits.len().saturating_sub(1);
                    let discard = self.search_bits.len().saturating_sub(retained);
                    self.search_bits.drain(..discard);
                    discarded_search_bits = discarded_search_bits.saturating_add(discard);
                }
                continue;
            }

            let bit = self
                .whitening
                .as_mut()
                .map_or(*bit, |whitening| whitening.transform(*bit));
            self.packet_bits.push(bit);
            if self.packet_target_bits.is_none() {
                self.packet_target_bits = self.decode_length_target(&mut output);
                if self.sync_range.is_none() {
                    continue;
                }
            }
            if self
                .packet_target_bits
                .is_some_and(|target| self.packet_bits.len() == target)
            {
                output.packets.push(self.take_packet()?);
            }
        }
        if discarded_search_bits > 0 {
            output.diagnostics.push(packet_diagnostic(
                "radio.sync.search_limit",
                format!("discarded {discarded_search_bits} noise bits while searching for sync"),
                None,
                Severity::Info,
            ));
        }
        if let Some(last) = batch.bits.last() {
            self.last_input_end = Some(last.range.end);
        }
        Ok(output)
    }

    pub fn finish(&mut self) -> RadioPacketFramingOutput {
        let mut output = RadioPacketFramingOutput::default();
        self.flush_partial_packet(&mut output, "end of stream");
        self.reset();
        output
    }

    fn matches_sync_suffix(&self) -> Option<usize> {
        if self.search_bits.len() < self.config.sync.bits.len() {
            return None;
        }
        let start = self.search_bits.len() - self.config.sync.bits.len();
        let errors = self.search_bits[start..]
            .iter()
            .zip(&self.config.sync.bits)
            .filter(|(actual, expected)| actual.value != **expected)
            .count();
        (errors <= self.config.sync.max_bit_errors).then_some(errors)
    }

    fn decode_length_target(&mut self, output: &mut RadioPacketFramingOutput) -> Option<usize> {
        let RadioPacketLength::LengthField {
            offset_bytes,
            width_bytes,
            endian,
            adjustment,
            max_packet_bytes,
        } = self.config.length
        else {
            return self.packet_target_bits;
        };
        let prefix_bytes = offset_bytes.saturating_add(width_bytes);
        if self.packet_bits.len() < prefix_bytes.saturating_mul(8) {
            return None;
        }
        let encoded = decode_length_bits(
            &self.packet_bits[offset_bytes * 8..prefix_bytes * 8],
            self.config.bit_order,
            endian,
        );
        let calculated = i128::from(encoded) + i128::from(adjustment);
        let valid = usize::try_from(calculated)
            .ok()
            .filter(|length| *length >= prefix_bytes && *length <= max_packet_bytes);
        let Some(packet_bytes) = valid else {
            output.diagnostics.push(packet_diagnostic(
                "radio.framing.invalid_length",
                format!(
                    "decoded radio packet length {calculated} is outside {prefix_bytes}..={max_packet_bytes}"
                ),
                packet_bit_range(&self.packet_bits),
                Severity::Error,
            ));
            self.start_searching();
            return None;
        };
        Some(packet_bytes.saturating_mul(8))
    }

    fn take_packet(&mut self) -> Result<RadioPacket, DecodeError> {
        let sync_range = self
            .sync_range
            .ok_or_else(|| DecodeError::new("radio packet is missing its sync range"))?;
        let bytes = pack_timed_bytes(&self.packet_bits, self.config.bit_order)?;
        let last_range = bytes.last().map_or(sync_range, |byte| byte.range);
        let confidence = if self.packet_bits.is_empty() {
            0.0
        } else {
            self.packet_bits
                .iter()
                .map(|bit| bit.confidence)
                .sum::<f32>()
                / self.packet_bits.len() as f32
        };
        let mut diagnostics = Vec::new();
        if self.sync_bit_errors > 0 {
            diagnostics.push(packet_diagnostic(
                "radio.sync.bit_errors",
                format!(
                    "accepted sync word with {} bit error(s)",
                    self.sync_bit_errors
                ),
                Some(sync_range),
                Severity::Warning,
            ));
        }
        let packet = RadioPacket {
            bytes,
            range: covering_range(sync_range, last_range),
            confidence,
            diagnostics,
        };
        self.start_searching();
        Ok(packet)
    }

    fn flush_partial_packet(&mut self, output: &mut RadioPacketFramingOutput, reason: &str) {
        if self.sync_range.is_none() || self.packet_bits.is_empty() {
            return;
        }
        if self.config.length == RadioPacketLength::UntilEndOfStream
            && self.packet_bits.len() % 8 == 0
        {
            if let Ok(packet) = self.take_packet() {
                output.packets.push(packet);
                return;
            }
        }
        output.diagnostics.push(packet_diagnostic(
            "radio.framing.incomplete_packet",
            format!(
                "discarded an incomplete radio packet containing {} bit(s) at {reason}",
                self.packet_bits.len()
            ),
            packet_bit_range(&self.packet_bits).or(self.sync_range),
            Severity::Warning,
        ));
    }

    fn start_searching(&mut self) {
        self.search_bits.clear();
        self.packet_bits.clear();
        self.packet_target_bits = fixed_target_bits(self.config.length);
        self.sync_range = None;
        self.sync_bit_errors = 0;
        if let Some(whitening) = &mut self.whitening {
            whitening.reset();
        }
    }
}

fn validate_packet_limit(packet_bytes: usize) -> Result<(), DecodeError> {
    if packet_bytes == 0 || packet_bytes > MAX_PACKET_BYTES {
        return Err(DecodeError::new(
            "radio packet byte limit must be between 1 and 1048576",
        ));
    }
    Ok(())
}

fn fixed_target_bits(length: RadioPacketLength) -> Option<usize> {
    match length {
        RadioPacketLength::UntilEndOfStream => None,
        RadioPacketLength::Fixed { packet_bytes } => Some(packet_bytes.saturating_mul(8)),
        RadioPacketLength::LengthField { .. } => None,
    }
}

fn decode_length_bits(bits: &[TimedBit], bit_order: BitOrder, endian: Endian) -> u64 {
    let bytes = bits
        .chunks_exact(8)
        .map(|bits| pack_byte(bits, bit_order))
        .collect::<Vec<_>>();
    match endian {
        Endian::Little => bytes
            .iter()
            .rev()
            .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte)),
        Endian::Big => bytes
            .iter()
            .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte)),
    }
}

fn pack_timed_bytes(bits: &[TimedBit], bit_order: BitOrder) -> Result<Vec<TimedByte>, DecodeError> {
    if bits.len() % 8 != 0 {
        return Err(DecodeError::new(
            "radio packet bit count is not byte aligned",
        ));
    }
    Ok(bits
        .chunks_exact(8)
        .map(|bits| TimedByte {
            value: pack_byte(bits, bit_order),
            range: covering_range(bits[0].range, bits[7].range),
            status: ByteStatus::default(),
        })
        .collect())
}

fn pack_byte(bits: &[TimedBit], bit_order: BitOrder) -> u8 {
    bits.iter().enumerate().fold(0_u8, |value, (index, bit)| {
        let shift = match bit_order {
            BitOrder::MostSignificantFirst => 7 - index,
            BitOrder::LeastSignificantFirst => index,
        };
        value | (u8::from(bit.value) << shift)
    })
}

fn covering_range(first: SampleRange, last: SampleRange) -> SampleRange {
    SampleRange {
        start: first.start.min(last.start),
        end: first.end.max(last.end),
    }
}

fn packet_bit_range(bits: &[TimedBit]) -> Option<SampleRange> {
    bits.first()
        .zip(bits.last())
        .map(|(first, last)| covering_range(first.range, last.range))
}

fn packet_diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    range: Option<SampleRange>,
    severity: Severity,
) -> DecodeDiagnostic {
    DecodeDiagnostic {
        code: code.into(),
        message: message.into(),
        severity,
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timed_bits(values: &[bool], first_sample: u64, discontinuity: bool) -> TimedBitBatch {
        TimedBitBatch {
            discontinuity,
            bits: values
                .iter()
                .enumerate()
                .map(|(index, value)| TimedBit {
                    value: *value,
                    confidence: 0.75,
                    range: SampleRange {
                        start: first_sample + index as u64 * 2,
                        end: first_sample + index as u64 * 2 + 2,
                    },
                })
                .collect(),
        }
    }

    fn byte_bits(value: u8, order: BitOrder) -> Vec<bool> {
        (0..8)
            .map(|index| {
                let shift = match order {
                    BitOrder::MostSignificantFirst => 7 - index,
                    BitOrder::LeastSignificantFirst => index,
                };
                value & (1 << shift) != 0
            })
            .collect()
    }

    fn fixed_config(bit_order: BitOrder) -> RadioPacketFramerConfig {
        RadioPacketFramerConfig {
            sync: SyncWordConfig {
                bits: vec![true, false, true, false],
                max_bit_errors: 0,
            },
            whitening: None,
            bit_order,
            length: RadioPacketLength::Fixed { packet_bytes: 2 },
            max_search_bits: 32,
        }
    }

    fn whitening_config() -> AdditiveWhiteningConfig {
        AdditiveWhiteningConfig {
            width: 7,
            polynomial: 0b100_0001,
            seed: 0b101_1011,
            direction: crate::WhiteningDirection::ShiftRight,
            invert_output: false,
            start_bit: 0,
            end_bit: None,
            advance_excluded_bits: false,
        }
    }

    fn apply_whitening(values: &[bool], config: AdditiveWhiteningConfig) -> Vec<bool> {
        let mut whitening = AdditiveWhitening::new(config).unwrap();
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                whitening
                    .transform(TimedBit {
                        value: *value,
                        confidence: 1.0,
                        range: SampleRange {
                            start: index as u64,
                            end: index as u64 + 1,
                        },
                    })
                    .value
            })
            .collect()
    }

    #[test]
    fn fixed_packets_decode_across_arbitrary_chunks() {
        let mut values = vec![false, false, true, false, true, false];
        values.extend(byte_bits(0xA5, BitOrder::MostSignificantFirst));
        values.extend(byte_bits(0x3C, BitOrder::MostSignificantFirst));
        let mut framer =
            RadioPacketFramer::new(fixed_config(BitOrder::MostSignificantFirst)).unwrap();

        let first = framer.push(&timed_bits(&values[..9], 0, false)).unwrap();
        let second = framer.push(&timed_bits(&values[9..], 18, false)).unwrap();

        assert!(first.packets.is_empty());
        assert_eq!(second.packets.len(), 1);
        assert_eq!(
            second.packets[0]
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xA5, 0x3C]
        );
        assert_eq!(second.packets[0].confidence, 0.75);
        assert!(second.packets[0].diagnostics.is_empty());
    }

    #[test]
    fn until_end_of_stream_emits_all_post_sync_bytes_on_finish() {
        let mut config = fixed_config(BitOrder::MostSignificantFirst);
        config.length = RadioPacketLength::UntilEndOfStream;
        let mut values = config.sync.bits.clone();
        values.extend(byte_bits(0xA5, config.bit_order));
        values.extend(byte_bits(0x3C, config.bit_order));
        let mut framer = RadioPacketFramer::new(config).unwrap();

        let pushed = framer.push(&timed_bits(&values, 0, false)).unwrap();
        let finished = framer.finish();

        assert!(pushed.packets.is_empty());
        assert!(finished.diagnostics.is_empty());
        assert_eq!(
            finished.packets[0]
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xA5, 0x3C]
        );
    }

    #[test]
    fn sync_hamming_limit_accepts_and_reports_bit_errors() {
        let mut config = fixed_config(BitOrder::LeastSignificantFirst);
        config.sync.max_bit_errors = 1;
        config.length = RadioPacketLength::Fixed { packet_bytes: 1 };
        let mut values = vec![true, true, true, false];
        values.extend(byte_bits(0x96, BitOrder::LeastSignificantFirst));
        let mut framer = RadioPacketFramer::new(config).unwrap();

        let output = framer.push(&timed_bits(&values, 0, false)).unwrap();

        assert_eq!(output.packets[0].bytes[0].value, 0x96);
        assert_eq!(
            output.packets[0].diagnostics[0].code,
            "radio.sync.bit_errors"
        );
    }

    #[test]
    fn length_field_frames_a_bounded_packet() {
        let config = RadioPacketFramerConfig {
            sync: SyncWordConfig {
                bits: vec![true, true, false, false],
                max_bit_errors: 0,
            },
            whitening: None,
            bit_order: BitOrder::MostSignificantFirst,
            length: RadioPacketLength::LengthField {
                offset_bytes: 0,
                width_bytes: 1,
                endian: Endian::Big,
                adjustment: 0,
                max_packet_bytes: 16,
            },
            max_search_bits: 32,
        };
        let mut values = config.sync.bits.clone();
        for byte in [3_u8, 0x10, 0x20] {
            values.extend(byte_bits(byte, config.bit_order));
        }
        let mut framer = RadioPacketFramer::new(config).unwrap();

        let output = framer.push(&timed_bits(&values, 0, false)).unwrap();

        assert_eq!(output.packets.len(), 1);
        assert_eq!(
            output.packets[0]
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [3, 0x10, 0x20]
        );
    }

    #[test]
    fn dewhitens_after_sync_before_decoding_a_length_field() {
        let mut config = RadioPacketFramerConfig {
            sync: SyncWordConfig {
                bits: vec![true, true, false, false],
                max_bit_errors: 0,
            },
            whitening: Some(whitening_config()),
            bit_order: BitOrder::MostSignificantFirst,
            length: RadioPacketLength::LengthField {
                offset_bytes: 0,
                width_bytes: 1,
                endian: Endian::Big,
                adjustment: 0,
                max_packet_bytes: 16,
            },
            max_search_bits: 32,
        };
        let mut clear_packet = Vec::new();
        for byte in [3_u8, 0x10, 0x20] {
            clear_packet.extend(byte_bits(byte, config.bit_order));
        }
        let encoded_packet = apply_whitening(&clear_packet, config.whitening.unwrap());
        let mut values = config.sync.bits.clone();
        values.extend(encoded_packet);
        let mut framer = RadioPacketFramer::new(config.clone()).unwrap();

        let first = framer.push(&timed_bits(&values[..17], 0, false)).unwrap();
        let second = framer.push(&timed_bits(&values[17..], 34, false)).unwrap();

        assert!(first.packets.is_empty());
        assert_eq!(
            second.packets[0]
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [3, 0x10, 0x20]
        );
        assert_eq!(second.packets[0].confidence, 0.75);

        config.length = RadioPacketLength::Fixed { packet_bytes: 1 };
        let mut repeated = config.sync.bits.clone();
        repeated.extend(apply_whitening(
            &byte_bits(0xA5, config.bit_order),
            config.whitening.unwrap(),
        ));
        repeated.extend(config.sync.bits.clone());
        repeated.extend(apply_whitening(
            &byte_bits(0x5A, config.bit_order),
            config.whitening.unwrap(),
        ));
        let packets = RadioPacketFramer::new(config)
            .unwrap()
            .push(&timed_bits(&repeated, 0, false))
            .unwrap()
            .packets;
        assert_eq!(
            packets
                .iter()
                .map(|packet| packet.bytes[0].value)
                .collect::<Vec<_>>(),
            [0xA5, 0x5A]
        );
    }

    #[test]
    fn invalid_length_resets_to_bounded_sync_search() {
        let mut config = fixed_config(BitOrder::MostSignificantFirst);
        config.length = RadioPacketLength::LengthField {
            offset_bytes: 0,
            width_bytes: 1,
            endian: Endian::Big,
            adjustment: 0,
            max_packet_bytes: 8,
        };
        let mut values = config.sync.bits.clone();
        values.extend(byte_bits(0xFF, config.bit_order));
        let mut framer = RadioPacketFramer::new(config).unwrap();

        let output = framer.push(&timed_bits(&values, 0, false)).unwrap();

        assert!(output.packets.is_empty());
        assert_eq!(output.diagnostics[0].code, "radio.framing.invalid_length");
        assert_eq!(framer.buffered_bit_count(), 0);
    }

    #[test]
    fn discontinuity_flushes_a_partial_packet_with_a_diagnostic() {
        let mut framer =
            RadioPacketFramer::new(fixed_config(BitOrder::MostSignificantFirst)).unwrap();
        let mut values = framer.config().sync.bits.clone();
        values.extend(byte_bits(0xA5, BitOrder::MostSignificantFirst));
        framer.push(&timed_bits(&values, 0, false)).unwrap();

        let output = framer.push(&timed_bits(&[], 100, true)).unwrap();

        assert_eq!(
            output.diagnostics[0].code,
            "radio.framing.incomplete_packet"
        );
        assert_eq!(framer.buffered_bit_count(), 0);
    }
}
