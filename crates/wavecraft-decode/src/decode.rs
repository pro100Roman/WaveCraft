use crate::{
    ChecksumAlgorithm, ExpectedValue, FieldKind, IntegerType, MessageSchema, ProtocolSchema,
    StringEncoding, TransportKind,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleRange {
    pub start: u64,
    pub end: u64,
}

impl SampleRange {
    pub fn new(start: u64, end: u64) -> Result<Self, DecodeError> {
        if start >= end {
            return Err(DecodeError::new(
                "sample range end must be greater than its start",
            ));
        }
        Ok(Self { start, end })
    }

    fn covering(first: Self, last: Self) -> Result<Self, DecodeError> {
        if first.start >= last.end {
            return Err(DecodeError::new("decoded byte ranges are not ordered"));
        }
        Ok(Self {
            start: first.start,
            end: last.end,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ByteStatus {
    pub parity_error: bool,
    pub framing_error: bool,
    /// Number of source bits whose demodulator confidence was below the
    /// configured acceptance threshold.
    pub low_confidence_bits: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimedByte {
    pub value: u8,
    pub range: SampleRange,
    pub status: ByteStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketCompleteness {
    Complete,
    Incomplete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportValue {
    Boolean(bool),
    Integer(i64),
    String(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportPacket {
    pub transport: TransportKind,
    pub streams: BTreeMap<String, Vec<TimedByte>>,
    pub attributes: BTreeMap<String, TransportValue>,
    pub completeness: PacketCompleteness,
}

impl TransportPacket {
    pub fn single_stream(
        transport: TransportKind,
        stream: impl Into<String>,
        bytes: Vec<TimedByte>,
        completeness: PacketCompleteness,
    ) -> Self {
        Self {
            transport,
            streams: BTreeMap::from([(stream.into(), bytes)]),
            attributes: BTreeMap::new(),
            completeness,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeDiagnostic {
    pub code: String,
    pub message: String,
    pub severity: Severity,
    pub range: Option<SampleRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodedValue {
    Integer(i64),
    Bytes(Vec<u8>),
    String(String),
    Boolean(bool),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedField {
    pub name: String,
    pub value: DecodedValue,
    pub display: Option<String>,
    pub range: SampleRange,
    pub children: Vec<DecodedField>,
    pub hidden: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedMessage {
    pub protocol_id: String,
    pub message_id: String,
    pub range: SampleRange,
    pub summary: String,
    pub severity: Severity,
    pub fields: Vec<DecodedField>,
    pub diagnostics: Vec<DecodeDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeError {
    message: String,
}

impl DecodeError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for DecodeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl Error for DecodeError {}

#[derive(Clone, Debug)]
pub struct CustomProtocolDecoder {
    schema: ProtocolSchema,
}

impl CustomProtocolDecoder {
    pub fn new(schema: ProtocolSchema) -> Self {
        Self { schema }
    }

    pub fn schema(&self) -> &ProtocolSchema {
        &self.schema
    }

    pub fn decode_framed_bytes(
        &self,
        frame: crate::FramedBytes,
    ) -> Result<Option<DecodedMessage>, DecodeError> {
        self.decode_packet(&TransportPacket::single_stream(
            self.schema.input.transport,
            self.schema.input.stream.clone(),
            frame.bytes,
            frame.completeness,
        ))
    }

    pub fn decode_packet(
        &self,
        packet: &TransportPacket,
    ) -> Result<Option<DecodedMessage>, DecodeError> {
        if packet.transport != self.schema.input.transport {
            return Err(DecodeError::new(format!(
                "protocol expects {:?} input but received {:?}",
                self.schema.input.transport, packet.transport
            )));
        }
        let bytes = packet
            .streams
            .get(&self.schema.input.stream)
            .ok_or_else(|| {
                DecodeError::new(format!(
                    "transport packet does not contain stream {:?}",
                    self.schema.input.stream
                ))
            })?;
        if bytes.is_empty() {
            return Ok(None);
        }
        validate_timed_bytes(bytes)?;
        let matching = self
            .schema
            .messages
            .iter()
            .filter(|message| {
                !message.matches.is_empty()
                    && message.matches.iter().all(|condition| {
                        bytes.get(condition.offset).is_some_and(|byte| {
                            byte.value & condition.mask == condition.value & condition.mask
                        })
                    })
            })
            .collect::<Vec<_>>();
        let message = match matching.as_slice() {
            [] => self
                .schema
                .messages
                .iter()
                .find(|message| message.is_default),
            [message] => Some(*message),
            _ => {
                return Err(DecodeError::new(format!(
                    "transport packet ambiguously matches {} message definitions",
                    matching.len()
                )));
            }
        };
        let Some(message) = message else {
            return Ok(None);
        };
        decode_message(
            &self.schema.protocol.id,
            message,
            bytes,
            packet.completeness,
        )
        .map(Some)
    }
}

pub(crate) fn validate_timed_bytes(bytes: &[TimedByte]) -> Result<(), DecodeError> {
    let mut previous_end = None;
    for byte in bytes {
        if byte.range.start >= byte.range.end {
            return Err(DecodeError::new("transport byte has an empty sample range"));
        }
        if previous_end.is_some_and(|end| byte.range.start < end) {
            return Err(DecodeError::new(
                "transport byte sample ranges overlap or are out of order",
            ));
        }
        previous_end = Some(byte.range.end);
    }
    Ok(())
}

fn decode_message(
    protocol_id: &str,
    message: &MessageSchema,
    bytes: &[TimedByte],
    completeness: PacketCompleteness,
) -> Result<DecodedMessage, DecodeError> {
    let required_bytes = message
        .fields
        .iter()
        .try_fold(0_usize, |total, field| {
            total.checked_add(field.kind.byte_width())
        })
        .ok_or_else(|| DecodeError::new("decoded field layout overflows"))?;
    if bytes.len() < required_bytes {
        let qualifier = if completeness == PacketCompleteness::Incomplete {
            "incomplete"
        } else {
            "complete"
        };
        return Err(DecodeError::new(format!(
            "{qualifier} packet has {} bytes but message {:?} requires {required_bytes}",
            bytes.len(),
            message.id
        )));
    }

    let mut fields = Vec::with_capacity(message.fields.len());
    let mut offsets = BTreeMap::new();
    let mut cursor = 0_usize;
    let mut diagnostics = Vec::new();
    for field_schema in &message.fields {
        let start = cursor;
        let end = start + field_schema.kind.byte_width();
        let source = &bytes[start..end];
        let range = SampleRange::covering(source[0].range, source[source.len() - 1].range)?;
        let raw = source.iter().map(|byte| byte.value).collect::<Vec<_>>();
        let (raw_value, mut field_diagnostics) = decode_value(&field_schema.kind, &raw, range)?;
        diagnostics.append(&mut field_diagnostics);
        for byte in source {
            if byte.status.parity_error {
                diagnostics.push(diagnostic(
                    "transport.parity_error",
                    format!("field {:?} contains a UART parity error", field_schema.name),
                    Severity::Error,
                    Some(byte.range),
                ));
            }
            if byte.status.framing_error {
                diagnostics.push(diagnostic(
                    "transport.framing_error",
                    format!(
                        "field {:?} contains a UART framing error",
                        field_schema.name
                    ),
                    Severity::Error,
                    Some(byte.range),
                ));
            }
        }
        let children = decode_bits(field_schema, &raw_value, range);
        let value = apply_mask(field_schema, raw_value);
        validate_expected(
            field_schema.expected.as_ref(),
            &value,
            range,
            &mut diagnostics,
        );

        let mut display = match &value {
            DecodedValue::Integer(value) => field_schema
                .display
                .fixed_point_q
                .map(|q| format_fixed_point(*value, q))
                .map(|value| match &field_schema.display.unit {
                    Some(unit) => format!("{value} {unit}"),
                    None => value,
                })
                .or_else(|| {
                    field_schema
                        .display
                        .unit
                        .as_ref()
                        .map(|unit| format!("{value} {unit}"))
                }),
            _ => field_schema.display.unit.clone(),
        };
        if let DecodedValue::Integer(value) = value {
            if !field_schema.enum_values.is_empty() {
                match field_schema.enum_values.get(&value) {
                    Some(label) => display = Some(label.clone()),
                    None => diagnostics.push(diagnostic(
                        "field.unknown_enum_value",
                        format!(
                            "field {:?} contains unknown enum value {value}",
                            field_schema.name
                        ),
                        Severity::Warning,
                        Some(range),
                    )),
                }
            }
        }
        offsets.insert(field_schema.name.as_str(), (start, end));
        fields.push(DecodedField {
            name: field_schema.name.clone(),
            value,
            display,
            range,
            children,
            hidden: field_schema.hidden,
        });
        cursor = end;
    }

    validate_checksums(message, bytes, &fields, &offsets, &mut diagnostics)?;
    let range = SampleRange::covering(bytes[0].range, bytes[required_bytes - 1].range)?;
    let summary = render_summary(message, &fields);
    let severity = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.severity)
        .max()
        .unwrap_or(Severity::Info);
    Ok(DecodedMessage {
        protocol_id: protocol_id.to_owned(),
        message_id: message.id.clone(),
        range,
        summary,
        severity,
        fields,
        diagnostics,
    })
}

fn apply_mask(field_schema: &crate::FieldSchema, value: DecodedValue) -> DecodedValue {
    match (field_schema.mask, value) {
        (Some(mask), DecodedValue::Integer(value)) => DecodedValue::Integer(value & mask),
        (_, value) => value,
    }
}

fn format_fixed_point(value: i64, q: u8) -> String {
    let scaled = value as f64 / (1_u64 << q) as f64;
    let mut formatted = format!("{scaled:.6}");
    while formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}

fn decode_value(
    kind: &FieldKind,
    raw: &[u8],
    range: SampleRange,
) -> Result<(DecodedValue, Vec<DecodeDiagnostic>), DecodeError> {
    let mut diagnostics = Vec::new();
    let value = match kind {
        FieldKind::Integer(kind) => DecodedValue::Integer(decode_integer(*kind, raw)?),
        FieldKind::Bytes { .. } => DecodedValue::Bytes(raw.to_vec()),
        FieldKind::String { encoding, .. } => match encoding {
            StringEncoding::Utf8 => match std::str::from_utf8(raw) {
                Ok(value) => DecodedValue::String(value.to_owned()),
                Err(_) => {
                    diagnostics.push(diagnostic(
                        "field.invalid_utf8",
                        "string field is not valid UTF-8",
                        Severity::Error,
                        Some(range),
                    ));
                    DecodedValue::String(String::from_utf8_lossy(raw).into_owned())
                }
            },
            StringEncoding::Ascii => {
                if !raw.is_ascii() {
                    diagnostics.push(diagnostic(
                        "field.invalid_ascii",
                        "string field contains a non-ASCII byte",
                        Severity::Error,
                        Some(range),
                    ));
                }
                DecodedValue::String(
                    raw.iter()
                        .map(|byte| char::from(if byte.is_ascii() { *byte } else { b'?' }))
                        .collect(),
                )
            }
        },
    };
    Ok((value, diagnostics))
}

fn decode_integer(kind: IntegerType, raw: &[u8]) -> Result<i64, DecodeError> {
    let value = match kind {
        IntegerType::U8 => i64::from(raw[0]),
        IntegerType::S8 => i64::from(raw[0] as i8),
        IntegerType::U16Le => i64::from(u16::from_le_bytes([raw[0], raw[1]])),
        IntegerType::U16Be => i64::from(u16::from_be_bytes([raw[0], raw[1]])),
        IntegerType::S16Le => i64::from(i16::from_le_bytes([raw[0], raw[1]])),
        IntegerType::S16Be => i64::from(i16::from_be_bytes([raw[0], raw[1]])),
        IntegerType::U32Le => i64::from(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])),
        IntegerType::U32Be => i64::from(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]])),
        IntegerType::S32Le => i64::from(i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])),
        IntegerType::S32Be => i64::from(i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]])),
    };
    Ok(value)
}

fn validate_expected(
    expected: Option<&ExpectedValue>,
    actual: &DecodedValue,
    range: SampleRange,
    diagnostics: &mut Vec<DecodeDiagnostic>,
) {
    let matches = match (expected, actual) {
        (None, _) => return,
        (Some(ExpectedValue::Integer(expected)), DecodedValue::Integer(actual)) => {
            expected == actual
        }
        (Some(ExpectedValue::Bytes(expected)), DecodedValue::Bytes(actual)) => expected == actual,
        (Some(ExpectedValue::String(expected)), DecodedValue::String(actual)) => expected == actual,
        _ => false,
    };
    if !matches {
        diagnostics.push(diagnostic(
            "field.unexpected_value",
            "field value does not match its expected constant",
            Severity::Error,
            Some(range),
        ));
    }
}

fn decode_bits(
    field_schema: &crate::FieldSchema,
    value: &DecodedValue,
    range: SampleRange,
) -> Vec<DecodedField> {
    let DecodedValue::Integer(value) = value else {
        return Vec::new();
    };
    field_schema
        .bits
        .iter()
        .map(|(name, definition)| DecodedField {
            name: name.clone(),
            value: DecodedValue::Boolean(value & (1_i64 << definition.bit) != 0),
            display: None,
            range,
            children: Vec::new(),
            hidden: false,
        })
        .collect()
}

fn validate_checksums(
    message: &MessageSchema,
    bytes: &[TimedByte],
    fields: &[DecodedField],
    offsets: &BTreeMap<&str, (usize, usize)>,
    diagnostics: &mut Vec<DecodeDiagnostic>,
) -> Result<(), DecodeError> {
    for checksum in &message.checksums {
        let (_, through_end) = offsets
            .get(checksum.through_field.as_str())
            .ok_or_else(|| DecodeError::new("validated checksum field reference disappeared"))?;
        let (expected_start, _) = offsets
            .get(checksum.expected_field.as_str())
            .ok_or_else(|| DecodeError::new("validated checksum field reference disappeared"))?;
        let expected = fields
            .iter()
            .find(|field| field.name == checksum.expected_field)
            .and_then(|field| match field.value {
                DecodedValue::Integer(value) => u8::try_from(value).ok(),
                _ => None,
            })
            .ok_or_else(|| DecodeError::new("checksum expected field is not a byte"))?;
        let source = bytes[checksum.from..*through_end]
            .iter()
            .map(|byte| byte.value);
        let actual = match checksum.algorithm {
            ChecksumAlgorithm::Crc8Smbus => crc8_smbus(source),
        };
        if actual != expected {
            diagnostics.push(diagnostic(
                "checksum.mismatch",
                format!(
                    "checksum field {:?} is 0x{expected:02X}; calculated 0x{actual:02X}",
                    checksum.expected_field
                ),
                Severity::Error,
                Some(bytes[*expected_start].range),
            ));
        }
    }
    Ok(())
}

fn crc8_smbus(bytes: impl Iterator<Item = u8>) -> u8 {
    let mut crc = 0_u8;
    for byte in bytes {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn render_summary(message: &MessageSchema, fields: &[DecodedField]) -> String {
    let Some(template) = &message.summary else {
        return message.name.clone();
    };
    let fields = fields
        .iter()
        .map(|field| (field.name.as_str(), field))
        .collect::<BTreeMap<_, _>>();
    let mut output = String::with_capacity(template.len());
    let mut remaining = template.as_str();
    while let Some(start) = remaining.find('{') {
        output.push_str(&remaining[..start]);
        let after_start = &remaining[start + 1..];
        // Schema validation rejects unterminated placeholders and unknown
        // fields. If a schema built by hand still contains one, keep the
        // template text instead of panicking while decoding captured data.
        let Some(end) = after_start.find('}') else {
            output.push_str(&remaining[start..]);
            return output;
        };
        let name = &after_start[..end];
        match fields.get(name) {
            Some(field) => output.push_str(&display_value(field)),
            None => {
                output.push('{');
                output.push_str(name);
                output.push('}');
            }
        }
        remaining = &after_start[end + 1..];
    }
    output.push_str(remaining);
    output
}

fn display_value(field: &DecodedField) -> String {
    if let Some(display) = &field.display {
        return display.clone();
    }
    match &field.value {
        DecodedValue::Integer(value) => value.to_string(),
        DecodedValue::Bytes(bytes) => bytes
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" "),
        DecodedValue::String(value) => value.clone(),
        DecodedValue::Boolean(value) => value.to_string(),
    }
}

fn diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    severity: Severity,
    range: Option<SampleRange>,
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
    use crate::load_protocol_schema;

    const SCHEMA: &str = r#"
schema: wavecraft.protocol/v1
protocol:
  id: example.motor
  name: Motor
  version: 1
input:
  transport: uart
  stream: data
framing:
  strategy: fixed_length
  bytes: 6
messages:
  - id: set_speed
    name: Set speed
    match:
      - offset: 0
        value: 0x10
    fields:
      - name: opcode
        type: u8
        enum:
          0x10: set_speed
      - name: motor
        type: u8
      - name: speed_rpm
        type: u16le
        display:
          unit: rpm
      - name: flags
        type: u8
        bits:
          enabled: 0
          reverse: 1
      - name: checksum
        type: u8
    validate:
      - checksum:
          algorithm: crc8_smbus
          from: 0
          through_field: flags
          expected_field: checksum
    summary: "Motor {motor}: {speed_rpm}"
"#;
    fn bytes(values: &[u8]) -> Vec<TimedByte> {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| TimedByte {
                value: *value,
                range: SampleRange {
                    start: (index * 10) as u64,
                    end: (index * 10 + 8) as u64,
                },
                status: ByteStatus::default(),
            })
            .collect()
    }

    fn packet(values: &[u8]) -> TransportPacket {
        TransportPacket {
            transport: TransportKind::Uart,
            streams: BTreeMap::from([("data".to_owned(), bytes(values))]),
            attributes: BTreeMap::new(),
            completeness: PacketCompleteness::Complete,
        }
    }

    fn summary_message(summary: &str) -> MessageSchema {
        MessageSchema {
            id: "test".to_owned(),
            name: "Test message".to_owned(),
            matches: Vec::new(),
            is_default: false,
            fields: Vec::new(),
            checksums: Vec::new(),
            summary: Some(summary.to_owned()),
        }
    }

    fn summary_field(name: &str, value: i64) -> DecodedField {
        DecodedField {
            name: name.to_owned(),
            value: DecodedValue::Integer(value),
            display: None,
            range: SampleRange { start: 0, end: 1 },
            children: Vec::new(),
            hidden: false,
        }
    }

    #[test]
    fn render_summary_substitutes_known_fields() {
        let fields = [summary_field("speed", 2000)];

        assert_eq!(
            render_summary(&summary_message("Speed {speed} rpm"), &fields),
            "Speed 2000 rpm"
        );
    }

    #[test]
    fn render_summary_keeps_text_for_unknown_or_unterminated_placeholders() {
        let fields = [summary_field("speed", 2000)];

        assert_eq!(
            render_summary(&summary_message("{speed} / {missing}"), &fields),
            "2000 / {missing}"
        );
        assert_eq!(
            render_summary(&summary_message("{speed} / {oops"), &fields),
            "2000 / {oops"
        );
    }

    #[test]
    fn decodes_fields_summary_bits_and_sample_ranges() {
        let schema = load_protocol_schema(SCHEMA).unwrap();
        let decoder = CustomProtocolDecoder::new(schema);
        let prefix = [0x10, 0x02, 0xD0, 0x07, 0x03];
        let checksum = crc8_smbus(prefix.into_iter());
        let message = decoder
            .decode_packet(&packet(&[0x10, 0x02, 0xD0, 0x07, 0x03, checksum]))
            .unwrap()
            .unwrap();

        assert_eq!(message.message_id, "set_speed");
        assert_eq!(message.summary, "Motor 2: 2000 rpm");
        assert_eq!(message.range, SampleRange { start: 0, end: 58 });
        assert_eq!(message.severity, Severity::Info);
        assert_eq!(message.fields[3].children.len(), 2);
        assert_eq!(
            message.fields[3].children[0].value,
            DecodedValue::Boolean(true)
        );
    }

    #[test]
    fn reports_checksum_mismatch_without_dropping_message() {
        let decoder = CustomProtocolDecoder::new(load_protocol_schema(SCHEMA).unwrap());
        let message = decoder
            .decode_packet(&packet(&[0x10, 0x02, 0xD0, 0x07, 0x03, 0x00]))
            .unwrap()
            .unwrap();
        assert_eq!(message.severity, Severity::Error);
        assert_eq!(message.diagnostics[0].code, "checksum.mismatch");
    }

    #[test]
    fn ignores_unmatched_packets() {
        let decoder = CustomProtocolDecoder::new(load_protocol_schema(SCHEMA).unwrap());
        assert!(decoder
            .decode_packet(&packet(&[0x11, 0, 0, 0, 0, 0]))
            .unwrap()
            .is_none());
    }

    #[test]
    fn decodes_output_from_incremental_framer() {
        let schema = load_protocol_schema(SCHEMA).unwrap();
        let mut framer = crate::ByteStreamFramer::new(schema.framing.clone()).unwrap();
        let decoder = CustomProtocolDecoder::new(schema);
        let prefix = [0x10, 0x02, 0xD0, 0x07, 0x03];
        let checksum = crc8_smbus(prefix.into_iter());
        assert!(framer
            .push_bytes(&bytes(&[0x10, 0x02]))
            .unwrap()
            .frames
            .is_empty());
        let mut tail = bytes(&[0xD0, 0x07, 0x03, checksum]);
        for byte in &mut tail {
            byte.range.start += 20;
            byte.range.end += 20;
        }
        let output = framer.push_bytes(&tail).unwrap();
        let message = decoder
            .decode_framed_bytes(output.frames.into_iter().next().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(message.summary, "Motor 2: 2000 rpm");
    }

    #[test]
    fn rejects_wrong_transport_and_truncated_packets() {
        let decoder = CustomProtocolDecoder::new(load_protocol_schema(SCHEMA).unwrap());
        let mut wrong_transport = packet(&[0x10, 0, 0, 0, 0, 0]);
        wrong_transport.transport = TransportKind::Spi;
        assert!(decoder.decode_packet(&wrong_transport).is_err());
        assert!(decoder.decode_packet(&packet(&[0x10, 0])).is_err());
    }
}
