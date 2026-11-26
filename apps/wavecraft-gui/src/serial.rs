//! Serial workspace state. Transport and protocol decoding live in wavecraft-serial.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::TryRecvError;
use wavecraft_core::{
    CaptureBatch, CaptureCoordinator, CaptureError, CaptureEvent, CaptureSession,
};
use wavecraft_serial::{
    parse_hex_bytes, parse_hex_u32, validate_text_separator, BinaryDecoder, BinarySchema,
    ByteOrder, CrcConfiguration, DataBits, FlowControl, Parity, SerialConfiguration, SerialSource,
    StopBits, TextDecoder, TextFieldSpec,
};

pub(crate) const RAW_CAPACITY: usize = 65_536;
pub(crate) const POINT_CAPACITY: usize = 10_000;
pub(crate) const MIN_GRAPH_TIME_WINDOW_SECONDS: f64 = 1e-9;
pub(crate) const COLORS: [[u8; 3]; 10] = [
    [99, 179, 255],
    [255, 204, 74],
    [70, 210, 185],
    [213, 126, 255],
    [255, 145, 77],
    [255, 105, 180],
    [166, 227, 86],
    [84, 216, 255],
    [255, 107, 107],
    [160, 150, 255],
];

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum SerialMode {
    #[default]
    Text,
    Binary,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SerialLineStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
    DashDot,
    DashDotDot,
    Star,
    Triangle,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TextField {
    pub name: String,
    #[serde(default)]
    pub separator: String,
    pub color: [u8; 3],
    #[serde(default)]
    pub line_style: SerialLineStyle,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum SerialTimestampUnit {
    #[default]
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

impl SerialTimestampUnit {
    pub const ALL: [Self; 4] = [
        Self::Seconds,
        Self::Milliseconds,
        Self::Microseconds,
        Self::Nanoseconds,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Seconds => "s",
            Self::Milliseconds => "ms",
            Self::Microseconds => "µs",
            Self::Nanoseconds => "ns",
        }
    }
    fn scale(self) -> f64 {
        match self {
            Self::Seconds => 1.0,
            Self::Milliseconds => 1e-3,
            Self::Microseconds => 1e-6,
            Self::Nanoseconds => 1e-9,
        }
    }
}

/// Applied axis configuration stays with the plotted data until Start/Open/Plot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum SerialXAxis {
    #[default]
    Samples,
    SampleRate(f64),
    Timestamp {
        index: usize,
        unit: SerialTimestampUnit,
        multiplier: f64,
    },
}

impl SerialXAxis {
    pub fn is_time(self) -> bool {
        !matches!(self, Self::Samples)
    }
    pub fn minimum_span(self) -> f64 {
        if self.is_time() {
            MIN_GRAPH_TIME_WINDOW_SECONDS
        } else {
            1.0
        }
    }
    pub fn minimum_x(self) -> f64 {
        if matches!(self, Self::Timestamp { .. }) {
            f64::NEG_INFINITY
        } else {
            0.0
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct SerialSettings {
    pub port: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: String,
    pub stop_bits: u8,
    pub flow_control: String,
    pub mode: SerialMode,
    pub timestamp_field: String,
    pub timestamp_unit: SerialTimestampUnit,
    pub timestamp_multiplier: String,
    pub sample_rate: String,
    pub fields: Vec<TextField>,
    pub schema_json: String,
    pub schema_name: String,
    pub start_word: String,
    pub stop_word: String,
    pub crc_width: u8,
    pub polynomial: String,
    pub seed: String,
    pub xor_output: String,
    pub reflect_input: bool,
    pub reflect_output: bool,
    pub crc_big_endian: bool,
    pub crc_include_start: bool,
    pub binary_colors: BTreeMap<String, [u8; 3]>,
    pub binary_line_styles: BTreeMap<String, SerialLineStyle>,
    pub graph_percent: u8,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            port: String::new(),
            baud: 115_200,
            data_bits: 8,
            parity: "None".into(),
            stop_bits: 1,
            flow_control: "None".into(),
            mode: SerialMode::Text,
            timestamp_field: String::new(),
            timestamp_unit: SerialTimestampUnit::default(),
            timestamp_multiplier: "1".into(),
            sample_rate: String::new(),
            fields: Vec::new(),
            schema_json: String::new(),
            schema_name: String::new(),
            start_word: "AA 55".into(),
            stop_word: "0D 0A".into(),
            crc_width: 16,
            polynomial: "1021".into(),
            seed: "FFFF".into(),
            xor_output: "0000".into(),
            reflect_input: false,
            reflect_output: false,
            crc_big_endian: false,
            crc_include_start: false,
            binary_colors: BTreeMap::new(),
            binary_line_styles: BTreeMap::new(),
            graph_percent: 65,
        }
    }
}

impl SerialSettings {
    fn x_axis(&self, names: &[String]) -> Result<SerialXAxis, CaptureError> {
        if !self.timestamp_field.is_empty() {
            let index = names
                .iter()
                .position(|name| name == &self.timestamp_field)
                .ok_or_else(|| {
                    CaptureError::new(
                        "Select a timestamp field from the configured fields or binary schema",
                    )
                })?;
            let multiplier = self
                .timestamp_multiplier
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && *value > 0.0)
                .ok_or_else(|| {
                    CaptureError::new("Timestamp multiplier must be a finite positive number")
                })?;
            let scale = self.timestamp_unit.scale() * multiplier;
            if !scale.is_finite() || scale <= 0.0 {
                return Err(
                    "Timestamp unit and multiplier must produce a finite positive scale".into(),
                );
            }
            return Ok(SerialXAxis::Timestamp {
                index,
                unit: self.timestamp_unit,
                multiplier,
            });
        }
        if self.sample_rate.trim().is_empty() {
            return Ok(SerialXAxis::Samples);
        }
        let rate = self.sample_rate.trim().parse::<f64>().ok().filter(|rate| rate.is_finite() && *rate > 0.0)
            .ok_or_else(|| CaptureError::new("Sample rate must be a finite positive number in Hz, or empty for sample numbers"))?;
        Ok(SerialXAxis::SampleRate(rate))
    }
    pub fn line_style(&self, name: &str) -> SerialLineStyle {
        match self.mode {
            SerialMode::Text => self
                .fields
                .iter()
                .find(|field| field.name.trim() == name)
                .map(|field| field.line_style),
            SerialMode::Binary => self.binary_line_styles.get(name).copied(),
        }
        .unwrap_or_default()
    }

    pub fn configuration(&self) -> Result<SerialConfiguration, CaptureError> {
        if self.port.trim().is_empty() {
            return Err("Select or enter a serial port".into());
        }
        if self.baud == 0 {
            return Err("Baud rate must be greater than zero".into());
        }
        Ok(SerialConfiguration {
            port: self.port.trim().to_owned(),
            baud_rate: self.baud,
            data_bits: match self.data_bits {
                5 => DataBits::Five,
                6 => DataBits::Six,
                7 => DataBits::Seven,
                _ => DataBits::Eight,
            },
            parity: match self.parity.as_str() {
                "Even" => Parity::Even,
                "Odd" => Parity::Odd,
                _ => Parity::None,
            },
            stop_bits: if self.stop_bits == 2 {
                StopBits::Two
            } else {
                StopBits::One
            },
            flow_control: match self.flow_control.as_str() {
                "RTS/CTS" => FlowControl::Hardware,
                "XON/XOFF" => FlowControl::Software,
                _ => FlowControl::None,
            },
        })
    }

    pub fn binary_decoder(&self) -> Result<Option<BinaryDecoder>, CaptureError> {
        if self.mode != SerialMode::Binary || self.schema_json.is_empty() {
            return Ok(None);
        }
        let crc = CrcConfiguration {
            width: self.crc_width,
            polynomial: parse_hex_u32(&self.polynomial)?,
            seed: parse_hex_u32(&self.seed)?,
            xor_output: parse_hex_u32(&self.xor_output)?,
            reflect_input: self.reflect_input,
            reflect_output: self.reflect_output,
            byte_order: if self.crc_big_endian {
                ByteOrder::Big
            } else {
                ByteOrder::Little
            },
            include_start_word: self.crc_include_start,
        };
        BinaryDecoder::new(
            BinarySchema::from_json(&self.schema_json)?,
            parse_hex_bytes(&self.start_word)?,
            parse_hex_bytes(&self.stop_word)?,
            crc,
        )
        .map(Some)
    }
}

pub(crate) struct Series {
    pub name: String,
    pub color: [u8; 3],
    pub points: VecDeque<[f64; 2]>,
}

/// Voltage/value viewport stored in normalized coordinates to retain the
/// existing overflow-safe scaling for very large decoded numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SerialGraphValueRange {
    pub minimum: f64,
    pub maximum: f64,
    pub scale: f64,
}

#[derive(Clone)]
pub(crate) struct ImportedSerialFile {
    path: std::path::PathBuf,
    bytes: std::sync::Arc<[u8]>,
}

pub(crate) struct SerialState {
    pub settings: SerialSettings,
    pub ports: Vec<String>,
    pub session: Option<CaptureSession>,
    pub stopping: bool,
    pub series: Vec<Series>,
    pub raw: VecDeque<u8>,
    pub imported_file: Option<ImportedSerialFile>,
    pub received_bytes: u64,
    pub decoded_records: u64,
    pub rejected_frames: u64,
    pub discarded_bytes: u64,
    pub invalid_x_records: u64,
    pub x_axis: SerialXAxis,
    pub status: String,
    pub error: Option<String>,
    pub baud_draft: Option<String>,
    pub follow: bool,
    pub time_window: f64,
    pub time_window_draft: Option<String>,
    pub graph_value_range: Option<SerialGraphValueRange>,
    pub latest_time: f64,
    pub graph_end: f64,
    pub split_drag_origin: Option<f32>,
    binary: Option<BinaryDecoder>,
    text: TextDecoder,
    raw_lines: Vec<String>,
    raw_dirty: bool,
    sample_index: u64,
    last_timestamp: Option<f64>,
}

impl Default for SerialState {
    fn default() -> Self {
        Self {
            settings: SerialSettings::default(),
            ports: Vec::new(),
            session: None,
            stopping: false,
            series: Vec::new(),
            raw: VecDeque::new(),
            imported_file: None,
            received_bytes: 0,
            decoded_records: 0,
            rejected_frames: 0,
            discarded_bytes: 0,
            invalid_x_records: 0,
            x_axis: SerialXAxis::default(),
            status: "Disconnected".into(),
            error: None,
            baud_draft: None,
            follow: true,
            time_window: 10.0,
            time_window_draft: None,
            graph_value_range: None,
            latest_time: 0.0,
            graph_end: 0.0,
            split_drag_origin: None,
            binary: None,
            text: TextDecoder::default(),
            raw_lines: Vec::new(),
            raw_dirty: true,
            sample_index: 0,
            last_timestamp: None,
        }
    }
}

impl SerialState {
    pub fn graph_time_bounds(&self) -> Option<(f64, f64)> {
        self.series
            .iter()
            .filter_map(|series| Some((series.points.front()?[0], series.points.back()?[0])))
            .reduce(|(start, end), (other_start, other_end)| {
                (start.min(other_start), end.max(other_end))
            })
    }

    pub fn graph_latest_time(&self) -> f64 {
        self.graph_time_bounds()
            .map_or(self.latest_time, |(_, end)| end)
    }

    pub fn graph_time_range(&self) -> (f64, f64) {
        let end = if self.follow {
            self.graph_latest_time()
        } else {
            self.graph_end
        };
        // Keep a nonzero representable span, even for a single sample at a
        // large timestamp or an extremely short imported recording.
        let window = self
            .time_window
            .max(self.x_axis.minimum_span())
            .max(end.abs().max(1.0) * f64::EPSILON * 4.0);
        let start = (end - window).max(self.x_axis.minimum_x());
        (start, (start + window).max(end))
    }

    pub fn fit_graph(&mut self) {
        self.graph_value_range = None;
        self.time_window_draft = None;
        if let Some((start, end)) = self.graph_time_bounds() {
            let span = (end - start).max(self.x_axis.minimum_span());
            // Reconstructing start from end - span can round above the first
            // sample. Widen by a few floating-point ulps to keep it included.
            self.time_window = span * (1.0 + f64::EPSILON * 4.0);
            self.graph_end = end;
            self.follow = true;
        }
    }

    pub fn invalidate_raw(&mut self) {
        self.raw_dirty = true;
    }
    pub fn refresh(&mut self) {
        match wavecraft_serial::available_ports() {
            Ok(mut ports) => {
                ports.sort();
                self.ports = ports;
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn apply_baud_draft(&mut self) -> Result<(), CaptureError> {
        if let Some(draft) = &self.baud_draft {
            self.settings.baud = draft
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|baud| (1..=12_000_000).contains(baud))
                .ok_or_else(|| CaptureError::new("Baud rate must be between 1 and 12000000"))?;
        }
        Ok(())
    }

    pub fn prepare(&mut self) -> Result<(), CaptureError> {
        let binary = self.settings.binary_decoder()?;
        if self.settings.fields.len() > wavecraft_serial::MAX_TEXT_FIELDS {
            return Err("At most 10 text fields are supported".into());
        }
        let names: Vec<_> = self
            .settings
            .fields
            .iter()
            .map(|field| field.name.trim())
            .collect();
        if self.settings.mode == SerialMode::Text
            && names
                .iter()
                .enumerate()
                .any(|(index, name)| name.is_empty() || names[..index].contains(name))
        {
            return Err("Text field names must be nonempty and unique".into());
        }
        if self.settings.mode == SerialMode::Text {
            for field in &self.settings.fields {
                validate_text_separator(&field.separator).map_err(|error| {
                    CaptureError::new(format!("Field '{}': {error}", field.name))
                })?;
            }
        }
        let channel_names: Vec<String> = match self.settings.mode {
            SerialMode::Text => names.iter().map(|name| (*name).to_owned()).collect(),
            SerialMode::Binary if binary.is_some() => {
                BinarySchema::from_json(&self.settings.schema_json)?.channel_names()
            }
            SerialMode::Binary => Vec::new(),
        };
        let x_axis = self.settings.x_axis(&channel_names)?;
        self.clear_data();
        self.x_axis = x_axis;
        self.binary = binary;
        self.series = match self.settings.mode {
            SerialMode::Text => self
                .settings
                .fields
                .iter()
                .map(|field| Series {
                    name: field.name.trim().to_owned(),
                    color: field.color,
                    points: VecDeque::new(),
                })
                .collect(),
            SerialMode::Binary if self.binary.is_some() => {
                BinarySchema::from_json(&self.settings.schema_json)?
                    .channel_names()
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| Series {
                        color: self
                            .settings
                            .binary_colors
                            .get(&name)
                            .copied()
                            .unwrap_or(COLORS[index % COLORS.len()]),
                        name,
                        points: VecDeque::new(),
                    })
                    .collect()
            }
            SerialMode::Binary => Vec::new(),
        };
        self.error = None;
        Ok(())
    }

    pub fn start(&mut self) -> Result<(), CaptureError> {
        self.apply_baud_draft()?;
        let configuration = self.settings.configuration()?;
        self.prepare()?;
        self.session = Some(CaptureCoordinator::start(vec![Box::new(SerialSource(
            configuration,
        ))])?);
        self.stopping = false;
        self.status = format!(
            "Receiving · {} · {} baud",
            self.settings.port, self.settings.baud
        );
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(session) = &self.session {
            session.request_stop();
            self.stopping = true;
            self.status = "Stopping…".into();
        }
    }

    pub fn clear_data(&mut self) {
        if self.imported_file.take().is_some() {
            self.status = "Disconnected".into();
        }
        self.raw.clear();
        self.raw_dirty = true;
        self.received_bytes = 0;
        self.decoded_records = 0;
        self.rejected_frames = 0;
        self.discarded_bytes = 0;
        self.invalid_x_records = 0;
        self.sample_index = 0;
        self.last_timestamp = None;
        self.latest_time = 0.0;
        self.graph_end = 0.0;
        self.follow = true;
        self.graph_value_range = None;
        self.time_window_draft = None;
        self.text = TextDecoder::default();
        if let Some(binary) = &mut self.binary {
            binary.clear();
        }
        for series in &mut self.series {
            series.points.clear();
        }
    }

    /// Raw viewing is independent of graph configuration. Invalid or unfinished
    /// decoding settings must not prevent opening the underlying bytes.
    pub fn import_raw(&mut self, path: &std::path::Path) -> Result<(), CaptureError> {
        use std::io::Read;
        const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;
        let file = std::fs::File::open(path).map_err(|error| {
            CaptureError::new(format!(
                "Could not open raw file {}: {error}",
                path.display()
            ))
        })?;
        let mut bytes = Vec::new();
        file.take((MAX_IMPORT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| CaptureError::new(format!("Could not read raw file: {error}")))?;
        if bytes.len() > MAX_IMPORT_BYTES {
            return Err("Raw imports are limited to 8 MiB".into());
        }
        if let Err(error) = self.prepare() {
            self.clear_data();
            self.binary = None;
            self.series.clear();
            self.x_axis = SerialXAxis::Samples;
            self.error = Some(format!(
                "Raw data imported; graph decoding unavailable: {error}"
            ));
        }
        self.ingest_file(ImportedSerialFile {
            path: path.to_owned(),
            bytes: bytes.into(),
        });
        Ok(())
    }

    pub fn plot_imported(&mut self) -> Result<(), CaptureError> {
        let file = self
            .imported_file
            .clone()
            .ok_or_else(|| CaptureError::new("Open a raw file before plotting"))?;
        if self.settings.mode == SerialMode::Text && self.settings.fields.is_empty() {
            return Err("Add text fields before plotting the opened file".into());
        }
        if self.settings.mode == SerialMode::Binary && self.settings.schema_json.is_empty() {
            return Err("Load a binary JSON schema before plotting the opened file".into());
        }
        self.prepare()?;
        self.ingest_file(file);
        self.fit_graph();
        Ok(())
    }

    fn ingest_file(&mut self, file: ImportedSerialFile) {
        for chunk in file.bytes.chunks(4096) {
            self.ingest(0.0, chunk);
        }
        let name = file.path.file_name().map_or_else(
            || file.path.to_string_lossy(),
            |name| name.to_string_lossy(),
        );
        self.status = format!("Imported · {name}");
        self.imported_file = Some(file);
    }

    /// Transport receive time does not define X; decoded records use the
    /// explicitly configured timestamp, sample rate, or ordinal sample number.
    pub fn ingest(&mut self, _received_seconds: f64, bytes: &[u8]) {
        self.received_bytes += bytes.len() as u64;
        self.raw.extend(bytes.iter().copied());
        let excess = self.raw.len().saturating_sub(RAW_CAPACITY);
        self.raw.drain(..excess);
        self.raw_dirty = true;
        if let Some(binary) = &mut self.binary {
            let records = binary.feed(bytes);
            let rejected = binary.rejected_frames;
            self.discarded_bytes = binary.discarded_bytes;
            for values in records {
                self.push_record(values.into_iter().map(Some).collect());
            }
            self.rejected_frames = rejected + self.invalid_x_records;
        } else if self.settings.mode == SerialMode::Text && !self.series.is_empty() {
            let fields: Vec<_> = self
                .series
                .iter()
                .zip(&self.settings.fields)
                .map(|(series, field)| TextFieldSpec {
                    name: &series.name,
                    separator: &field.separator,
                })
                .collect();
            let records = self.text.feed_with_fields(bytes, &fields);
            for values in records {
                self.push_record(values);
            }
            self.rejected_frames = self.text.oversized_records + self.invalid_x_records;
        }
    }

    fn push_record(&mut self, values: Vec<Option<f64>>) {
        if values.iter().all(Option::is_none) {
            return;
        }
        let sample = self.sample_index;
        self.sample_index = self.sample_index.saturating_add(1);
        let x = match self.x_axis {
            SerialXAxis::Samples => Some(sample as f64),
            SerialXAxis::SampleRate(rate) => Some(sample as f64 / rate),
            SerialXAxis::Timestamp {
                index,
                unit,
                multiplier,
            } => values
                .get(index)
                .copied()
                .flatten()
                .map(|value| value * (unit.scale() * multiplier))
                .filter(|value| {
                    self.last_timestamp
                        .map_or(true, |previous| *value >= previous)
                }),
        }
        .filter(|value| value.is_finite());
        let Some(x) = x else {
            self.invalid_x_records += 1;
            return;
        };
        self.last_timestamp = Some(x);
        self.latest_time = x;
        self.decoded_records += 1;
        for (index, (series, value)) in self.series.iter_mut().zip(values).enumerate() {
            if matches!(self.x_axis, SerialXAxis::Timestamp { index: timestamp, .. } if timestamp == index)
            {
                continue;
            }
            if let Some(value) = value.filter(|value| value.is_finite()) {
                if series.points.len() == POINT_CAPACITY {
                    series.points.pop_front();
                }
                series.points.push_back([x, value]);
            }
        }
    }

    pub fn poll(&mut self) {
        // Bound work per GUI frame; the coordinator's queue provides backpressure.
        for _ in 0..64 {
            let Some(session) = &self.session else {
                break;
            };
            match session.try_recv() {
                Ok(CaptureEvent::Frame(frame)) => {
                    if let CaptureBatch::Serial(bytes) = frame.payload {
                        self.ingest(frame.captured_at.as_secs_f64(), &bytes);
                    }
                }
                Ok(CaptureEvent::SourceFailed { error, .. }) => {
                    self.error = Some(error.to_string());
                    self.finish();
                    break;
                }
                Ok(CaptureEvent::SourceStopped { .. }) | Err(TryRecvError::Disconnected) => {
                    self.finish();
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
    }

    fn finish(&mut self) {
        if let Some(session) = self.session.take() {
            if let Err(error) = session.stop() {
                self.error = Some(error.to_string());
            }
        }
        self.stopping = false;
        self.status = "Disconnected".into();
    }

    pub fn raw_lines(&mut self) -> &[String] {
        if self.raw_dirty {
            self.raw_lines = match self.settings.mode {
                SerialMode::Text => {
                    let mut text = String::new();
                    for (index, byte) in self.raw.iter().enumerate() {
                        match byte {
                            b'\n' => text.push('\n'),
                            b'\r' => {
                                if self.raw.get(index + 1) != Some(&b'\n') {
                                    text.push('\n');
                                }
                            }
                            b'\t' => text.push_str("    "),
                            32..=126 => text.push(char::from(*byte)),
                            _ => text.push_str(&format!("\\x{byte:02X}")),
                        }
                    }
                    // Wrap very long records to keep rendering and horizontal navigation bounded.
                    text.lines()
                        .flat_map(|line| {
                            line.as_bytes()
                                .chunks(256)
                                .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
                                .collect::<Vec<_>>()
                        })
                        .collect()
                }
                SerialMode::Binary => {
                    let bytes: Vec<_> = self.raw.iter().copied().collect();
                    let base = self.received_bytes.saturating_sub(bytes.len() as u64);
                    bytes
                        .chunks(16)
                        .enumerate()
                        .map(|(index, chunk)| {
                            let hex = chunk
                                .iter()
                                .map(|byte| format!("{byte:02X} "))
                                .collect::<String>();
                            let ascii: String = chunk
                                .iter()
                                .map(|byte| {
                                    if (32..=126).contains(byte) {
                                        char::from(*byte)
                                    } else {
                                        '.'
                                    }
                                })
                                .collect();
                            format!("{:08X}  {hex:48} {ascii}", base + (index * 16) as u64)
                        })
                        .collect()
                }
            };
            self.raw_dirty = false;
        }
        &self.raw_lines
    }

    pub fn load_schema(&mut self, name: String, json: String) -> Result<(), CaptureError> {
        let schema = BinarySchema::from_json(&json)?;
        self.settings.schema_name = name;
        self.settings.schema_json = json;
        let names = schema.channel_names();
        self.settings.binary_line_styles = names
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    self.settings
                        .binary_line_styles
                        .get(name)
                        .copied()
                        .unwrap_or_default(),
                )
            })
            .collect();
        self.settings.binary_colors = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let color = self
                    .settings
                    .binary_colors
                    .get(&name)
                    .copied()
                    .unwrap_or(COLORS[index % COLORS.len()]);
                (name, color)
            })
            .collect();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis_state(mode: SerialMode) -> SerialState {
        let mut state = SerialState::default();
        state.settings.mode = mode;
        if mode == SerialMode::Text {
            state.settings.fields = ["t", "v"]
                .into_iter()
                .enumerate()
                .map(|(index, name)| TextField {
                    name: name.into(),
                    separator: String::new(),
                    color: COLORS[index],
                    line_style: Default::default(),
                })
                .collect();
        } else {
            state.load_schema("timestamp.json".into(), r#"{"start_word_bytes":2,"stop_word_bytes":2,"fields":[{"name":"t","kind":"float","size_bytes":8},{"name":"v","kind":"float","size_bytes":4}]}"#.into()).unwrap();
        }
        state
    }

    fn axis_records(mode: SerialMode, records: &[(f64, f32)]) -> Vec<u8> {
        let crc = CrcConfiguration {
            width: 16,
            polynomial: 0x1021,
            seed: 0xffff,
            xor_output: 0,
            reflect_input: false,
            reflect_output: false,
            byte_order: ByteOrder::Little,
            include_start_word: false,
        };
        let mut bytes = Vec::new();
        for &(timestamp, value) in records {
            if mode == SerialMode::Text {
                bytes.extend_from_slice(format!("t={timestamp} v={value}\r\n").as_bytes());
            } else {
                let mut payload = timestamp.to_le_bytes().to_vec();
                payload.extend_from_slice(&value.to_le_bytes());
                bytes.extend_from_slice(&[0xaa, 0x55]);
                bytes.extend_from_slice(&payload);
                bytes.extend_from_slice(&(crc.checksum(&payload).unwrap() as u16).to_le_bytes());
                bytes.extend_from_slice(b"\r\n");
            }
        }
        bytes
    }

    #[test]
    fn axes_use_sample_numbers_or_shared_rate_without_guessing_timestamp_fields() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let bytes = axis_records(mode, &[(100.0, 1.0), (250.0, 2.0), (900.0, 3.0)]);
            let mut state = axis_state(mode);
            state.prepare().unwrap();
            for chunk in bytes.chunks(7) {
                state.ingest(9999.0, chunk);
            }
            assert_eq!(state.x_axis, SerialXAxis::Samples);
            assert_eq!(
                state.series[1]
                    .points
                    .iter()
                    .map(|point| point[0])
                    .collect::<Vec<_>>(),
                vec![0.0, 1.0, 2.0]
            );
            assert_eq!(state.series[0].points.len(), 3); // t is ordinary Y until explicitly selected.
            state.settings.sample_rate = "100".into();
            state.prepare().unwrap();
            for chunk in bytes.chunks(3) {
                state.ingest(0.0, chunk);
            }
            assert_eq!(state.x_axis, SerialXAxis::SampleRate(100.0));
            assert_eq!(
                state.series[1]
                    .points
                    .iter()
                    .map(|point| point[0])
                    .collect::<Vec<_>>(),
                vec![0.0, 0.01, 0.02]
            );
            state.clear_data();
            state.ingest(123.0, &bytes);
            assert_eq!(state.series[1].points.front().unwrap()[0], 0.0);
        }
    }

    #[test]
    fn selected_timestamp_uses_shared_unit_and_multiplier_and_overrides_rate() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let bytes = axis_records(mode, &[(-1.0, 1.0), (0.0, 2.0), (2.0, 3.0)]);
            let mut state = axis_state(mode);
            state.settings.timestamp_field = "t".into();
            state.settings.timestamp_unit = SerialTimestampUnit::Milliseconds;
            state.settings.timestamp_multiplier = "10".into();
            state.settings.sample_rate = "invalid but unused".into();
            state.prepare().unwrap();
            for chunk in bytes.chunks(2) {
                state.ingest(9999.0, chunk);
            }
            assert_eq!(
                state.series[1]
                    .points
                    .iter()
                    .map(|point| point[0])
                    .collect::<Vec<_>>(),
                vec![-0.01, 0.0, 0.02]
            );
            assert!(state.series[0].points.is_empty());
            assert_eq!(state.decoded_records, 3);
            state.fit_graph();
            let (start, end) = state.graph_time_range();
            assert!(start <= -0.01 && end >= 0.02);
            let restored: SerialSettings =
                serde_json::from_str(&serde_json::to_string(&state.settings).unwrap()).unwrap();
            assert_eq!(restored, state.settings);
            state.settings.mode = if mode == SerialMode::Text {
                SerialMode::Binary
            } else {
                SerialMode::Text
            };
            assert_eq!(
                state.settings.timestamp_unit,
                SerialTimestampUnit::Milliseconds
            );
            assert_eq!(state.settings.timestamp_multiplier, "10");
            assert_eq!(state.settings.sample_rate, "invalid but unused");
        }
    }

    #[test]
    fn invalid_timestamps_are_rejected_without_reordering_or_losing_raw_bytes() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let bytes = axis_records(mode, &[(2.0, 2.0), (1.0, 1.0), (f64::NAN, 3.0), (3.0, 4.0)]);
            let mut state = axis_state(mode);
            state.settings.timestamp_field = "t".into();
            state.prepare().unwrap();
            for chunk in bytes.chunks(5) {
                state.ingest(0.0, chunk);
            }
            assert_eq!(state.decoded_records, 2);
            assert_eq!(state.invalid_x_records, 2);
            assert_eq!(state.rejected_frames, 2);
            assert_eq!(
                state.series[1]
                    .points
                    .iter()
                    .map(|point| point[0])
                    .collect::<Vec<_>>(),
                vec![2.0, 3.0]
            );
            assert_eq!(state.raw.iter().copied().collect::<Vec<_>>(), bytes);
        }
    }

    #[test]
    fn axis_settings_validate_before_clearing_existing_data() {
        let mut state = axis_state(SerialMode::Text);
        state.prepare().unwrap();
        state.ingest(0.0, b"t=1 v=2\n");
        let raw = state.raw.clone();
        for rate in ["0", "-1", "NaN", "inf", "oops"] {
            state.settings.sample_rate = rate.into();
            assert!(state.prepare().is_err());
            assert_eq!(state.raw, raw);
        }
        state.settings.timestamp_field = "missing".into();
        assert!(state.prepare().is_err());
        state.settings.timestamp_field = "t".into();
        for multiplier in ["0", "-1", "NaN", "", "inf"] {
            state.settings.timestamp_multiplier = multiplier.into();
            assert!(state.prepare().is_err());
            assert_eq!(state.raw, raw);
        }
    }

    #[test]
    fn replot_rebuilds_coordinates_from_common_axis_settings() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let mut state = axis_state(mode);
            state.prepare().unwrap();
            state.ingest_file(ImportedSerialFile {
                path: "ticks.raw".into(),
                bytes: axis_records(mode, &[(1.0, 10.0), (2.0, 20.0), (3.0, 30.0)]).into(),
            });
            let raw = state.raw.clone();
            assert_eq!(state.series[1].points.back().unwrap()[0], 2.0);
            state.settings.timestamp_field = "t".into();
            state.settings.timestamp_unit = SerialTimestampUnit::Milliseconds;
            state.settings.timestamp_multiplier = "10".into();
            assert_eq!(state.x_axis, SerialXAxis::Samples); // Existing graph retains its axis until Plot.
            state.plot_imported().unwrap();
            assert_eq!(
                state.series[1]
                    .points
                    .iter()
                    .map(|point| point[0])
                    .collect::<Vec<_>>(),
                vec![0.01, 0.02, 0.03]
            );
            state.settings.timestamp_field.clear();
            state.settings.sample_rate = "1000".into();
            state.settings.baud = 9600;
            state.plot_imported().unwrap();
            assert_eq!(
                state.series[1]
                    .points
                    .iter()
                    .map(|point| point[0])
                    .collect::<Vec<_>>(),
                vec![0.0, 0.001, 0.002]
            );
            assert_eq!(state.raw, raw);
            assert!(state.status.contains("ticks.raw"));
        }
    }

    #[test]
    fn ascii_separators_apply_to_live_records_and_can_be_changed_before_plot() {
        let mut state = SerialState::default();
        state.settings.fields = vec![
            TextField {
                name: "temperature".into(),
                separator: "->".into(),
                color: COLORS[0],
                line_style: Default::default(),
            },
            TextField {
                name: "speed".into(),
                separator: "".into(),
                color: COLORS[1],
                line_style: Default::default(),
            },
        ];
        state.prepare().unwrap();
        state.ingest(0.5, b"temperature-");
        state.ingest(1.0, b">-2.5 speed: 42\ntemperature=99 speed=43\n");
        assert_eq!(
            state.series[0]
                .points
                .iter()
                .map(|point| point[1])
                .collect::<Vec<_>>(),
            vec![-2.5]
        );
        assert_eq!(
            state.series[1]
                .points
                .iter()
                .map(|point| point[1])
                .collect::<Vec<_>>(),
            vec![42.0, 43.0]
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/examples/serial-ascii.txt");
        state.import_raw(&path).unwrap();
        assert!(state.series[0].points.is_empty());
        let raw = state.raw.clone();
        state.settings.fields[0].separator = "=".into();
        state.plot_imported().unwrap();
        assert_eq!(state.series[0].points.len(), 512);
        assert_eq!(state.raw, raw);
        for separator in ["abcd", "→"] {
            state.settings.fields[0].separator = separator.into();
            let error = state.plot_imported().unwrap_err().to_string();
            assert!(error.contains("temperature") && error.contains("3 ASCII"));
            assert_eq!(state.series[0].points.len(), 512);
            assert_eq!(state.raw, raw);
        }
        state.settings.fields[0].separator.clear();
        state.plot_imported().unwrap();
        assert_eq!(state.series[0].points.len(), 512);
    }

    #[test]
    fn opened_file_can_be_plotted_after_configuring_fields_or_schema() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples");
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let mut state = SerialState::default();
            state.settings.mode = mode;
            let name = if mode == SerialMode::Text {
                "serial-ascii.txt"
            } else {
                "serial-binary.bin"
            };
            state.import_raw(&root.join(name)).unwrap();
            assert!(state.status.contains(name));
            assert!(state.series.is_empty());
            let raw = state.raw.clone();
            assert!(state.plot_imported().is_err());
            assert_eq!(state.raw, raw);
            assert!(state.imported_file.is_some());
            if mode == SerialMode::Text {
                state.settings.fields.push(TextField {
                    separator: String::new(),
                    name: "temperature".into(),
                    color: COLORS[1],
                    line_style: SerialLineStyle::Dashed,
                });
            } else {
                state
                    .load_schema(
                        "serial-schema.json".into(),
                        include_str!("../../../docs/examples/serial-schema.json").into(),
                    )
                    .unwrap();
                state
                    .settings
                    .binary_colors
                    .insert("temperature".into(), COLORS[1]);
            }
            for _ in 0..2 {
                state.plot_imported().unwrap();
                assert_eq!(state.decoded_records, 512);
                assert_eq!(state.series[0].points.len(), 512);
                assert_eq!(state.series[0].color, COLORS[1]);
                assert_eq!(state.raw, raw);
                assert!(state.status.contains(name));
                assert!(state.imported_file.is_some());
                assert!(state.error.is_none());
                let (start, end) = state.graph_time_range();
                let (first, last) = state.graph_time_bounds().unwrap();
                assert!(start <= first && end >= last);
            }
            state.clear_data();
            assert!(state.imported_file.is_none());
            assert!(!state.status.contains(name));
        }
    }

    #[test]
    fn plotting_uses_full_file_snapshot_and_new_fields_and_timing() {
        let mut state = SerialState::default();
        let bytes = include_bytes!("../../../docs/examples/serial-ascii.txt").repeat(4);
        assert!(bytes.len() > RAW_CAPACITY);
        let total = bytes.len();
        state.ingest_file(ImportedSerialFile {
            path: "no-longer-on-disk.txt".into(),
            bytes: bytes.into(),
        });
        let raw = state.raw.clone();
        assert_eq!(raw.len(), RAW_CAPACITY);
        state.settings.fields.push(TextField {
            separator: String::new(),
            name: "temperature".into(),
            color: COLORS[0],
            line_style: Default::default(),
        });
        state.plot_imported().unwrap();
        assert_eq!(state.received_bytes, total as u64);
        assert_eq!(state.decoded_records, 2048);
        assert_eq!(state.series[0].points.len(), 2048);
        let original_end = state.graph_latest_time();
        state.baud_draft = Some("57600".into());
        state.settings.fields[0].name = "speed".into();
        state.settings.sample_rate = "0.5".into();
        state.plot_imported().unwrap();
        assert_eq!(state.series[0].name, "speed");
        assert_eq!(state.series[0].points.len(), 2048);
        assert!((state.graph_latest_time() - original_end * 2.0).abs() < 1e-10);
        assert_eq!(state.raw, raw);
        state.settings.fields[0].name.clear();
        assert!(state.plot_imported().is_err());
        assert_eq!(state.series[0].points.len(), 2048);
        assert_eq!(state.raw, raw);
        assert!(state.status.contains("no-longer-on-disk.txt"));
        // Preparing a new live capture releases the opened-file snapshot.
        state.settings.fields[0].name = "speed".into();
        state.prepare().unwrap();
        assert!(state.imported_file.is_none());
        assert!(!state.status.contains("no-longer-on-disk.txt"));
    }

    #[test]
    fn raw_file_import_works_without_fields_or_schema_and_with_incomplete_settings() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            for incomplete in [false, true] {
                let mut state = SerialState::default();
                state.settings.mode = mode;
                if incomplete {
                    if mode == SerialMode::Text {
                        state.settings.fields.push(TextField {
                            separator: String::new(),
                            name: String::new(),
                            color: COLORS[0],
                            line_style: Default::default(),
                        });
                    } else {
                        state.settings.schema_json = "{".into();
                    }
                }
                state.ingest(1.0, b"previous file");
                let name = if mode == SerialMode::Text {
                    "serial-ascii.txt"
                } else {
                    "serial-binary.bin"
                };
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../docs/examples")
                    .join(name);
                let expected = std::fs::read(&path).unwrap();
                state.import_raw(&path).unwrap();
                assert_eq!(state.raw.iter().copied().collect::<Vec<_>>(), expected);
                assert_eq!(state.received_bytes, expected.len() as u64);
                assert_eq!(state.decoded_records, 0);
                assert!(state.series.is_empty());
                assert!(state.binary.is_none());
                assert_eq!(state.error.is_some(), incomplete);
                assert!(!state.raw_lines().is_empty());
            }
        }
    }

    #[test]
    fn file_decoding_does_not_depend_on_baud_draft() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples");
        let mut state = SerialState::default();
        state.settings.mode = SerialMode::Binary;
        state
            .load_schema(
                "serial-schema.json".into(),
                include_str!("../../../docs/examples/serial-schema.json").into(),
            )
            .unwrap();
        state.import_raw(&root.join("serial-binary.bin")).unwrap();
        assert_eq!(state.decoded_records, 512);
        assert_eq!(state.series.len(), 5);
        assert!(state.error.is_none());
        state.baud_draft = Some(String::new());
        state.import_raw(&root.join("serial-binary.bin")).unwrap();
        assert_eq!(state.received_bytes, 6656);
        assert!(state.binary.is_some());
        assert_eq!(state.decoded_records, 512);
        assert!(state.error.is_none());
        let retained = state.raw.clone();
        assert!(state
            .import_raw(&root.join("missing-raw-file-for-import-test.bin"))
            .is_err());
        assert_eq!(state.raw, retained);
    }

    #[test]
    fn absent_schema_or_fields_keeps_exact_raw_bytes_and_no_plotted_values() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let mut state = SerialState::default();
            state.settings.mode = mode;
            // Invalid CRC settings are irrelevant when no schema is configured.
            state.settings.polynomial = "invalid".into();
            state.prepare().unwrap();
            state.ingest(0.5, b"temp=42\r\n\x00\xff");
            assert_eq!(
                state.raw.iter().copied().collect::<Vec<_>>(),
                b"temp=42\r\n\x00\xff"
            );
            assert!(state.series.is_empty());
            assert_eq!(state.decoded_records, 0);
            assert!(!state.raw_lines().is_empty());
        }
    }

    #[test]
    fn imported_and_live_records_use_sample_numbers_independent_of_receive_timing() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let mut imported = SerialState::default();
            imported.settings.mode = mode;
            let bytes = if mode == SerialMode::Text {
                imported.settings.fields.push(TextField {
                    separator: String::new(),
                    name: "v".into(),
                    color: COLORS[0],
                    line_style: SerialLineStyle::Solid,
                });
                b"v=1\nv=2\nv=3\n".to_vec()
            } else {
                imported
                    .load_schema(
                        "example.json".into(),
                        include_str!("../../../docs/examples/serial-schema.json").into(),
                    )
                    .unwrap();
                let crc = CrcConfiguration {
                    width: 16,
                    polynomial: 0x1021,
                    seed: 0xffff,
                    reflect_input: false,
                    reflect_output: false,
                    xor_output: 0,
                    byte_order: ByteOrder::Little,
                    include_start_word: false,
                };
                let mut bytes = Vec::new();
                for value in [1.0_f32, 2.0, 3.0] {
                    let mut payload = value.to_le_bytes().to_vec();
                    payload.extend_from_slice(&[0, 0, 0]);
                    bytes.extend_from_slice(&[0xaa, 0x55]);
                    bytes.extend_from_slice(&payload);
                    bytes
                        .extend_from_slice(&(crc.checksum(&payload).unwrap() as u16).to_le_bytes());
                    bytes.extend_from_slice(b"\r\n");
                }
                bytes
            };
            let mut live = SerialState {
                settings: imported.settings.clone(),
                ..Default::default()
            };
            imported.prepare().unwrap();
            live.prepare().unwrap();
            imported.ingest(0.0006, &bytes);
            live.ingest(0.0006, &bytes);
            assert_eq!(imported.decoded_records, 3, "{mode:?}");
            let points = &imported.series[0].points;
            assert!(points
                .iter()
                .zip(points.iter().skip(1))
                .all(|(a, b)| a[0] < b[0]));
            assert_eq!(live.series[0].points, *points);
            assert_eq!(
                points.iter().map(|point| point[0]).collect::<Vec<_>>(),
                vec![0.0, 1.0, 2.0]
            );
            assert_eq!(imported.raw.iter().copied().collect::<Vec<_>>(), bytes);
            let point_start = points.front().unwrap()[0];
            let point_end = points.back().unwrap()[0];
            imported.fit_graph();
            let (start, end) = imported.graph_time_range();
            assert!((start - point_start).abs() < 1e-12);
            assert!((end - point_end).abs() < 1e-12);
        }
    }

    #[test]
    fn fit_without_data_is_a_noop_and_a_single_sample_has_a_finite_visible_range() {
        let mut state = SerialState {
            time_window: 4.0,
            ..Default::default()
        };
        state.fit_graph();
        assert_eq!(state.time_window, 4.0);
        state.settings.fields.push(TextField {
            separator: String::new(),
            name: "v".into(),
            color: COLORS[0],
            line_style: SerialLineStyle::Solid,
        });
        state.prepare().unwrap();
        state.settings.fields.push(TextField {
            name: "ts".into(),
            separator: String::new(),
            color: COLORS[1],
            line_style: Default::default(),
        });
        state.settings.timestamp_field = "ts".into();
        state.prepare().unwrap();
        state.ingest(0.0, b"v=1 ts=1000000000000\n");
        state.fit_graph();
        let (start, end) = state.graph_time_range();
        assert!(start.is_finite() && end.is_finite() && start < end);
        assert!(start <= 1e12 && end >= 1e12);
    }

    #[test]
    fn text_points_keep_channel_identity_color_and_missing_values() {
        let mut state = SerialState::default();
        state.settings.fields = vec![
            TextField {
                separator: String::new(),
                name: "temp".into(),
                color: COLORS[1],
                line_style: Default::default(),
            },
            TextField {
                separator: String::new(),
                name: "rpm".into(),
                color: COLORS[2],
                line_style: Default::default(),
            },
        ];
        state.prepare().unwrap();
        state.ingest(0.1, b"temp=1 rpm=");
        state.ingest(0.2, b"4\ntemp=2\n");
        assert_eq!(
            state.series[0].points.iter().copied().collect::<Vec<_>>(),
            [[0.0, 1.0], [1.0, 2.0]]
        );
        assert_eq!(
            state.series[1].points.iter().copied().collect::<Vec<_>>(),
            [[0.0, 4.0]]
        );
        assert_eq!(state.series[0].color, COLORS[1]);
        state.follow = false;
        state.graph_end = 10.0;
        state.clear_data();
        assert!(state.series.iter().all(|series| series.points.is_empty()));
        assert!(state.raw.is_empty());
        assert!(state.follow);
        assert_eq!(state.graph_end, 0.0);
    }

    #[test]
    fn raw_ascii_display_preserves_cr_records_and_escapes_nonprintable_bytes() {
        let mut state = SerialState::default();
        state.ingest(1.0, b"a\rb\r\nc\n\x00\xff");
        assert_eq!(state.raw_lines(), ["a", "b", "c", "\\x00\\xFF"]);
        assert_eq!(
            state.raw.iter().copied().collect::<Vec<_>>(),
            b"a\rb\r\nc\n\x00\xff"
        );
    }

    #[test]
    fn retains_bounded_raw_and_plot_history_with_total_byte_counter() {
        let mut state = SerialState::default();
        state.settings.fields.push(TextField {
            separator: String::new(),
            name: "v".into(),
            color: COLORS[0],
            line_style: Default::default(),
        });
        state.prepare().unwrap();
        for index in 0..POINT_CAPACITY + 100 {
            state.ingest(index as f64, b"v=123456\n");
        }
        assert_eq!(state.raw.len(), RAW_CAPACITY);
        assert_eq!(state.series[0].points.len(), POINT_CAPACITY);
        assert_eq!(state.series[0].points.front().unwrap()[0], 100.0);
        assert_eq!(state.received_bytes, (POINT_CAPACITY as u64 + 100) * 9);
    }

    #[test]
    fn applying_new_mode_cannot_reuse_an_old_binary_decoder() {
        let mut state = SerialState::default();
        state.settings.mode = SerialMode::Binary;
        state
            .load_schema(
                "example.json".into(),
                include_str!("../../../docs/examples/serial-schema.json").into(),
            )
            .unwrap();
        state.prepare().unwrap();
        assert_eq!(state.series.len(), 5);
        assert!(state.binary.is_some());
        state.settings.schema_json.clear();
        state.prepare().unwrap();
        state.ingest(1.0, &[0xaa, 0x55]);
        assert!(state.series.is_empty());
        assert!(state.binary.is_none());
        state.settings.mode = SerialMode::Text;
        state.prepare().unwrap();
        assert!(state.series.is_empty());
    }

    #[test]
    fn parsing_binary_fields_initializes_styles_and_keeps_customizations_for_retained_names() {
        let mut state = SerialState::default();
        state.settings.mode = SerialMode::Binary;
        let original = include_str!("../../../docs/examples/serial-schema.json");
        state
            .load_schema("original.json".into(), original.into())
            .unwrap();
        assert_eq!(state.settings.binary_colors.len(), 5);
        assert_eq!(state.settings.binary_line_styles.len(), 5);
        state
            .settings
            .binary_colors
            .insert("temperature".into(), [1, 2, 3]);
        state
            .settings
            .binary_line_styles
            .insert("temperature".into(), SerialLineStyle::Star);
        state
            .settings
            .binary_line_styles
            .insert("speed".into(), SerialLineStyle::Triangle);
        state
            .load_schema("updated.json".into(), original.replace("enabled", "ready"))
            .unwrap();
        assert_eq!(state.settings.binary_colors["temperature"], [1, 2, 3]);
        assert_eq!(
            state.settings.line_style("temperature"),
            SerialLineStyle::Star
        );
        assert_eq!(
            state.settings.line_style("speed"),
            SerialLineStyle::Triangle
        );
        assert!(!state.settings.binary_colors.contains_key("enabled"));
        assert!(!state.settings.binary_line_styles.contains_key("enabled"));
        assert_eq!(state.settings.line_style("ready"), SerialLineStyle::Solid);
        let serialized = serde_json::to_string(&state.settings).unwrap();
        let restored: SerialSettings = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored, state.settings);
    }

    #[test]
    fn old_serial_settings_default_to_solid_lines_in_both_modes() {
        let mut settings: SerialSettings = serde_json::from_str(
            r#"{
            "fields": [{"name":"voltage", "color":[12,34,56]}]
        }"#,
        )
        .unwrap();
        assert_eq!(settings.line_style("voltage"), SerialLineStyle::Solid);
        assert!(settings.fields[0].separator.is_empty());
        assert!(settings.timestamp_field.is_empty());
        assert!(settings.sample_rate.is_empty());
        assert_eq!(settings.timestamp_multiplier, "1");
        settings.fields[0].line_style = SerialLineStyle::DashDotDot;
        assert_eq!(settings.line_style("voltage"), SerialLineStyle::DashDotDot);
        settings.mode = SerialMode::Binary;
        assert_eq!(settings.line_style("voltage"), SerialLineStyle::Solid);
        settings
            .binary_line_styles
            .insert("voltage".into(), SerialLineStyle::Dashed);
        assert_eq!(settings.line_style("voltage"), SerialLineStyle::Dashed);
    }

    #[test]
    fn serial_workspace_settings_restore_on_restart() {
        let mut app = crate::app::test_support::app();
        app.serial.settings.port = "/dev/test-uart".into();
        app.serial.settings.sample_rate = "1000".into();
        app.serial.settings.timestamp_unit = SerialTimestampUnit::Milliseconds;
        app.serial.settings.timestamp_multiplier = "10".into();
        app.serial.settings.fields.push(TextField {
            separator: "=>".into(),
            name: "voltage".into(),
            color: [12, 34, 56],
            line_style: SerialLineStyle::DashDot,
        });
        app.serial
            .settings
            .binary_line_styles
            .insert("flags.ready".into(), SerialLineStyle::Dotted);
        app.serial.settings.graph_percent = 40;
        app.active_workspace_view = Some(crate::WorkspaceView::Serial);
        app.save_global_settings();
        let mut restored = crate::WaveCraftApp::with_settings(None, app.saved_capture_settings);
        assert!(restored.serial_view_is_active());
        restored.reconcile_waveform_tab();
        assert!(restored.serial_view_is_active());
        assert_eq!(restored.serial.settings, app.serial.settings);
        assert!(restored.serial.session.is_none());
    }
}
