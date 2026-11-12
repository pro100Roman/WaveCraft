//! Versioned, streaming recordings for target system traces.

use crc32fast::hash;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::{self, Read, Write};
use wavecraft_trace::TraceEvent;

const FILE_MAGIC: [u8; 8] = *b"WCTRACE\n";
const FILE_HEADER_BYTES: usize = 12;
const RECORD_HEADER_BYTES: usize = 16;
const RECORD_SESSION: u8 = 1;
const RECORD_RAW_CHUNK: u8 = 2;
const RECORD_EVENT: u8 = 3;
const RECORD_DISCONTINUITY: u8 = 4;
const RECORD_FOOTER: u8 = 5;
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

pub const FORMAT_VERSION: u16 = 1;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TraceRecordingHeader {
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceCompletionStatus {
    Complete,
    Cancelled,
    Failed,
}

impl TraceCompletionStatus {
    fn encode(self) -> u8 {
        match self {
            Self::Complete => 1,
            Self::Cancelled => 2,
            Self::Failed => 3,
        }
    }

    fn decode(value: u8) -> Result<Self, TraceRecordingError> {
        match value {
            1 => Ok(Self::Complete),
            2 => Ok(Self::Cancelled),
            3 => Ok(Self::Failed),
            _ => Err(TraceRecordingError::invalid(format!(
                "unknown trace completion status {value}"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TraceRecord {
    RawChunk { stream_id: u32, bytes: Vec<u8> },
    Event(TraceEvent),
    Discontinuity { stream_id: u32, reason: String },
}

#[derive(Debug)]
pub enum TraceRecordingError {
    Io(io::Error),
    Invalid(String),
    Incomplete,
}

impl TraceRecordingError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl Display for TraceRecordingError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "trace recording I/O failed: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid trace recording: {message}"),
            Self::Incomplete => formatter.write_str("trace recording ended without a valid footer"),
        }
    }
}

impl Error for TraceRecordingError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Invalid(_) | Self::Incomplete => None,
        }
    }
}

impl From<io::Error> for TraceRecordingError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct TraceRecordingWriter<W: Write> {
    output: W,
    raw_chunk_count: u64,
    event_count: u64,
    discontinuity_count: u64,
    finished: bool,
}

impl<W: Write> TraceRecordingWriter<W> {
    pub fn new(
        mut output: W,
        metadata: BTreeMap<String, String>,
    ) -> Result<Self, TraceRecordingError> {
        output.write_all(&FILE_MAGIC)?;
        output.write_all(&FORMAT_VERSION.to_le_bytes())?;
        output.write_all(&0_u16.to_le_bytes())?;
        let payload = serde_json::to_vec(&metadata).map_err(|error| {
            TraceRecordingError::invalid(format!("session metadata cannot be encoded: {error}"))
        })?;
        write_framed_record(&mut output, RECORD_SESSION, &payload)?;
        Ok(Self {
            output,
            raw_chunk_count: 0,
            event_count: 0,
            discontinuity_count: 0,
            finished: false,
        })
    }

    pub fn write_raw_chunk(
        &mut self,
        stream_id: u32,
        bytes: &[u8],
    ) -> Result<(), TraceRecordingError> {
        self.ensure_open()?;
        ensure_stream_payload_size(bytes.len(), "raw chunk")?;
        let mut payload = Vec::with_capacity(4_usize.saturating_add(bytes.len()));
        payload.extend_from_slice(&stream_id.to_le_bytes());
        payload.extend_from_slice(bytes);
        write_framed_record(&mut self.output, RECORD_RAW_CHUNK, &payload)?;
        self.raw_chunk_count = checked_increment(self.raw_chunk_count, "raw chunk")?;
        Ok(())
    }

    pub fn write_event(&mut self, event: &TraceEvent) -> Result<(), TraceRecordingError> {
        self.ensure_open()?;
        let payload = serde_json::to_vec(event).map_err(|error| {
            TraceRecordingError::invalid(format!("trace event cannot be encoded: {error}"))
        })?;
        write_framed_record(&mut self.output, RECORD_EVENT, &payload)?;
        self.event_count = checked_increment(self.event_count, "event")?;
        Ok(())
    }

    pub fn write_discontinuity(
        &mut self,
        stream_id: u32,
        reason: &str,
    ) -> Result<(), TraceRecordingError> {
        self.ensure_open()?;
        ensure_stream_payload_size(reason.len(), "discontinuity")?;
        let mut payload = Vec::with_capacity(4_usize.saturating_add(reason.len()));
        payload.extend_from_slice(&stream_id.to_le_bytes());
        payload.extend_from_slice(reason.as_bytes());
        write_framed_record(&mut self.output, RECORD_DISCONTINUITY, &payload)?;
        self.discontinuity_count = checked_increment(self.discontinuity_count, "discontinuity")?;
        Ok(())
    }

    pub fn finish(&mut self, status: TraceCompletionStatus) -> Result<(), TraceRecordingError> {
        self.ensure_open()?;
        let mut payload = Vec::with_capacity(25);
        payload.push(status.encode());
        payload.extend_from_slice(&self.raw_chunk_count.to_le_bytes());
        payload.extend_from_slice(&self.event_count.to_le_bytes());
        payload.extend_from_slice(&self.discontinuity_count.to_le_bytes());
        write_framed_record(&mut self.output, RECORD_FOOTER, &payload)?;
        self.output.flush()?;
        self.finished = true;
        Ok(())
    }

    pub fn into_inner(self) -> W {
        self.output
    }

    fn ensure_open(&self) -> Result<(), TraceRecordingError> {
        if self.finished {
            Err(TraceRecordingError::invalid(
                "cannot append a record after the trace recording footer",
            ))
        } else {
            Ok(())
        }
    }
}

pub struct TraceRecordingReader<R: Read> {
    input: R,
    header: TraceRecordingHeader,
    raw_chunk_count: u64,
    event_count: u64,
    discontinuity_count: u64,
    completion_status: Option<TraceCompletionStatus>,
}

impl<R: Read> TraceRecordingReader<R> {
    pub fn new(mut input: R) -> Result<Self, TraceRecordingError> {
        let mut file_header = [0_u8; FILE_HEADER_BYTES];
        read_exact_or_incomplete(&mut input, &mut file_header)?;
        if file_header[..FILE_MAGIC.len()] != FILE_MAGIC {
            return Err(TraceRecordingError::invalid(
                "file magic does not match WaveCraft System Trace",
            ));
        }
        let version = u16::from_le_bytes([file_header[8], file_header[9]]);
        if version != FORMAT_VERSION {
            return Err(TraceRecordingError::invalid(format!(
                "trace format version {version} is unsupported; expected {FORMAT_VERSION}"
            )));
        }
        if file_header[10..12] != [0, 0] {
            return Err(TraceRecordingError::invalid(
                "file header contains unsupported flags",
            ));
        }
        let session = read_framed_record(&mut input)?;
        if session.kind != RECORD_SESSION {
            return Err(TraceRecordingError::invalid(
                "the first trace record is not session metadata",
            ));
        }
        let metadata = serde_json::from_slice(&session.payload).map_err(|error| {
            TraceRecordingError::invalid(format!("session metadata cannot be decoded: {error}"))
        })?;
        Ok(Self {
            input,
            header: TraceRecordingHeader { metadata },
            raw_chunk_count: 0,
            event_count: 0,
            discontinuity_count: 0,
            completion_status: None,
        })
    }

    pub fn header(&self) -> &TraceRecordingHeader {
        &self.header
    }

    pub fn completion_status(&self) -> Option<TraceCompletionStatus> {
        self.completion_status
    }

    pub fn read_record(&mut self) -> Result<Option<TraceRecord>, TraceRecordingError> {
        if self.completion_status.is_some() {
            return Ok(None);
        }
        let record = read_framed_record(&mut self.input)?;
        match record.kind {
            RECORD_RAW_CHUNK => {
                let (stream_id, bytes) = decode_stream_payload(record.payload, "raw chunk")?;
                self.raw_chunk_count = checked_increment(self.raw_chunk_count, "raw chunk")?;
                Ok(Some(TraceRecord::RawChunk { stream_id, bytes }))
            }
            RECORD_EVENT => {
                let event = serde_json::from_slice(&record.payload).map_err(|error| {
                    TraceRecordingError::invalid(format!(
                        "normalized trace event cannot be decoded: {error}"
                    ))
                })?;
                self.event_count = checked_increment(self.event_count, "event")?;
                Ok(Some(TraceRecord::Event(event)))
            }
            RECORD_DISCONTINUITY => {
                let (stream_id, reason) = decode_stream_payload(record.payload, "discontinuity")?;
                let reason = String::from_utf8(reason).map_err(|_| {
                    TraceRecordingError::invalid("discontinuity reason is not UTF-8")
                })?;
                self.discontinuity_count =
                    checked_increment(self.discontinuity_count, "discontinuity")?;
                Ok(Some(TraceRecord::Discontinuity { stream_id, reason }))
            }
            RECORD_FOOTER => {
                let footer = decode_footer(&record.payload)?;
                if footer.raw_chunk_count != self.raw_chunk_count
                    || footer.event_count != self.event_count
                    || footer.discontinuity_count != self.discontinuity_count
                {
                    return Err(TraceRecordingError::invalid(format!(
                        "footer counts ({}, {}, {}) do not match records read ({}, {}, {})",
                        footer.raw_chunk_count,
                        footer.event_count,
                        footer.discontinuity_count,
                        self.raw_chunk_count,
                        self.event_count,
                        self.discontinuity_count
                    )));
                }
                ensure_end_of_file(&mut self.input)?;
                self.completion_status = Some(footer.status);
                Ok(None)
            }
            RECORD_SESSION => Err(TraceRecordingError::invalid(
                "trace recording contains duplicate session metadata",
            )),
            kind => Err(TraceRecordingError::invalid(format!(
                "unknown trace record kind {kind}"
            ))),
        }
    }
}

struct FramedRecord {
    kind: u8,
    payload: Vec<u8>,
}

struct Footer {
    status: TraceCompletionStatus,
    raw_chunk_count: u64,
    event_count: u64,
    discontinuity_count: u64,
}

fn write_framed_record(
    output: &mut impl Write,
    kind: u8,
    payload: &[u8],
) -> Result<(), TraceRecordingError> {
    if payload.len() > MAX_RECORD_BYTES {
        return Err(TraceRecordingError::invalid(format!(
            "record payload is {} bytes; maximum is {MAX_RECORD_BYTES}",
            payload.len()
        )));
    }
    let payload_len = u64::try_from(payload.len())
        .map_err(|_| TraceRecordingError::invalid("record length does not fit in u64"))?;
    let mut header = [0_u8; RECORD_HEADER_BYTES];
    header[0] = kind;
    header[4..12].copy_from_slice(&payload_len.to_le_bytes());
    header[12..16].copy_from_slice(&hash(payload).to_le_bytes());
    output.write_all(&header)?;
    output.write_all(payload)?;
    Ok(())
}

fn read_framed_record(input: &mut impl Read) -> Result<FramedRecord, TraceRecordingError> {
    let mut header = [0_u8; RECORD_HEADER_BYTES];
    read_exact_or_incomplete(input, &mut header)?;
    if header[1..4] != [0, 0, 0] {
        return Err(TraceRecordingError::invalid(
            "trace record contains unsupported flags",
        ));
    }
    let payload_len = u64::from_le_bytes([
        header[4], header[5], header[6], header[7], header[8], header[9], header[10], header[11],
    ]);
    let payload_len = usize::try_from(payload_len)
        .map_err(|_| TraceRecordingError::invalid("record length does not fit in memory"))?;
    if payload_len > MAX_RECORD_BYTES {
        return Err(TraceRecordingError::invalid(format!(
            "record payload is {payload_len} bytes; maximum is {MAX_RECORD_BYTES}"
        )));
    }
    let expected_crc = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    let mut payload = vec![0_u8; payload_len];
    read_exact_or_incomplete(input, &mut payload)?;
    if hash(&payload) != expected_crc {
        return Err(TraceRecordingError::invalid(
            "trace record checksum does not match its payload",
        ));
    }
    Ok(FramedRecord {
        kind: header[0],
        payload,
    })
}

fn decode_stream_payload(
    payload: Vec<u8>,
    record_name: &str,
) -> Result<(u32, Vec<u8>), TraceRecordingError> {
    if payload.len() < 4 {
        return Err(TraceRecordingError::invalid(format!(
            "{record_name} record has no stream identifier"
        )));
    }
    let stream_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    Ok((stream_id, payload[4..].to_vec()))
}

fn decode_footer(payload: &[u8]) -> Result<Footer, TraceRecordingError> {
    if payload.len() != 25 {
        return Err(TraceRecordingError::invalid(
            "trace recording footer has the wrong size",
        ));
    }
    Ok(Footer {
        status: TraceCompletionStatus::decode(payload[0])?,
        raw_chunk_count: read_u64(&payload[1..9]),
        event_count: read_u64(&payload[9..17]),
        discontinuity_count: read_u64(&payload[17..25]),
    })
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn checked_increment(value: u64, name: &str) -> Result<u64, TraceRecordingError> {
    value
        .checked_add(1)
        .ok_or_else(|| TraceRecordingError::invalid(format!("trace {name} count overflowed")))
}

fn ensure_stream_payload_size(size: usize, name: &str) -> Result<(), TraceRecordingError> {
    if size > MAX_RECORD_BYTES.saturating_sub(4) {
        Err(TraceRecordingError::invalid(format!(
            "{name} payload is {size} bytes; maximum is {}",
            MAX_RECORD_BYTES - 4
        )))
    } else {
        Ok(())
    }
}

fn read_exact_or_incomplete(
    input: &mut impl Read,
    buffer: &mut [u8],
) -> Result<(), TraceRecordingError> {
    match input.read_exact(buffer) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(TraceRecordingError::Incomplete)
        }
        Err(error) => Err(TraceRecordingError::Io(error)),
    }
}

fn ensure_end_of_file(input: &mut impl Read) -> Result<(), TraceRecordingError> {
    let mut byte = [0_u8; 1];
    match input.read(&mut byte) {
        Ok(0) => Ok(()),
        Ok(_) => Err(TraceRecordingError::invalid(
            "trace recording contains data after its footer",
        )),
        Err(error) => Err(TraceRecordingError::Io(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use wavecraft_trace::TraceEventKind;

    fn trace_event() -> TraceEvent {
        TraceEvent {
            sequence: 4,
            core_id: 1,
            timestamp_ticks: 123,
            kind: TraceEventKind::TaskStarted { task_id: 9 },
        }
    }

    fn complete_recording() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut metadata = BTreeMap::new();
            metadata.insert("probe.provider".to_owned(), "st-link".to_owned());
            let mut writer = TraceRecordingWriter::new(&mut bytes, metadata)
                .expect("recording writer is created");
            writer
                .write_raw_chunk(7, &[0, 0xff, 3])
                .expect("raw chunk is written");
            writer
                .write_event(&trace_event())
                .expect("event is written");
            writer
                .write_discontinuity(7, "target reset")
                .expect("discontinuity is written");
            writer
                .finish(TraceCompletionStatus::Complete)
                .expect("footer is written");
        }
        bytes
    }

    #[test]
    fn round_trips_metadata_raw_chunks_events_and_discontinuities() {
        let bytes = complete_recording();
        let mut reader = TraceRecordingReader::new(Cursor::new(bytes)).expect("recording opens");

        assert_eq!(
            reader.header().metadata.get("probe.provider"),
            Some(&"st-link".to_owned())
        );
        assert_eq!(
            reader.read_record().expect("raw record reads"),
            Some(TraceRecord::RawChunk {
                stream_id: 7,
                bytes: vec![0, 0xff, 3],
            })
        );
        assert_eq!(
            reader.read_record().expect("event record reads"),
            Some(TraceRecord::Event(trace_event()))
        );
        assert_eq!(
            reader.read_record().expect("discontinuity reads"),
            Some(TraceRecord::Discontinuity {
                stream_id: 7,
                reason: "target reset".to_owned(),
            })
        );
        assert_eq!(reader.read_record().expect("footer reads"), None);
        assert_eq!(
            reader.completion_status(),
            Some(TraceCompletionStatus::Complete)
        );
    }

    #[test]
    fn rejects_a_corrupted_record_payload() {
        let mut bytes = complete_recording();
        let raw_record_offset = {
            let mut cursor = Cursor::new(bytes.as_slice());
            cursor.set_position(FILE_HEADER_BYTES as u64);
            read_framed_record(&mut cursor).expect("session record reads");
            cursor.position() as usize
        };
        let raw_payload_offset = raw_record_offset + RECORD_HEADER_BYTES + 4;
        bytes[raw_payload_offset] ^= 0x80;

        let mut reader = TraceRecordingReader::new(Cursor::new(bytes)).expect("session is intact");
        let error = reader
            .read_record()
            .expect_err("corrupted raw record is rejected");
        assert!(error.to_string().contains("checksum"));
    }

    #[test]
    fn reports_a_stream_without_a_footer_as_incomplete() {
        let mut bytes = complete_recording();
        bytes.truncate(bytes.len() - (RECORD_HEADER_BYTES + 25));

        let mut reader = TraceRecordingReader::new(Cursor::new(bytes)).expect("session opens");
        loop {
            match reader.read_record() {
                Ok(Some(_)) => {}
                Ok(None) => panic!("recording without a footer was accepted"),
                Err(error) => {
                    assert!(matches!(error, TraceRecordingError::Incomplete));
                    break;
                }
            }
        }
    }

    #[test]
    fn refuses_records_after_the_footer() {
        let mut writer =
            TraceRecordingWriter::new(Vec::new(), BTreeMap::new()).expect("writer is created");
        writer
            .finish(TraceCompletionStatus::Cancelled)
            .expect("footer is written");

        let error = writer
            .write_raw_chunk(0, &[1])
            .expect_err("append is rejected");
        assert!(error.to_string().contains("after"));
    }
}
