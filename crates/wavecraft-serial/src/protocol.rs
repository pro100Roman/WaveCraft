use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use wavecraft_core::CaptureError;

pub const MAX_TEXT_FIELDS: usize = 10;
pub const MAX_TEXT_SEPARATOR_BYTES: usize = 3;

/// A literal separator is matched immediately after the field name. An empty
/// separator keeps the automatic name/value matching used by `TextDecoder::feed`.
#[derive(Clone, Copy)]
pub struct TextFieldSpec<'a> {
    pub name: &'a str,
    pub separator: &'a str,
}

pub fn validate_text_separator(separator: &str) -> Result<(), CaptureError> {
    if !separator.is_ascii() || separator.len() > MAX_TEXT_SEPARATOR_BYTES {
        return Err("Text field separators must contain at most 3 ASCII characters".into());
    }
    Ok(())
}
const MAX_FRAME_BYTES: usize = 65_536;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ByteOrder {
    #[default]
    Little,
    Big,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    #[default]
    Unsigned,
    Signed,
    Float,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BitField {
    pub name: String,
    pub bit_offset: u8,
    pub bit_width: u8,
    #[serde(default)]
    pub signed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryField {
    pub name: String,
    pub size_bytes: usize,
    #[serde(default)]
    pub kind: FieldKind,
    #[serde(default)]
    pub bits: Vec<BitField>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BinarySchema {
    pub start_word_bytes: usize,
    pub stop_word_bytes: usize,
    #[serde(default)]
    pub byte_order: ByteOrder,
    pub fields: Vec<BinaryField>,
}

impl BinarySchema {
    pub fn from_json(json: &str) -> Result<Self, CaptureError> {
        if json.len() > 1_048_576 {
            return Err("JSON schema exceeds 1 MiB".into());
        }
        let schema: Self = serde_json::from_str(json)
            .map_err(|error| CaptureError::new(format!("Invalid serial schema: {error}")))?;
        schema.validate()?;
        Ok(schema)
    }

    pub fn validate(&self) -> Result<(), CaptureError> {
        if !(1..=32).contains(&self.start_word_bytes) || !(1..=32).contains(&self.stop_word_bytes) {
            return Err("Start and stop word lengths must be 1–32 bytes".into());
        }
        if self.fields.is_empty() || self.fields.len() > 64 {
            return Err("Schema requires 1–64 fields".into());
        }
        let mut names = HashSet::new();
        for field in &self.fields {
            if field.name.trim().is_empty() || !names.insert(field.name.clone()) {
                return Err("Field names must be nonempty and unique".into());
            }
            if !(1..=8).contains(&field.size_bytes)
                || (field.kind == FieldKind::Float && !matches!(field.size_bytes, 4 | 8))
            {
                return Err(CaptureError::new(format!(
                    "Invalid byte size for {} (integers: 1–8, floats: 4 or 8)",
                    field.name
                )));
            }
            if field.bits.len() > 64 || (field.kind == FieldKind::Float && !field.bits.is_empty()) {
                return Err("Bit fields require an integer parent and at most 64 entries".into());
            }
            for bit in &field.bits {
                if bit.name.trim().is_empty()
                    || !names.insert(bit.name.clone())
                    || bit.bit_width == 0
                    || usize::from(bit.bit_offset) + usize::from(bit.bit_width)
                        > field.size_bytes * 8
                {
                    return Err(CaptureError::new(format!(
                        "Invalid or duplicate bit field {}",
                        bit.name
                    )));
                }
            }
        }
        if self.channel_names().len() > 128 {
            return Err("Schema exceeds 128 plotted fields".into());
        }
        Ok(())
    }

    pub fn channel_names(&self) -> Vec<String> {
        self.fields
            .iter()
            .flat_map(|field| {
                if field.bits.is_empty() {
                    vec![field.name.clone()]
                } else {
                    field.bits.iter().map(|bit| bit.name.clone()).collect()
                }
            })
            .collect()
    }

    fn payload_bytes(&self) -> usize {
        self.fields.iter().map(|field| field.size_bytes).sum()
    }

    fn decode(&self, payload: &[u8]) -> Vec<f64> {
        let mut values = Vec::new();
        let mut offset = 0;
        for field in &self.fields {
            let raw = integer(&payload[offset..offset + field.size_bytes], self.byte_order);
            offset += field.size_bytes;
            if field.bits.is_empty() {
                let value = match field.kind {
                    FieldKind::Unsigned => raw as f64,
                    FieldKind::Signed => signed(raw, (field.size_bytes * 8) as u8),
                    FieldKind::Float if field.size_bytes == 4 => f32::from_bits(raw as u32) as f64,
                    FieldKind::Float => f64::from_bits(raw),
                };
                values.push(value);
            } else {
                for bit in &field.bits {
                    let value = (raw >> bit.bit_offset) & mask(bit.bit_width);
                    values.push(if bit.signed {
                        signed(value, bit.bit_width)
                    } else {
                        value as f64
                    });
                }
            }
        }
        values
    }
}

fn mask(bits: u8) -> u64 {
    u64::MAX >> (64 - bits)
}
fn signed(raw: u64, bits: u8) -> f64 {
    ((raw << (64 - bits)) as i64 >> (64 - bits)) as f64
}
fn integer(bytes: &[u8], order: ByteOrder) -> u64 {
    match order {
        ByteOrder::Little => bytes.iter().enumerate().fold(0, |value, (index, byte)| {
            value | (u64::from(*byte) << (index * 8))
        }),
        ByteOrder::Big => bytes
            .iter()
            .fold(0, |value, byte| (value << 8) | u64::from(*byte)),
    }
}

pub fn parse_hex_bytes(text: &str) -> Result<Vec<u8>, CaptureError> {
    let text = text
        .trim()
        .strip_prefix("0x")
        .or_else(|| text.trim().strip_prefix("0X"))
        .unwrap_or(text.trim());
    let compact: String = text
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '_')
        .collect();
    if compact.len() % 2 != 0 || !compact.is_ascii() {
        return Err("Hex words require pairs of hexadecimal digits".into());
    }
    (0..compact.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&compact[index..index + 2], 16)
                .map_err(|_| CaptureError::new("Invalid hexadecimal word"))
        })
        .collect()
}

pub fn parse_hex_u32(text: &str) -> Result<u32, CaptureError> {
    let text = text.trim();
    u32::from_str_radix(
        text.strip_prefix("0x")
            .or_else(|| text.strip_prefix("0X"))
            .unwrap_or(text),
        16,
    )
    .map_err(|_| CaptureError::new("Enter a hexadecimal integer (up to 32 bits)"))
}

#[derive(Clone, Debug)]
pub struct CrcConfiguration {
    pub width: u8,
    /// Normal polynomial with the implicit highest term omitted, even for reflected CRCs.
    pub polynomial: u32,
    pub seed: u32,
    pub reflect_input: bool,
    pub reflect_output: bool,
    pub xor_output: u32,
    pub byte_order: ByteOrder,
    pub include_start_word: bool,
}

impl CrcConfiguration {
    pub fn validate(&self) -> Result<(), CaptureError> {
        if !matches!(self.width, 8 | 16 | 32) {
            return Err("CRC width must be 8, 16, or 32".into());
        }
        let mask = mask(self.width);
        if self.polynomial == 0
            || [self.polynomial, self.seed, self.xor_output]
                .iter()
                .any(|value| u64::from(*value) > mask)
        {
            return Err("CRC polynomial, seed and final XOR must fit the selected width; polynomial cannot be zero".into());
        }
        Ok(())
    }

    pub fn checksum(&self, data: &[u8]) -> Result<u32, CaptureError> {
        self.validate()?;
        let mask = mask(self.width) as u32;
        let high = 1u32 << (self.width - 1);
        let mut crc = self.seed;
        for byte in data {
            let byte = if self.reflect_input {
                byte.reverse_bits()
            } else {
                *byte
            };
            crc ^= u32::from(byte) << (self.width - 8);
            for _ in 0..8 {
                crc = if crc & high != 0 {
                    (crc << 1) ^ self.polynomial
                } else {
                    crc << 1
                };
            }
            crc &= mask;
        }
        if self.reflect_output {
            crc = crc.reverse_bits() >> (32 - self.width);
        }
        Ok((crc ^ self.xor_output) & mask)
    }
}

pub struct BinaryDecoder {
    schema: BinarySchema,
    start: Vec<u8>,
    stop: Vec<u8>,
    crc: CrcConfiguration,
    pending: Vec<u8>,
    pub rejected_frames: u64,
    pub discarded_bytes: u64,
}

impl BinaryDecoder {
    pub fn clear(&mut self) {
        self.pending.clear();
        self.rejected_frames = 0;
        self.discarded_bytes = 0;
    }
    pub fn new(
        schema: BinarySchema,
        start: Vec<u8>,
        stop: Vec<u8>,
        crc: CrcConfiguration,
    ) -> Result<Self, CaptureError> {
        schema.validate()?;
        crc.validate()?;
        if start.len() != schema.start_word_bytes || stop.len() != schema.stop_word_bytes {
            return Err("Hex word lengths must match the JSON schema".into());
        }
        Ok(Self {
            schema,
            start,
            stop,
            crc,
            pending: Vec::new(),
            rejected_frames: 0,
            discarded_bytes: 0,
        })
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<f64>> {
        let mut records = Vec::new();
        let payload_end = self.start.len() + self.schema.payload_bytes();
        let crc_end = payload_end + usize::from(self.crc.width / 8);
        let frame_bytes = crc_end + self.stop.len();
        for chunk in bytes.chunks(4096) {
            self.pending.extend_from_slice(chunk);
            let mut offset = 0;
            while self.pending.len() - offset >= self.start.len() {
                if !self.pending[offset..].starts_with(&self.start) {
                    offset += 1;
                    self.discarded_bytes += 1;
                    continue;
                }
                if self.pending.len() - offset < frame_bytes {
                    break;
                }
                let frame = &self.pending[offset..offset + frame_bytes];
                let crc_start = if self.crc.include_start_word {
                    0
                } else {
                    self.start.len()
                };
                let valid = frame[crc_end..] == self.stop
                    && self
                        .crc
                        .checksum(&frame[crc_start..payload_end])
                        .ok()
                        .map(u64::from)
                        == Some(integer(&frame[payload_end..crc_end], self.crc.byte_order));
                if valid {
                    records.push(self.schema.decode(&frame[self.start.len()..payload_end]));
                    offset += frame_bytes;
                } else {
                    self.rejected_frames += 1;
                    offset += 1;
                }
            }
            self.pending.drain(..offset);
        }
        records
    }
}

/// ASCII records end at LF or CR. Incomplete records survive USB/read boundaries.
#[derive(Default)]
pub struct TextDecoder {
    pending: Vec<u8>,
    dropping: bool,
    pub oversized_records: u64,
}

impl TextDecoder {
    pub fn feed(&mut self, bytes: &[u8], fields: &[String]) -> Vec<Vec<Option<f64>>> {
        let fields: Vec<_> = fields
            .iter()
            .map(|name| TextFieldSpec {
                name,
                separator: "",
            })
            .collect();
        self.feed_with_fields(bytes, &fields)
    }

    pub fn feed_with_fields(
        &mut self,
        bytes: &[u8],
        fields: &[TextFieldSpec<'_>],
    ) -> Vec<Vec<Option<f64>>> {
        let mut records = Vec::new();
        for byte in bytes {
            if matches!(*byte, b'\r' | b'\n') {
                if !self.dropping && !self.pending.is_empty() {
                    let line = String::from_utf8_lossy(&self.pending);
                    records.push(
                        fields
                            .iter()
                            .take(MAX_TEXT_FIELDS)
                            .map(|field| text_value(&line, field.name, field.separator))
                            .collect(),
                    );
                }
                self.pending.clear();
                self.dropping = false;
            } else if !self.dropping {
                if self.pending.len() == MAX_FRAME_BYTES {
                    self.pending.clear();
                    self.dropping = true;
                    self.oversized_records += 1;
                } else {
                    self.pending.push(*byte);
                }
            }
        }
        records
    }
}

fn text_value(line: &str, field: &str, separator: &str) -> Option<f64> {
    let field = field.trim();
    if field.is_empty() || validate_text_separator(separator).is_err() {
        return None;
    }
    for (index, _) in line.match_indices(field) {
        let boundary = |c: char| c.is_ascii_alphanumeric() || c == '_';
        if line[..index].chars().next_back().is_some_and(boundary) {
            continue;
        }
        let tail = &line[index + field.len()..];
        let tail = if separator.is_empty() {
            if tail.chars().next().is_some_and(boundary) {
                continue;
            }
            tail.trim_start_matches(|c: char| {
                c.is_ascii_whitespace() || matches!(c, '"' | '\'' | ':' | '=')
            })
        } else {
            let Some(tail) = tail.strip_prefix(separator) else {
                continue;
            };
            tail.trim_start_matches(|c: char| c.is_ascii_whitespace())
        };
        let end = tail
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E')))
            .unwrap_or(tail.len());
        if let Ok(value) = tail[..end].parse::<f64>() {
            if value.is_finite() {
                return Some(value);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_field_separators_survive_fragmented_records_and_mix_with_auto_matching() {
        let fields = [
            TextFieldSpec {
                name: "temperature",
                separator: "->",
            },
            TextFieldSpec {
                name: "rpm",
                separator: "[=]",
            },
            TextFieldSpec {
                name: "enabled",
                separator: "",
            },
        ];
        let bytes = b"temperature-> -2.5e1 rpm[=]1200 enabled: 1\r\ntemperature=99 rpm=99 {\"enabled\":0}\n";
        let expected = vec![
            vec![Some(-25.0), Some(1200.0), Some(1.0)],
            vec![None, None, Some(0.0)],
        ];
        for split in 0..=bytes.len() {
            let mut decoder = TextDecoder::default();
            let mut records = decoder.feed_with_fields(&bytes[..split], &fields);
            records.extend(decoder.feed_with_fields(&bytes[split..], &fields));
            assert_eq!(records, expected, "split {split}");
        }
    }

    #[test]
    fn custom_separators_are_literal_ascii_including_spaces_letters_and_digits() {
        for separator in ["=", "->", " : ", "abc", "123", "\t", "[=]", "\0", "\x1b["] {
            validate_text_separator(separator).unwrap();
            let line = format!("temp{separator}-2.5e+1");
            assert_eq!(
                text_value(&line, "temp", separator),
                Some(-25.0),
                "{separator:?}"
            );
        }
        assert_eq!(
            text_value("attempt->1 temperature->2 temp->3", "temp", "->"),
            Some(3.0)
        );
        assert_eq!(text_value("temp ->3", "temp", "->"), None);
        assert_eq!(text_value("temp=3", "temp", "->"), None);
        for line in ["temp=3", "temp: 3", "temp 3", "{\"temp\":3}"] {
            assert_eq!(text_value(line, "temp", ""), Some(3.0));
        }
        for separator in ["abcd", "→", "é"] {
            assert!(validate_text_separator(separator).is_err());
            assert_eq!(text_value("temp=3", "temp", separator), None);
        }
    }

    fn crc(width: u8, poly: u32, seed: u32, reflect: bool, xor: u32) -> CrcConfiguration {
        CrcConfiguration {
            width,
            polynomial: poly,
            seed,
            reflect_input: reflect,
            reflect_output: reflect,
            xor_output: xor,
            byte_order: ByteOrder::Little,
            include_start_word: false,
        }
    }

    fn schema() -> BinarySchema {
        BinarySchema::from_json(
            r#"{
            "start_word_bytes":2,"stop_word_bytes":2,"byte_order":"little",
            "fields":[
                {"name":"temperature","size_bytes":4,"kind":"float"},
                {"name":"position","size_bytes":3,"kind":"signed"},
                {"name":"status","size_bytes":1,"bits":[
                    {"name":"enabled","bit_offset":0,"bit_width":1},
                    {"name":"delta","bit_offset":1,"bit_width":3,"signed":true},
                    {"name":"mode","bit_offset":4,"bit_width":4}
                ]}
            ]}"#,
        )
        .unwrap()
    }

    fn packet(crc: &CrcConfiguration) -> Vec<u8> {
        let mut packet = vec![0xaa, 0x55];
        packet.extend(23.5_f32.to_le_bytes());
        packet.extend([0xfd, 0xff, 0xff, 0x7b]); // -3, enabled=1, delta=-3, mode=7
        let checksum = crc
            .checksum(&packet[if crc.include_start_word { 0 } else { 2 }..])
            .unwrap();
        let bytes = if crc.byte_order == ByteOrder::Little {
            checksum.to_le_bytes().to_vec()
        } else {
            checksum.to_be_bytes()[4 - usize::from(crc.width / 8)..].to_vec()
        };
        packet.extend(&bytes[..usize::from(crc.width / 8)]);
        packet.extend([0x0d, 0x0a]);
        packet
    }

    #[test]
    fn crc_matches_independent_catalogue_check_values() {
        for (config, expected) in [
            (crc(8, 0x07, 0, false, 0), 0xf4),
            (crc(8, 0x31, 0, true, 0), 0xa1),
            (crc(16, 0x1021, 0xffff, false, 0), 0x29b1),
            (crc(16, 0x8005, 0xffff, true, 0), 0x4b37),
            (
                crc(32, 0x04c11db7, 0xffffffff, true, 0xffffffff),
                0xcbf43926,
            ),
        ] {
            assert_eq!(config.checksum(b"123456789").unwrap(), expected);
        }
    }

    #[test]
    fn binary_records_survive_every_read_boundary_and_crc_byte_order() {
        for width in [8, 16, 32] {
            for order in [ByteOrder::Little, ByteOrder::Big] {
                for include_start in [false, true] {
                    let mut config = crc(width, 7, 0, false, 0);
                    config.byte_order = order;
                    config.include_start_word = include_start;
                    let bytes = packet(&config);
                    for split in 0..=bytes.len() {
                        let mut decoder = BinaryDecoder::new(
                            schema(),
                            vec![0xaa, 0x55],
                            vec![0x0d, 0x0a],
                            config.clone(),
                        )
                        .unwrap();
                        let mut records = decoder.feed(&bytes[..split]);
                        records.extend(decoder.feed(&bytes[split..]));
                        assert_eq!(records, vec![vec![23.5, -3.0, 1.0, -3.0, 7.0]]);
                    }
                }
            }
        }
    }

    #[test]
    fn binary_resynchronizes_after_noise_bad_crc_and_bad_stop_word() {
        let config = crc(16, 0x1021, 0xffff, false, 0);
        let valid = packet(&config);
        let mut bad_crc = valid.clone();
        bad_crc[3] ^= 1;
        let mut bad_stop = valid.clone();
        *bad_stop.last_mut().unwrap() = 0;
        let mut bytes = vec![0, 0xaa, 0xaa];
        bytes.extend(bad_crc);
        bytes.extend(bad_stop);
        bytes.extend(&valid);
        bytes.extend(&valid);
        let mut decoder =
            BinaryDecoder::new(schema(), vec![0xaa, 0x55], vec![13, 10], config).unwrap();
        let records: Vec<_> = bytes
            .chunks(3)
            .flat_map(|chunk| decoder.feed(chunk))
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(decoder.rejected_frames, 2);
        assert!(decoder.discarded_bytes > 0);
        assert!(decoder.pending.is_empty());
    }

    #[test]
    fn big_endian_scalars_and_full_width_bit_fields_decode() {
        let schema = BinarySchema::from_json(r#"{"start_word_bytes":1,"stop_word_bytes":1,"byte_order":"big","fields":[
            {"name":"wide","size_bytes":8,"bits":[{"name":"signed","bit_offset":0,"bit_width":64,"signed":true}]},
            {"name":"float","size_bytes":8,"kind":"float"}]}"#).unwrap();
        let mut payload = (-42_i64).to_be_bytes().to_vec();
        payload.extend(1.25_f64.to_be_bytes());
        assert_eq!(schema.decode(&payload), vec![-42.0, 1.25]);
    }

    #[test]
    fn rejects_invalid_schema_words_and_crc_without_panics() {
        let base = schema();
        for size in [0, 9, usize::MAX] {
            let mut invalid = base.clone();
            invalid.fields[0].size_bytes = size;
            assert!(invalid.validate().is_err());
        }
        for (offset, width) in [(0, 0), (7, 2), (255, 255)] {
            let mut invalid = base.clone();
            invalid.fields[2].bits[0].bit_offset = offset;
            invalid.fields[2].bits[0].bit_width = width;
            assert!(invalid.validate().is_err());
        }
        let mut duplicate = base.clone();
        duplicate.fields[2].bits[0].name = "temperature".into();
        assert!(duplicate.validate().is_err());
        assert!(BinarySchema::from_json("{}").is_err());
        assert!(BinarySchema::from_json(
            r#"{"start_word_bytes":1,"stop_word_bytes":1,"fields":[],"typo":true}"#
        )
        .is_err());
        assert!(
            BinaryDecoder::new(base, vec![0xaa], vec![13, 10], crc(8, 7, 0, false, 0)).is_err()
        );
        for config in [
            crc(0, 7, 0, false, 0),
            crc(64, 7, 0, false, 0),
            crc(8, 0, 0, false, 0),
            crc(8, 0x100, 0, false, 0),
            crc(8, 7, 256, false, 0),
        ] {
            assert!(config.checksum(b"data").is_err());
        }
        assert_eq!(
            parse_hex_bytes("0xAA 55_0d0A").unwrap(),
            [0xaa, 0x55, 13, 10]
        );
        for invalid in ["a", "xx", "🦀", "０１"] {
            assert!(parse_hex_bytes(invalid).is_err());
        }
    }

    #[test]
    fn text_fields_handle_fragmented_records_exponents_and_name_boundaries() {
        let mut decoder = TextDecoder::default();
        let fields = vec!["temp".into(), "rpm".into(), "missing".into()];
        assert!(decoder.feed(b"attempt=99 temp", &fields).is_empty());
        let records = decoder.feed(
            b"erature=100 temp: -1.25e+2, rpm=42\r\n{\"temp\":2.5,\"rpm\":1e3}\n",
            &fields,
        );
        assert_eq!(
            records,
            vec![
                vec![Some(-125.0), Some(42.0), None],
                vec![Some(2.5), Some(1000.0), None]
            ]
        );
        assert_eq!(
            text_value("attempt=3 temperature=3 temp=NaN temp=4", "temp", ""),
            Some(4.0)
        );
        assert_eq!(text_value("temp=1e999", "temp", ""), None);
        assert_eq!(text_value("temp=42", "", ""), None);
    }

    #[test]
    fn bounds_unterminated_text_and_binary_noise_then_recovers() {
        let mut text = TextDecoder::default();
        let fields = vec!["temp".into()];
        assert!(text.feed(&vec![b'x'; 1_000_000], &fields).is_empty());
        assert!(text.pending.is_empty());
        assert_eq!(text.oversized_records, 1);
        assert_eq!(text.feed(b"\ntemp=3\n", &fields), vec![vec![Some(3.0)]]);
        let config = crc(8, 7, 0, false, 0);
        let mut binary =
            BinaryDecoder::new(schema(), vec![0xaa, 0x55], vec![13, 10], config.clone()).unwrap();
        assert!(binary.feed(&vec![0; 1_000_000]).is_empty());
        assert!(binary.pending.len() < 2);
        assert_eq!(binary.feed(&packet(&config)).len(), 1);
    }
}
