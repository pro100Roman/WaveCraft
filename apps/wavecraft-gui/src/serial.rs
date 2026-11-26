//! Serial workspace state. Transport and protocol decoding live in wavecraft-serial.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::TryRecvError;
use wavecraft_core::{
    CaptureBatch, CaptureCoordinator, CaptureError, CaptureEvent, CaptureSession,
};
use wavecraft_serial::{
    parse_hex_bytes, parse_hex_u32, BinaryDecoder, BinarySchema, ByteOrder, CrcConfiguration,
    DataBits, FlowControl, Parity, SerialConfiguration, SerialSource, StopBits, TextDecoder,
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
    pub color: [u8; 3],
    #[serde(default)]
    pub line_style: SerialLineStyle,
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

pub(crate) struct SerialState {
    pub settings: SerialSettings,
    pub ports: Vec<String>,
    pub session: Option<CaptureSession>,
    pub stopping: bool,
    pub series: Vec<Series>,
    pub raw: VecDeque<u8>,
    pub received_bytes: u64,
    pub decoded_records: u64,
    pub rejected_frames: u64,
    pub discarded_bytes: u64,
    pub status: String,
    pub error: Option<String>,
    pub baud_draft: Option<String>,
    pub follow: bool,
    pub time_window: f64,
    pub latest_time: f64,
    pub graph_end: f64,
    pub split_drag_origin: Option<f32>,
    binary: Option<BinaryDecoder>,
    text: TextDecoder,
    raw_lines: Vec<String>,
    raw_dirty: bool,
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
            received_bytes: 0,
            decoded_records: 0,
            rejected_frames: 0,
            discarded_bytes: 0,
            status: "Disconnected".into(),
            error: None,
            baud_draft: None,
            follow: true,
            time_window: 10.0,
            latest_time: 0.0,
            graph_end: 0.0,
            split_drag_origin: None,
            binary: None,
            text: TextDecoder::default(),
            raw_lines: Vec::new(),
            raw_dirty: true,
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
            .max(MIN_GRAPH_TIME_WINDOW_SECONDS)
            .max(end.abs().max(1.0) * f64::EPSILON * 4.0);
        let start = (end - window).max(0.0);
        (start, (start + window).max(end))
    }

    pub fn fit_graph(&mut self) {
        if let Some((start, end)) = self.graph_time_bounds() {
            let span = (end - start).max(MIN_GRAPH_TIME_WINDOW_SECONDS);
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
        self.apply_baud_draft()?;
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
        self.clear_data();
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
        self.raw.clear();
        self.raw_dirty = true;
        self.received_bytes = 0;
        self.decoded_records = 0;
        self.rejected_frames = 0;
        self.discarded_bytes = 0;
        self.latest_time = 0.0;
        self.graph_end = 0.0;
        self.follow = true;
        self.text = TextDecoder::default();
        if let Some(binary) = &mut self.binary {
            binary.clear();
        }
        for series in &mut self.series {
            series.points.clear();
        }
    }

    pub fn ingest(&mut self, seconds: f64, bytes: &[u8]) {
        self.ingest_interval(seconds, seconds, bytes);
    }

    /// Raw files have no capture timestamps. Spread their decoded records over
    /// the byte interval estimated from the UART baud rate, rather than giving
    /// an entire read block one timestamp. Live receive timing stays unchanged.
    pub fn ingest_imported(&mut self, start_seconds: f64, end_seconds: f64, bytes: &[u8]) {
        self.ingest_interval(start_seconds, end_seconds, bytes);
    }

    fn ingest_interval(&mut self, start_seconds: f64, seconds: f64, bytes: &[u8]) {
        self.received_bytes += bytes.len() as u64;
        self.latest_time = seconds;
        self.raw.extend(bytes.iter().copied());
        let excess = self.raw.len().saturating_sub(RAW_CAPACITY);
        self.raw.drain(..excess);
        self.raw_dirty = true;
        if let Some(binary) = &mut self.binary {
            let records = binary.feed(bytes);
            self.rejected_frames = binary.rejected_frames;
            self.discarded_bytes = binary.discarded_bytes;
            let count = records.len();
            for (index, values) in records.into_iter().enumerate() {
                self.decoded_records += 1;
                let time =
                    start_seconds + (seconds - start_seconds) * (index + 1) as f64 / count as f64;
                self.push_values(time, values.into_iter().map(Some));
            }
        } else if self.settings.mode == SerialMode::Text && !self.series.is_empty() {
            let fields: Vec<_> = self
                .series
                .iter()
                .map(|series| series.name.clone())
                .collect();
            let records = self.text.feed(bytes, &fields);
            let count = records.len();
            for (index, values) in records.into_iter().enumerate() {
                if values.iter().any(Option::is_some) {
                    self.decoded_records += 1;
                }
                let time =
                    start_seconds + (seconds - start_seconds) * (index + 1) as f64 / count as f64;
                self.push_values(time, values.into_iter());
            }
            self.rejected_frames = self.text.oversized_records;
        }
    }

    fn push_values(&mut self, seconds: f64, values: impl Iterator<Item = Option<f64>>) {
        for (series, value) in self.series.iter_mut().zip(values) {
            if let Some(value) = value.filter(|value| value.is_finite()) {
                if series.points.len() == POINT_CAPACITY {
                    series.points.pop_front();
                }
                series.points.push_back([seconds, value]);
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
    fn raw_import_separates_records_in_one_read_while_live_records_share_receive_time() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let mut imported = SerialState::default();
            imported.settings.mode = mode;
            let bytes = if mode == SerialMode::Text {
                imported.settings.fields.push(TextField {
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
            imported.ingest_imported(0.0, 0.0006, &bytes);
            live.ingest(0.0006, &bytes);
            assert_eq!(imported.decoded_records, 3, "{mode:?}");
            let points = &imported.series[0].points;
            assert!(points
                .iter()
                .zip(points.iter().skip(1))
                .all(|(a, b)| a[0] < b[0]));
            assert!(live.series[0].points.iter().all(|point| point[0] == 0.0006));
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
            name: "v".into(),
            color: COLORS[0],
            line_style: SerialLineStyle::Solid,
        });
        state.prepare().unwrap();
        state.ingest(1e12, b"v=1\n");
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
                name: "temp".into(),
                color: COLORS[1],
                line_style: Default::default(),
            },
            TextField {
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
            [[0.2, 1.0], [0.2, 2.0]]
        );
        assert_eq!(
            state.series[1].points.iter().copied().collect::<Vec<_>>(),
            [[0.2, 4.0]]
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
        app.serial.settings.fields.push(TextField {
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
