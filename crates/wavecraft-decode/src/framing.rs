use crate::{
    DecodeDiagnostic, DecodeError, Endian, FramingSchema, LengthField, PacketCompleteness,
    ResyncStrategy, SampleRange, Severity, TimedByte,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FramedBytes {
    pub bytes: Vec<TimedByte>,
    pub completeness: PacketCompleteness,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FramingOutput {
    pub frames: Vec<FramedBytes>,
    pub diagnostics: Vec<DecodeDiagnostic>,
}

#[derive(Clone, Debug)]
pub struct ByteStreamFramer {
    framing: FramingSchema,
    buffer: Vec<TimedByte>,
}

impl ByteStreamFramer {
    pub fn new(framing: FramingSchema) -> Result<Self, DecodeError> {
        if matches!(
            framing,
            FramingSchema::TransportTransaction { .. }
                | FramingSchema::SegmentedTransportTransaction { .. }
        ) {
            return Err(DecodeError::new(
                "transport transaction framing consumes complete transport packets",
            ));
        }
        Ok(Self {
            framing,
            buffer: Vec::new(),
        })
    }

    pub fn buffered_byte_count(&self) -> usize {
        self.buffer.len()
    }

    pub fn push_bytes(&mut self, bytes: &[TimedByte]) -> Result<FramingOutput, DecodeError> {
        crate::decode::validate_timed_bytes(bytes)?;
        if let (Some(previous), Some(next)) = (self.buffer.last(), bytes.first()) {
            if next.range.start < previous.range.end {
                return Err(DecodeError::new(
                    "new transport bytes overlap buffered sample ranges",
                ));
            }
        }
        self.buffer.extend_from_slice(bytes);
        let framing = self.framing.clone();
        match framing {
            FramingSchema::FixedLength { bytes } => Ok(self.frame_fixed(bytes)),
            FramingSchema::Delimiter {
                delimiter,
                include_delimiter,
                max_frame_bytes,
            } => Ok(self.frame_delimited(&delimiter, include_delimiter, max_frame_bytes)),
            FramingSchema::LengthField {
                sync,
                length,
                max_frame_bytes,
                resync,
            } => Ok(self.frame_by_length(&sync, length, max_frame_bytes, resync)),
            FramingSchema::Gap {
                max_frame_bytes, ..
            } => Ok(self.enforce_gap_limit(max_frame_bytes)),
            FramingSchema::TransportTransaction { .. }
            | FramingSchema::SegmentedTransportTransaction { .. } => {
                unreachable!("rejected by constructor")
            }
        }
    }

    pub fn push_gap(&mut self, duration_us: u64) -> FramingOutput {
        let FramingSchema::Gap { minimum_us, .. } = self.framing else {
            return FramingOutput::default();
        };
        if duration_us < minimum_us || self.buffer.is_empty() {
            return FramingOutput::default();
        }
        FramingOutput {
            frames: vec![self.take_frame(self.buffer.len(), PacketCompleteness::Complete)],
            diagnostics: Vec::new(),
        }
    }

    pub fn finish(&mut self) -> Option<FramedBytes> {
        (!self.buffer.is_empty())
            .then(|| self.take_frame(self.buffer.len(), PacketCompleteness::Incomplete))
    }

    fn frame_fixed(&mut self, frame_bytes: usize) -> FramingOutput {
        let mut output = FramingOutput::default();
        while self.buffer.len() >= frame_bytes {
            output
                .frames
                .push(self.take_frame(frame_bytes, PacketCompleteness::Complete));
        }
        output
    }

    fn frame_delimited(
        &mut self,
        delimiter: &[u8],
        include_delimiter: bool,
        max_frame_bytes: usize,
    ) -> FramingOutput {
        let mut output = FramingOutput::default();
        loop {
            let values = self.values();
            if let Some(position) = find_subsequence(&values, delimiter) {
                let consumed = position + delimiter.len();
                if include_delimiter {
                    output
                        .frames
                        .push(self.take_frame(consumed, PacketCompleteness::Complete));
                } else {
                    let frame = self.take_frame(position, PacketCompleteness::Complete);
                    self.buffer.drain(..delimiter.len());
                    if !frame.bytes.is_empty() {
                        output.frames.push(frame);
                    }
                }
                continue;
            }
            if self.buffer.len() > max_frame_bytes {
                output.diagnostics.push(diagnostic(
                    "framing.frame_too_long",
                    format!("delimiter frame exceeded {max_frame_bytes} bytes"),
                    buffered_range(&self.buffer),
                ));
                let retained = longest_suffix_prefix(&values, delimiter);
                let discard = self.buffer.len() - retained;
                self.buffer.drain(..discard);
            }
            break;
        }
        output
    }

    fn frame_by_length(
        &mut self,
        sync: &[u8],
        length: LengthField,
        max_frame_bytes: usize,
        resync: ResyncStrategy,
    ) -> FramingOutput {
        let mut output = FramingOutput::default();
        let prefix_bytes = length.offset + length.width;
        loop {
            if !sync.is_empty() && !self.align_to_sync(sync, &mut output) {
                break;
            }
            if self.buffer.len() < prefix_bytes {
                break;
            }
            let encoded = decode_length(&self.buffer[length.offset..prefix_bytes], length.endian);
            let calculated = i128::from(encoded) + i128::from(length.adjustment);
            let valid_length = usize::try_from(calculated)
                .ok()
                .filter(|bytes| *bytes >= prefix_bytes && *bytes <= max_frame_bytes);
            let Some(frame_bytes) = valid_length else {
                output.diagnostics.push(diagnostic(
                    "framing.invalid_length",
                    format!(
                        "decoded frame length {calculated} is outside {prefix_bytes}..={max_frame_bytes}"
                    ),
                    buffered_prefix_range(&self.buffer, prefix_bytes),
                ));
                match resync {
                    ResyncStrategy::NextSync => {
                        self.buffer.drain(..1);
                    }
                    ResyncStrategy::DropFrame => {
                        self.buffer.clear();
                        break;
                    }
                }
                continue;
            };
            if self.buffer.len() < frame_bytes {
                break;
            }
            output
                .frames
                .push(self.take_frame(frame_bytes, PacketCompleteness::Complete));
        }
        output
    }

    fn align_to_sync(&mut self, sync: &[u8], output: &mut FramingOutput) -> bool {
        if self.buffer.len() < sync.len() {
            return false;
        }
        let values = self.values();
        match find_subsequence(&values, sync) {
            Some(0) => true,
            Some(position) => {
                let discarded = self.buffer.drain(..position).collect::<Vec<_>>();
                output.diagnostics.push(diagnostic(
                    "framing.discarded_prefix",
                    format!("discarded {position} bytes before the next sync sequence"),
                    buffered_range(&discarded),
                ));
                true
            }
            None => {
                let retained = longest_suffix_prefix(&values, sync);
                let discard = self.buffer.len() - retained;
                if discard > 0 {
                    let discarded = self.buffer.drain(..discard).collect::<Vec<_>>();
                    output.diagnostics.push(diagnostic(
                        "framing.sync_not_found",
                        format!("discarded {discard} bytes while searching for sync"),
                        buffered_range(&discarded),
                    ));
                }
                false
            }
        }
    }

    fn enforce_gap_limit(&mut self, max_frame_bytes: usize) -> FramingOutput {
        if self.buffer.len() <= max_frame_bytes {
            return FramingOutput::default();
        }
        let range = buffered_range(&self.buffer);
        self.buffer.clear();
        FramingOutput {
            frames: Vec::new(),
            diagnostics: vec![diagnostic(
                "framing.frame_too_long",
                format!("gap-delimited frame exceeded {max_frame_bytes} bytes"),
                range,
            )],
        }
    }

    fn values(&self) -> Vec<u8> {
        self.buffer.iter().map(|byte| byte.value).collect()
    }

    fn take_frame(&mut self, count: usize, completeness: PacketCompleteness) -> FramedBytes {
        FramedBytes {
            bytes: self.buffer.drain(..count).collect(),
            completeness,
        }
    }
}

fn decode_length(bytes: &[TimedByte], endian: Endian) -> u64 {
    match endian {
        Endian::Little => bytes
            .iter()
            .rev()
            .fold(0_u64, |value, byte| (value << 8) | u64::from(byte.value)),
        Endian::Big => bytes
            .iter()
            .fold(0_u64, |value, byte| (value << 8) | u64::from(byte.value)),
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn longest_suffix_prefix(values: &[u8], delimiter: &[u8]) -> usize {
    let maximum = values.len().min(delimiter.len().saturating_sub(1));
    (1..=maximum)
        .rev()
        .find(|length| values[values.len() - length..] == delimiter[..*length])
        .unwrap_or(0)
}

fn buffered_range(bytes: &[TimedByte]) -> Option<SampleRange> {
    bytes
        .first()
        .zip(bytes.last())
        .map(|(first, last)| SampleRange {
            start: first.range.start,
            end: last.range.end,
        })
}

fn buffered_prefix_range(bytes: &[TimedByte], count: usize) -> Option<SampleRange> {
    bytes
        .first()
        .zip(bytes.get(count - 1))
        .map(|(first, last)| SampleRange {
            start: first.range.start,
            end: last.range.end,
        })
}

fn diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    range: Option<SampleRange>,
) -> DecodeDiagnostic {
    DecodeDiagnostic {
        code: code.into(),
        message: message.into(),
        severity: Severity::Error,
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ByteStatus;

    fn bytes(values: &[u8], sample_offset: u64) -> Vec<TimedByte> {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| TimedByte {
                value: *value,
                range: SampleRange {
                    start: sample_offset + (index * 10) as u64,
                    end: sample_offset + (index * 10 + 8) as u64,
                },
                status: ByteStatus::default(),
            })
            .collect()
    }

    #[test]
    fn fixed_length_frames_across_arbitrary_input_chunks() {
        let mut framer = ByteStreamFramer::new(FramingSchema::FixedLength { bytes: 3 }).unwrap();
        assert!(framer
            .push_bytes(&bytes(&[1, 2], 0))
            .unwrap()
            .frames
            .is_empty());
        let output = framer.push_bytes(&bytes(&[3, 4, 5, 6], 20)).unwrap();
        assert_eq!(output.frames.len(), 2);
        assert_eq!(
            output.frames[0]
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(framer.buffered_byte_count(), 0);
    }

    #[test]
    fn delimiter_split_across_chunks_is_retained() {
        let mut framer = ByteStreamFramer::new(FramingSchema::Delimiter {
            delimiter: vec![0x0D, 0x0A],
            include_delimiter: false,
            max_frame_bytes: 32,
        })
        .unwrap();
        assert!(framer
            .push_bytes(&bytes(b"OK\r", 0))
            .unwrap()
            .frames
            .is_empty());
        let output = framer.push_bytes(&bytes(b"\n", 30)).unwrap();
        assert_eq!(output.frames.len(), 1);
        assert_eq!(
            output.frames[0]
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            b"OK"
        );
    }

    #[test]
    fn gap_emits_only_after_configured_duration() {
        let mut framer = ByteStreamFramer::new(FramingSchema::Gap {
            minimum_us: 500,
            max_frame_bytes: 16,
        })
        .unwrap();
        framer.push_bytes(&bytes(&[1, 2], 0)).unwrap();
        assert!(framer.push_gap(499).frames.is_empty());
        let output = framer.push_gap(500);
        assert_eq!(output.frames.len(), 1);
        assert_eq!(output.frames[0].completeness, PacketCompleteness::Complete);
    }

    #[test]
    fn length_field_discards_noise_and_frames_after_sync() {
        let mut framer = ByteStreamFramer::new(FramingSchema::LengthField {
            sync: vec![0xAA],
            length: LengthField {
                offset: 1,
                width: 1,
                endian: Endian::Little,
                adjustment: 2,
            },
            max_frame_bytes: 16,
            resync: ResyncStrategy::NextSync,
        })
        .unwrap();
        let output = framer
            .push_bytes(&bytes(&[0x00, 0xAA, 0x02, 0x10, 0x11], 0))
            .unwrap();
        assert_eq!(output.frames.len(), 1);
        assert_eq!(output.diagnostics[0].code, "framing.discarded_prefix");
        assert_eq!(
            output.frames[0]
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xAA, 0x02, 0x10, 0x11]
        );
    }

    #[test]
    fn finish_marks_buffered_tail_incomplete() {
        let mut framer = ByteStreamFramer::new(FramingSchema::FixedLength { bytes: 4 }).unwrap();
        framer.push_bytes(&bytes(&[1, 2], 0)).unwrap();
        let tail = framer.finish().unwrap();
        assert_eq!(tail.completeness, PacketCompleteness::Incomplete);
        assert_eq!(framer.buffered_byte_count(), 0);
    }
}
