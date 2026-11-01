use crate::{CompletionStatus, RecordingError, RecordingReader, RecordingWriter};
use dsview_core::{
    CaptureBatch, CaptureEvent, CaptureFrame, DeviceDescriptor, DeviceId, InstrumentKind,
    LogicBatch, ScopeBatch,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::{self, Read, Seek, Write};
use std::time::Duration;
use zip::result::ZipError;
use zip::write::FileOptions;

const SALEAE_MAGIC: [u8; 8] = *b"<SALEAE>";
const SALEAE_BINARY_VERSION: u32 = 3;
const SALEAE_META_VERSION: u32 = 22;
const TYPE_DIGITAL: u32 = 100;
const TYPE_ANALOG: u32 = 101;
const TYPE_TRIGGER: u32 = 103;
const MAX_META_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CHANNELS: usize = 16;
const MAX_SAMPLES_PER_CHANNEL: usize = 128 * 1024 * 1024;
const MAX_TOTAL_SAMPLE_VALUES: usize = 256 * 1024 * 1024;
const DIGITAL_CHUNK_DATA_BYTES: usize = 60_000;
const ANALOG_SAMPLES_PER_CHUNK: usize = 417_040;

const TRIGGER_STORE: [u8; 32] = [
    b'<',
    b'S',
    b'A',
    b'L',
    b'E',
    b'A',
    b'E',
    b'>',
    3,
    0,
    0,
    0,
    TYPE_TRIGGER as u8,
    0,
    0,
    0,
    1,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaleaeChannel {
    pub index: u8,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SaleaeDigitalCapture {
    pub sample_rate_hz: u64,
    /// One word per sample; bit N is Saleae digital channel N.
    pub samples: Vec<u32>,
    pub channels: Vec<SaleaeChannel>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SaleaeAnalogChannel {
    pub index: u8,
    pub name: String,
    pub samples: Vec<f32>,
    pub minimum_voltage: f32,
    pub maximum_voltage: f32,
}

impl SaleaeAnalogChannel {
    pub fn with_automatic_range(index: u8, name: String, samples: Vec<f32>) -> Self {
        let peak = samples
            .iter()
            .copied()
            .filter(|sample| sample.is_finite())
            .map(f32::abs)
            .fold(0.0_f32, f32::max);
        let half_range = (peak * 1.05).max(1.0e-6);
        Self {
            index,
            name,
            samples,
            minimum_voltage: -half_range,
            maximum_voltage: half_range,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SaleaeCapture {
    pub digital: Option<SaleaeDigitalCapture>,
    pub analog_sample_rate_hz: Option<u64>,
    pub analog: Vec<SaleaeAnalogChannel>,
}

#[derive(Debug)]
pub enum SaleaeError {
    Io(io::Error),
    Zip(ZipError),
    Json(serde_json::Error),
    Invalid(String),
}

impl SaleaeError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl Display for SaleaeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Saleae capture I/O failed: {error}"),
            Self::Zip(error) => write!(formatter, "invalid Saleae ZIP archive: {error}"),
            Self::Json(error) => write!(formatter, "invalid Saleae metadata JSON: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid Saleae capture: {message}"),
        }
    }
}

impl Error for SaleaeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Zip(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<io::Error> for SaleaeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ZipError> for SaleaeError {
    fn from(error: ZipError) -> Self {
        Self::Zip(error)
    }
}

impl From<serde_json::Error> for SaleaeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug)]
pub enum SaleaeConversionError {
    Saleae(SaleaeError),
    Recording(RecordingError),
    Invalid(String),
}

impl SaleaeConversionError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl Display for SaleaeConversionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Saleae(error) => error.fmt(formatter),
            Self::Recording(error) => error.fmt(formatter),
            Self::Invalid(message) => write!(formatter, "capture conversion failed: {message}"),
        }
    }
}

impl Error for SaleaeConversionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Saleae(error) => Some(error),
            Self::Recording(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<SaleaeError> for SaleaeConversionError {
    fn from(error: SaleaeError) -> Self {
        Self::Saleae(error)
    }
}

impl From<RecordingError> for SaleaeConversionError {
    fn from(error: RecordingError) -> Self {
        Self::Recording(error)
    }
}

#[derive(Clone, Debug)]
struct BinEntry {
    kind: u32,
    channel: u8,
    file: String,
}

pub fn read_saleae<R: Read + Seek>(input: R) -> Result<SaleaeCapture, SaleaeError> {
    let mut archive = zip::ZipArchive::new(input)?;
    let metadata = {
        let mut file = archive.by_name("meta.json").map_err(|error| match error {
            ZipError::FileNotFound => SaleaeError::invalid("archive has no meta.json"),
            other => SaleaeError::Zip(other),
        })?;
        if file.size() > MAX_META_BYTES {
            return Err(SaleaeError::invalid(format!(
                "meta.json is {} bytes; maximum is {MAX_META_BYTES}",
                file.size()
            )));
        }
        serde_json::from_reader::<_, Value>(&mut file)?
    };
    let entries = bin_entries(&mut archive, &metadata)?;
    if entries.is_empty() {
        return Err(SaleaeError::invalid("metadata contains no channel data"));
    }

    let mut digital_parts = Vec::new();
    let mut analog = Vec::new();
    let mut analog_rate = None;
    let mut total_values = 0_usize;
    for entry in entries {
        let name = channel_name(&metadata, entry.kind, entry.channel);
        let mut file = archive.by_name(&entry.file).map_err(|error| match error {
            ZipError::FileNotFound => SaleaeError::invalid(format!(
                "metadata references missing channel file {}",
                entry.file
            )),
            other => SaleaeError::Zip(other),
        })?;
        match entry.kind {
            TYPE_DIGITAL => {
                let (rate, samples) = read_digital_bin(&mut file)?;
                total_values = total_values.saturating_add(samples.len());
                digital_parts.push((entry.channel, name, rate, samples));
            }
            TYPE_ANALOG => {
                let (rate, raw) = read_analog_bin(&mut file)?;
                if analog_rate.is_some_and(|previous| previous != rate) {
                    return Err(SaleaeError::invalid(
                        "analog channels use different sample rates",
                    ));
                }
                analog_rate = Some(rate);
                total_values = total_values.saturating_add(raw.len());
                let (minimum, maximum) = analog_range(&metadata, entry.channel);
                let midpoint = (minimum + maximum) * 0.5;
                let half_range = (maximum - minimum) * 0.5;
                let samples = raw
                    .into_iter()
                    .map(|sample| midpoint + f32::from(sample) / 2047.0 * half_range)
                    .collect();
                analog.push(SaleaeAnalogChannel {
                    index: entry.channel,
                    name,
                    samples,
                    minimum_voltage: minimum,
                    maximum_voltage: maximum,
                });
            }
            _ => unreachable!("bin_entries only returns channel entries"),
        }
        if total_values > MAX_TOTAL_SAMPLE_VALUES {
            return Err(SaleaeError::invalid(format!(
                "capture contains more than {MAX_TOTAL_SAMPLE_VALUES} sample values"
            )));
        }
    }

    digital_parts.sort_by_key(|part| part.0);
    analog.sort_by_key(|channel| channel.index);
    let digital = combine_digital_channels(digital_parts)?;

    Ok(SaleaeCapture {
        digital,
        analog_sample_rate_hz: analog_rate,
        analog,
    })
}

fn combine_digital_channels(
    parts: Vec<(u8, String, u64, Vec<bool>)>,
) -> Result<Option<SaleaeDigitalCapture>, SaleaeError> {
    let Some((_, _, first_rate, first_samples)) = parts.first() else {
        return Ok(None);
    };
    let first_rate = *first_rate;
    let sample_count = first_samples.len();
    let mut packed = vec![0_u32; sample_count];
    let mut channels = Vec::with_capacity(parts.len());
    for (index, name, rate, samples) in parts {
        if rate != first_rate {
            return Err(SaleaeError::invalid(
                "digital channels use different sample rates",
            ));
        }
        if samples.len() != sample_count {
            return Err(SaleaeError::invalid(
                "digital channels contain different sample counts",
            ));
        }
        if index >= 32 {
            return Err(SaleaeError::invalid(format!(
                "digital channel {index} cannot fit in DSView's 32-bit samples"
            )));
        }
        let mask = 1_u32 << index;
        for (destination, high) in packed.iter_mut().zip(samples) {
            if high {
                *destination |= mask;
            }
        }
        channels.push(SaleaeChannel { index, name });
    }
    Ok(Some(SaleaeDigitalCapture {
        sample_rate_hz: first_rate,
        samples: packed,
        channels,
    }))
}

pub fn write_saleae<W: Write + Seek>(
    output: W,
    capture: &SaleaeCapture,
) -> Result<(), SaleaeError> {
    validate_capture(capture)?;
    let metadata = build_metadata(capture);
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut archive = zip::ZipWriter::new(output);
    archive.start_file("meta.json", options)?;
    serde_json::to_writer(&mut archive, &metadata)?;

    if let Some(digital) = &capture.digital {
        for channel in &digital.channels {
            archive.start_file(format!("digital-{}.bin", channel.index), options)?;
            write_digital_bin(&mut archive, digital, channel.index)?;
        }
    }
    for channel in &capture.analog {
        archive.start_file(format!("analog-{}.bin", channel.index), options)?;
        write_analog_bin(
            &mut archive,
            channel,
            capture
                .analog_sample_rate_hz
                .expect("validation requires an analog sample rate"),
        )?;
    }
    archive.start_file("trigger-store.bin", options)?;
    archive.write_all(&TRIGGER_STORE)?;
    let mut output = archive.finish()?;
    output.flush()?;
    Ok(())
}

fn validate_capture(capture: &SaleaeCapture) -> Result<(), SaleaeError> {
    if capture.digital.is_none() && capture.analog.is_empty() {
        return Err(SaleaeError::invalid("capture contains no channels"));
    }
    let mut seen_digital = 0_u32;
    if let Some(digital) = &capture.digital {
        if digital.sample_rate_hz == 0 || digital.samples.is_empty() {
            return Err(SaleaeError::invalid(
                "digital sample rate and sample data must be non-zero",
            ));
        }
        if digital.samples.len() > MAX_SAMPLES_PER_CHANNEL {
            return Err(SaleaeError::invalid("digital capture is too large"));
        }
        if digital.channels.is_empty() || digital.channels.len() > MAX_CHANNELS {
            return Err(SaleaeError::invalid("digital channel count is invalid"));
        }
        for channel in &digital.channels {
            if usize::from(channel.index) >= MAX_CHANNELS
                || seen_digital & (1_u32 << channel.index) != 0
            {
                return Err(SaleaeError::invalid(format!(
                    "digital channel {} is duplicated or unsupported",
                    channel.index
                )));
            }
            seen_digital |= 1_u32 << channel.index;
        }
    }
    let mut seen_analog = 0_u32;
    if !capture.analog.is_empty() && capture.analog_sample_rate_hz == Some(0) {
        return Err(SaleaeError::invalid("analog sample rate must be non-zero"));
    }
    if capture.analog.is_empty() && capture.analog_sample_rate_hz.is_some() {
        return Err(SaleaeError::invalid(
            "analog sample rate is present without analog channels",
        ));
    }
    if !capture.analog.is_empty() && capture.analog_sample_rate_hz.is_none() {
        return Err(SaleaeError::invalid("analog sample rate is missing"));
    }
    for channel in &capture.analog {
        if usize::from(channel.index) >= MAX_CHANNELS || seen_analog & (1_u32 << channel.index) != 0
        {
            return Err(SaleaeError::invalid(format!(
                "analog channel {} is duplicated or unsupported",
                channel.index
            )));
        }
        seen_analog |= 1_u32 << channel.index;
        if channel.samples.is_empty() || channel.samples.len() > MAX_SAMPLES_PER_CHANNEL {
            return Err(SaleaeError::invalid(format!(
                "analog channel {} has an invalid sample count",
                channel.index
            )));
        }
        if !channel.minimum_voltage.is_finite()
            || !channel.maximum_voltage.is_finite()
            || channel.maximum_voltage <= channel.minimum_voltage
            || channel.samples.iter().any(|sample| !sample.is_finite())
        {
            return Err(SaleaeError::invalid(format!(
                "analog channel {} contains an invalid voltage or range",
                channel.index
            )));
        }
    }
    Ok(())
}

fn bin_entries<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    metadata: &Value,
) -> Result<Vec<BinEntry>, SaleaeError> {
    let mut entries = Vec::new();
    if let Some(bin_data) = metadata.get("binData").and_then(Value::as_array) {
        for item in bin_data {
            let kind = match item.get("type").and_then(Value::as_str) {
                Some("Digital") => TYPE_DIGITAL,
                Some("Analog") => TYPE_ANALOG,
                _ => continue,
            };
            let channel = item
                .get("deviceChannel")
                .and_then(Value::as_u64)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(|| SaleaeError::invalid("binData has an invalid deviceChannel"))?;
            let file = item
                .get("file")
                .and_then(Value::as_str)
                .ok_or_else(|| SaleaeError::invalid("binData entry has no file"))?
                .trim_start_matches("./")
                .to_owned();
            entries.push(BinEntry {
                kind,
                channel,
                file,
            });
        }
    }
    if entries.is_empty() {
        for index in 0..archive.len() {
            let name = archive.by_index(index)?.name().to_owned();
            let (kind, prefix) = if name.starts_with("digital-") {
                (TYPE_DIGITAL, "digital-")
            } else if name.starts_with("analog-") {
                (TYPE_ANALOG, "analog-")
            } else {
                continue;
            };
            let channel = name
                .strip_prefix(prefix)
                .and_then(|name| name.strip_suffix(".bin"))
                .and_then(|index| index.parse::<u8>().ok())
                .ok_or_else(|| SaleaeError::invalid(format!("invalid channel filename {name}")))?;
            entries.push(BinEntry {
                kind,
                channel,
                file: name,
            });
        }
    }
    entries.sort_by_key(|entry| (entry.kind, entry.channel));
    entries.dedup_by_key(|entry| (entry.kind, entry.channel));
    if entries.len() > MAX_CHANNELS * 2 {
        return Err(SaleaeError::invalid("capture contains too many channels"));
    }
    Ok(entries)
}

fn channel_name(metadata: &Value, kind: u32, channel: u8) -> String {
    let type_name = if kind == TYPE_DIGITAL {
        "Digital"
    } else {
        "Analog"
    };
    metadata
        .pointer("/data/rowsSettings")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter().find(|row| {
                row.pointer("/channel/type").and_then(Value::as_str) == Some(type_name)
                    && row
                        .pointer("/channel/deviceChannel")
                        .and_then(Value::as_u64)
                        == Some(u64::from(channel))
            })
        })
        .and_then(|row| row.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn analog_range(metadata: &Value, channel: u8) -> (f32, f32) {
    let range = metadata
        .pointer("/data/legacyDeviceCalibration/fullScaleVoltageRanges")
        .and_then(Value::as_array)
        .and_then(|ranges| ranges.get(usize::from(channel)));
    let minimum = range
        .and_then(|range| range.get("minimumVoltage"))
        .and_then(Value::as_f64)
        .unwrap_or(-10.0) as f32;
    let maximum = range
        .and_then(|range| range.get("maximumVoltage"))
        .and_then(Value::as_f64)
        .unwrap_or(10.0) as f32;
    if minimum.is_finite() && maximum.is_finite() && maximum > minimum {
        (minimum, maximum)
    } else {
        (-10.0, 10.0)
    }
}

struct ChannelHeader {
    sample_rate_hz: u64,
    chunk_count: u64,
}

fn read_channel_header(
    input: &mut impl Read,
    expected_type: u32,
) -> Result<ChannelHeader, SaleaeError> {
    let mut magic = [0_u8; 8];
    input.read_exact(&mut magic)?;
    if magic != SALEAE_MAGIC {
        return Err(SaleaeError::invalid(
            "channel binary magic does not match <SALEAE>",
        ));
    }
    let version = read_u32(input)?;
    if version != SALEAE_BINARY_VERSION {
        return Err(SaleaeError::invalid(format!(
            "channel binary version {version} is unsupported; expected {SALEAE_BINARY_VERSION}"
        )));
    }
    let kind = read_u32(input)?;
    if kind != expected_type {
        return Err(SaleaeError::invalid(format!(
            "channel binary type {kind} does not match metadata"
        )));
    }
    if read_u8(input)? != 1 {
        return Err(SaleaeError::invalid("channel header is not marked valid"));
    }
    let rate = f64::from_le_bytes(read_array(input)?);
    if !rate.is_finite() || rate < 1.0 || rate > u64::MAX as f64 {
        return Err(SaleaeError::invalid("channel sample rate is invalid"));
    }
    let _capture_start_unix_ms = read_u64(input)?;
    let offset = f64::from_le_bytes(read_array(input)?);
    if !offset.is_finite() {
        return Err(SaleaeError::invalid("channel time offset is invalid"));
    }
    if read_u8(input)? != 0 || read_u8(input)? != 0 {
        return Err(SaleaeError::invalid(
            "channel header contains unsupported optional fields",
        ));
    }
    let chunk_count = read_u64(input)?;
    if chunk_count == 0 || chunk_count > MAX_SAMPLES_PER_CHANNEL as u64 {
        return Err(SaleaeError::invalid("channel chunk count is invalid"));
    }
    Ok(ChannelHeader {
        sample_rate_hz: rate.round() as u64,
        chunk_count,
    })
}

fn read_digital_bin(input: &mut impl Read) -> Result<(u64, Vec<bool>), SaleaeError> {
    let header = read_channel_header(input, TYPE_DIGITAL)?;
    let mut samples = Vec::new();
    for chunk_index in 0..header.chunk_count {
        let start = read_u64(input)?;
        let end = read_u64(input)?;
        let packed = read_u64(input)?;
        let final_flag = packed & 0xffff;
        let byte_count = ((packed >> 16) & 0xffff) as usize;
        if packed >> 32 != 0 || final_flag > 1 {
            return Err(SaleaeError::invalid("digital chunk header is invalid"));
        }
        if start != samples.len() as u64 || end < start {
            return Err(SaleaeError::invalid("digital chunks are not contiguous"));
        }
        let expected = usize::try_from(end - start)
            .map_err(|_| SaleaeError::invalid("digital chunk is too large"))?;
        if samples.len().saturating_add(expected) > MAX_SAMPLES_PER_CHANNEL {
            return Err(SaleaeError::invalid("digital channel is too large"));
        }
        if read_u16(input)? != 0 {
            return Err(SaleaeError::invalid(
                "digital chunk metadata is unsupported",
            ));
        }
        let mut high = true;
        let mut encoded = vec![0_u8; byte_count];
        input.read_exact(&mut encoded)?;
        let mut cursor = 0;
        let chunk_start = samples.len();
        while cursor < encoded.len() {
            let run = decode_run(&encoded, &mut cursor)?;
            let run = usize::try_from(run)
                .map_err(|_| SaleaeError::invalid("digital run length is too large"))?;
            if samples.len().saturating_add(run) > chunk_start.saturating_add(expected) {
                return Err(SaleaeError::invalid(
                    "digital runs exceed their chunk sample range",
                ));
            }
            samples.resize(samples.len() + run, high);
            high = !high;
        }
        if samples.len() != chunk_start + expected {
            return Err(SaleaeError::invalid(
                "digital runs do not fill their chunk sample range",
            ));
        }
        if (chunk_index + 1 == header.chunk_count) != (final_flag == 1) {
            return Err(SaleaeError::invalid(
                "digital final-chunk flag is inconsistent",
            ));
        }
    }
    ensure_eof(input, "digital channel")?;
    Ok((header.sample_rate_hz, samples))
}

fn decode_run(encoded: &[u8], cursor: &mut usize) -> Result<u64, SaleaeError> {
    let first = *encoded
        .get(*cursor)
        .ok_or_else(|| SaleaeError::invalid("digital run is truncated"))?;
    *cursor += 1;
    if first < 0x40 {
        return Ok(u64::from(first) + 1);
    }
    if first & 0xc0 != 0x40 {
        return Err(SaleaeError::invalid("digital run has an invalid prefix"));
    }
    let mut value = u64::from(first & 0x3f);
    loop {
        let byte = *encoded
            .get(*cursor)
            .ok_or_else(|| SaleaeError::invalid("digital run is truncated"))?;
        *cursor += 1;
        value = value
            .checked_shl(7)
            .and_then(|value| value.checked_add(u64::from(byte & 0x7f)))
            .ok_or_else(|| SaleaeError::invalid("digital run length overflowed"))?;
        if byte & 0x80 == 0 {
            return value
                .checked_add(1)
                .ok_or_else(|| SaleaeError::invalid("digital run length overflowed"));
        }
    }
}

fn read_analog_bin(input: &mut impl Read) -> Result<(u64, Vec<i16>), SaleaeError> {
    let header = read_channel_header(input, TYPE_ANALOG)?;
    let mut samples = Vec::new();
    for _ in 0..header.chunk_count {
        let start = read_u64(input)?;
        let end = read_u64(input)?;
        let count = read_u64(input)?;
        if start != samples.len() as u64 || end < start || end - start != count {
            return Err(SaleaeError::invalid("analog chunks are not contiguous"));
        }
        let count = usize::try_from(count)
            .map_err(|_| SaleaeError::invalid("analog chunk is too large"))?;
        if count == 0 || samples.len().saturating_add(count) > MAX_SAMPLES_PER_CHANNEL {
            return Err(SaleaeError::invalid("analog channel is too large or empty"));
        }
        samples.reserve(count);
        for _ in 0..count {
            samples.push(i16::from_le_bytes(read_array(input)?));
        }
        skip_pyramid(input, count)?;
    }
    ensure_eof(input, "analog channel")?;
    Ok((header.sample_rate_hz, samples))
}

fn skip_pyramid(input: &mut impl Read, sample_count: usize) -> Result<(), SaleaeError> {
    let first_count = read_u32(input)? as usize;
    let _size_field = read_u32(input)?;
    if read_u32(input)? != 0 {
        return Err(SaleaeError::invalid("analog pyramid header is invalid"));
    }
    let level_count = read_u32(input)? as usize;
    if read_u32(input)? != 0 || read_u32(input)? as usize != first_count || read_u32(input)? != 0 {
        return Err(SaleaeError::invalid("analog pyramid header is invalid"));
    }
    let expected_first = sample_count.div_ceil(16);
    if first_count != expected_first || level_count == 0 || level_count > 64 {
        return Err(SaleaeError::invalid(
            "analog pyramid dimensions are invalid",
        ));
    }
    skip_bytes(input, first_count.saturating_mul(4))?;
    let mut previous = first_count;
    for _ in 1..level_count {
        let count = usize::try_from(read_u64(input)?)
            .map_err(|_| SaleaeError::invalid("analog pyramid level is too large"))?;
        if count > previous / 2 {
            return Err(SaleaeError::invalid("analog pyramid level is invalid"));
        }
        skip_bytes(input, count.saturating_mul(4))?;
        previous = count;
    }
    Ok(())
}

fn write_universal_header(
    output: &mut impl Write,
    kind: u32,
    sample_rate_hz: u64,
    chunk_count: u64,
) -> Result<(), SaleaeError> {
    output.write_all(&SALEAE_MAGIC)?;
    output.write_all(&SALEAE_BINARY_VERSION.to_le_bytes())?;
    output.write_all(&kind.to_le_bytes())?;
    output.write_all(&[1])?;
    output.write_all(&(sample_rate_hz as f64).to_le_bytes())?;
    output.write_all(&0_u64.to_le_bytes())?;
    output.write_all(&0_f64.to_le_bytes())?;
    output.write_all(&[0, 0])?;
    output.write_all(&chunk_count.to_le_bytes())?;
    Ok(())
}

struct DigitalChunk {
    start: u64,
    end: u64,
    encoded: Vec<u8>,
}

fn write_digital_bin(
    output: &mut impl Write,
    digital: &SaleaeDigitalCapture,
    channel: u8,
) -> Result<(), SaleaeError> {
    let mask = 1_u32 << channel;
    let mut chunks = Vec::new();
    let mut start = 0_usize;
    while start < digital.samples.len() {
        let mut encoded = Vec::new();
        let mut cursor = start;
        while cursor < digital.samples.len() {
            // Saleae RLE chunks implicitly start high. Native Logic captures
            // represent a low-starting signal with a one-sample high prefix.
            let state = cursor == 0 || digital.samples[cursor] & mask != 0;
            let mut end = cursor + 1;
            while end < digital.samples.len() && (digital.samples[end] & mask != 0) == state {
                end += 1;
            }
            let run = encode_run((end - cursor) as u64);
            if !encoded.is_empty() && encoded.len() + run.len() > DIGITAL_CHUNK_DATA_BYTES && state
            {
                break;
            }
            encoded.extend_from_slice(&run);
            cursor = end;
        }
        chunks.push(DigitalChunk {
            start: start as u64,
            end: cursor as u64,
            encoded,
        });
        start = cursor;
    }
    write_universal_header(
        output,
        TYPE_DIGITAL,
        digital.sample_rate_hz,
        chunks.len() as u64,
    )?;
    let chunk_count = chunks.len();
    for (index, chunk) in chunks.into_iter().enumerate() {
        let final_flag = u64::from(index + 1 == chunk_count);
        let packed = final_flag | ((chunk.encoded.len() as u64) << 16);
        output.write_all(&chunk.start.to_le_bytes())?;
        output.write_all(&chunk.end.to_le_bytes())?;
        output.write_all(&packed.to_le_bytes())?;
        output.write_all(&0_u16.to_le_bytes())?;
        output.write_all(&chunk.encoded)?;
    }
    Ok(())
}

fn encode_run(run: u64) -> Vec<u8> {
    let value = run - 1;
    if value < 0x40 {
        return vec![value as u8];
    }
    let bits = (u64::BITS - value.leading_zeros()) as usize;
    let continuation_count = (bits.saturating_sub(6).div_ceil(7)).max(1);
    let mut shift = continuation_count * 7;
    let mut encoded = Vec::with_capacity(continuation_count + 1);
    encoded.push(0x40 | ((value >> shift) as u8 & 0x3f));
    for index in 0..continuation_count {
        shift -= 7;
        let mut byte = ((value >> shift) & 0x7f) as u8;
        if index + 1 < continuation_count {
            byte |= 0x80;
        }
        encoded.push(byte);
    }
    encoded
}

fn write_analog_bin(
    output: &mut impl Write,
    channel: &SaleaeAnalogChannel,
    sample_rate_hz: u64,
) -> Result<(), SaleaeError> {
    let chunk_count = channel.samples.len().div_ceil(ANALOG_SAMPLES_PER_CHUNK);
    write_universal_header(output, TYPE_ANALOG, sample_rate_hz, chunk_count as u64)?;
    let midpoint = (channel.minimum_voltage + channel.maximum_voltage) * 0.5;
    let half_range = (channel.maximum_voltage - channel.minimum_voltage) * 0.5;
    for (chunk_index, samples) in channel.samples.chunks(ANALOG_SAMPLES_PER_CHUNK).enumerate() {
        let start = chunk_index * ANALOG_SAMPLES_PER_CHUNK;
        let end = start + samples.len();
        let raw = samples
            .iter()
            .map(|sample| {
                (((*sample - midpoint) / half_range * 2047.0).round() as i32).clamp(-2047, 2047)
                    as i16
            })
            .collect::<Vec<_>>();
        output.write_all(&(start as u64).to_le_bytes())?;
        output.write_all(&(end as u64).to_le_bytes())?;
        output.write_all(&(raw.len() as u64).to_le_bytes())?;
        for sample in &raw {
            output.write_all(&sample.to_le_bytes())?;
        }
        write_pyramid(output, &raw)?;
    }
    Ok(())
}

fn write_pyramid(output: &mut impl Write, samples: &[i16]) -> Result<(), SaleaeError> {
    let first = samples
        .chunks(16)
        .map(|group| {
            let minimum = *group
                .iter()
                .min()
                .expect("analog pyramid group is non-empty");
            let maximum = *group
                .iter()
                .max()
                .expect("analog pyramid group is non-empty");
            (minimum, maximum)
        })
        .collect::<Vec<_>>();
    let mut levels = vec![first];
    while levels.last().map_or(0, Vec::len) > 1 {
        let previous = levels.last().expect("pyramid has a first level");
        let next = previous
            .chunks_exact(2)
            .map(|pair| (pair[0].0.min(pair[1].0), pair[0].1.max(pair[1].1)))
            .collect::<Vec<_>>();
        levels.push(next);
    }
    let append_empty = levels[0].len() == 1;
    let level_count = levels.len() + usize::from(append_empty);
    let pair_count = levels.iter().map(Vec::len).sum::<usize>();
    let size_field = 848_usize
        .saturating_add(samples.len().saturating_mul(2))
        .saturating_add(pair_count.saturating_mul(4));
    for value in [
        levels[0].len(),
        size_field,
        0,
        level_count,
        0,
        levels[0].len(),
        0,
    ] {
        output.write_all(&(value as u32).to_le_bytes())?;
    }
    write_pairs(output, &levels[0])?;
    for level in &levels[1..] {
        output.write_all(&(level.len() as u64).to_le_bytes())?;
        write_pairs(output, level)?;
    }
    if append_empty {
        output.write_all(&0_u64.to_le_bytes())?;
    }
    Ok(())
}

fn write_pairs(output: &mut impl Write, pairs: &[(i16, i16)]) -> Result<(), SaleaeError> {
    for (minimum, maximum) in pairs {
        output.write_all(&minimum.to_le_bytes())?;
        output.write_all(&maximum.to_le_bytes())?;
    }
    Ok(())
}

fn build_metadata(capture: &SaleaeCapture) -> Value {
    let digital_rate = capture
        .digital
        .as_ref()
        .map_or(1, |digital| digital.sample_rate_hz);
    let analog_rate = capture.analog_sample_rate_hz.unwrap_or(1);
    let digital_count = capture
        .digital
        .as_ref()
        .map_or(0, |digital| digital.samples.len());
    let analog_count = capture
        .analog
        .iter()
        .map(|channel| channel.samples.len())
        .max()
        .unwrap_or(0);
    let duration =
        (digital_count as f64 / digital_rate as f64).max(analog_count as f64 / analog_rate as f64);
    let mut enabled = Vec::new();
    let mut rows = Vec::new();
    let mut bin_data = Vec::new();
    if let Some(digital) = &capture.digital {
        for channel in &digital.channels {
            enabled.push(json!({"type": "Digital", "index": channel.index}));
            rows.push(row_json("Digital", channel.index, &channel.name, 45));
            bin_data.push(json!({
                "category": "legacy", "type": "Digital", "deviceChannel": channel.index,
                "file": format!("./digital-{}.bin", channel.index)
            }));
        }
    }
    for channel in &capture.analog {
        enabled.push(json!({"type": "Analog", "index": channel.index}));
        rows.push(row_json("Analog", channel.index, &channel.name, 100));
        bin_data.push(json!({
            "category": "legacy", "type": "Analog", "deviceChannel": channel.index,
            "file": format!("./analog-{}.bin", channel.index)
        }));
    }
    let capabilities = (0..16)
        .flat_map(|index| {
            ["Digital", "Analog"]
                .into_iter()
                .map(move |kind| json!({"type": kind, "index": index, "capability": "Toggleable"}))
        })
        .collect::<Vec<_>>();
    let mut ranges = vec![json!({"minimumVoltage": -10.0, "maximumVoltage": 10.0}); 16];
    for channel in &capture.analog {
        ranges[usize::from(channel.index)] = json!({
            "minimumVoltage": channel.minimum_voltage,
            "maximumVoltage": channel.maximum_voltage
        });
    }
    json!({
        "version": SALEAE_META_VERSION,
        "data": {
            "renderViewState": {"type": "PanAndZoom", "leftEdgeTimeSec": 0, "timeScaleSeconds": duration.max(0.001)},
            "captureStartTime": {"unixTimeMilliseconds": 0, "fractionalMilliseconds": 0.0},
            "timingMarkers": {"markers": {}, "pairs": {}},
            "measurements": [], "highLevelAnalyzers": [], "analyzers": [],
            "rowsSettings": rows,
            "legacyDevice": {
                "deviceId": "1000005", "name": "Logic Pro 16", "deviceType": "LogicPro16",
                "isSimulation": true,
                "capabilities": {
                    "channelCapabilities": capabilities,
                    "sampleRateOptions": [{"digital": digital_rate, "analog": analog_rate}],
                    "digitalThresholdOptions": [{"description": "1.2 Volts"}, {"description": "1.8 Volts"}, {"description": "3.3+ Volts"}],
                    "isPhysicalDevice": false
                }
            },
            "legacyDeviceCalibration": {"fullScaleVoltageRanges": ranges},
            "legacySettings": {
                "enabledChannels": enabled,
                "sampleRate": {"digital": digital_rate, "analog": analog_rate},
                "digitalThreshold": {"description": "1.2 Volts"},
                "glitchFilter": {"enabled": false, "channels": []}
            },
            "captureSettings": {
                "bufferSizeMb": 3072,
                "timerModeSettings": {"stopAfterSeconds": duration},
                "commonCaptureSettings": {"trimAfterCapture": false, "trimTimeSeconds": 0.001},
                "triggerSettings": {
                    "eventChannel": {"category": "legacy", "type": "Digital", "deviceChannel": 0},
                    "triggerSourceGeneration": 0, "scopeEventType": "Rising", "scopeThreshold": 1,
                    "scopeHysteresisPercentage": 0.02, "digitalEventType": "Rising",
                    "digitalLinkedChannels": [], "digitalLegacyPostTriggerBufferSeconds": 1,
                    "mode": "Auto", "holdOffSeconds": 0.001,
                    "pulseDuration": {"min": 0.001, "max": 0.01},
                    "realTriggerTimeoutViewRatio": 4, "minRealTriggerTimeoutSeconds": 1,
                    "autoTriggerTimeoutViewRatio": 2
                },
                "captureMode": "FreeRun", "captureTriggerType": "Signal"
            },
            "timeManager": {"t0": {"type": "startOfCapture"}},
            "captureNotes": "", "dataTable": {"columns": {}}, "digitalTriggerTime": -1,
            "analyzerTrigger": {"settings": {"holdoffSeconds": 0.2, "searchQuery": ""}},
            "name": "Exported by DSView"
        },
        "binData": bin_data
    })
}

fn row_json(kind: &str, index: u8, name: &str, height: u32) -> Value {
    json!({
        "id": format!("00000000-0000-0000-0000-{}{:011}", if kind == "Digital" {'d'} else {'a'}, index),
        "height": height, "isMarkedHidden": false, "type": "channel",
        "name": if name.is_empty() { format!("Channel {index}") } else { name.to_owned() },
        "channel": {"category": "legacy", "type": kind, "deviceChannel": index}
    })
}

pub fn import_saleae_recording<R: Read + Seek, W: Write>(
    input: R,
    output: W,
) -> Result<(), SaleaeConversionError> {
    let capture = read_saleae(input)?;
    let mut devices = Vec::new();
    let mut metadata = BTreeMap::new();
    metadata.insert("import.format".to_owned(), "saleae.sal".to_owned());
    if let Some(digital) = &capture.digital {
        devices.push(DeviceDescriptor {
            id: DeviceId::new("saleae-digital"),
            name: "Saleae digital capture".to_owned(),
            kind: InstrumentKind::LogicAnalyzer,
        });
        metadata.insert(
            "logic.sample_rate_hz".to_owned(),
            digital.sample_rate_hz.to_string(),
        );
        metadata.insert(
            "logic.sample_count".to_owned(),
            digital.samples.len().to_string(),
        );
        for channel in &digital.channels {
            metadata.insert(
                format!("logic.ch{}.enabled", channel.index),
                "true".to_owned(),
            );
            metadata.insert(
                format!("logic.ch{}.name", channel.index),
                channel.name.clone(),
            );
        }
    }
    if !capture.analog.is_empty() {
        devices.push(DeviceDescriptor {
            id: DeviceId::new("saleae-analog"),
            name: "Saleae analog capture".to_owned(),
            kind: InstrumentKind::Oscilloscope,
        });
        let rate = capture
            .analog_sample_rate_hz
            .expect("parsed analog channels have a sample rate");
        metadata.insert("scope.sample_rate_hz".to_owned(), rate.to_string());
        metadata.insert(
            "scope.sample_count".to_owned(),
            capture.analog[0].samples.len().to_string(),
        );
        for channel in &capture.analog {
            metadata.insert(
                format!("scope.ch{}.enabled", channel.index),
                "true".to_owned(),
            );
            metadata.insert(
                format!("scope.ch{}.name", channel.index),
                channel.name.clone(),
            );
        }
    }
    let mut writer = RecordingWriter::new(output, &devices, metadata)?;
    if let Some(digital) = capture.digital {
        let channel_count = digital
            .channels
            .iter()
            .map(|channel| channel.index + 1)
            .max()
            .unwrap_or(0);
        writer.write_event(&CaptureEvent::Frame(CaptureFrame {
            device_id: DeviceId::new("saleae-digital"),
            sequence: 0,
            captured_at: Duration::ZERO,
            payload: CaptureBatch::Logic(LogicBatch {
                sample_rate_hz: digital.sample_rate_hz,
                channel_count,
                samples: digital.samples,
            }),
        }))?;
    }
    if !capture.analog.is_empty() {
        let rate = capture
            .analog_sample_rate_hz
            .expect("parsed analog channels have a sample rate");
        writer.write_event(&CaptureEvent::Frame(CaptureFrame {
            device_id: DeviceId::new("saleae-analog"),
            sequence: 0,
            captured_at: Duration::ZERO,
            payload: CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: rate,
                trigger_sample_index: None,
                channels: capture
                    .analog
                    .into_iter()
                    .map(|channel| channel.samples)
                    .collect(),
            }),
        }))?;
    }
    writer.finish(CompletionStatus::Complete)?;
    Ok(())
}

pub fn export_saleae_recording<R: Read, W: Write + Seek>(
    input: R,
    output: W,
) -> Result<(), SaleaeConversionError> {
    let mut reader = RecordingReader::new(input)?;
    let metadata = reader.header().metadata.clone();
    let mut logic_device = None;
    let mut digital_rate = 0;
    let mut digital_channel_count = 0_u8;
    let mut digital_samples = Vec::new();
    let mut scope_device = None;
    let mut analog_rate = 0;
    let mut analog_samples = Vec::<Vec<f32>>::new();
    while let Some(event) = reader.read_event()? {
        let CaptureEvent::Frame(frame) = event else {
            continue;
        };
        match frame.payload {
            CaptureBatch::Logic(batch) => {
                if logic_device
                    .as_ref()
                    .is_some_and(|id| id != &frame.device_id)
                {
                    return Err(SaleaeConversionError::invalid(
                        "Saleae export supports only one logic source",
                    ));
                }
                logic_device = Some(frame.device_id);
                if digital_rate != 0
                    && (digital_rate != batch.sample_rate_hz
                        || digital_channel_count != batch.channel_count)
                {
                    return Err(SaleaeConversionError::invalid(
                        "logic frame layout changes during the recording",
                    ));
                }
                digital_rate = batch.sample_rate_hz;
                digital_channel_count = batch.channel_count;
                digital_samples.extend(batch.samples);
            }
            CaptureBatch::Scope(batch) => {
                if scope_device
                    .as_ref()
                    .is_some_and(|id| id != &frame.device_id)
                {
                    return Err(SaleaeConversionError::invalid(
                        "Saleae export supports only one scope source",
                    ));
                }
                scope_device = Some(frame.device_id);
                if analog_rate != 0
                    && (analog_rate != batch.sample_rate_hz
                        || analog_samples.len() != batch.channels.len())
                {
                    return Err(SaleaeConversionError::invalid(
                        "scope frame layout changes during the recording",
                    ));
                }
                analog_rate = batch.sample_rate_hz;
                if analog_samples.is_empty() {
                    analog_samples = vec![Vec::new(); batch.channels.len()];
                }
                for (destination, source) in analog_samples.iter_mut().zip(batch.channels) {
                    destination.extend(source);
                }
            }
        }
    }
    let digital = if digital_samples.is_empty() {
        None
    } else {
        let enabled = (0..digital_channel_count)
            .filter(|index| {
                metadata
                    .get(&format!("logic.ch{index}.enabled"))
                    .map_or(true, |value| value == "true")
            })
            .map(|index| SaleaeChannel {
                index,
                name: metadata
                    .get(&format!("logic.ch{index}.name"))
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect();
        Some(SaleaeDigitalCapture {
            sample_rate_hz: digital_rate,
            samples: digital_samples,
            channels: enabled,
        })
    };
    let physical_analog_channels = enabled_analog_channels(&metadata, analog_samples.len());
    let analog: Vec<SaleaeAnalogChannel> = analog_samples
        .into_iter()
        .zip(physical_analog_channels)
        .map(|(samples, index)| {
            SaleaeAnalogChannel::with_automatic_range(
                index,
                metadata
                    .get(&format!("scope.ch{index}.name"))
                    .cloned()
                    .unwrap_or_default(),
                samples,
            )
        })
        .collect();
    let capture = SaleaeCapture {
        digital,
        analog_sample_rate_hz: (!analog.is_empty()).then_some(analog_rate),
        analog,
    };
    write_saleae(output, &capture)?;
    Ok(())
}

fn enabled_analog_channels(metadata: &BTreeMap<String, String>, count: usize) -> Vec<u8> {
    let enabled = (0_u8..32)
        .filter(|index| {
            metadata
                .get(&format!("scope.ch{index}.enabled"))
                .is_some_and(|value| value == "true")
        })
        .collect::<Vec<_>>();
    if enabled.len() == count {
        enabled
    } else {
        (0..count)
            .filter_map(|index| u8::try_from(index).ok())
            .collect()
    }
}

fn read_u8(input: &mut impl Read) -> Result<u8, SaleaeError> {
    Ok(read_array::<1>(input)?[0])
}

fn read_u16(input: &mut impl Read) -> Result<u16, SaleaeError> {
    Ok(u16::from_le_bytes(read_array(input)?))
}

fn read_u32(input: &mut impl Read) -> Result<u32, SaleaeError> {
    Ok(u32::from_le_bytes(read_array(input)?))
}

fn read_u64(input: &mut impl Read) -> Result<u64, SaleaeError> {
    Ok(u64::from_le_bytes(read_array(input)?))
}

fn read_array<const N: usize>(input: &mut impl Read) -> Result<[u8; N], SaleaeError> {
    let mut bytes = [0_u8; N];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn skip_bytes(input: &mut impl Read, mut count: usize) -> Result<(), SaleaeError> {
    let mut buffer = [0_u8; 8 * 1024];
    while count > 0 {
        let length = count.min(buffer.len());
        input.read_exact(&mut buffer[..length])?;
        count -= length;
    }
    Ok(())
}

fn ensure_eof(input: &mut impl Read, name: &str) -> Result<(), SaleaeError> {
    let mut trailing = [0_u8; 1];
    if input.read(&mut trailing)? != 0 {
        return Err(SaleaeError::invalid(format!(
            "{name} contains trailing data"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn example_capture() -> SaleaeCapture {
        SaleaeCapture {
            digital: Some(SaleaeDigitalCapture {
                sample_rate_hz: 10_000_000,
                samples: vec![3, 1, 1, 3, 2, 0, 0, 1],
                channels: vec![
                    SaleaeChannel {
                        index: 0,
                        name: "Clock".to_owned(),
                    },
                    SaleaeChannel {
                        index: 1,
                        name: "Data".to_owned(),
                    },
                ],
            }),
            analog_sample_rate_hz: Some(2_000_000),
            analog: vec![SaleaeAnalogChannel {
                index: 0,
                name: "Voltage".to_owned(),
                samples: vec![-2.5, -1.0, 0.0, 1.0, 2.5, 1.25, -0.5, 0.25],
                minimum_voltage: -3.3,
                maximum_voltage: 3.3,
            }],
        }
    }

    #[test]
    fn saleae_round_trip_preserves_digital_and_analog_capture() {
        let original = example_capture();
        let mut bytes = Cursor::new(Vec::new());
        write_saleae(&mut bytes, &original).expect("capture should serialize");
        bytes.set_position(0);
        let decoded = read_saleae(bytes).expect("capture should parse");

        assert_eq!(decoded.digital, original.digital);
        assert_eq!(decoded.analog.len(), 1);
        assert_eq!(decoded.analog[0].name, "Voltage");
        for (actual, expected) in decoded.analog[0]
            .samples
            .iter()
            .zip(&original.analog[0].samples)
        {
            assert!((actual - expected).abs() < 0.002);
        }
    }

    #[test]
    fn digital_run_encoding_covers_boundaries() {
        for run in [1, 64, 65, 128, 8_192, 1_000_000, u32::MAX as u64] {
            let encoded = encode_run(run);
            let mut cursor = 0;
            assert_eq!(decode_run(&encoded, &mut cursor).unwrap(), run);
            assert_eq!(cursor, encoded.len());
        }
    }

    #[test]
    fn digital_chunks_start_high_as_required_by_saleae_rle() {
        const CHUNK_RLE_OFFSET: usize = 51 + 26;
        let low_capture = SaleaeDigitalCapture {
            sample_rate_hz: 1_000_000,
            samples: vec![0, 0, 0],
            channels: Vec::new(),
        };

        let mut low_first = Vec::new();
        write_digital_bin(&mut low_first, &low_capture, 0).unwrap();
        assert_eq!(&low_first[CHUNK_RLE_OFFSET..CHUNK_RLE_OFFSET + 2], &[0, 1]);

        let high_capture = SaleaeDigitalCapture {
            sample_rate_hz: 1_000_000,
            samples: vec![1, 1, 0],
            channels: Vec::new(),
        };
        let mut high_first = Vec::new();
        write_digital_bin(&mut high_first, &high_capture, 0).unwrap();
        assert_eq!(&high_first[CHUNK_RLE_OFFSET..CHUNK_RLE_OFFSET + 2], &[1, 0]);
    }

    #[test]
    fn conversion_round_trip_preserves_recording_samples() {
        let capture = example_capture();
        let mut sal = Cursor::new(Vec::new());
        write_saleae(&mut sal, &capture).unwrap();
        sal.set_position(0);
        let mut recording = Vec::new();
        import_saleae_recording(&mut sal, &mut recording).unwrap();
        let mut exported = Cursor::new(Vec::new());
        export_saleae_recording(Cursor::new(recording), &mut exported).unwrap();
        exported.set_position(0);
        let decoded = read_saleae(exported).unwrap();
        assert_eq!(
            decoded.digital.unwrap().samples,
            capture.digital.unwrap().samples
        );
        assert_eq!(decoded.analog.len(), 1);
        assert_eq!(
            decoded.analog[0].samples.len(),
            capture.analog[0].samples.len()
        );
    }

    #[test]
    fn rejects_binary_versions_that_are_not_supported() {
        let capture = example_capture();
        let mut bytes = Cursor::new(Vec::new());
        write_saleae(&mut bytes, &capture).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes.into_inner())).unwrap();
        let mut digital = Vec::new();
        archive
            .by_name("digital-0.bin")
            .unwrap()
            .read_to_end(&mut digital)
            .unwrap();
        digital[8..12].copy_from_slice(&2_u32.to_le_bytes());
        let error = read_digital_bin(&mut digital.as_slice()).unwrap_err();
        assert!(error.to_string().contains("version 2 is unsupported"));
    }
}
