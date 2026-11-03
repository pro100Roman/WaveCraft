use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use yaml_rust2::parser::{Event, EventReceiver, Parser};
use yaml_rust2::yaml::Hash;
use yaml_rust2::{Yaml, YamlLoader};

pub const PROTOCOL_SCHEMA_VERSION: &str = "wavecraft.protocol/v1";

const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
const MAX_SCHEMA_NODES: usize = 16_384;
const MAX_MESSAGES: usize = 256;
const MAX_FIELDS_PER_MESSAGE: usize = 1_024;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_TEXT_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportKind {
    Uart,
    Spi,
    I2c,
}

impl TransportKind {
    fn parse(value: &str, path: &str) -> Result<Self, SchemaError> {
        match value {
            "uart" | "usart" => Ok(Self::Uart),
            "spi" => Ok(Self::Spi),
            "i2c" => Ok(Self::I2c),
            _ => Err(SchemaError::at(
                path,
                format!("unsupported transport {value:?}; expected uart, spi, or i2c"),
            )),
        }
    }

    fn accepts_stream(self, stream: &str) -> bool {
        match self {
            Self::Uart => matches!(stream, "data" | "rx" | "tx"),
            Self::Spi => matches!(stream, "mosi" | "miso"),
            Self::I2c => stream == "data",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolMetadata {
    pub id: String,
    pub name: String,
    pub version: u64,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputSchema {
    pub transport: TransportKind,
    pub stream: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endian {
    Little,
    Big,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LengthField {
    pub offset: usize,
    pub width: usize,
    pub endian: Endian,
    pub adjustment: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FramingSchema {
    TransportTransaction {
        max_frame_bytes: usize,
    },
    Gap {
        minimum_us: u64,
        max_frame_bytes: usize,
    },
    Delimiter {
        delimiter: Vec<u8>,
        include_delimiter: bool,
        max_frame_bytes: usize,
    },
    FixedLength {
        bytes: usize,
    },
    LengthField {
        sync: Vec<u8>,
        length: LengthField,
        max_frame_bytes: usize,
        resync: ResyncStrategy,
    },
}

impl FramingSchema {
    pub fn max_frame_bytes(&self) -> usize {
        match self {
            Self::TransportTransaction { max_frame_bytes }
            | Self::Gap {
                max_frame_bytes, ..
            }
            | Self::Delimiter {
                max_frame_bytes, ..
            }
            | Self::LengthField {
                max_frame_bytes, ..
            } => *max_frame_bytes,
            Self::FixedLength { bytes } => *bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResyncStrategy {
    NextSync,
    DropFrame,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteMatch {
    pub offset: usize,
    pub value: u8,
    pub mask: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegerType {
    U8,
    S8,
    U16Le,
    U16Be,
    S16Le,
    S16Be,
    U32Le,
    U32Be,
    S32Le,
    S32Be,
}

impl IntegerType {
    pub const fn byte_width(self) -> usize {
        match self {
            Self::U8 | Self::S8 => 1,
            Self::U16Le | Self::U16Be | Self::S16Le | Self::S16Be => 2,
            Self::U32Le | Self::U32Be | Self::S32Le | Self::S32Be => 4,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "u8" => Some(Self::U8),
            "s8" => Some(Self::S8),
            "u16le" => Some(Self::U16Le),
            "u16be" => Some(Self::U16Be),
            "s16le" => Some(Self::S16Le),
            "s16be" => Some(Self::S16Be),
            "u32le" => Some(Self::U32Le),
            "u32be" => Some(Self::U32Be),
            "s32le" => Some(Self::S32Le),
            "s32be" => Some(Self::S32Be),
            _ => None,
        }
    }

    fn contains(self, value: i64) -> bool {
        match self {
            Self::U8 => (0..=u8::MAX as i64).contains(&value),
            Self::S8 => (i8::MIN as i64..=i8::MAX as i64).contains(&value),
            Self::U16Le | Self::U16Be => (0..=u16::MAX as i64).contains(&value),
            Self::S16Le | Self::S16Be => (i16::MIN as i64..=i16::MAX as i64).contains(&value),
            Self::U32Le | Self::U32Be => (0..=u32::MAX as i64).contains(&value),
            Self::S32Le | Self::S32Be => (i32::MIN as i64..=i32::MAX as i64).contains(&value),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StringEncoding {
    Ascii,
    Utf8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldKind {
    Integer(IntegerType),
    Bytes {
        size: usize,
    },
    String {
        size: usize,
        encoding: StringEncoding,
    },
}

impl FieldKind {
    pub const fn byte_width(&self) -> usize {
        match self {
            Self::Integer(kind) => kind.byte_width(),
            Self::Bytes { size } | Self::String { size, .. } => *size,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpectedValue {
    Integer(i64),
    Bytes(Vec<u8>),
    String(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BitDefinition {
    pub bit: u8,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisplayOptions {
    pub unit: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldSchema {
    pub name: String,
    pub kind: FieldKind,
    pub expected: Option<ExpectedValue>,
    pub enum_values: BTreeMap<i64, String>,
    pub bits: BTreeMap<String, BitDefinition>,
    pub hidden: bool,
    pub display: DisplayOptions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChecksumAlgorithm {
    Crc8Smbus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChecksumValidation {
    pub algorithm: ChecksumAlgorithm,
    pub from: usize,
    pub through_field: String,
    pub expected_field: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageSchema {
    pub id: String,
    pub name: String,
    pub matches: Vec<ByteMatch>,
    pub is_default: bool,
    pub fields: Vec<FieldSchema>,
    pub checksums: Vec<ChecksumValidation>,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolSchema {
    pub protocol: ProtocolMetadata,
    pub input: InputSchema,
    pub framing: FramingSchema,
    pub messages: Vec<MessageSchema>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaError {
    path: String,
    message: String,
}

impl SchemaError {
    fn at(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for SchemaError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            self.message.fmt(formatter)
        } else {
            write!(formatter, "{}: {}", self.path, self.message)
        }
    }
}

impl Error for SchemaError {}

pub fn load_protocol_schema(source: &str) -> Result<ProtocolSchema, SchemaError> {
    if source.len() > MAX_SCHEMA_BYTES {
        return Err(SchemaError::at(
            "",
            format!("schema exceeds the {MAX_SCHEMA_BYTES}-byte limit"),
        ));
    }
    preflight_yaml(source)?;
    let documents = YamlLoader::load_from_str(source)
        .map_err(|error| SchemaError::at("", format!("invalid YAML: {error}")))?;
    if documents.len() != 1 {
        return Err(SchemaError::at(
            "",
            "a protocol file must contain exactly one YAML document",
        ));
    }
    let root = &documents[0];
    validate_tree_bounds(root)?;
    parse_schema(root)
}

#[derive(Default)]
struct SchemaEventSink {
    node_count: usize,
    has_alias: bool,
    has_tag: bool,
}

impl EventReceiver for SchemaEventSink {
    fn on_event(&mut self, event: Event) {
        match event {
            Event::Alias(_) => self.has_alias = true,
            Event::Scalar(_, _, _, tag)
            | Event::SequenceStart(_, tag)
            | Event::MappingStart(_, tag) => {
                self.node_count = self.node_count.saturating_add(1);
                self.has_tag |= tag.is_some();
            }
            _ => {}
        }
    }
}

fn preflight_yaml(source: &str) -> Result<(), SchemaError> {
    let mut sink = SchemaEventSink::default();
    Parser::new_from_str(source)
        .load(&mut sink, true)
        .map_err(|error| SchemaError::at("", format!("invalid YAML: {error}")))?;
    if sink.has_alias {
        return Err(SchemaError::at("", "YAML aliases are not supported"));
    }
    if sink.has_tag {
        return Err(SchemaError::at("", "custom YAML tags are not supported"));
    }
    if sink.node_count > MAX_SCHEMA_NODES {
        return Err(SchemaError::at(
            "",
            format!("schema contains more than {MAX_SCHEMA_NODES} YAML nodes"),
        ));
    }
    Ok(())
}

fn parse_schema(root: &Yaml) -> Result<ProtocolSchema, SchemaError> {
    let root = as_map(root, "")?;
    ensure_only_keys(
        root,
        &["schema", "protocol", "input", "framing", "messages"],
        "",
    )?;
    let version = required_string(root, "schema", "schema")?;
    if version != PROTOCOL_SCHEMA_VERSION {
        return Err(SchemaError::at(
            "schema",
            format!("unsupported schema version {version:?}; expected {PROTOCOL_SCHEMA_VERSION:?}"),
        ));
    }

    let protocol = parse_protocol(required(root, "protocol", "protocol")?)?;
    let input = parse_input(required(root, "input", "input")?)?;
    let framing = parse_framing(required(root, "framing", "framing")?, input.transport)?;
    let messages = parse_messages(
        required(root, "messages", "messages")?,
        framing.max_frame_bytes(),
    )?;

    Ok(ProtocolSchema {
        protocol,
        input,
        framing,
        messages,
    })
}

fn parse_protocol(value: &Yaml) -> Result<ProtocolMetadata, SchemaError> {
    let path = "protocol";
    let map = as_map(value, path)?;
    ensure_only_keys(map, &["id", "name", "version", "description"], path)?;
    let id = required_string(map, "id", "protocol.id")?;
    validate_id(&id, "protocol.id")?;
    let name = required_nonempty_string(map, "name", "protocol.name")?;
    let version = required_u64(map, "version", "protocol.version")?;
    let description = optional_string(map, "description", "protocol.description")?;
    if description
        .as_ref()
        .is_some_and(|value| value.len() > MAX_TEXT_BYTES)
    {
        return Err(SchemaError::at(
            "protocol.description",
            format!("description exceeds {MAX_TEXT_BYTES} bytes"),
        ));
    }
    Ok(ProtocolMetadata {
        id,
        name,
        version,
        description,
    })
}

fn parse_input(value: &Yaml) -> Result<InputSchema, SchemaError> {
    let path = "input";
    let map = as_map(value, path)?;
    ensure_only_keys(map, &["transport", "stream"], path)?;
    let transport_name = required_string(map, "transport", "input.transport")?;
    let transport = TransportKind::parse(&transport_name, "input.transport")?;
    let stream = required_nonempty_string(map, "stream", "input.stream")?;
    if !transport.accepts_stream(&stream) {
        return Err(SchemaError::at(
            "input.stream",
            format!("stream {stream:?} is not available for {transport_name}"),
        ));
    }
    Ok(InputSchema { transport, stream })
}

fn parse_framing(value: &Yaml, transport: TransportKind) -> Result<FramingSchema, SchemaError> {
    let path = "framing";
    let map = as_map(value, path)?;
    let strategy = required_string(map, "strategy", "framing.strategy")?;
    match strategy.as_str() {
        "transport_transaction" => {
            ensure_only_keys(map, &["strategy", "max_frame_bytes"], path)?;
            if transport == TransportKind::Uart {
                return Err(SchemaError::at(
                    "framing.strategy",
                    "transport_transaction is only valid for SPI or I2C",
                ));
            }
            let max_frame_bytes = optional_frame_limit(map, MAX_FRAME_BYTES)?;
            Ok(FramingSchema::TransportTransaction { max_frame_bytes })
        }
        "gap" => {
            ensure_only_keys(map, &["strategy", "minimum_us", "max_frame_bytes"], path)?;
            if transport != TransportKind::Uart {
                return Err(SchemaError::at(
                    "framing.strategy",
                    "gap framing is only valid for UART/USART",
                ));
            }
            let minimum_us = required_u64(map, "minimum_us", "framing.minimum_us")?;
            if minimum_us == 0 {
                return Err(SchemaError::at(
                    "framing.minimum_us",
                    "gap duration must be greater than zero",
                ));
            }
            let max_frame_bytes = required_frame_limit(map)?;
            Ok(FramingSchema::Gap {
                minimum_us,
                max_frame_bytes,
            })
        }
        "delimiter" => {
            ensure_only_keys(
                map,
                &[
                    "strategy",
                    "delimiter",
                    "include_delimiter",
                    "max_frame_bytes",
                ],
                path,
            )?;
            let delimiter = parse_byte_array(
                required(map, "delimiter", "framing.delimiter")?,
                "framing.delimiter",
            )?;
            if delimiter.is_empty() {
                return Err(SchemaError::at(
                    "framing.delimiter",
                    "delimiter must not be empty",
                ));
            }
            let include_delimiter =
                optional_bool(map, "include_delimiter", "framing.include_delimiter")?
                    .unwrap_or(false);
            let max_frame_bytes = required_frame_limit(map)?;
            if delimiter.len() > max_frame_bytes {
                return Err(SchemaError::at(
                    "framing.delimiter",
                    "delimiter is longer than max_frame_bytes",
                ));
            }
            Ok(FramingSchema::Delimiter {
                delimiter,
                include_delimiter,
                max_frame_bytes,
            })
        }
        "fixed_length" => {
            ensure_only_keys(map, &["strategy", "bytes"], path)?;
            let bytes = required_usize(map, "bytes", "framing.bytes")?;
            validate_frame_limit(bytes, "framing.bytes")?;
            Ok(FramingSchema::FixedLength { bytes })
        }
        "length_field" => {
            ensure_only_keys(
                map,
                &["strategy", "sync", "length", "max_frame_bytes", "resync"],
                path,
            )?;
            let sync = match get(map, "sync") {
                Some(value) => parse_byte_array(value, "framing.sync")?,
                None => Vec::new(),
            };
            let length = parse_length_field(required(map, "length", "framing.length")?)?;
            let max_frame_bytes = required_frame_limit(map)?;
            if sync.len() > max_frame_bytes {
                return Err(SchemaError::at(
                    "framing.sync",
                    "sync sequence is longer than max_frame_bytes",
                ));
            }
            let required_prefix = length.offset.checked_add(length.width).ok_or_else(|| {
                SchemaError::at("framing.length", "length-field prefix overflows")
            })?;
            if required_prefix > max_frame_bytes {
                return Err(SchemaError::at(
                    "framing.length",
                    "length field lies beyond max_frame_bytes",
                ));
            }
            let resync = match optional_string(map, "resync", "framing.resync")?.as_deref() {
                None | Some("drop_frame") => ResyncStrategy::DropFrame,
                Some("next_sync") if !sync.is_empty() => ResyncStrategy::NextSync,
                Some("next_sync") => {
                    return Err(SchemaError::at(
                        "framing.resync",
                        "next_sync requires a non-empty sync sequence",
                    ));
                }
                Some(other) => {
                    return Err(SchemaError::at(
                        "framing.resync",
                        format!("unsupported resynchronization strategy {other:?}"),
                    ));
                }
            };
            Ok(FramingSchema::LengthField {
                sync,
                length,
                max_frame_bytes,
                resync,
            })
        }
        _ => Err(SchemaError::at(
            "framing.strategy",
            format!("unsupported framing strategy {strategy:?}"),
        )),
    }
}

fn parse_length_field(value: &Yaml) -> Result<LengthField, SchemaError> {
    let path = "framing.length";
    let map = as_map(value, path)?;
    ensure_only_keys(map, &["offset", "width", "endian", "adjustment"], path)?;
    let offset = required_usize(map, "offset", "framing.length.offset")?;
    let width = required_usize(map, "width", "framing.length.width")?;
    if !matches!(width, 1 | 2 | 4) {
        return Err(SchemaError::at(
            "framing.length.width",
            "length width must be 1, 2, or 4 bytes",
        ));
    }
    let endian = match optional_string(map, "endian", "framing.length.endian")?.as_deref() {
        None | Some("little") => Endian::Little,
        Some("big") => Endian::Big,
        Some(other) => {
            return Err(SchemaError::at(
                "framing.length.endian",
                format!("unsupported byte order {other:?}"),
            ));
        }
    };
    let adjustment = optional_i64(map, "adjustment", "framing.length.adjustment")?.unwrap_or(0);
    Ok(LengthField {
        offset,
        width,
        endian,
        adjustment,
    })
}

fn parse_messages(value: &Yaml, max_frame_bytes: usize) -> Result<Vec<MessageSchema>, SchemaError> {
    let values = as_array(value, "messages")?;
    if values.is_empty() {
        return Err(SchemaError::at(
            "messages",
            "at least one message is required",
        ));
    }
    if values.len() > MAX_MESSAGES {
        return Err(SchemaError::at(
            "messages",
            format!("message count exceeds the limit of {MAX_MESSAGES}"),
        ));
    }
    let mut messages = Vec::with_capacity(values.len());
    let mut ids = BTreeSet::new();
    let mut default_id = None;
    for (index, value) in values.iter().enumerate() {
        let path = format!("messages[{index}]");
        let message = parse_message(value, &path, max_frame_bytes)?;
        if !ids.insert(message.id.clone()) {
            return Err(SchemaError::at(
                format!("{path}.id"),
                format!("duplicate message id {:?}", message.id),
            ));
        }
        if message.is_default {
            if let Some(previous) = default_id {
                return Err(SchemaError::at(
                    format!("{path}.default"),
                    format!("default message is already declared by {previous:?}"),
                ));
            }
            default_id = Some(message.id.clone());
        }
        messages.push(message);
    }
    for left in 0..messages.len() {
        if messages[left].matches.is_empty() {
            continue;
        }
        for right in left + 1..messages.len() {
            if messages[left].matches == messages[right].matches {
                return Err(SchemaError::at(
                    format!("messages[{right}].match"),
                    format!("match clause duplicates message {:?}", messages[left].id),
                ));
            }
        }
    }
    Ok(messages)
}

fn parse_message(
    value: &Yaml,
    path: &str,
    max_frame_bytes: usize,
) -> Result<MessageSchema, SchemaError> {
    let map = as_map(value, path)?;
    ensure_only_keys(
        map,
        &[
            "id", "name", "match", "default", "fields", "validate", "summary",
        ],
        path,
    )?;
    let id_path = format!("{path}.id");
    let id = required_string(map, "id", &id_path)?;
    validate_id(&id, &id_path)?;
    let name = required_nonempty_string(map, "name", &format!("{path}.name"))?;
    let is_default = optional_bool(map, "default", &format!("{path}.default"))?.unwrap_or(false);
    let matches = match get(map, "match") {
        Some(value) => parse_matches(value, &format!("{path}.match"), max_frame_bytes)?,
        None => Vec::new(),
    };
    if matches.is_empty() && !is_default {
        return Err(SchemaError::at(
            format!("{path}.match"),
            "a message requires a match clause or default: true",
        ));
    }
    if is_default && !matches.is_empty() {
        return Err(SchemaError::at(
            format!("{path}.default"),
            "a default message must not also declare match conditions",
        ));
    }
    let fields = parse_fields(
        required(map, "fields", &format!("{path}.fields"))?,
        &format!("{path}.fields"),
        max_frame_bytes,
    )?;
    let checksums = match get(map, "validate") {
        Some(value) => parse_validations(value, &format!("{path}.validate"), &fields)?,
        None => Vec::new(),
    };
    let summary = optional_string(map, "summary", &format!("{path}.summary"))?;
    if let Some(summary) = &summary {
        validate_summary(summary, &fields, &format!("{path}.summary"))?;
    }
    Ok(MessageSchema {
        id,
        name,
        matches,
        is_default,
        fields,
        checksums,
        summary,
    })
}

fn parse_matches(
    value: &Yaml,
    path: &str,
    max_frame_bytes: usize,
) -> Result<Vec<ByteMatch>, SchemaError> {
    let values = as_array(value, path)?;
    if values.is_empty() {
        return Err(SchemaError::at(path, "match list must not be empty"));
    }
    let mut result = Vec::with_capacity(values.len());
    let mut offsets = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let item_path = format!("{path}[{index}]");
        let map = as_map(value, &item_path)?;
        ensure_only_keys(map, &["offset", "value", "mask"], &item_path)?;
        let offset = required_usize(map, "offset", &format!("{item_path}.offset"))?;
        if offset >= max_frame_bytes {
            return Err(SchemaError::at(
                format!("{item_path}.offset"),
                "match offset lies beyond max_frame_bytes",
            ));
        }
        if !offsets.insert(offset) {
            return Err(SchemaError::at(
                format!("{item_path}.offset"),
                format!("duplicate match offset {offset}"),
            ));
        }
        result.push(ByteMatch {
            offset,
            value: required_u8(map, "value", &format!("{item_path}.value"))?,
            mask: optional_u8(map, "mask", &format!("{item_path}.mask"))?.unwrap_or(u8::MAX),
        });
    }
    Ok(result)
}

fn parse_fields(
    value: &Yaml,
    path: &str,
    max_frame_bytes: usize,
) -> Result<Vec<FieldSchema>, SchemaError> {
    let values = as_array(value, path)?;
    if values.is_empty() {
        return Err(SchemaError::at(path, "at least one field is required"));
    }
    if values.len() > MAX_FIELDS_PER_MESSAGE {
        return Err(SchemaError::at(
            path,
            format!("field count exceeds the limit of {MAX_FIELDS_PER_MESSAGE}"),
        ));
    }
    let mut result = Vec::with_capacity(values.len());
    let mut names = BTreeSet::new();
    let mut total_bytes = 0_usize;
    for (index, value) in values.iter().enumerate() {
        let item_path = format!("{path}[{index}]");
        let field = parse_field(value, &item_path)?;
        if !names.insert(field.name.clone()) {
            return Err(SchemaError::at(
                format!("{item_path}.name"),
                format!("duplicate field name {:?}", field.name),
            ));
        }
        total_bytes = total_bytes
            .checked_add(field.kind.byte_width())
            .ok_or_else(|| SchemaError::at(path, "field layout overflows"))?;
        if total_bytes > max_frame_bytes {
            return Err(SchemaError::at(
                &item_path,
                "field layout exceeds max_frame_bytes",
            ));
        }
        result.push(field);
    }
    Ok(result)
}

fn parse_field(value: &Yaml, path: &str) -> Result<FieldSchema, SchemaError> {
    let map = as_map(value, path)?;
    ensure_only_keys(
        map,
        &[
            "name", "type", "size", "encoding", "expect", "enum", "bits", "hidden", "display",
        ],
        path,
    )?;
    let name_path = format!("{path}.name");
    let name = required_string(map, "name", &name_path)?;
    validate_id(&name, &name_path)?;
    let kind_name = required_string(map, "type", &format!("{path}.type"))?;
    let kind = if let Some(integer) = IntegerType::parse(&kind_name) {
        if get(map, "size").is_some() || get(map, "encoding").is_some() {
            return Err(SchemaError::at(
                format!("{path}.size"),
                "integer fields do not accept size or encoding",
            ));
        }
        FieldKind::Integer(integer)
    } else {
        let size = required_usize(map, "size", &format!("{path}.size"))?;
        validate_frame_limit(size, &format!("{path}.size"))?;
        match kind_name.as_str() {
            "bytes" => {
                if get(map, "encoding").is_some() {
                    return Err(SchemaError::at(
                        format!("{path}.encoding"),
                        "byte fields do not accept a string encoding",
                    ));
                }
                FieldKind::Bytes { size }
            }
            "string" => {
                let encoding = match optional_string(map, "encoding", &format!("{path}.encoding"))?
                    .as_deref()
                {
                    None | Some("utf8") => StringEncoding::Utf8,
                    Some("ascii") => StringEncoding::Ascii,
                    Some(other) => {
                        return Err(SchemaError::at(
                            format!("{path}.encoding"),
                            format!("unsupported string encoding {other:?}"),
                        ));
                    }
                };
                FieldKind::String { size, encoding }
            }
            _ => {
                return Err(SchemaError::at(
                    format!("{path}.type"),
                    format!("unsupported field type {kind_name:?}"),
                ));
            }
        }
    };

    let expected = match get(map, "expect") {
        Some(value) => Some(parse_expected(value, &kind, &format!("{path}.expect"))?),
        None => None,
    };
    let enum_values = match get(map, "enum") {
        Some(value) => parse_enum(value, &kind, &format!("{path}.enum"))?,
        None => BTreeMap::new(),
    };
    let bits = match get(map, "bits") {
        Some(value) => parse_bits(value, &kind, &format!("{path}.bits"))?,
        None => BTreeMap::new(),
    };
    if !enum_values.is_empty() && !bits.is_empty() {
        return Err(SchemaError::at(
            path,
            "a field cannot declare both enum and bits",
        ));
    }
    let hidden = optional_bool(map, "hidden", &format!("{path}.hidden"))?.unwrap_or(false);
    let display = match get(map, "display") {
        Some(value) => parse_display(value, &format!("{path}.display"))?,
        None => DisplayOptions::default(),
    };
    Ok(FieldSchema {
        name,
        kind,
        expected,
        enum_values,
        bits,
        hidden,
        display,
    })
}

fn parse_expected(
    value: &Yaml,
    kind: &FieldKind,
    path: &str,
) -> Result<ExpectedValue, SchemaError> {
    match kind {
        FieldKind::Integer(integer) => {
            let value = parse_i64(value, path)?;
            if !integer.contains(value) {
                return Err(SchemaError::at(
                    path,
                    "expected value does not fit field type",
                ));
            }
            Ok(ExpectedValue::Integer(value))
        }
        FieldKind::Bytes { size } => {
            let bytes = parse_byte_array(value, path)?;
            if bytes.len() != *size {
                return Err(SchemaError::at(
                    path,
                    format!(
                        "expected byte sequence has length {}, not {size}",
                        bytes.len()
                    ),
                ));
            }
            Ok(ExpectedValue::Bytes(bytes))
        }
        FieldKind::String { size, .. } => {
            let string = as_string(value, path)?;
            if string.len() != *size {
                return Err(SchemaError::at(
                    path,
                    format!("expected string has {} bytes, not {size}", string.len()),
                ));
            }
            Ok(ExpectedValue::String(string))
        }
    }
}

fn parse_enum(
    value: &Yaml,
    kind: &FieldKind,
    path: &str,
) -> Result<BTreeMap<i64, String>, SchemaError> {
    let FieldKind::Integer(integer) = kind else {
        return Err(SchemaError::at(
            path,
            "enum is only valid on integer fields",
        ));
    };
    let map = as_map(value, path)?;
    if map.is_empty() {
        return Err(SchemaError::at(path, "enum must not be empty"));
    }
    let mut result = BTreeMap::new();
    for (key, value) in map {
        let number = parse_i64(key, path)?;
        if !integer.contains(number) {
            return Err(SchemaError::at(
                path,
                format!("enum value {number} does not fit field type"),
            ));
        }
        let label = as_nonempty_string(value, path)?;
        if result.insert(number, label).is_some() {
            return Err(SchemaError::at(
                path,
                format!("duplicate enum value {number}"),
            ));
        }
    }
    Ok(result)
}

fn parse_bits(
    value: &Yaml,
    kind: &FieldKind,
    path: &str,
) -> Result<BTreeMap<String, BitDefinition>, SchemaError> {
    let FieldKind::Integer(integer) = kind else {
        return Err(SchemaError::at(
            path,
            "bits are only valid on integer fields",
        ));
    };
    let map = as_map(value, path)?;
    if map.is_empty() {
        return Err(SchemaError::at(path, "bits must not be empty"));
    }
    let bit_width = integer.byte_width() * 8;
    let mut result = BTreeMap::new();
    let mut used_bits = BTreeSet::new();
    for (key, value) in map {
        let Yaml::String(name) = key else {
            return Err(SchemaError::at(path, "bit names must be strings"));
        };
        validate_id(name, path)?;
        let bit = parse_u64(value, path)?;
        if bit >= bit_width as u64 {
            return Err(SchemaError::at(
                path,
                format!("bit {bit} lies outside a {bit_width}-bit field"),
            ));
        }
        if !used_bits.insert(bit) {
            return Err(SchemaError::at(
                path,
                format!("bit {bit} is declared more than once"),
            ));
        }
        result.insert(name.clone(), BitDefinition { bit: bit as u8 });
    }
    Ok(result)
}

fn parse_display(value: &Yaml, path: &str) -> Result<DisplayOptions, SchemaError> {
    let map = as_map(value, path)?;
    ensure_only_keys(map, &["unit"], path)?;
    Ok(DisplayOptions {
        unit: optional_string(map, "unit", &format!("{path}.unit"))?,
    })
}

fn parse_validations(
    value: &Yaml,
    path: &str,
    fields: &[FieldSchema],
) -> Result<Vec<ChecksumValidation>, SchemaError> {
    let values = as_array(value, path)?;
    let mut cursor = 0_usize;
    let mut field_positions = BTreeMap::new();
    for (index, field) in fields.iter().enumerate() {
        let start = cursor;
        cursor = cursor
            .checked_add(field.kind.byte_width())
            .ok_or_else(|| SchemaError::at(path, "field layout overflows"))?;
        field_positions.insert(field.name.as_str(), (index, start, cursor));
    }
    let total_bytes = cursor;
    let mut checksums = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let item_path = format!("{path}[{index}]");
        let item = as_map(value, &item_path)?;
        ensure_only_keys(item, &["checksum"], &item_path)?;
        let checksum_path = format!("{item_path}.checksum");
        let checksum = as_map(required(item, "checksum", &checksum_path)?, &checksum_path)?;
        ensure_only_keys(
            checksum,
            &["algorithm", "from", "through_field", "expected_field"],
            &checksum_path,
        )?;
        let algorithm_name =
            required_string(checksum, "algorithm", &format!("{checksum_path}.algorithm"))?;
        let algorithm = match algorithm_name.as_str() {
            "crc8_smbus" => ChecksumAlgorithm::Crc8Smbus,
            _ => {
                return Err(SchemaError::at(
                    format!("{checksum_path}.algorithm"),
                    format!("unsupported checksum algorithm {algorithm_name:?}"),
                ));
            }
        };
        let from = required_usize(checksum, "from", &format!("{checksum_path}.from"))?;
        if from >= total_bytes {
            return Err(SchemaError::at(
                format!("{checksum_path}.from"),
                "checksum start lies beyond the decoded field layout",
            ));
        }
        let through_field = required_string(
            checksum,
            "through_field",
            &format!("{checksum_path}.through_field"),
        )?;
        let expected_field = required_string(
            checksum,
            "expected_field",
            &format!("{checksum_path}.expected_field"),
        )?;
        let (through_index, _, through_end) =
            field_positions.get(through_field.as_str()).ok_or_else(|| {
                SchemaError::at(
                    format!("{checksum_path}.through_field"),
                    format!("unknown field {through_field:?}"),
                )
            })?;
        let (expected_index, _, _) =
            field_positions
                .get(expected_field.as_str())
                .ok_or_else(|| {
                    SchemaError::at(
                        format!("{checksum_path}.expected_field"),
                        format!("unknown field {expected_field:?}"),
                    )
                })?;
        if through_index >= expected_index {
            return Err(SchemaError::at(
                &checksum_path,
                "through_field must appear before expected_field",
            ));
        }
        if from >= *through_end {
            return Err(SchemaError::at(
                format!("{checksum_path}.from"),
                "checksum start must precede the end of through_field",
            ));
        }
        if fields[*expected_index].kind != FieldKind::Integer(IntegerType::U8) {
            return Err(SchemaError::at(
                format!("{checksum_path}.expected_field"),
                "crc8_smbus expected_field must have type u8",
            ));
        }
        checksums.push(ChecksumValidation {
            algorithm,
            from,
            through_field,
            expected_field,
        });
    }
    Ok(checksums)
}

fn validate_summary(summary: &str, fields: &[FieldSchema], path: &str) -> Result<(), SchemaError> {
    if summary.len() > MAX_TEXT_BYTES {
        return Err(SchemaError::at(
            path,
            format!("summary exceeds {MAX_TEXT_BYTES} bytes"),
        ));
    }
    let field_names = fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<BTreeSet<_>>();
    let bytes = summary.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'}' {
            return Err(SchemaError::at(path, "summary contains an unmatched '}'"));
        }
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        let start = index + 1;
        let Some(relative_end) = bytes[start..].iter().position(|byte| *byte == b'}') else {
            return Err(SchemaError::at(path, "summary contains an unmatched '{'"));
        };
        let end = start + relative_end;
        let name = &summary[start..end];
        if name.is_empty() || name.as_bytes().contains(&b'{') {
            return Err(SchemaError::at(
                path,
                "summary contains an invalid placeholder",
            ));
        }
        if !field_names.contains(name) {
            return Err(SchemaError::at(
                path,
                format!("summary references unknown field {name:?}"),
            ));
        }
        index = end + 1;
    }
    Ok(())
}

fn validate_tree_bounds(root: &Yaml) -> Result<(), SchemaError> {
    let mut stack = vec![root];
    let mut count = 0_usize;
    while let Some(value) = stack.pop() {
        count += 1;
        if count > MAX_SCHEMA_NODES {
            return Err(SchemaError::at(
                "",
                format!("schema contains more than {MAX_SCHEMA_NODES} YAML nodes"),
            ));
        }
        match value {
            Yaml::Array(values) => stack.extend(values),
            Yaml::Hash(values) => {
                for (key, value) in values {
                    stack.push(key);
                    stack.push(value);
                }
            }
            Yaml::Alias(_) => {
                return Err(SchemaError::at("", "YAML aliases are not supported"));
            }
            Yaml::String(value) | Yaml::Real(value) if value.len() > MAX_TEXT_BYTES => {
                return Err(SchemaError::at(
                    "",
                    format!("YAML scalar exceeds {MAX_TEXT_BYTES} bytes"),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_id(value: &str, path: &str) -> Result<(), SchemaError> {
    if value.is_empty() {
        return Err(SchemaError::at(path, "identifier must not be empty"));
    }
    if value.len() > 128 {
        return Err(SchemaError::at(path, "identifier exceeds 128 bytes"));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(SchemaError::at(
            path,
            "identifier may contain only ASCII letters, digits, '.', '_', and '-'",
        ));
    }
    Ok(())
}

fn validate_frame_limit(value: usize, path: &str) -> Result<(), SchemaError> {
    if value == 0 {
        return Err(SchemaError::at(path, "value must be greater than zero"));
    }
    if value > MAX_FRAME_BYTES {
        return Err(SchemaError::at(
            path,
            format!("value exceeds the {MAX_FRAME_BYTES}-byte application limit"),
        ));
    }
    Ok(())
}

fn required_frame_limit(map: &Hash) -> Result<usize, SchemaError> {
    let value = required_usize(map, "max_frame_bytes", "framing.max_frame_bytes")?;
    validate_frame_limit(value, "framing.max_frame_bytes")?;
    Ok(value)
}

fn optional_frame_limit(map: &Hash, default: usize) -> Result<usize, SchemaError> {
    let value = match get(map, "max_frame_bytes") {
        Some(value) => parse_usize(value, "framing.max_frame_bytes")?,
        None => default,
    };
    validate_frame_limit(value, "framing.max_frame_bytes")?;
    Ok(value)
}

fn ensure_only_keys(map: &Hash, allowed: &[&str], path: &str) -> Result<(), SchemaError> {
    for key in map.keys() {
        let Yaml::String(key) = key else {
            return Err(SchemaError::at(path, "mapping keys must be strings"));
        };
        if !allowed.contains(&key.as_str()) {
            let key_path = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            return Err(SchemaError::at(key_path, "unknown field"));
        }
    }
    Ok(())
}

fn get<'a>(map: &'a Hash, key: &str) -> Option<&'a Yaml> {
    map.get(&Yaml::String(key.to_owned()))
}

fn required<'a>(map: &'a Hash, key: &str, path: &str) -> Result<&'a Yaml, SchemaError> {
    get(map, key).ok_or_else(|| SchemaError::at(path, "required field is missing"))
}

fn as_map<'a>(value: &'a Yaml, path: &str) -> Result<&'a Hash, SchemaError> {
    match value {
        Yaml::Hash(value) => Ok(value),
        _ => Err(SchemaError::at(path, "expected a mapping")),
    }
}

fn as_array<'a>(value: &'a Yaml, path: &str) -> Result<&'a [Yaml], SchemaError> {
    match value {
        Yaml::Array(value) => Ok(value),
        _ => Err(SchemaError::at(path, "expected a sequence")),
    }
}

fn as_string(value: &Yaml, path: &str) -> Result<String, SchemaError> {
    match value {
        Yaml::String(value) => Ok(value.clone()),
        _ => Err(SchemaError::at(path, "expected a string")),
    }
}

fn as_nonempty_string(value: &Yaml, path: &str) -> Result<String, SchemaError> {
    let value = as_string(value, path)?;
    if value.is_empty() {
        return Err(SchemaError::at(path, "string must not be empty"));
    }
    if value.len() > MAX_TEXT_BYTES {
        return Err(SchemaError::at(
            path,
            format!("string exceeds {MAX_TEXT_BYTES} bytes"),
        ));
    }
    Ok(value)
}

fn required_string(map: &Hash, key: &str, path: &str) -> Result<String, SchemaError> {
    as_string(required(map, key, path)?, path)
}

fn required_nonempty_string(map: &Hash, key: &str, path: &str) -> Result<String, SchemaError> {
    as_nonempty_string(required(map, key, path)?, path)
}

fn optional_string(map: &Hash, key: &str, path: &str) -> Result<Option<String>, SchemaError> {
    get(map, key)
        .map(|value| as_string(value, path))
        .transpose()
}

fn optional_bool(map: &Hash, key: &str, path: &str) -> Result<Option<bool>, SchemaError> {
    get(map, key)
        .map(|value| match value {
            Yaml::Boolean(value) => Ok(*value),
            _ => Err(SchemaError::at(path, "expected a boolean")),
        })
        .transpose()
}

fn required_u64(map: &Hash, key: &str, path: &str) -> Result<u64, SchemaError> {
    parse_u64(required(map, key, path)?, path)
}

fn required_usize(map: &Hash, key: &str, path: &str) -> Result<usize, SchemaError> {
    parse_usize(required(map, key, path)?, path)
}

fn required_u8(map: &Hash, key: &str, path: &str) -> Result<u8, SchemaError> {
    let value = parse_u64(required(map, key, path)?, path)?;
    u8::try_from(value).map_err(|_| SchemaError::at(path, "value does not fit in one byte"))
}

fn optional_u8(map: &Hash, key: &str, path: &str) -> Result<Option<u8>, SchemaError> {
    get(map, key)
        .map(|value| {
            let value = parse_u64(value, path)?;
            u8::try_from(value).map_err(|_| SchemaError::at(path, "value does not fit in one byte"))
        })
        .transpose()
}

fn optional_i64(map: &Hash, key: &str, path: &str) -> Result<Option<i64>, SchemaError> {
    get(map, key)
        .map(|value| parse_i64(value, path))
        .transpose()
}

fn parse_usize(value: &Yaml, path: &str) -> Result<usize, SchemaError> {
    let value = parse_u64(value, path)?;
    usize::try_from(value).map_err(|_| SchemaError::at(path, "value does not fit this platform"))
}

fn parse_u64(value: &Yaml, path: &str) -> Result<u64, SchemaError> {
    let value = parse_i64(value, path)?;
    u64::try_from(value).map_err(|_| SchemaError::at(path, "expected a non-negative integer"))
}

fn parse_i64(value: &Yaml, path: &str) -> Result<i64, SchemaError> {
    match value {
        Yaml::Integer(value) => Ok(*value),
        Yaml::String(value) => {
            parse_integer_literal(value).ok_or_else(|| SchemaError::at(path, "expected an integer"))
        }
        _ => Err(SchemaError::at(path, "expected an integer")),
    }
}

fn parse_integer_literal(value: &str) -> Option<i64> {
    let value = value.replace('_', "");
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value.as_str()), |digits| (true, digits));
    let (radix, digits) = if let Some(digits) = digits.strip_prefix("0x") {
        (16, digits)
    } else if let Some(digits) = digits.strip_prefix("0X") {
        (16, digits)
    } else if let Some(digits) = digits.strip_prefix("0b") {
        (2, digits)
    } else if let Some(digits) = digits.strip_prefix("0B") {
        (2, digits)
    } else {
        (10, digits)
    };
    let magnitude = i64::from_str_radix(digits, radix).ok()?;
    if negative {
        magnitude.checked_neg()
    } else {
        Some(magnitude)
    }
}

fn parse_byte_array(value: &Yaml, path: &str) -> Result<Vec<u8>, SchemaError> {
    let values = as_array(value, path)?;
    if values.len() > MAX_FRAME_BYTES {
        return Err(SchemaError::at(
            path,
            "byte sequence exceeds application limit",
        ));
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let value = parse_u64(value, &format!("{path}[{index}]"))?;
            u8::try_from(value).map_err(|_| {
                SchemaError::at(format!("{path}[{index}]"), "value does not fit in one byte")
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/examples/motor-controller.wavecraft-protocol.yaml"
    ));

    #[test]
    fn loads_documented_uart_schema() {
        let schema = load_protocol_schema(EXAMPLE).unwrap();
        assert_eq!(schema.protocol.id, "example.motor-controller");
        assert_eq!(schema.input.transport, TransportKind::Uart);
        assert_eq!(schema.messages[0].fields.len(), 7);
        assert_eq!(schema.messages[0].checksums.len(), 1);
        assert_eq!(schema.framing.max_frame_bytes(), 256);
    }

    #[test]
    fn rejects_unknown_fields_with_a_path() {
        let source = EXAMPLE.replace("  version: 1", "  version: 1\n  typo: true");
        let error = load_protocol_schema(&source).unwrap_err();
        assert_eq!(error.path(), "protocol.typo");
        assert_eq!(error.message(), "unknown field");
    }

    #[test]
    fn rejects_stream_that_transport_does_not_provide() {
        let source = EXAMPLE.replace("  stream: data", "  stream: mosi");
        let error = load_protocol_schema(&source).unwrap_err();
        assert_eq!(error.path(), "input.stream");
    }

    #[test]
    fn rejects_unbounded_gap_framing() {
        let source = EXAMPLE
            .replace(
                "  strategy: length_field\n  sync: [0xAA, 0x55]\n  length:\n    offset: 2\n    width: 1\n    endian: little\n    adjustment: 4\n  max_frame_bytes: 256\n  resync: next_sync",
                "  strategy: gap\n  minimum_us: 500",
            );
        let error = load_protocol_schema(&source).unwrap_err();
        assert_eq!(error.path(), "framing.max_frame_bytes");
    }

    #[test]
    fn rejects_duplicate_message_matches() {
        let source = EXAMPLE.replace(
            "    summary: \"Motor {motor}: {speed_rpm}\"",
            "    summary: \"Motor {motor}: {speed_rpm}\"\n  - id: duplicate\n    name: Duplicate\n    match:\n      - offset: 3\n        value: 0x10\n    fields:\n      - name: value\n        type: u8",
        );
        let error = load_protocol_schema(&source).unwrap_err();
        assert_eq!(error.path(), "messages[1].match");
    }

    #[test]
    fn rejects_summary_that_references_unknown_field() {
        let source = EXAMPLE.replace("{speed_rpm}", "{missing}");
        let error = load_protocol_schema(&source).unwrap_err();
        assert_eq!(error.path(), "messages[0].summary");
    }

    #[test]
    fn rejects_yaml_aliases_before_they_are_expanded() {
        let source = EXAMPLE.replace(
            "      - name: sync\n        type: bytes",
            "      - &shared\n        name: sync\n        type: bytes",
        );
        let source = source.replace(
            "      - name: payload_length\n        type: u8",
            "      - *shared",
        );
        let error = load_protocol_schema(&source).unwrap_err();
        assert_eq!(error.message(), "YAML aliases are not supported");
    }

    #[test]
    fn loads_transaction_framed_i2c_schema() {
        let source = r#"
schema: wavecraft.protocol/v1
protocol:
  id: example.sensor
  name: Sensor
  version: 1
input:
  transport: i2c
  stream: data
framing:
  strategy: transport_transaction
  max_frame_bytes: 64
messages:
  - id: response
    name: Response
    default: true
    fields:
      - name: temperature
        type: s16be
"#;
        let schema = load_protocol_schema(source).unwrap();
        assert_eq!(schema.input.transport, TransportKind::I2c);
        assert!(schema.messages[0].is_default);
    }
}
