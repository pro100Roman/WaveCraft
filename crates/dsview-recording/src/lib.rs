//! Versioned, lossless, streaming capture recordings.

use crc32fast::hash;
use dsview_core::{
    CaptureBatch, CaptureError, CaptureEvent, CaptureFrame, DeviceDescriptor, DeviceId,
    InstrumentKind, LogicBatch, ScopeBatch,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::{self, Read, Write};
use std::time::Duration;

const FILE_MAGIC: [u8; 8] = *b"DSVIEW\x1a\n";
const FILE_HEADER_BYTES: usize = 12;
const RECORD_HEADER_BYTES: usize = 16;
const RECORD_SESSION: u8 = 1;
const RECORD_FRAME: u8 = 2;
const RECORD_SOURCE_FAILED: u8 = 3;
const RECORD_SOURCE_STOPPED: u8 = 4;
const RECORD_FOOTER: u8 = 5;
const MAX_RECORD_BYTES: usize = 256 * 1024 * 1024;
const MAX_STRING_BYTES: usize = 1024 * 1024;
const MAX_DEVICES: usize = 1_024;
const MAX_METADATA_ENTRIES: usize = 4_096;
const MAX_SCOPE_CHANNELS: usize = 256;

pub const FORMAT_VERSION: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingHeader {
    pub devices: Vec<DeviceDescriptor>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionStatus {
    Complete,
    Cancelled,
    Failed,
}

impl CompletionStatus {
    fn encode(self) -> u8 {
        match self {
            Self::Complete => 1,
            Self::Cancelled => 2,
            Self::Failed => 3,
        }
    }

    fn decode(value: u8) -> Result<Self, RecordingError> {
        match value {
            1 => Ok(Self::Complete),
            2 => Ok(Self::Cancelled),
            3 => Ok(Self::Failed),
            _ => Err(RecordingError::invalid(format!(
                "unknown recording completion status {value}"
            ))),
        }
    }
}

#[derive(Debug)]
pub enum RecordingError {
    Io(io::Error),
    Invalid(String),
    Incomplete,
}

impl RecordingError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl Display for RecordingError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "recording I/O failed: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid recording: {message}"),
            Self::Incomplete => formatter.write_str("recording ended without a valid footer"),
        }
    }
}

impl Error for RecordingError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Invalid(_) | Self::Incomplete => None,
        }
    }
}

impl From<io::Error> for RecordingError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct RecordingWriter<W: Write> {
    output: W,
    event_count: u64,
    finished: bool,
}

impl<W: Write> RecordingWriter<W> {
    pub fn new(
        mut output: W,
        devices: &[DeviceDescriptor],
        metadata: BTreeMap<String, String>,
    ) -> Result<Self, RecordingError> {
        output.write_all(&FILE_MAGIC)?;
        output.write_all(&FORMAT_VERSION.to_le_bytes())?;
        output.write_all(&0_u16.to_le_bytes())?;
        let payload = encode_session(devices, &metadata)?;
        write_record(&mut output, RECORD_SESSION, &payload)?;
        Ok(Self {
            output,
            event_count: 0,
            finished: false,
        })
    }

    pub fn write_event(&mut self, event: &CaptureEvent) -> Result<(), RecordingError> {
        if self.finished {
            return Err(RecordingError::invalid(
                "cannot append an event after the recording footer",
            ));
        }
        let (kind, payload) = encode_event(event)?;
        write_record(&mut self.output, kind, &payload)?;
        self.event_count = self
            .event_count
            .checked_add(1)
            .ok_or_else(|| RecordingError::invalid("recording event count overflowed"))?;
        Ok(())
    }

    pub fn finish(&mut self, status: CompletionStatus) -> Result<(), RecordingError> {
        if self.finished {
            return Err(RecordingError::invalid(
                "recording footer has already been written",
            ));
        }
        let mut payload = Vec::with_capacity(9);
        payload.push(status.encode());
        push_u64(&mut payload, self.event_count);
        write_record(&mut self.output, RECORD_FOOTER, &payload)?;
        self.output.flush()?;
        self.finished = true;
        Ok(())
    }
}

pub struct RecordingReader<R: Read> {
    input: R,
    header: RecordingHeader,
    event_count: u64,
    completion_status: Option<CompletionStatus>,
}

impl<R: Read> RecordingReader<R> {
    pub fn new(mut input: R) -> Result<Self, RecordingError> {
        let mut file_header = [0_u8; FILE_HEADER_BYTES];
        read_exact_or_invalid(&mut input, &mut file_header, "file header is truncated")?;
        if file_header[..FILE_MAGIC.len()] != FILE_MAGIC {
            return Err(RecordingError::invalid("file magic does not match DSView"));
        }
        let version = u16::from_le_bytes([file_header[8], file_header[9]]);
        if version != FORMAT_VERSION {
            return Err(RecordingError::invalid(format!(
                "format version {version} is unsupported; expected {FORMAT_VERSION}"
            )));
        }
        if file_header[10..12] != [0, 0] {
            return Err(RecordingError::invalid(
                "file header contains unsupported flags",
            ));
        }
        let record = read_record(&mut input)?;
        if record.kind != RECORD_SESSION {
            return Err(RecordingError::invalid(
                "the first recording record is not session metadata",
            ));
        }
        let header = decode_session(&record.payload)?;
        Ok(Self {
            input,
            header,
            event_count: 0,
            completion_status: None,
        })
    }

    pub fn header(&self) -> &RecordingHeader {
        &self.header
    }

    pub fn completion_status(&self) -> Option<CompletionStatus> {
        self.completion_status
    }

    pub fn read_event(&mut self) -> Result<Option<CaptureEvent>, RecordingError> {
        if self.completion_status.is_some() {
            return Ok(None);
        }
        let record = read_record(&mut self.input)?;
        if record.kind == RECORD_FOOTER {
            let (status, expected_event_count) = decode_footer(&record.payload)?;
            if expected_event_count != self.event_count {
                return Err(RecordingError::invalid(format!(
                    "footer reports {expected_event_count} events but {} were read",
                    self.event_count
                )));
            }
            ensure_end_of_file(&mut self.input)?;
            self.completion_status = Some(status);
            return Ok(None);
        }
        let event = decode_event(record.kind, &record.payload)?;
        self.event_count = self
            .event_count
            .checked_add(1)
            .ok_or_else(|| RecordingError::invalid("recording event count overflowed"))?;
        Ok(Some(event))
    }
}

struct Record {
    kind: u8,
    payload: Vec<u8>,
}

fn write_record(output: &mut impl Write, kind: u8, payload: &[u8]) -> Result<(), RecordingError> {
    if payload.len() > MAX_RECORD_BYTES {
        return Err(RecordingError::invalid(format!(
            "record payload is {} bytes; maximum is {MAX_RECORD_BYTES}",
            payload.len()
        )));
    }
    let payload_len = u64::try_from(payload.len())
        .map_err(|_| RecordingError::invalid("record length does not fit in u64"))?;
    let mut header = [0_u8; RECORD_HEADER_BYTES];
    header[0] = kind;
    header[4..12].copy_from_slice(&payload_len.to_le_bytes());
    header[12..16].copy_from_slice(&hash(payload).to_le_bytes());
    output.write_all(&header)?;
    output.write_all(payload)?;
    Ok(())
}

fn read_record(input: &mut impl Read) -> Result<Record, RecordingError> {
    let mut header = [0_u8; RECORD_HEADER_BYTES];
    read_first_byte(input, &mut header[0])?;
    read_exact_or_incomplete(input, &mut header[1..])?;
    if header[1..4] != [0, 0, 0] {
        return Err(RecordingError::invalid(
            "record header contains unsupported flags",
        ));
    }
    let mut length_bytes = [0_u8; 8];
    length_bytes.copy_from_slice(&header[4..12]);
    let payload_len_u64 = u64::from_le_bytes(length_bytes);
    let payload_len = usize::try_from(payload_len_u64)
        .map_err(|_| RecordingError::invalid("record length does not fit this platform"))?;
    if payload_len > MAX_RECORD_BYTES {
        return Err(RecordingError::invalid(format!(
            "record payload is {payload_len} bytes; maximum is {MAX_RECORD_BYTES}"
        )));
    }
    let mut expected_crc_bytes = [0_u8; 4];
    expected_crc_bytes.copy_from_slice(&header[12..16]);
    let expected_crc = u32::from_le_bytes(expected_crc_bytes);
    let mut payload = vec![0_u8; payload_len];
    read_exact_or_incomplete(input, &mut payload)?;
    let actual_crc = hash(&payload);
    if actual_crc != expected_crc {
        return Err(RecordingError::invalid(format!(
            "record checksum mismatch: expected {expected_crc:08x}, got {actual_crc:08x}"
        )));
    }
    Ok(Record {
        kind: header[0],
        payload,
    })
}

fn read_first_byte(input: &mut impl Read, destination: &mut u8) -> Result<(), RecordingError> {
    loop {
        match input.read(std::slice::from_mut(destination)) {
            Ok(0) => return Err(RecordingError::Incomplete),
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn ensure_end_of_file(input: &mut impl Read) -> Result<(), RecordingError> {
    let mut byte = [0_u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => {
                return Err(RecordingError::invalid(
                    "recording contains data after its footer",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn read_exact_or_incomplete(
    input: &mut impl Read,
    destination: &mut [u8],
) -> Result<(), RecordingError> {
    input.read_exact(destination).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            RecordingError::Incomplete
        } else {
            RecordingError::Io(error)
        }
    })
}

fn read_exact_or_invalid(
    input: &mut impl Read,
    destination: &mut [u8],
    message: &str,
) -> Result<(), RecordingError> {
    input.read_exact(destination).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            RecordingError::invalid(message)
        } else {
            RecordingError::Io(error)
        }
    })
}

fn encode_session(
    devices: &[DeviceDescriptor],
    metadata: &BTreeMap<String, String>,
) -> Result<Vec<u8>, RecordingError> {
    if devices.len() > MAX_DEVICES {
        return Err(RecordingError::invalid("too many devices in session"));
    }
    if metadata.len() > MAX_METADATA_ENTRIES {
        return Err(RecordingError::invalid(
            "too many metadata entries in session",
        ));
    }
    let mut payload = Vec::new();
    push_count(&mut payload, devices.len(), "device")?;
    for device in devices {
        payload.push(encode_instrument_kind(device.kind));
        push_string(&mut payload, device.id.as_str())?;
        push_string(&mut payload, &device.name)?;
    }
    push_count(&mut payload, metadata.len(), "metadata")?;
    for (key, value) in metadata {
        push_string(&mut payload, key)?;
        push_string(&mut payload, value)?;
    }
    Ok(payload)
}

fn decode_session(payload: &[u8]) -> Result<RecordingHeader, RecordingError> {
    let mut reader = PayloadReader::new(payload);
    let device_count = reader.read_count(MAX_DEVICES, "device")?;
    let mut devices = Vec::with_capacity(device_count);
    for _ in 0..device_count {
        let kind = decode_instrument_kind(reader.read_u8()?)?;
        let id = DeviceId::new(reader.read_string()?);
        let name = reader.read_string()?;
        devices.push(DeviceDescriptor { id, name, kind });
    }
    let metadata_count = reader.read_count(MAX_METADATA_ENTRIES, "metadata")?;
    let mut metadata = BTreeMap::new();
    for _ in 0..metadata_count {
        let key = reader.read_string()?;
        let value = reader.read_string()?;
        if metadata.insert(key.clone(), value).is_some() {
            return Err(RecordingError::invalid(format!(
                "duplicate metadata key {key:?}"
            )));
        }
    }
    reader.finish()?;
    Ok(RecordingHeader { devices, metadata })
}

fn encode_event(event: &CaptureEvent) -> Result<(u8, Vec<u8>), RecordingError> {
    match event {
        CaptureEvent::Frame(frame) => Ok((RECORD_FRAME, encode_frame(frame)?)),
        CaptureEvent::SourceFailed { device_id, error } => {
            let mut payload = Vec::new();
            push_string(&mut payload, device_id.as_str())?;
            push_string(&mut payload, &error.to_string())?;
            Ok((RECORD_SOURCE_FAILED, payload))
        }
        CaptureEvent::SourceStopped { device_id } => {
            let mut payload = Vec::new();
            push_string(&mut payload, device_id.as_str())?;
            Ok((RECORD_SOURCE_STOPPED, payload))
        }
    }
}

fn decode_event(kind: u8, payload: &[u8]) -> Result<CaptureEvent, RecordingError> {
    match kind {
        RECORD_FRAME => decode_frame(payload).map(CaptureEvent::Frame),
        RECORD_SOURCE_FAILED => {
            let mut reader = PayloadReader::new(payload);
            let device_id = DeviceId::new(reader.read_string()?);
            let error = CaptureError::new(reader.read_string()?);
            reader.finish()?;
            Ok(CaptureEvent::SourceFailed { device_id, error })
        }
        RECORD_SOURCE_STOPPED => {
            let mut reader = PayloadReader::new(payload);
            let device_id = DeviceId::new(reader.read_string()?);
            reader.finish()?;
            Ok(CaptureEvent::SourceStopped { device_id })
        }
        RECORD_SESSION => Err(RecordingError::invalid(
            "session metadata may only appear once at the start",
        )),
        RECORD_FOOTER => Err(RecordingError::invalid("unexpected recording footer")),
        _ => Err(RecordingError::invalid(format!(
            "unknown record kind {kind}"
        ))),
    }
}

fn encode_frame(frame: &CaptureFrame) -> Result<Vec<u8>, RecordingError> {
    let mut payload = Vec::new();
    push_string(&mut payload, frame.device_id.as_str())?;
    push_u64(&mut payload, frame.sequence);
    push_u64(&mut payload, frame.captured_at.as_secs());
    push_u32(&mut payload, frame.captured_at.subsec_nanos());
    match &frame.payload {
        CaptureBatch::Logic(batch) => {
            payload.push(1);
            push_u64(&mut payload, batch.sample_rate_hz);
            payload.push(batch.channel_count);
            push_length_u64(&mut payload, batch.samples.len(), "logic sample")?;
            ensure_sample_bytes(batch.samples.len(), 4)?;
            for sample in &batch.samples {
                push_u32(&mut payload, *sample);
            }
        }
        CaptureBatch::Scope(batch) => {
            payload.push(2);
            push_u64(&mut payload, batch.sample_rate_hz);
            if batch.channels.is_empty() || batch.channels.len() > MAX_SCOPE_CHANNELS {
                return Err(RecordingError::invalid(
                    "scope frame has an unsupported channel count",
                ));
            }
            let sample_count = batch.channels[0].len();
            if batch
                .channels
                .iter()
                .any(|channel| channel.len() != sample_count)
            {
                return Err(RecordingError::invalid(
                    "scope frame channels have different sample counts",
                ));
            }
            push_count(&mut payload, batch.channels.len(), "scope channel")?;
            push_length_u64(&mut payload, sample_count, "scope sample")?;
            let value_count = batch
                .channels
                .len()
                .checked_mul(sample_count)
                .ok_or_else(|| RecordingError::invalid("scope sample count overflowed"))?;
            ensure_sample_bytes(value_count, 4)?;
            for channel in &batch.channels {
                for sample in channel {
                    push_u32(&mut payload, sample.to_bits());
                }
            }
        }
    }
    if payload.len() > MAX_RECORD_BYTES {
        return Err(RecordingError::invalid(
            "capture frame is too large to record",
        ));
    }
    Ok(payload)
}

fn decode_frame(payload: &[u8]) -> Result<CaptureFrame, RecordingError> {
    let mut reader = PayloadReader::new(payload);
    let device_id = DeviceId::new(reader.read_string()?);
    let sequence = reader.read_u64()?;
    let seconds = reader.read_u64()?;
    let nanos = reader.read_u32()?;
    if nanos >= 1_000_000_000 {
        return Err(RecordingError::invalid(
            "frame timestamp nanoseconds are out of range",
        ));
    }
    let captured_at = Duration::new(seconds, nanos);
    let payload_kind = reader.read_u8()?;
    let capture_payload = match payload_kind {
        1 => {
            let sample_rate_hz = reader.read_u64()?;
            let channel_count = reader.read_u8()?;
            if channel_count == 0 || channel_count > 32 {
                return Err(RecordingError::invalid(
                    "logic channel count is out of range",
                ));
            }
            let sample_count = reader.read_length("logic sample")?;
            reader.require_values(sample_count, 4, "logic samples")?;
            let mut samples = Vec::with_capacity(sample_count);
            for _ in 0..sample_count {
                samples.push(reader.read_u32()?);
            }
            CaptureBatch::Logic(LogicBatch {
                sample_rate_hz,
                channel_count,
                samples,
            })
        }
        2 => {
            let sample_rate_hz = reader.read_u64()?;
            let channel_count = reader.read_count(MAX_SCOPE_CHANNELS, "scope channel")?;
            if channel_count == 0 {
                return Err(RecordingError::invalid("scope frame has no channels"));
            }
            let sample_count = reader.read_length("scope sample")?;
            let value_count = channel_count
                .checked_mul(sample_count)
                .ok_or_else(|| RecordingError::invalid("scope sample count overflowed"))?;
            reader.require_values(value_count, 4, "scope samples")?;
            let mut channels = Vec::with_capacity(channel_count);
            for _ in 0..channel_count {
                let mut channel = Vec::with_capacity(sample_count);
                for _ in 0..sample_count {
                    channel.push(f32::from_bits(reader.read_u32()?));
                }
                channels.push(channel);
            }
            CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz,
                channels,
            })
        }
        _ => {
            return Err(RecordingError::invalid(format!(
                "unknown capture payload kind {payload_kind}"
            )))
        }
    };
    reader.finish()?;
    Ok(CaptureFrame {
        device_id,
        sequence,
        captured_at,
        payload: capture_payload,
    })
}

fn decode_footer(payload: &[u8]) -> Result<(CompletionStatus, u64), RecordingError> {
    let mut reader = PayloadReader::new(payload);
    let status = CompletionStatus::decode(reader.read_u8()?)?;
    let event_count = reader.read_u64()?;
    reader.finish()?;
    Ok((status, event_count))
}

fn encode_instrument_kind(kind: InstrumentKind) -> u8 {
    match kind {
        InstrumentKind::LogicAnalyzer => 1,
        InstrumentKind::Oscilloscope => 2,
    }
}

fn decode_instrument_kind(value: u8) -> Result<InstrumentKind, RecordingError> {
    match value {
        1 => Ok(InstrumentKind::LogicAnalyzer),
        2 => Ok(InstrumentKind::Oscilloscope),
        _ => Err(RecordingError::invalid(format!(
            "unknown instrument kind {value}"
        ))),
    }
}

fn ensure_sample_bytes(value_count: usize, bytes_per_value: usize) -> Result<(), RecordingError> {
    let bytes = value_count
        .checked_mul(bytes_per_value)
        .ok_or_else(|| RecordingError::invalid("sample byte count overflowed"))?;
    if bytes > MAX_RECORD_BYTES {
        return Err(RecordingError::invalid("sample payload is too large"));
    }
    Ok(())
}

fn push_count(destination: &mut Vec<u8>, value: usize, name: &str) -> Result<(), RecordingError> {
    let value = u32::try_from(value)
        .map_err(|_| RecordingError::invalid(format!("{name} count does not fit in u32")))?;
    push_u32(destination, value);
    Ok(())
}

fn push_length_u64(
    destination: &mut Vec<u8>,
    value: usize,
    name: &str,
) -> Result<(), RecordingError> {
    let value = u64::try_from(value)
        .map_err(|_| RecordingError::invalid(format!("{name} count does not fit in u64")))?;
    push_u64(destination, value);
    Ok(())
}

fn push_string(destination: &mut Vec<u8>, value: &str) -> Result<(), RecordingError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(RecordingError::invalid("string is too large"));
    }
    push_count(destination, value.len(), "string byte")?;
    destination.extend_from_slice(value.as_bytes());
    Ok(())
}

fn push_u32(destination: &mut Vec<u8>, value: u32) {
    destination.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(destination: &mut Vec<u8>, value: u64) {
    destination.extend_from_slice(&value.to_le_bytes());
}

struct PayloadReader<'a> {
    payload: &'a [u8],
    offset: usize,
}

impl<'a> PayloadReader<'a> {
    fn new(payload: &'a [u8]) -> Self {
        Self { payload, offset: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, RecordingError> {
        let bytes = self.take(1)?;
        Ok(bytes[0])
    }

    fn read_u32(&mut self) -> Result<u32, RecordingError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, RecordingError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_count(&mut self, maximum: usize, name: &str) -> Result<usize, RecordingError> {
        let count = usize::try_from(self.read_u32()?)
            .map_err(|_| RecordingError::invalid(format!("{name} count is too large")))?;
        if count > maximum {
            return Err(RecordingError::invalid(format!(
                "{name} count {count} exceeds maximum {maximum}"
            )));
        }
        Ok(count)
    }

    fn read_length(&mut self, name: &str) -> Result<usize, RecordingError> {
        usize::try_from(self.read_u64()?)
            .map_err(|_| RecordingError::invalid(format!("{name} count is too large")))
    }

    fn read_string(&mut self) -> Result<String, RecordingError> {
        let length = self.read_count(MAX_STRING_BYTES, "string byte")?;
        let bytes = self.take(length)?;
        let value = std::str::from_utf8(bytes)
            .map_err(|_| RecordingError::invalid("string is not valid UTF-8"))?;
        Ok(value.to_owned())
    }

    fn require_values(
        &self,
        count: usize,
        bytes_per_value: usize,
        name: &str,
    ) -> Result<(), RecordingError> {
        let required = count
            .checked_mul(bytes_per_value)
            .ok_or_else(|| RecordingError::invalid(format!("{name} byte count overflowed")))?;
        if required > self.payload.len().saturating_sub(self.offset) {
            return Err(RecordingError::invalid(format!(
                "{name} exceed the record payload"
            )));
        }
        Ok(())
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], RecordingError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| RecordingError::invalid("record offset overflowed"))?;
        let bytes = self
            .payload
            .get(self.offset..end)
            .ok_or_else(|| RecordingError::invalid("record payload is truncated"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn finish(self) -> Result<(), RecordingError> {
        if self.offset == self.payload.len() {
            Ok(())
        } else {
            Err(RecordingError::invalid(format!(
                "record has {} trailing bytes",
                self.payload.len() - self.offset
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn descriptors() -> Vec<DeviceDescriptor> {
        vec![
            DeviceDescriptor {
                id: DeviceId::new("logic-1"),
                name: "Logic".to_owned(),
                kind: InstrumentKind::LogicAnalyzer,
            },
            DeviceDescriptor {
                id: DeviceId::new("scope-1"),
                name: "Scope".to_owned(),
                kind: InstrumentKind::Oscilloscope,
            },
        ]
    }

    fn logic_event() -> CaptureEvent {
        CaptureEvent::Frame(CaptureFrame {
            device_id: DeviceId::new("logic-1"),
            sequence: 7,
            captured_at: Duration::new(2, 345),
            payload: CaptureBatch::Logic(LogicBatch {
                sample_rate_hz: 1_000_000,
                channel_count: 16,
                samples: vec![0x0000, 0x0001, 0xffff],
            }),
        })
    }

    fn scope_event() -> CaptureEvent {
        CaptureEvent::Frame(CaptureFrame {
            device_id: DeviceId::new("scope-1"),
            sequence: 8,
            captured_at: Duration::new(2, 678),
            payload: CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000,
                channels: vec![vec![0.25, -0.5], vec![1.0, -1.0]],
            }),
        })
    }

    #[test]
    fn round_trips_complete_logic_and_scope_capture() {
        let descriptors = descriptors();
        let mut metadata = BTreeMap::new();
        metadata.insert("capture.command".to_owned(), "test".to_owned());
        let events = vec![
            logic_event(),
            scope_event(),
            CaptureEvent::SourceStopped {
                device_id: DeviceId::new("logic-1"),
            },
        ];
        let mut bytes = Vec::new();
        {
            let mut writer =
                RecordingWriter::new(&mut bytes, &descriptors, metadata.clone()).unwrap();
            for event in &events {
                writer.write_event(event).unwrap();
            }
            writer.finish(CompletionStatus::Complete).unwrap();
        }

        let mut reader = RecordingReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(reader.header().devices, descriptors);
        assert_eq!(reader.header().metadata, metadata);
        let mut decoded = Vec::new();
        while let Some(event) = reader.read_event().unwrap() {
            decoded.push(event);
        }
        assert_eq!(decoded, events);
        assert_eq!(reader.completion_status(), Some(CompletionStatus::Complete));
    }

    #[test]
    fn reports_capture_without_footer_as_incomplete() {
        let mut bytes = Vec::new();
        {
            let mut writer =
                RecordingWriter::new(&mut bytes, &descriptors(), BTreeMap::new()).unwrap();
            writer.write_event(&logic_event()).unwrap();
        }
        let mut reader = RecordingReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(reader.read_event().unwrap(), Some(logic_event()));
        assert!(matches!(
            reader.read_event(),
            Err(RecordingError::Incomplete)
        ));
    }

    #[test]
    fn rejects_corrupted_record_checksum() {
        let mut bytes = Vec::new();
        RecordingWriter::new(&mut bytes, &descriptors(), BTreeMap::new()).unwrap();
        bytes[FILE_HEADER_BYTES + RECORD_HEADER_BYTES] ^= 0x80;
        let result = RecordingReader::new(Cursor::new(bytes));
        assert!(matches!(result, Err(RecordingError::Invalid(_))));
    }

    #[test]
    fn rejects_scope_channels_with_different_lengths() {
        let event = CaptureEvent::Frame(CaptureFrame {
            device_id: DeviceId::new("scope-1"),
            sequence: 0,
            captured_at: Duration::ZERO,
            payload: CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000,
                channels: vec![vec![0.0], vec![0.0, 1.0]],
            }),
        });
        let mut bytes = Vec::new();
        let mut writer = RecordingWriter::new(&mut bytes, &descriptors(), BTreeMap::new()).unwrap();
        assert!(matches!(
            writer.write_event(&event),
            Err(RecordingError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_data_after_footer() {
        let mut bytes = Vec::new();
        {
            let mut writer =
                RecordingWriter::new(&mut bytes, &descriptors(), BTreeMap::new()).unwrap();
            writer.finish(CompletionStatus::Complete).unwrap();
        }
        bytes.push(0xff);
        let mut reader = RecordingReader::new(Cursor::new(bytes)).unwrap();
        assert!(matches!(
            reader.read_event(),
            Err(RecordingError::Invalid(_))
        ));
    }
}
