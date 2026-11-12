//! Incremental decoder for the SEGGER SystemView target event stream.

use std::error::Error;
use std::fmt;

use crate::{TraceEvent, TraceEventKind};

const FIRST_LENGTH_PREFIXED_EVENT_ID: u64 = 24;
const MAX_VARINT_BYTES: usize = 5;
const DEFAULT_MAX_PACKET_SIZE: usize = 64 * 1024;

/// A SystemView wire-format decoding failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    VarintOverflow,
    PacketTooLarge {
        declared_size: usize,
        maximum_size: usize,
    },
    MalformedPayload {
        event_id: u32,
        reason: &'static str,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VarintOverflow => formatter.write_str("SystemView varint exceeds 32 bits"),
            Self::PacketTooLarge {
                declared_size,
                maximum_size,
            } => write!(
                formatter,
                "SystemView packet size {declared_size} exceeds limit {maximum_size}"
            ),
            Self::MalformedPayload { event_id, reason } => {
                write!(formatter, "malformed SystemView event {event_id}: {reason}")
            }
        }
    }
}

impl Error for DecodeError {}

/// Stateful decoder that accepts arbitrarily split SystemView byte chunks.
#[derive(Clone, Debug)]
pub struct SystemViewDecoder {
    buffer: Vec<u8>,
    core_id: u32,
    next_sequence: u64,
    timestamp_ticks: u64,
    max_packet_size: usize,
}

impl SystemViewDecoder {
    pub fn new(core_id: u32) -> Self {
        Self::with_max_packet_size(core_id, DEFAULT_MAX_PACKET_SIZE)
    }

    pub fn with_max_packet_size(core_id: u32, max_packet_size: usize) -> Self {
        Self {
            buffer: Vec::new(),
            core_id,
            next_sequence: 0,
            timestamp_ticks: 0,
            max_packet_size: max_packet_size.max(1),
        }
    }

    /// Discards buffered bytes and starts a new timestamp and sequence epoch.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.next_sequence = 0;
        self.timestamp_ticks = 0;
    }

    pub fn pending_bytes(&self) -> usize {
        self.buffer.len()
    }

    pub fn timestamp_ticks(&self) -> u64 {
        self.timestamp_ticks
    }

    /// Decodes all complete packets in `bytes` and retains a partial tail.
    ///
    /// A malformed packet clears the partial buffer because the wire format has
    /// no synchronization marker. The decoder can accept a fresh packet on the
    /// next call; use [`Self::reset`] when a new capture epoch begins.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<TraceEvent>, DecodeError> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();

        loop {
            let packet = match parse_packet(&self.buffer, self.max_packet_size) {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(error) => {
                    self.buffer.clear();
                    return Err(error);
                }
            };

            self.buffer.drain(..packet.consumed);
            self.timestamp_ticks = self.timestamp_ticks.saturating_add(packet.timestamp_delta);
            events.push(TraceEvent {
                sequence: self.next_sequence,
                core_id: self.core_id,
                timestamp_ticks: self.timestamp_ticks,
                kind: packet.kind,
            });
            self.next_sequence = self.next_sequence.saturating_add(1);
        }

        if self.buffer.len() > self.max_packet_size {
            let declared_size = self.buffer.len();
            self.buffer.clear();
            return Err(DecodeError::PacketTooLarge {
                declared_size,
                maximum_size: self.max_packet_size,
            });
        }

        Ok(events)
    }
}

impl Default for SystemViewDecoder {
    fn default() -> Self {
        Self::new(0)
    }
}

struct ParsedPacket {
    kind: TraceEventKind,
    timestamp_delta: u64,
    consumed: usize,
}

fn parse_packet(input: &[u8], max_packet_size: usize) -> Result<Option<ParsedPacket>, DecodeError> {
    let Some((event_id, event_id_size)) = parse_varint(input)? else {
        return Ok(None);
    };
    let event_id_u32 = event_id as u32;
    let mut cursor = event_id_size;

    let kind = if event_id < FIRST_LENGTH_PREFIXED_EVENT_ID {
        let Some((kind, payload_size)) = decode_compact_event(event_id_u32, &input[cursor..])?
        else {
            return Ok(None);
        };
        cursor += payload_size;
        kind
    } else {
        let Some((payload_size, encoded_size)) = parse_varint(&input[cursor..])? else {
            return Ok(None);
        };
        cursor += encoded_size;
        let payload_size =
            usize::try_from(payload_size).map_err(|_| DecodeError::PacketTooLarge {
                declared_size: usize::MAX,
                maximum_size: max_packet_size,
            })?;
        if payload_size > max_packet_size {
            return Err(DecodeError::PacketTooLarge {
                declared_size: payload_size,
                maximum_size: max_packet_size,
            });
        }
        let Some(payload_end) = cursor.checked_add(payload_size) else {
            return Err(DecodeError::PacketTooLarge {
                declared_size: usize::MAX,
                maximum_size: max_packet_size,
            });
        };
        if input.len() < payload_end {
            return Ok(None);
        }
        let kind = decode_length_prefixed_event(event_id_u32, &input[cursor..payload_end])?;
        cursor = payload_end;
        kind
    };

    let Some((timestamp_delta, timestamp_size)) = parse_varint(&input[cursor..])? else {
        return Ok(None);
    };
    cursor += timestamp_size;

    Ok(Some(ParsedPacket {
        kind,
        timestamp_delta,
        consumed: cursor,
    }))
}

fn decode_compact_event(
    event_id: u32,
    payload: &[u8],
) -> Result<Option<(TraceEventKind, usize)>, DecodeError> {
    let mut cursor = 0;
    macro_rules! value {
        () => {
            match take_varint(payload, &mut cursor)? {
                Some(value) => value,
                None => return Ok(None),
            }
        };
    }
    macro_rules! string {
        () => {
            match take_string(payload, &mut cursor)? {
                Some(value) => value,
                None => return Ok(None),
            }
        };
    }

    let kind = match event_id {
        0 => TraceEventKind::NoOp,
        1 => TraceEventKind::Overflow {
            dropped_events: value!(),
        },
        2 => TraceEventKind::IsrEntered { isr_id: value!() },
        3 => TraceEventKind::IsrExited,
        4 => TraceEventKind::TaskStarted { task_id: value!() },
        5 => TraceEventKind::TaskStopped,
        6 => TraceEventKind::TaskBecameReady { task_id: value!() },
        7 => TraceEventKind::TaskStoppedReady {
            task_id: value!(),
            cause: value!(),
        },
        8 => TraceEventKind::TaskCreated { task_id: value!() },
        9 => TraceEventKind::TaskInfo {
            task_id: value!(),
            priority: value!(),
            name: string!(),
        },
        10 => TraceEventKind::TraceStarted,
        11 => TraceEventKind::TraceStopped,
        12 => TraceEventKind::ClockSyncCycles { cycles: value!() },
        13 => {
            let low = value!();
            let high = value!();
            TraceEventKind::ClockSyncMicros {
                micros: low | (high << 32),
            }
        }
        14 => TraceEventKind::SystemDescription(string!()),
        15 => TraceEventKind::MarkerStarted {
            marker_id: value!(),
        },
        16 => TraceEventKind::MarkerStopped {
            marker_id: value!(),
        },
        17 => TraceEventKind::Idle,
        18 => TraceEventKind::IsrExitedToScheduler,
        19 => TraceEventKind::TimerEntered { timer_id: value!() },
        20 => TraceEventKind::TimerExited,
        21 => TraceEventKind::StackInfo {
            task_id: value!(),
            stack_base: value!(),
            stack_size: value!(),
            stack_usage: value!(),
        },
        22 => TraceEventKind::ModuleDescription {
            module_id: value!(),
            event_offset: value!(),
            name: string!(),
        },
        23 => TraceEventKind::DataSample {
            sample_id: value!(),
            value: f32::from_bits(value!() as u32),
        },
        _ => {
            return Err(DecodeError::MalformedPayload {
                event_id,
                reason: "unknown compact event ID",
            });
        }
    };

    Ok(Some((kind, cursor)))
}

fn decode_length_prefixed_event(
    event_id: u32,
    payload: &[u8],
) -> Result<TraceEventKind, DecodeError> {
    if !matches!(event_id, 24..=29) {
        return Ok(TraceEventKind::Unknown {
            event_id,
            payload: payload.to_vec(),
        });
    }

    let mut reader = PayloadReader::new(event_id, payload);
    let kind = match event_id {
        24 => TraceEventKind::TargetInfo {
            system_frequency_hz: reader.varint()?,
            cpu_frequency_hz: reader.varint()?,
            ram_base: reader.varint()?,
            id_shift: reader.varint()?,
        },
        25 => TraceEventKind::ResourceName {
            resource_id: reader.varint()?,
            name: reader.string()?,
        },
        26 => {
            let format = reader.string()?;
            let options = reader.varint()?;
            let argument_count = reader.varint()?;
            if argument_count > payload.len() as u64 {
                return Err(reader.error("argument count exceeds payload size"));
            }
            let mut arguments = Vec::with_capacity(argument_count as usize);
            for _ in 0..argument_count {
                arguments.push(reader.varint()?);
            }
            TraceEventKind::FormattedPrint {
                format,
                options,
                arguments,
            }
        }
        27 => TraceEventKind::ModuleCount {
            count: reader.varint()?,
        },
        28 => {
            let call_event_id = reader.varint()?;
            let return_value = if reader.is_finished() {
                None
            } else {
                Some(reader.varint()?)
            };
            TraceEventKind::ApiCallEnded {
                event_id: call_event_id,
                return_value,
            }
        }
        29 => TraceEventKind::TaskTerminated {
            task_id: reader.varint()?,
        },
        _ => {
            return Err(reader.error("unsupported reserved event ID"));
        }
    };
    reader.finish()?;
    Ok(kind)
}

struct PayloadReader<'a> {
    event_id: u32,
    payload: &'a [u8],
    cursor: usize,
}

impl<'a> PayloadReader<'a> {
    fn new(event_id: u32, payload: &'a [u8]) -> Self {
        Self {
            event_id,
            payload,
            cursor: 0,
        }
    }

    fn error(&self, reason: &'static str) -> DecodeError {
        DecodeError::MalformedPayload {
            event_id: self.event_id,
            reason,
        }
    }

    fn varint(&mut self) -> Result<u64, DecodeError> {
        match take_varint(self.payload, &mut self.cursor)? {
            Some(value) => Ok(value),
            None => Err(self.error("truncated varint")),
        }
    }

    fn string(&mut self) -> Result<String, DecodeError> {
        match take_string(self.payload, &mut self.cursor)? {
            Some(value) => Ok(value),
            None => Err(self.error("truncated string")),
        }
    }

    fn is_finished(&self) -> bool {
        self.cursor == self.payload.len()
    }

    fn finish(self) -> Result<(), DecodeError> {
        if self.is_finished() {
            Ok(())
        } else {
            Err(self.error("unexpected trailing payload bytes"))
        }
    }
}

fn take_varint(input: &[u8], cursor: &mut usize) -> Result<Option<u64>, DecodeError> {
    let Some((value, encoded_size)) = parse_varint(&input[*cursor..])? else {
        return Ok(None);
    };
    *cursor += encoded_size;
    Ok(Some(value))
}

fn parse_varint(input: &[u8]) -> Result<Option<(u64, usize)>, DecodeError> {
    let mut value = 0_u64;
    for (index, byte) in input.iter().copied().take(MAX_VARINT_BYTES).enumerate() {
        if index == MAX_VARINT_BYTES - 1 && byte & 0xf0 != 0 {
            return Err(DecodeError::VarintOverflow);
        }
        value |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(Some((value, index + 1)));
        }
    }
    if input.len() < MAX_VARINT_BYTES {
        Ok(None)
    } else {
        Err(DecodeError::VarintOverflow)
    }
}

fn take_string(input: &[u8], cursor: &mut usize) -> Result<Option<String>, DecodeError> {
    if input.len() <= *cursor {
        return Ok(None);
    }
    let first = input[*cursor];
    let (length, header_size) = if first == u8::MAX {
        if input.len().saturating_sub(*cursor) < 3 {
            return Ok(None);
        }
        let length = usize::from(input[*cursor + 1]) << 8 | usize::from(input[*cursor + 2]);
        (length, 3)
    } else {
        (usize::from(first), 1)
    };
    let Some(start) = cursor.checked_add(header_size) else {
        return Err(DecodeError::VarintOverflow);
    };
    let Some(end) = start.checked_add(length) else {
        return Err(DecodeError::VarintOverflow);
    };
    if input.len() < end {
        return Ok(None);
    }
    *cursor = end;
    Ok(Some(
        String::from_utf8_lossy(&input[start..end]).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_varint(mut value: u32) -> Vec<u8> {
        let mut encoded = Vec::new();
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            encoded.push(byte);
            if value == 0 {
                return encoded;
            }
        }
    }

    fn compact_packet(event_id: u8, payload: &[u8], delta: u32) -> Vec<u8> {
        let mut packet = vec![event_id];
        packet.extend_from_slice(payload);
        packet.extend(encode_varint(delta));
        packet
    }

    fn length_packet(event_id: u32, payload: &[u8], delta: u32) -> Vec<u8> {
        let mut packet = encode_varint(event_id);
        packet.extend(encode_varint(payload.len() as u32));
        packet.extend_from_slice(payload);
        packet.extend(encode_varint(delta));
        packet
    }

    #[test]
    fn decodes_packets_across_every_chunk_boundary() {
        let packet = compact_packet(
            9,
            &[0xac, 0x02, 4, 6, b'w', b'o', b'r', b'k', b'e', b'r'],
            300,
        );

        for split in 0..=packet.len() {
            let mut decoder = SystemViewDecoder::new(2);
            let first = decoder.push(&packet[..split]).expect("prefix is valid");
            let second = decoder.push(&packet[split..]).expect("suffix is valid");
            let events: Vec<_> = first.into_iter().chain(second).collect();

            assert_eq!(events.len(), 1, "split at {split}");
            assert_eq!(events[0].core_id, 2);
            assert_eq!(events[0].timestamp_ticks, 300);
            assert_eq!(
                events[0].kind,
                TraceEventKind::TaskInfo {
                    task_id: 300,
                    priority: 4,
                    name: "worker".to_owned(),
                }
            );
        }
    }

    #[test]
    fn accumulates_timestamp_deltas_and_sequences() {
        let mut stream = compact_packet(10, &[], 5);
        stream.extend(compact_packet(4, &[7], 130));
        stream.extend(compact_packet(3, &[], 2));

        let events = SystemViewDecoder::default()
            .push(&stream)
            .expect("stream is valid");

        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event.timestamp_ticks)
                .collect::<Vec<_>>(),
            [5, 135, 137]
        );
    }

    #[test]
    fn decodes_init_and_preserves_unknown_length_prefixed_events() {
        let mut init_payload = Vec::new();
        init_payload.extend(encode_varint(1_000));
        init_payload.extend(encode_varint(168_000_000));
        init_payload.extend(encode_varint(0x2000_0000));
        init_payload.extend(encode_varint(2));
        let mut stream = length_packet(24, &init_payload, 0);
        stream.extend(length_packet(47, &[1, 2, 3], 9));

        let events = SystemViewDecoder::default()
            .push(&stream)
            .expect("stream is valid");

        assert_eq!(
            events[0].kind,
            TraceEventKind::TargetInfo {
                system_frequency_hz: 1_000,
                cpu_frequency_hz: 168_000_000,
                ram_base: 0x2000_0000,
                id_shift: 2,
            }
        );
        assert_eq!(
            events[1].kind,
            TraceEventKind::Unknown {
                event_id: 47,
                payload: vec![1, 2, 3],
            }
        );
    }

    #[test]
    fn malformed_packet_is_reported_and_the_next_packet_can_decode() {
        let mut decoder = SystemViewDecoder::default();
        let error = decoder
            .push(&[0x80, 0x80, 0x80, 0x80, 0x10])
            .expect_err("five-byte overflow is invalid");
        assert_eq!(error, DecodeError::VarintOverflow);
        assert_eq!(decoder.pending_bytes(), 0);

        let events = decoder
            .push(&compact_packet(10, &[], 3))
            .expect("a fresh packet decodes");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, TraceEventKind::TraceStarted);
        assert_eq!(events[0].timestamp_ticks, 3);
    }

    #[test]
    fn rejects_oversized_declared_payloads_without_waiting_for_body() {
        let mut decoder = SystemViewDecoder::with_max_packet_size(0, 8);
        let error = decoder
            .push(&[24, 9])
            .expect_err("declared payload exceeds limit");
        assert_eq!(
            error,
            DecodeError::PacketTooLarge {
                declared_size: 9,
                maximum_size: 8,
            }
        );
    }
}
