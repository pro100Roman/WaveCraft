//! ELF metadata and SystemView API description support.

use std::collections::BTreeMap;
use std::fmt;

use object::{Object, ObjectSection, ObjectSymbol};

const MAX_API_EVENT_ID: u32 = 511;
const DEFAULT_MESSAGE_SECTION: &str = ".sv-data";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetError {
    message: String,
}

impl AssetError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AssetError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElfSymbol {
    pub address: u64,
    pub size: u64,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemViewElf {
    rtt_control_block: Option<u64>,
    symbols: BTreeMap<u64, ElfSymbol>,
    messages: BTreeMap<u64, String>,
}

impl SystemViewElf {
    pub fn parse(data: &[u8]) -> Result<Self, AssetError> {
        let file = object::File::parse(data)
            .map_err(|error| AssetError::new(format!("invalid ELF file: {error}")))?;
        if file.format() != object::BinaryFormat::Elf {
            return Err(AssetError::new("application file is not an ELF binary"));
        }

        let mut symbols = BTreeMap::new();
        let mut rtt_control_block = None;
        for symbol in file.symbols().chain(file.dynamic_symbols()) {
            if !symbol.is_definition() || symbol.address() == 0 {
                continue;
            }
            let Ok(name) = symbol.name() else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            if name == "_SEGGER_RTT" {
                rtt_control_block = Some(symbol.address());
            }
            symbols
                .entry(symbol.address())
                .or_insert_with(|| ElfSymbol {
                    address: symbol.address(),
                    size: symbol.size(),
                    name: name.to_owned(),
                });
        }

        let messages = file
            .section_by_name(DEFAULT_MESSAGE_SECTION)
            .map(|section| extract_message_strings(section.address(), section.data()))
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            rtt_control_block,
            symbols,
            messages,
        })
    }

    pub fn rtt_control_block(&self) -> Option<u64> {
        self.rtt_control_block
    }

    pub fn symbols(&self) -> impl Iterator<Item = &ElfSymbol> {
        self.symbols.values()
    }

    pub fn symbol_name(&self, address: u64) -> Option<&str> {
        let (_, symbol) = self.symbols.range(..=address).next_back()?;
        let contains_address = symbol.address == address
            || (symbol.size > 0 && address.saturating_sub(symbol.address) < symbol.size);
        contains_address.then_some(symbol.name.as_str())
    }

    pub fn message(&self, address: u64) -> Option<&str> {
        self.messages.get(&address).map(String::as_str)
    }

    pub fn messages(&self) -> impl Iterator<Item = (u64, &str)> {
        self.messages
            .iter()
            .map(|(address, message)| (*address, message.as_str()))
    }

    pub fn decode_print_elf(
        &self,
        address: u64,
        payload: &[u8],
    ) -> Result<DecodedPrintElf, AssetError> {
        let format = self.message(address).ok_or_else(|| {
            AssetError::new(format!(
                "no {DEFAULT_MESSAGE_SECTION} message at address 0x{address:x}"
            ))
        })?;
        decode_print_elf(format, payload)
    }
}

fn extract_message_strings(
    section_address: u64,
    section_data: Result<&[u8], object::Error>,
) -> Result<BTreeMap<u64, String>, AssetError> {
    let data = section_data
        .map_err(|error| AssetError::new(format!("failed to read .sv-data: {error}")))?;
    let mut messages = BTreeMap::new();
    let mut start = 0_usize;
    for (index, byte) in data.iter().copied().enumerate() {
        if byte != 0 {
            continue;
        }
        if index > start {
            let bytes = &data[start..index];
            if let Ok(message) = std::str::from_utf8(bytes) {
                messages.insert(
                    section_address.saturating_add(start as u64),
                    message.to_owned(),
                );
            }
        }
        start = index.saturating_add(1);
    }
    if start < data.len() {
        let bytes = &data[start..];
        if let Ok(message) = std::str::from_utf8(bytes) {
            messages.insert(
                section_address.saturating_add(start as u64),
                message.to_owned(),
            );
        }
    }
    Ok(messages)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrintElfValue {
    Signed(i64),
    Unsigned(u64),
    Pointer(u64),
    String(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedPrintElf {
    pub format: String,
    pub values: Vec<PrintElfValue>,
}

impl DecodedPrintElf {
    pub fn render(&self) -> String {
        render_print_elf(&self.format, &self.values)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrintSpecifier {
    Signed,
    Unsigned,
    Pointer,
    String,
}

fn print_specifiers(format: &str) -> Vec<PrintSpecifier> {
    let bytes = format.as_bytes();
    let mut specifiers = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        index += 1;
        if index >= bytes.len() {
            break;
        }
        match bytes[index] {
            b'%' => {}
            b'd' | b'i' => specifiers.push(PrintSpecifier::Signed),
            b'u' => specifiers.push(PrintSpecifier::Unsigned),
            b'p' => specifiers.push(PrintSpecifier::Pointer),
            b's' => specifiers.push(PrintSpecifier::String),
            _ => {}
        }
        index += 1;
    }
    specifiers
}

fn decode_print_elf(format: &str, payload: &[u8]) -> Result<DecodedPrintElf, AssetError> {
    let specifiers = print_specifiers(format);
    let integer_count = specifiers
        .iter()
        .filter(|specifier| **specifier != PrintSpecifier::String)
        .count();
    let mut cursor = 0_usize;
    let mut integers = Vec::with_capacity(integer_count);
    for _ in 0..integer_count {
        integers.push(read_varint(payload, &mut cursor)?);
    }
    let string_count = specifiers.len().saturating_sub(integer_count);
    let mut strings = Vec::with_capacity(string_count);
    for _ in 0..string_count {
        strings.push(read_string(payload, &mut cursor)?);
    }
    if cursor != payload.len() {
        return Err(AssetError::new("Print-ELF payload has trailing bytes"));
    }

    let mut integer_index = 0_usize;
    let mut string_index = 0_usize;
    let mut values = Vec::with_capacity(specifiers.len());
    for specifier in specifiers {
        let value = match specifier {
            PrintSpecifier::Signed => {
                let raw = integers[integer_index];
                integer_index += 1;
                PrintElfValue::Signed(i64::from(raw as u32 as i32))
            }
            PrintSpecifier::Unsigned => {
                let raw = integers[integer_index];
                integer_index += 1;
                PrintElfValue::Unsigned(raw)
            }
            PrintSpecifier::Pointer => {
                let raw = integers[integer_index];
                integer_index += 1;
                PrintElfValue::Pointer(raw)
            }
            PrintSpecifier::String => {
                let value = strings[string_index].clone();
                string_index += 1;
                PrintElfValue::String(value)
            }
        };
        values.push(value);
    }
    Ok(DecodedPrintElf {
        format: format.to_owned(),
        values,
    })
}

fn render_print_elf(format: &str, values: &[PrintElfValue]) -> String {
    let bytes = format.as_bytes();
    let mut output = String::new();
    let mut literal_start = 0_usize;
    let mut index = 0_usize;
    let mut value_index = 0_usize;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        output.push_str(&format[literal_start..index]);
        index += 1;
        if index >= bytes.len() {
            output.push('%');
            literal_start = index;
            break;
        }
        if bytes[index] == b'%' {
            output.push('%');
        } else if let Some(value) = values.get(value_index) {
            match value {
                PrintElfValue::Signed(value) => output.push_str(&value.to_string()),
                PrintElfValue::Unsigned(value) => output.push_str(&value.to_string()),
                PrintElfValue::Pointer(value) => output.push_str(&format!("0x{value:08x}")),
                PrintElfValue::String(value) => output.push_str(value),
            }
            value_index += 1;
        } else {
            output.push('%');
            output.push(bytes[index] as char);
        }
        index += 1;
        literal_start = index;
    }
    output.push_str(&format[literal_start..]);
    output
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ApiFormatKind {
    Binary,
    Bytes,
    Signed,
    Time,
    Resource,
    Pointer,
    String,
    Task,
    Unsigned,
    Hex,
    Named(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ApiParameterFormat {
    label: String,
    kind: ApiFormatKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiEventDescription {
    pub event_id: u32,
    pub name: String,
    parameter_text: String,
    parameters: Vec<ApiParameterFormat>,
    return_text: Option<String>,
    return_parameter: Option<ApiParameterFormat>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiValue {
    Binary(u64),
    Bytes(Vec<u8>),
    Signed(i64),
    Time(u64),
    Resource(u64),
    Pointer(u64),
    String(String),
    Task(u64),
    Unsigned(u64),
    Hex(u64),
    Named {
        type_name: String,
        value: u64,
        name: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedApiParameter {
    pub label: String,
    pub value: ApiValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedApiEvent {
    pub event_id: u32,
    pub name: String,
    pub parameter_text: String,
    pub parameters: Vec<DecodedApiParameter>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemViewDescription {
    events: BTreeMap<u32, ApiEventDescription>,
    named_types: BTreeMap<String, BTreeMap<u64, String>>,
}

impl SystemViewDescription {
    pub fn parse(text: &str) -> Result<Self, AssetError> {
        let mut description = Self::default();
        for (line_index, source_line) in text.lines().enumerate() {
            let line_number = line_index + 1;
            let line = source_line
                .split_once('#')
                .map_or(source_line, |(content, _)| content)
                .trim();
            if line.is_empty() || line.starts_with("Option ") {
                continue;
            }
            if let Some(named_type) = line.strip_prefix("NamedType ") {
                description.parse_named_type(named_type, line_number)?;
            } else if line.starts_with("TaskState ") || line.starts_with("PreemptingTaskStates ") {
                continue;
            } else {
                description.parse_event(line, line_number)?;
            }
        }
        Ok(description)
    }

    pub fn events(&self) -> impl Iterator<Item = &ApiEventDescription> {
        self.events.values()
    }

    pub fn event(&self, event_id: u32) -> Option<&ApiEventDescription> {
        self.events.get(&event_id)
    }

    pub fn decode_api_call(
        &self,
        event_id: u32,
        payload: &[u8],
    ) -> Result<DecodedApiEvent, AssetError> {
        let description = self.events.get(&event_id).ok_or_else(|| {
            AssetError::new(format!(
                "event {event_id} is absent from the description file"
            ))
        })?;
        let mut cursor = 0_usize;
        let mut parameters = Vec::with_capacity(description.parameters.len());
        for parameter in &description.parameters {
            parameters.push(DecodedApiParameter {
                label: parameter.label.clone(),
                value: self.decode_value(&parameter.kind, payload, &mut cursor)?,
            });
        }
        if cursor != payload.len() {
            return Err(AssetError::new(format!(
                "event {event_id} payload has {} trailing byte(s)",
                payload.len().saturating_sub(cursor)
            )));
        }
        Ok(DecodedApiEvent {
            event_id,
            name: description.name.clone(),
            parameter_text: description.parameter_text.clone(),
            parameters,
        })
    }

    pub fn decode_api_return(
        &self,
        event_id: u32,
        return_value: Option<u64>,
    ) -> Result<Option<DecodedApiParameter>, AssetError> {
        let description = self.events.get(&event_id).ok_or_else(|| {
            AssetError::new(format!(
                "event {event_id} is absent from the description file"
            ))
        })?;
        let Some(format) = &description.return_parameter else {
            return Ok(None);
        };
        let Some(value) = return_value else {
            return Ok(None);
        };
        Ok(Some(DecodedApiParameter {
            label: format.label.clone(),
            value: self.value_from_u64(&format.kind, value)?,
        }))
    }

    pub fn return_text(&self, event_id: u32) -> Option<&str> {
        self.events.get(&event_id)?.return_text.as_deref()
    }

    fn parse_named_type(&mut self, line: &str, line_number: usize) -> Result<(), AssetError> {
        let mut fields = line.split_whitespace();
        let type_name = fields.next().ok_or_else(|| {
            AssetError::new(format!("description line {line_number}: missing type name"))
        })?;
        let values = self.named_types.entry(type_name.to_owned()).or_default();
        for field in fields {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            if key == "*" {
                continue;
            }
            let key = parse_description_number(key).ok_or_else(|| {
                AssetError::new(format!(
                    "description line {line_number}: invalid named value {key}"
                ))
            })?;
            values.insert(
                key,
                value.trim_matches('"').trim_end_matches(',').to_owned(),
            );
        }
        Ok(())
    }

    fn parse_event(&mut self, line: &str, line_number: usize) -> Result<(), AssetError> {
        let event_id_text = line.split_whitespace().next().unwrap_or_default();
        let after_event_id = line[event_id_text.len()..].trim_start();
        let name = after_event_id.split_whitespace().next().unwrap_or_default();
        let formats = after_event_id[name.len()..].trim_start();
        let event_id = event_id_text.parse::<u32>().map_err(|_| {
            AssetError::new(format!(
                "description line {line_number}: invalid event ID {event_id_text}"
            ))
        })?;
        if event_id > MAX_API_EVENT_ID {
            return Ok(());
        }
        if name.is_empty() {
            return Err(AssetError::new(format!(
                "description line {line_number}: missing function name"
            )));
        }
        let (parameter_text, return_text) = formats
            .split_once('|')
            .map_or((formats, None), |(parameters, returns)| {
                (parameters.trim(), Some(returns.trim()))
            });
        let parameters = parse_api_formats(parameter_text, line_number)?;
        let return_parameter = return_text
            .map(|text| parse_api_formats(text, line_number))
            .transpose()?
            .and_then(|mut formats| formats.drain(..).next());
        let event = ApiEventDescription {
            event_id,
            name: name.to_owned(),
            parameter_text: parameter_text.to_owned(),
            parameters,
            return_text: return_text.map(ToOwned::to_owned),
            return_parameter,
        };
        self.events.insert(event_id, event);
        Ok(())
    }

    fn decode_value(
        &self,
        kind: &ApiFormatKind,
        payload: &[u8],
        cursor: &mut usize,
    ) -> Result<ApiValue, AssetError> {
        match kind {
            ApiFormatKind::Bytes => Ok(ApiValue::Bytes(read_bytes(payload, cursor)?.to_vec())),
            ApiFormatKind::String => Ok(ApiValue::String(read_string(payload, cursor)?)),
            _ => self.value_from_u64(kind, read_varint(payload, cursor)?),
        }
    }

    fn value_from_u64(&self, kind: &ApiFormatKind, value: u64) -> Result<ApiValue, AssetError> {
        Ok(match kind {
            ApiFormatKind::Binary => ApiValue::Binary(value),
            ApiFormatKind::Signed => ApiValue::Signed(i64::from(value as u32 as i32)),
            ApiFormatKind::Time => ApiValue::Time(value),
            ApiFormatKind::Resource => ApiValue::Resource(value),
            ApiFormatKind::Pointer => ApiValue::Pointer(value),
            ApiFormatKind::Task => ApiValue::Task(value),
            ApiFormatKind::Unsigned => ApiValue::Unsigned(value),
            ApiFormatKind::Hex => ApiValue::Hex(value),
            ApiFormatKind::Named(type_name) => ApiValue::Named {
                type_name: type_name.clone(),
                value,
                name: self
                    .named_types
                    .get(type_name)
                    .and_then(|values| values.get(&value))
                    .cloned(),
            },
            ApiFormatKind::Bytes | ApiFormatKind::String => {
                return Err(AssetError::new(
                    "byte and string values require length-prefixed payload data",
                ));
            }
        })
    }
}

fn parse_api_formats(
    text: &str,
    line_number: usize,
) -> Result<Vec<ApiParameterFormat>, AssetError> {
    let bytes = text.as_bytes();
    let mut formats = Vec::new();
    let mut search_start = 0_usize;
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'%') {
            index += 2;
            continue;
        }
        let label = parameter_label(&text[search_start..index]);
        index += 1;
        let specifier_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
        {
            index += 1;
        }
        if specifier_start == index {
            return Err(AssetError::new(format!(
                "description line {line_number}: empty parameter formatter"
            )));
        }
        let specifier = &text[specifier_start..index];
        formats.push(ApiParameterFormat {
            label,
            kind: parse_api_format_kind(specifier),
        });
        search_start = index;
    }
    Ok(formats)
}

fn parameter_label(text: &str) -> String {
    text.trim()
        .trim_end_matches(['=', ':'])
        .split_whitespace()
        .next_back()
        .unwrap_or_default()
        .to_owned()
}

fn parse_api_format_kind(specifier: &str) -> ApiFormatKind {
    match specifier {
        "b" => ApiFormatKind::Binary,
        "B" => ApiFormatKind::Bytes,
        "d" | "i" => ApiFormatKind::Signed,
        "D" => ApiFormatKind::Time,
        "I" => ApiFormatKind::Resource,
        "p" => ApiFormatKind::Pointer,
        "s" => ApiFormatKind::String,
        "t" | "T" => ApiFormatKind::Task,
        "u" | "us" => ApiFormatKind::Unsigned,
        "x" => ApiFormatKind::Hex,
        named => ApiFormatKind::Named(named.to_owned()),
    }
}

fn read_varint(data: &[u8], cursor: &mut usize) -> Result<u64, AssetError> {
    let mut value = 0_u64;
    let mut shift = 0_u32;
    loop {
        let byte = data.get(*cursor).copied().ok_or_else(|| {
            AssetError::new("truncated SystemView value in description-driven payload")
        })?;
        *cursor += 1;
        let part = u64::from(byte & 0x7f);
        if shift >= 64 || part.checked_shl(shift).is_none() {
            return Err(AssetError::new("SystemView value exceeds 64 bits"));
        }
        value |= part << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
}

fn read_bytes<'a>(data: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], AssetError> {
    let length = usize::try_from(read_varint(data, cursor)?)
        .map_err(|_| AssetError::new("SystemView byte array length exceeds host limits"))?;
    let end = cursor
        .checked_add(length)
        .ok_or_else(|| AssetError::new("SystemView byte array length overflows"))?;
    let bytes = data
        .get(*cursor..end)
        .ok_or_else(|| AssetError::new("truncated SystemView byte array"))?;
    *cursor = end;
    Ok(bytes)
}

fn read_string(data: &[u8], cursor: &mut usize) -> Result<String, AssetError> {
    let bytes = read_bytes(data, cursor)?;
    String::from_utf8(bytes.to_vec())
        .map_err(|_| AssetError::new("SystemView string is not valid UTF-8"))
}

fn parse_u64(text: &str) -> Option<u64> {
    text.strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .map_or_else(
            || text.parse().ok(),
            |hex| u64::from_str_radix(hex, 16).ok(),
        )
}

fn parse_description_number(text: &str) -> Option<u64> {
    if let Some(value) = text.strip_prefix('-') {
        let magnitude = value.parse::<i64>().ok()?;
        let signed = magnitude.checked_neg()?;
        let signed = i32::try_from(signed).ok()?;
        return Some(u64::from(signed as u32));
    }
    parse_u64(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::write::{Object as WritableObject, Symbol, SymbolSection};
    use object::{Architecture, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope};

    fn encoded_values(values: &[u64]) -> Vec<u8> {
        let mut encoded = Vec::new();
        for value in values.iter().copied() {
            let mut value = value;
            loop {
                let mut byte = (value & 0x7f) as u8;
                value >>= 7;
                if value != 0 {
                    byte |= 0x80;
                }
                encoded.push(byte);
                if value == 0 {
                    break;
                }
            }
        }
        encoded
    }

    #[test]
    fn description_decodes_typed_api_values_and_named_types() {
        let description = SystemViewDescription::parse(
            "# example\nNamedType Result 0=OK 1=BUSY\n32     QueueSend     Queue=%I Count=%u Name=%s | Returns %Result\n",
        )
        .expect("description parses");
        let mut payload = encoded_values(&[0x20, 3]);
        payload.extend(encoded_values(&[4]));
        payload.extend_from_slice(b"work");

        let decoded = description
            .decode_api_call(32, &payload)
            .expect("API call decodes");

        assert_eq!(decoded.name, "QueueSend");
        assert_eq!(
            decoded.parameters,
            vec![
                DecodedApiParameter {
                    label: "Queue".to_owned(),
                    value: ApiValue::Resource(0x20),
                },
                DecodedApiParameter {
                    label: "Count".to_owned(),
                    value: ApiValue::Unsigned(3),
                },
                DecodedApiParameter {
                    label: "Name".to_owned(),
                    value: ApiValue::String("work".to_owned()),
                },
            ]
        );
        assert_eq!(
            description
                .decode_api_return(32, Some(1))
                .expect("return decodes"),
            Some(DecodedApiParameter {
                label: "Returns".to_owned(),
                value: ApiValue::Named {
                    type_name: "Result".to_owned(),
                    value: 1,
                    name: Some("BUSY".to_owned()),
                },
            })
        );
    }

    #[test]
    fn description_ignores_api_ids_above_the_supported_analysis_range() {
        let description = SystemViewDescription::parse("512 ModuleEvent Value=%u")
            .expect("higher module events do not invalidate the description");
        assert!(description.event(512).is_none());
    }

    #[test]
    fn description_accepts_systemview_task_state_and_named_type_extensions() {
        let description = SystemViewDescription::parse(
            "Option ReversePriority\n\
             NamedType ErrCode *=%i 0=OK -1=FAILED\n\
             TaskState 0xFF 4=Delayed, 27=Suspended\n\
             PreemptingTaskStates 0\n\
             59 Wait Task=%T Timeout=%us | Returns %ErrCode\n",
        )
        .expect("real-world extensions parse");
        let payload = encoded_values(&[7, 2]);

        let call = description
            .decode_api_call(59, &payload)
            .expect("extended formatters decode");
        let returned = description
            .decode_api_return(59, Some(u64::from(u32::MAX)))
            .expect("signed named return decodes")
            .expect("return is described");

        assert_eq!(call.parameters[0].value, ApiValue::Task(7));
        assert_eq!(call.parameters[1].value, ApiValue::Unsigned(2));
        assert_eq!(
            returned.value,
            ApiValue::Named {
                type_name: "ErrCode".to_owned(),
                value: u64::from(u32::MAX),
                name: Some("FAILED".to_owned()),
            }
        );
    }

    #[test]
    fn print_elf_decodes_integer_block_before_string_block() {
        let mut payload = encoded_values(&[7, 0x2000_0100]);
        payload.extend(encoded_values(&[5]));
        payload.extend_from_slice(b"ready");

        let decoded = decode_print_elf("count=%u name=%s ptr=%p", &payload)
            .expect("Print-ELF payload decodes");

        assert_eq!(
            decoded.values,
            vec![
                PrintElfValue::Unsigned(7),
                PrintElfValue::String("ready".to_owned()),
                PrintElfValue::Pointer(0x2000_0100),
            ]
        );
        assert_eq!(decoded.render(), "count=7 name=ready ptr=0x20000100");
    }

    #[test]
    fn message_strings_preserve_section_addresses() {
        let messages =
            extract_message_strings(0x1000, Ok(b"first\0\0second=%u\0")).expect("messages extract");

        assert_eq!(messages.get(&0x1000).map(String::as_str), Some("first"));
        assert_eq!(messages.get(&0x1007).map(String::as_str), Some("second=%u"));
    }

    #[test]
    fn elf_extracts_rtt_symbol_resource_symbols_and_message_section() {
        let mut object = WritableObject::new(
            object::BinaryFormat::Elf,
            Architecture::Arm,
            Endianness::Little,
        );
        let messages = object.add_section(
            Vec::new(),
            DEFAULT_MESSAGE_SECTION.as_bytes().to_vec(),
            SectionKind::ReadOnlyData,
        );
        object.append_section_data(messages, b"count=%u\0", 1);
        object.add_symbol(Symbol {
            name: b"_SEGGER_RTT".to_vec(),
            value: 0x2000_0100,
            size: 128,
            kind: SymbolKind::Data,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Section(messages),
            flags: SymbolFlags::None,
        });
        object.add_symbol(Symbol {
            name: b"queue_control".to_vec(),
            value: 0x2000_0200,
            size: 32,
            kind: SymbolKind::Data,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Section(messages),
            flags: SymbolFlags::None,
        });
        let bytes = object.write().expect("test ELF writes");

        let elf = SystemViewElf::parse(&bytes).expect("test ELF parses");

        assert_eq!(elf.rtt_control_block(), Some(0x2000_0100));
        assert_eq!(elf.symbol_name(0x2000_0204), Some("queue_control"));
        assert_eq!(elf.message(0), Some("count=%u"));
    }
}
