//! Recording files: choosing paths, writing captures and decoded data, and
//! opening native `.dsv`, decoded-data, and Saleae recordings.

use crate::{
    channel_names_from_recording_metadata, coupling_label, empty_logic_channel_names,
    empty_scope_channel_names, format_capture_timestamp, ingest_radio_iq,
    insert_channel_name_metadata, insert_radio_analyzer_channels_metadata,
    logic_settings_from_recording_metadata, logic_trigger_condition_key, normalized_channel_name,
    radio_settings_from_recording_metadata, read_saleae, scope_settings_from_recording_metadata,
    waveform::radio::RawRadioMessage, Arc, BTreeMap, BufReader, ByteStatus, CaptureBatch,
    CaptureEvent, CaptureTarget, CompletedCapture, DeviceDescriptor, DsCopeCaptureConfig,
    DsCopeTriggerSlope, DsCopeTriggerSource, DsLogicCaptureConfig, DsLogicInputFilter,
    DsLogicOperationMode, DsLogicStopOption, DsLogicTriggerConfig, Duration, FileRecorder,
    InstrumentKind, LogicCaptureSettings, ProtocolAnalyzerInstance, ProtocolCaptureDecoder,
    RadioAnalysisStore, RadioAnalyzerBank, RadioAnalyzerChannelSettings, RadioAnalyzerRuntime,
    RadioDataMode, RadioDemodulation, RadioPacketPipelineOutput, RadioSettings,
    RawRadioMessageStart, RawRadioOutput, Read, RecordingReader, RecordingWriter,
    SaleaeAnalogChannel, SaleaeCapture, SaleaeChannel, SaleaeDigitalCapture, SampleRange,
    ScopeBatch, ScopeCaptureSettings, Seek, SpectrumProcessor, SpectrumWindow, TimedByte,
    WaveformStore, WaveformTab, WhiteningDirection, DSCOPE_SAMPLE_RATES_HZ,
    DSLOGIC_SAMPLE_RATES_HZ, LOGIC_CHANNEL_COUNT, MAX_WAVEFORM_SAMPLES, RADIO_SPECTRUM_FFT_LENGTH,
};
use serde::Deserialize;
use serde_json::json;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};
use wavecraft_decode::RadioPacket;
use wavecraft_recording::CompletionStatus;

fn normalize_recording_path(mut path: PathBuf) -> PathBuf {
    let has_dsv_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("dsv"))
        .unwrap_or(false);
    if !has_dsv_extension {
        path.set_extension("dsv");
    }
    path
}

/// Returns `path` if it is free, otherwise the first free `stem-N.dsv`
/// sibling. `None` only if every numbered name is taken.
fn available_recording_path(path: PathBuf) -> Option<PathBuf> {
    if !path.exists() {
        return Some(path);
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("capture");
    (2_u32..=u32::MAX)
        .map(|index| parent.join(format!("{stem}-{index}.dsv")))
        .find(|candidate| !candidate.exists())
}

pub(crate) fn choose_recording_save_path(current_path: &str) -> Option<PathBuf> {
    let default_name = Path::new(current_path.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("capture.dsv");
    rfd::FileDialog::new()
        .set_title("Save complete WaveCraft capture")
        .add_filter("WaveCraft capture", &["dsv"])
        .set_file_name(default_name)
        .save_file()
        .map(normalize_recording_path)
        .and_then(available_recording_path)
}

fn normalize_decoded_data_path(mut path: PathBuf) -> PathBuf {
    let has_jsonl_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"));
    if !has_jsonl_extension {
        path.set_extension("jsonl");
    }
    path
}

/// Returns `path` if it is free, otherwise the first free `stem-N.jsonl`
/// sibling. `None` only if every numbered name is taken.
fn available_decoded_data_path(path: PathBuf) -> Option<PathBuf> {
    if !path.exists() {
        return Some(path);
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("decoded-data");
    (2_u32..=u32::MAX)
        .map(|index| parent.join(format!("{stem}-{index}.jsonl")))
        .find(|candidate| !candidate.exists())
}

pub(crate) fn choose_decoded_data_save_path(current_path: &str) -> Option<PathBuf> {
    let default_name = Path::new(current_path.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("decoded-data.jsonl");
    rfd::FileDialog::new()
        .set_title("Save decoded RF data")
        .add_filter("JSON Lines", &["jsonl"])
        .set_file_name(default_name)
        .save_file()
        .map(normalize_decoded_data_path)
        .and_then(available_decoded_data_path)
}

pub(crate) struct DecodedDataRecorder<W: Write> {
    output: W,
    data_mode: RadioDataMode,
    sample_rate_hz: u64,
    record_count: u64,
    last_raw_message_sequence: u64,
    finished: bool,
}

impl<W: Write> DecodedDataRecorder<W> {
    fn new(mut output: W, settings: &RadioSettings) -> Result<Self, String> {
        let channels = settings
            .analyzer
            .channels
            .iter()
            .map(|channel| {
                json!({
                    "name": channel.name,
                    "frequency_offset_hz": channel.frequency_offset_hz,
                })
            })
            .collect::<Vec<_>>();
        let data_mode = match settings.analyzer.data_mode {
            RadioDataMode::Raw => "raw",
            RadioDataMode::Packets => "packets",
        };
        write_json_line(
            &mut output,
            &json!({
                "type": "session",
                "format": "wavecraft-decoded-radio",
                "format_version": 1,
                "center_frequency_hz": settings.capture.center_frequency_hz,
                "sample_rate_hz": settings.capture.sample_rate_hz,
                "bandwidth_hz": settings.capture.bandwidth_hz,
                "data_mode": data_mode,
                "channels": channels,
            }),
        )?;
        Ok(Self {
            output,
            data_mode: settings.analyzer.data_mode,
            sample_rate_hz: settings.capture.sample_rate_hz,
            record_count: 0,
            last_raw_message_sequence: 0,
            finished: false,
        })
    }

    pub(crate) fn records_packets(&self) -> bool {
        self.data_mode == RadioDataMode::Packets
    }

    pub(crate) fn records_raw_messages(&self) -> bool {
        self.data_mode == RadioDataMode::Raw
    }

    pub(crate) fn skip_raw_messages_through(&mut self, sequence: u64) {
        self.last_raw_message_sequence = self.last_raw_message_sequence.max(sequence);
    }

    pub(crate) fn write_packet(
        &mut self,
        channel_frequency_offset_hz: i64,
        channel_name: &str,
        packet: &RadioPacket,
    ) -> Result<(), String> {
        if !self.records_packets() {
            return Ok(());
        }
        if self.finished {
            return Err("cannot append a packet after the decoded-data footer".to_owned());
        }
        let sequence = self.record_count.saturating_add(1);
        let preamble = packet
            .preamble
            .iter()
            .map(|byte| byte.value)
            .collect::<Vec<_>>();
        let data = packet
            .bytes
            .iter()
            .map(|byte| byte.value)
            .collect::<Vec<_>>();
        let uncertain_bits = packet
            .preamble
            .iter()
            .chain(&packet.bytes)
            .map(|byte| u64::from(byte.status.low_confidence_bits))
            .sum::<u64>();
        let sample_start = packet
            .preamble
            .first()
            .or_else(|| packet.bytes.first())
            .map_or(packet.range.start, |byte| byte.range.start);
        write_json_line(
            &mut self.output,
            &json!({
                "type": "packet",
                "sequence": sequence,
                "channel_name": channel_name,
                "channel_frequency_offset_hz": channel_frequency_offset_hz,
                "timestamp": format_capture_timestamp(sample_start, self.sample_rate_hz),
                "sample_start": sample_start,
                "sample_end": packet.range.end,
                "confidence": packet.confidence,
                "uncertain_bits": uncertain_bits,
                "preamble": preamble,
                "data": data,
            }),
        )?;
        self.record_count = sequence;
        Ok(())
    }

    pub(crate) fn write_raw_message(&mut self, message: &RawRadioMessage) -> Result<(), String> {
        if !self.records_raw_messages()
            || !message.is_complete()
            || message.sequence() <= self.last_raw_message_sequence
        {
            return Ok(());
        }
        if self.finished {
            return Err("cannot append decoded data after the decoded-data footer".to_owned());
        }
        let sequence = self.record_count.saturating_add(1);
        let preamble = message.bytes()[..message.preamble_bytes()]
            .iter()
            .map(|byte| byte.value)
            .collect::<Vec<_>>();
        let data = message
            .data_bytes()
            .iter()
            .map(|byte| byte.value)
            .collect::<Vec<_>>();
        let sample_end = message.bytes().last().map(|byte| byte.range.end);
        write_json_line(
            &mut self.output,
            &json!({
                "type": "raw_message",
                "sequence": sequence,
                "capture_sequence": message.sequence(),
                "channel_name": message.channel_name(),
                "channel_frequency_offset_hz": message.channel_frequency_offset_hz(),
                "timestamp": format_capture_timestamp(message.start_sample(), self.sample_rate_hz),
                "sample_start": message.start_sample(),
                "sample_end": sample_end,
                "uncertain_bits": message.uncertain_bits(),
                "preamble": preamble,
                "data": data,
            }),
        )?;
        self.record_count = sequence;
        self.last_raw_message_sequence = message.sequence();
        Ok(())
    }

    pub(crate) fn finish(&mut self, status: CompletionStatus) -> Result<(), String> {
        if self.finished {
            return Err("decoded-data footer has already been written".to_owned());
        }
        let status = match status {
            CompletionStatus::Complete => "complete",
            CompletionStatus::Cancelled => "cancelled",
            CompletionStatus::Failed => "failed",
        };
        write_json_line(
            &mut self.output,
            &json!({
                "type": "footer",
                "status": status,
                "record_count": self.record_count,
            }),
        )?;
        self.output
            .flush()
            .map_err(|error| format!("failed to flush decoded data: {error}"))?;
        self.finished = true;
        Ok(())
    }
}

fn write_json_line(output: &mut impl Write, value: &serde_json::Value) -> Result<(), String> {
    serde_json::to_writer(&mut *output, value)
        .map_err(|error| format!("failed to encode decoded data: {error}"))?;
    output
        .write_all(b"\n")
        .map_err(|error| format!("failed to write decoded data: {error}"))
}

pub(crate) type FileDecodedDataRecorder = DecodedDataRecorder<BufWriter<File>>;

pub(crate) fn create_decoded_data_recorder(
    path: &Path,
    settings: &RadioSettings,
) -> Result<FileDecodedDataRecorder, String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to create decoded data recording {} without overwriting an existing file: {error}",
                path.display()
            )
        })?;
    DecodedDataRecorder::new(BufWriter::new(file), settings)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedDataChannel {
    pub name: String,
    pub frequency_offset_hz: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedDataSession {
    pub center_frequency_hz: u64,
    pub sample_rate_hz: u64,
    pub bandwidth_hz: u64,
    pub data_mode: RadioDataMode,
    pub channels: Vec<DecodedDataChannel>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DecodedDataRecord {
    RawMessage {
        channel_name: String,
        channel_frequency_offset_hz: i64,
        sample_start: Option<u64>,
        sample_end: Option<u64>,
        uncertain_bits: u64,
        preamble: Vec<u8>,
        data: Vec<u8>,
    },
    Packet {
        channel_name: String,
        channel_frequency_offset_hz: i64,
        sample_start: u64,
        sample_end: u64,
        confidence: f32,
        uncertain_bits: u64,
        preamble: Vec<u8>,
        data: Vec<u8>,
    },
}

#[derive(Deserialize)]
struct DecodedDataSessionLine {
    #[serde(rename = "type")]
    kind: String,
    format: String,
    format_version: u64,
    center_frequency_hz: u64,
    sample_rate_hz: u64,
    bandwidth_hz: u64,
    data_mode: String,
    channels: Vec<DecodedDataChannelLine>,
}

#[derive(Deserialize)]
struct DecodedDataChannelLine {
    name: String,
    frequency_offset_hz: i64,
}

#[derive(Deserialize)]
struct DecodedRawMessageLine {
    channel_name: String,
    channel_frequency_offset_hz: i64,
    sample_start: Option<u64>,
    sample_end: Option<u64>,
    #[serde(default)]
    uncertain_bits: u64,
    preamble: Vec<u8>,
    data: Vec<u8>,
}

#[derive(Deserialize)]
struct DecodedPacketLine {
    channel_name: String,
    channel_frequency_offset_hz: i64,
    sample_start: u64,
    sample_end: u64,
    confidence: f32,
    #[serde(default)]
    uncertain_bits: u64,
    preamble: Vec<u8>,
    data: Vec<u8>,
}

#[derive(Deserialize)]
struct DecodedDataFooterLine {
    status: String,
    record_count: u64,
}

pub(crate) fn read_decoded_data(
    input: impl BufRead,
    mut ingest: impl FnMut(DecodedDataRecord) -> Result<(), String>,
) -> Result<(DecodedDataSession, CompletionStatus), String> {
    let mut lines = input.lines().enumerate();
    let (session_line_number, session_text) =
        next_nonempty_line(&mut lines)?.ok_or_else(|| "decoded data file is empty".to_owned())?;
    let session_line: DecodedDataSessionLine = serde_json::from_str(&session_text)
        .map_err(|error| json_line_error(session_line_number, error))?;
    if session_line.kind != "session"
        || session_line.format != "wavecraft-decoded-radio"
        || session_line.format_version != 1
    {
        return Err("decoded data session header is unsupported".to_owned());
    }
    let data_mode = match session_line.data_mode.as_str() {
        "raw" => RadioDataMode::Raw,
        "packets" => RadioDataMode::Packets,
        mode => return Err(format!("decoded data mode {mode:?} is unsupported")),
    };
    let session = DecodedDataSession {
        center_frequency_hz: session_line.center_frequency_hz,
        sample_rate_hz: session_line.sample_rate_hz,
        bandwidth_hz: session_line.bandwidth_hz,
        data_mode,
        channels: session_line
            .channels
            .into_iter()
            .map(|channel| DecodedDataChannel {
                name: channel.name,
                frequency_offset_hz: channel.frequency_offset_hz,
            })
            .collect(),
    };
    let mut record_count = 0_u64;
    let mut completion = None;
    while let Some((line_number, text)) = next_nonempty_line(&mut lines)? {
        if completion.is_some() {
            return Err(format!(
                "decoded data line {} appears after the footer",
                line_number + 1
            ));
        }
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|error| json_line_error(line_number, error))?;
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("decoded data line {} has no type", line_number + 1))?;
        let record = match kind {
            "raw_message" => {
                let line: DecodedRawMessageLine = serde_json::from_value(value)
                    .map_err(|error| json_line_error(line_number, error))?;
                Some(DecodedDataRecord::RawMessage {
                    channel_name: line.channel_name,
                    channel_frequency_offset_hz: line.channel_frequency_offset_hz,
                    sample_start: line.sample_start,
                    sample_end: line.sample_end,
                    uncertain_bits: line.uncertain_bits,
                    preamble: line.preamble,
                    data: line.data,
                })
            }
            "packet" => {
                let line: DecodedPacketLine = serde_json::from_value(value)
                    .map_err(|error| json_line_error(line_number, error))?;
                Some(DecodedDataRecord::Packet {
                    channel_name: line.channel_name,
                    channel_frequency_offset_hz: line.channel_frequency_offset_hz,
                    sample_start: line.sample_start,
                    sample_end: line.sample_end,
                    confidence: line.confidence,
                    uncertain_bits: line.uncertain_bits,
                    preamble: line.preamble,
                    data: line.data,
                })
            }
            "footer" => {
                let line: DecodedDataFooterLine = serde_json::from_value(value)
                    .map_err(|error| json_line_error(line_number, error))?;
                if line.record_count != record_count {
                    return Err(format!(
                        "decoded data footer reports {} records but {record_count} were read",
                        line.record_count
                    ));
                }
                completion = Some(match line.status.as_str() {
                    "complete" => CompletionStatus::Complete,
                    "cancelled" => CompletionStatus::Cancelled,
                    "failed" => CompletionStatus::Failed,
                    status => {
                        return Err(format!(
                            "decoded data completion status {status:?} is unsupported"
                        ))
                    }
                });
                None
            }
            other => {
                return Err(format!(
                    "decoded data line {} has unsupported type {other:?}",
                    line_number + 1
                ))
            }
        };
        if let Some(record) = record {
            ingest(record)?;
            record_count = record_count
                .checked_add(1)
                .ok_or_else(|| "decoded data record count overflowed".to_owned())?;
        }
    }
    let completion = completion.ok_or_else(|| "decoded data file has no footer".to_owned())?;
    Ok((session, completion))
}

fn next_nonempty_line(
    lines: &mut impl Iterator<Item = (usize, std::io::Result<String>)>,
) -> Result<Option<(usize, String)>, String> {
    for (line_number, line) in lines {
        let line = line.map_err(|error| {
            format!(
                "failed to read decoded data line {}: {error}",
                line_number + 1
            )
        })?;
        if !line.trim().is_empty() {
            return Ok(Some((line_number, line)));
        }
    }
    Ok(None)
}

fn json_line_error(line_number: usize, error: serde_json::Error) -> String {
    format!(
        "decoded data line {} is invalid JSON: {error}",
        line_number + 1
    )
}

pub(crate) fn choose_recording_to_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open capture")
        .add_filter("Supported captures", &["dsv", "sal", "jsonl"])
        .add_filter("WaveCraft capture", &["dsv"])
        .add_filter("Saleae Logic 2 capture", &["sal"])
        .add_filter("WaveCraft decoded RF data", &["jsonl"])
        .pick_file()
}

fn normalize_saleae_path(mut path: PathBuf) -> PathBuf {
    let has_sal_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sal"));
    if !has_sal_extension {
        path.set_extension("sal");
    }
    path
}

pub(crate) fn choose_saleae_export_path() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Export Saleae Logic 2 capture")
        .add_filter("Saleae Logic 2 capture", &["sal"])
        .set_file_name("capture.sal")
        .save_file()
        .map(normalize_saleae_path)
}

pub(crate) fn create_saleae_export_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|error| format!("failed to create or replace {}: {error}", path.display()))
}

/// The view that can display a recording with these instruments, or `None`
/// when the combination is not supported (for example logic with radio).
pub(crate) fn recording_capture_target(
    has_logic: bool,
    has_scope: bool,
    has_radio: bool,
) -> Option<CaptureTarget> {
    match (has_logic, has_scope, has_radio) {
        (true, true, true) | (true, true, false) => Some(CaptureTarget::Combined),
        (true, false, false) => Some(CaptureTarget::LogicAnalyzer),
        (false, true, false) => Some(CaptureTarget::Oscilloscope),
        (false, false, true) => Some(CaptureTarget::RadioReceiver),
        _ => None,
    }
}

pub(crate) struct LoadedRecording {
    pub(crate) capture: CompletedCapture,
    pub(crate) logic_settings: Option<LogicCaptureSettings>,
    pub(crate) scope_settings: Option<ScopeCaptureSettings>,
    pub(crate) radio_settings: Option<RadioSettings>,
    pub(crate) logic_channel_names: [String; LOGIC_CHANNEL_COUNT],
    pub(crate) scope_channel_names: [String; 2],
    pub(crate) completion: CompletionStatus,
}

pub(crate) struct GuiRecordingSettings<'a> {
    pub(crate) logic_config: DsLogicCaptureConfig,
    pub(crate) logic_settings: LogicCaptureSettings,
    pub(crate) scope_config: DsCopeCaptureConfig,
    pub(crate) scope_settings: ScopeCaptureSettings,
    pub(crate) radio_settings: &'a RadioSettings,
    pub(crate) logic_channel_names: &'a [String; LOGIC_CHANNEL_COUNT],
    pub(crate) scope_channel_names: &'a [String; 2],
}

pub(crate) fn start_gui_recording(
    path: &Path,
    devices: &[DeviceDescriptor],
    settings: GuiRecordingSettings<'_>,
) -> Result<FileRecorder, String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to create recording {} without overwriting an existing file: {error}",
                path.display()
            )
        })?;
    let metadata = gui_recording_metadata(&settings);
    RecordingWriter::new(BufWriter::new(file), devices, metadata).map_err(|error| error.to_string())
}

/// The session metadata that describes everything a capture was configured
/// with. Readers rebuild the capture settings from these keys, so key names and
/// values are part of the `.dsv` format.
fn gui_recording_metadata(settings: &GuiRecordingSettings<'_>) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert("capture.command".to_owned(), "gui".to_owned());
    insert_radio_capture_metadata(&mut metadata, settings.radio_settings);
    insert_radio_analyzer_metadata(&mut metadata, settings.radio_settings);
    insert_logic_capture_metadata(&mut metadata, settings.logic_config);
    insert_logic_trigger_metadata(&mut metadata, settings.logic_config);
    insert_logic_channel_metadata(
        &mut metadata,
        settings.logic_config,
        &settings.logic_settings,
    );
    insert_channel_name_metadata(
        &mut metadata,
        settings.logic_channel_names,
        settings.scope_channel_names,
    );
    insert_scope_capture_metadata(&mut metadata, settings.scope_config);
    insert_scope_trigger_metadata(&mut metadata, settings.scope_config);
    insert_scope_channel_metadata(&mut metadata, &settings.scope_settings);
    metadata
}

/// The radio capture, trigger, correction, and gain keys.
fn insert_radio_capture_metadata(
    metadata: &mut BTreeMap<String, String>,
    radio_settings: &RadioSettings,
) {
    metadata.insert(
        "radio.center_frequency_hz".to_owned(),
        radio_settings.capture.center_frequency_hz.to_string(),
    );
    metadata.insert(
        "radio.sample_rate_hz".to_owned(),
        radio_settings.capture.sample_rate_hz.to_string(),
    );
    metadata.insert(
        "radio.bandwidth_hz".to_owned(),
        radio_settings.capture.bandwidth_hz.to_string(),
    );
    metadata.insert(
        "radio.receive_channel".to_owned(),
        radio_settings.capture.receive_channel.to_string(),
    );
    metadata.insert(
        "radio.single_duration_millis".to_owned(),
        radio_settings.single_duration_millis.to_string(),
    );
    metadata.insert(
        "radio.trigger_enabled".to_owned(),
        radio_settings.trigger_enabled.to_string(),
    );
    metadata.insert(
        "radio.trigger_posttrigger_micros".to_owned(),
        radio_settings.trigger_posttrigger_micros.to_string(),
    );
    metadata.insert(
        "radio.trigger_level_dbfs_tenths".to_owned(),
        radio_settings.trigger_level_dbfs_tenths.to_string(),
    );
    metadata.insert(
        "radio.frequency_correction_milli_ppm".to_owned(),
        radio_settings.frequency_correction_milli_ppm.to_string(),
    );
    metadata.insert(
        "radio.iq_correction_enabled".to_owned(),
        radio_settings.iq_correction_enabled.to_string(),
    );
    metadata.insert(
        "radio.amplifier_enabled".to_owned(),
        radio_settings.amplifier_enabled.to_string(),
    );
    metadata.insert(
        "radio.antenna_power_enabled".to_owned(),
        radio_settings.antenna_power_enabled.to_string(),
    );
    metadata.insert(
        "radio.analyzer.enabled".to_owned(),
        radio_settings.analyzer.is_enabled().to_string(),
    );
    metadata.insert(
        "radio.analyzer.demodulation".to_owned(),
        match radio_settings.analyzer.demodulation {
            RadioDemodulation::None => "none",
            RadioDemodulation::BinaryFsk => "fsk2",
            RadioDemodulation::BinaryGfsk => "gfsk2",
        }
        .to_owned(),
    );
    for (id, gain_db) in &radio_settings.gains_db {
        metadata.insert(format!("radio.gain.{id}_db"), gain_db.to_string());
    }
}

fn insert_radio_analyzer_metadata(
    metadata: &mut BTreeMap<String, String>,
    radio_settings: &RadioSettings,
) {
    insert_radio_analyzer_signal_metadata(metadata, radio_settings);
    insert_radio_analyzer_data_metadata(metadata, radio_settings);
    insert_radio_analyzer_whitening_metadata(metadata, radio_settings);
    insert_radio_analyzer_channels_metadata(metadata, &radio_settings.analyzer.channels);
}

fn insert_radio_analyzer_fields<const COUNT: usize>(
    metadata: &mut BTreeMap<String, String>,
    fields: [(&str, String); COUNT],
) {
    for (key, value) in fields {
        metadata.insert(format!("radio.analyzer.{key}"), value);
    }
}

/// Channel filter, symbol timing, and clock-recovery keys.
fn insert_radio_analyzer_signal_metadata(
    metadata: &mut BTreeMap<String, String>,
    radio_settings: &RadioSettings,
) {
    insert_radio_analyzer_fields(
        metadata,
        [
            (
                "channel_offset_hz",
                radio_settings
                    .analyzer
                    .channels
                    .first()
                    .map_or(0, |channel| channel.frequency_offset_hz)
                    .to_string(),
            ),
            (
                "channel_bandwidth_hz",
                radio_settings.analyzer.channel_bandwidth_hz.to_string(),
            ),
            (
                "output_sample_rate_hz",
                radio_settings.analyzer.output_sample_rate_hz.to_string(),
            ),
            (
                "filter_tap_count",
                radio_settings.analyzer.filter_tap_count.to_string(),
            ),
            (
                "symbol_rate_hz",
                radio_settings.analyzer.symbol_rate_hz.to_string(),
            ),
            (
                "deviation_hz",
                radio_settings.analyzer.deviation_hz.to_string(),
            ),
            (
                "gfsk_bt_milli",
                radio_settings.analyzer.gfsk_bt_milli.to_string(),
            ),
            ("inverted", radio_settings.analyzer.inverted.to_string()),
            (
                "adaptive_timing",
                radio_settings.analyzer.adaptive_timing.to_string(),
            ),
            (
                "timing_loop_bandwidth_milli",
                radio_settings
                    .analyzer
                    .timing_loop_bandwidth_milli
                    .to_string(),
            ),
            (
                "max_clock_error_ppm",
                radio_settings.analyzer.max_clock_error_ppm.to_string(),
            ),
        ],
    );
}

/// Raw/packet mode and the preamble and sync-word keys.
fn insert_radio_analyzer_data_metadata(
    metadata: &mut BTreeMap<String, String>,
    radio_settings: &RadioSettings,
) {
    insert_radio_analyzer_fields(
        metadata,
        [
            (
                "data_mode",
                match radio_settings.analyzer.data_mode {
                    RadioDataMode::Raw => "raw",
                    RadioDataMode::Packets => "packets",
                }
                .to_owned(),
            ),
            (
                "preamble_byte",
                radio_settings.analyzer.preamble_byte.to_string(),
            ),
            (
                "preamble_bytes",
                radio_settings.analyzer.preamble_bytes.to_string(),
            ),
            ("sync_word", radio_settings.analyzer.sync_word.to_string()),
            (
                "sync_word_bytes",
                radio_settings.analyzer.sync_word_bytes.to_string(),
            ),
            (
                "maximum_preamble_bit_errors",
                radio_settings
                    .analyzer
                    .maximum_preamble_bit_errors
                    .to_string(),
            ),
            (
                "minimum_bit_confidence_milli",
                radio_settings
                    .analyzer
                    .minimum_bit_confidence_milli
                    .to_string(),
            ),
            (
                "maximum_uncertain_bits",
                radio_settings.analyzer.maximum_uncertain_bits.to_string(),
            ),
        ],
    );
}

fn insert_radio_analyzer_whitening_metadata(
    metadata: &mut BTreeMap<String, String>,
    radio_settings: &RadioSettings,
) {
    insert_radio_analyzer_fields(
        metadata,
        [
            (
                "whitening_enabled",
                radio_settings.analyzer.whitening_enabled.to_string(),
            ),
            (
                "whitening_preset",
                radio_settings
                    .analyzer
                    .whitening_preset
                    .metadata_value()
                    .to_owned(),
            ),
            (
                "whitening_width",
                radio_settings.analyzer.whitening_width.to_string(),
            ),
            (
                "whitening_polynomial",
                radio_settings.analyzer.whitening_polynomial.to_string(),
            ),
            (
                "whitening_seed",
                radio_settings.analyzer.whitening_seed.to_string(),
            ),
            (
                "whitening_direction",
                match radio_settings.analyzer.whitening_direction {
                    WhiteningDirection::ShiftRight => "right",
                    WhiteningDirection::ShiftLeft => "left",
                }
                .to_owned(),
            ),
            (
                "whitening_invert_output",
                radio_settings.analyzer.whitening_invert_output.to_string(),
            ),
            (
                "whitening_start_bit",
                radio_settings.analyzer.whitening_start_bit.to_string(),
            ),
            (
                "whitening_end_bit",
                radio_settings
                    .analyzer
                    .whitening_end_bit
                    .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            ),
            (
                "whitening_advance_excluded_bits",
                radio_settings
                    .analyzer
                    .whitening_advance_excluded_bits
                    .to_string(),
            ),
        ],
    );
}

fn insert_logic_capture_metadata(
    metadata: &mut BTreeMap<String, String>,
    logic_config: DsLogicCaptureConfig,
) {
    metadata.insert(
        "logic.sample_rate_hz".to_owned(),
        logic_config.sample_rate_hz().to_string(),
    );
    metadata.insert(
        "logic.sample_count".to_owned(),
        logic_config.sample_count().to_string(),
    );
    metadata.insert(
        "logic.operation_mode".to_owned(),
        match logic_config.operation_mode() {
            DsLogicOperationMode::Buffer => "buffer",
            DsLogicOperationMode::Stream => "stream",
            DsLogicOperationMode::InternalTest => "internal_test",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.stop_option".to_owned(),
        match logic_config.stop_option() {
            DsLogicStopOption::StopImmediately => "stop_immediately",
            DsLogicStopOption::UploadCapturedData => "upload_captured_data",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.threshold_voltage".to_owned(),
        logic_config.threshold_voltage().to_string(),
    );
    metadata.insert(
        "logic.input_filter".to_owned(),
        match logic_config.input_filter() {
            DsLogicInputFilter::None => "none",
            DsLogicInputFilter::OneSampleClock => "one_sample_clock",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.rle_enabled".to_owned(),
        logic_config.rle_enabled().to_string(),
    );
    metadata.insert(
        "logic.external_clock".to_owned(),
        logic_config.external_clock().to_string(),
    );
    metadata.insert(
        "logic.falling_clock_edge".to_owned(),
        logic_config.falling_clock_edge().to_string(),
    );
    metadata.insert(
        "logic.trigger.enabled".to_owned(),
        logic_config.trigger().is_some().to_string(),
    );
}

/// The trigger mode and conditions. Simple triggers also write the one-channel
/// keys that recordings made before multi-channel triggers used.
fn insert_logic_trigger_metadata(
    metadata: &mut BTreeMap<String, String>,
    logic_config: DsLogicCaptureConfig,
) {
    match logic_config.trigger() {
        Some(DsLogicTriggerConfig::Simple(trigger)) => {
            metadata.insert("logic.trigger.mode".to_owned(), "simple".to_owned());
            let mut first_condition = None;
            for (channel, condition) in trigger.conditions().into_iter().enumerate() {
                let Some(condition) = condition else {
                    continue;
                };
                first_condition.get_or_insert((channel, condition));
                metadata.insert(
                    format!("logic.trigger.ch{channel}.condition"),
                    logic_trigger_condition_key(condition).to_owned(),
                );
            }
            if let Some((channel, condition)) = first_condition {
                // Keep the original one-channel keys readable by recordings made
                // while simple triggers supported only one condition.
                metadata.insert("logic.trigger.channel".to_owned(), channel.to_string());
                metadata.insert(
                    "logic.trigger.condition".to_owned(),
                    logic_trigger_condition_key(condition).to_owned(),
                );
            }
        }
        Some(DsLogicTriggerConfig::Smart(trigger)) => {
            metadata.insert("logic.trigger.mode".to_owned(), "smart".to_owned());
            metadata.insert(
                "logic.trigger.stage_count".to_owned(),
                trigger.stage_count().to_string(),
            );
            for (stage_index, stage) in trigger.stages().into_iter().enumerate() {
                let Some(stage) = stage else {
                    continue;
                };
                for (channel, condition) in stage.conditions().into_iter().enumerate() {
                    if let Some(condition) = condition {
                        metadata.insert(
                            format!("logic.trigger.stage{stage_index}.ch{channel}.condition"),
                            logic_trigger_condition_key(condition).to_owned(),
                        );
                    }
                }
            }
        }
        None => {}
    }
}

fn insert_logic_channel_metadata(
    metadata: &mut BTreeMap<String, String>,
    logic_config: DsLogicCaptureConfig,
    logic_settings: &LogicCaptureSettings,
) {
    for (channel, enabled) in logic_config.enabled_channels().into_iter().enumerate() {
        metadata.insert(format!("logic.ch{channel}.enabled"), enabled.to_string());
        metadata.insert(
            format!("logic.ch{channel}.maximum_height"),
            logic_settings.maximum_heights[channel].to_string(),
        );
    }
}

fn insert_scope_capture_metadata(
    metadata: &mut BTreeMap<String, String>,
    scope_config: DsCopeCaptureConfig,
) {
    metadata.insert(
        "scope.sample_rate_hz".to_owned(),
        scope_config.sample_rate_hz().to_string(),
    );
    metadata.insert(
        "scope.sample_count".to_owned(),
        scope_config.sample_count().to_string(),
    );
}

fn insert_scope_trigger_metadata(
    metadata: &mut BTreeMap<String, String>,
    scope_config: DsCopeCaptureConfig,
) {
    let trigger = scope_config.trigger();
    metadata.insert(
        "scope.trigger.position_percent".to_owned(),
        trigger.position_percent().to_string(),
    );
    metadata.insert(
        "scope.trigger.source".to_owned(),
        match trigger.source() {
            DsCopeTriggerSource::Auto => "auto",
            DsCopeTriggerSource::ChannelZero => "ch0",
            DsCopeTriggerSource::ChannelOne => "ch1",
            DsCopeTriggerSource::ChannelZeroAndOne => "ch0_and_ch1",
            DsCopeTriggerSource::ChannelZeroOrOne => "ch0_or_ch1",
        }
        .to_owned(),
    );
    metadata.insert(
        "scope.trigger.auto_channel".to_owned(),
        trigger.auto_channel().to_string(),
    );
    metadata.insert(
        "scope.trigger.slope".to_owned(),
        match trigger.slope() {
            DsCopeTriggerSlope::Rising => "rising",
            DsCopeTriggerSlope::Falling => "falling",
        }
        .to_owned(),
    );
    metadata.insert(
        "scope.trigger.holdoff_ns".to_owned(),
        trigger.holdoff_ns().to_string(),
    );
    metadata.insert(
        "scope.trigger.noise_sensitivity".to_owned(),
        trigger.noise_sensitivity().to_string(),
    );
    metadata.insert(
        "scope.trigger.level".to_owned(),
        trigger.level().to_string(),
    );
    for (channel, level) in trigger.levels().into_iter().enumerate() {
        metadata.insert(
            format!("scope.trigger.ch{channel}.level"),
            level.to_string(),
        );
    }
}

fn insert_scope_channel_metadata(
    metadata: &mut BTreeMap<String, String>,
    scope_settings: &ScopeCaptureSettings,
) {
    for (index, channel) in scope_settings.channels.iter().enumerate() {
        metadata.insert(
            format!("scope.ch{index}.enabled"),
            channel.enabled.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.volts_per_div_mv"),
            channel.volts_per_div_mv.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.coupling"),
            coupling_label(channel.coupling).to_ascii_lowercase(),
        );
        metadata.insert(
            format!("scope.ch{index}.probe_attenuation"),
            channel.probe_attenuation.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.vertical_offset"),
            channel.vertical_offset.to_string(),
        );
    }
}

pub(crate) fn finish_gui_recording(
    recorder: &mut Option<FileRecorder>,
    status: CompletionStatus,
) -> Result<(), String> {
    if let Some(recorder) = recorder {
        recorder.finish(status).map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn finish_decoded_data_recording(
    recorder: &mut Option<FileDecodedDataRecorder>,
    status: CompletionStatus,
) -> Result<(), String> {
    if let Some(recorder) = recorder {
        recorder.finish(status)?;
    }
    Ok(())
}

pub(crate) fn decoded_data_open_request(
    path: &Path,
    current_view: Option<WaveformTab>,
) -> Result<bool, String> {
    let is_jsonl = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"));
    if !is_jsonl {
        return Ok(false);
    }
    if current_view != Some(WaveformTab::RadioReceiver) {
        return Err("Select the RF view before opening decoded .jsonl data".to_owned());
    }
    Ok(true)
}

#[cfg(test)]
pub(crate) fn read_gui_recording(input: impl Read) -> Result<LoadedRecording, String> {
    read_gui_recording_with_analyzers(input, &[])
}

pub(crate) fn read_gui_recording_with_analyzers(
    input: impl Read,
    analyzers: &[ProtocolAnalyzerInstance],
) -> Result<LoadedRecording, String> {
    let mut reader = RecordingReader::new(input).map_err(|error| error.to_string())?;
    let header = reader.header().clone();
    let has_scope_channel_metadata = (0..2).any(|index| {
        header
            .metadata
            .contains_key(&format!("scope.ch{index}.enabled"))
    });
    let logic_settings = logic_settings_from_recording_metadata(&header.metadata);
    let mut scope_settings = scope_settings_from_recording_metadata(&header.metadata);
    let radio_settings = radio_settings_from_recording_metadata(&header.metadata);
    let (logic_channel_names, scope_channel_names) =
        channel_names_from_recording_metadata(&header.metadata);
    let mut enabled_scope_channels = scope_settings.channels.map(|channel| channel.enabled);
    if !enabled_scope_channels.into_iter().any(|enabled| enabled) {
        enabled_scope_channels = [true, true];
        scope_settings.channels[0].enabled = true;
        scope_settings.channels[1].enabled = true;
    }
    let mut waveforms = WaveformStore {
        scope_capacity: (scope_settings.sample_count as usize).min(MAX_WAVEFORM_SAMPLES),
        ..WaveformStore::default()
    };
    if header.metadata.contains_key("logic.sample_count") {
        waveforms.configure_logic_capture(logic_settings.sample_count);
    }
    let mut radio_spectrum =
        SpectrumProcessor::new(RADIO_SPECTRUM_FFT_LENGTH, SpectrumWindow::Hann)?
            .with_iq_correction(radio_settings.iq_correction_enabled);
    let radio_analyzers = match radio_settings.analyzer.signal_profiles() {
        Ok(profiles) if profiles.is_empty() => Vec::new(),
        Ok(profiles) => match radio_settings.analyzer.decoder_configs() {
            Ok(configs) => match profiles
                .into_iter()
                .zip(configs)
                .enumerate()
                .map(|(index, (profile, config))| {
                    RadioAnalyzerRuntime::new_named(
                        profile,
                        config,
                        radio_settings.analyzer.channel_name(index),
                    )
                })
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(analyzers) => analyzers,
                Err(error) => {
                    waveforms.radio_analysis.set_error(error);
                    Vec::new()
                }
            },
            Err(error) => {
                waveforms.radio_analysis.set_error(error);
                Vec::new()
            }
        },
        Err(error) => {
            waveforms.radio_analysis.set_error(error);
            Vec::new()
        }
    };
    let mut radio_analyzers = RadioAnalyzerBank::new(radio_analyzers);
    let mut protocol_decode = ProtocolCaptureDecoder::new(analyzers);
    while let Some(event) = reader.read_event().map_err(|error| error.to_string())? {
        if let CaptureEvent::Frame(frame) = event {
            if let CaptureBatch::Logic(logic) = &frame.payload {
                protocol_decode.ingest_logic_frame(
                    frame.device_id.as_str(),
                    frame.sequence,
                    &logic.samples,
                    logic.sample_rate_hz,
                );
            }
            if let CaptureBatch::Iq(iq) = &frame.payload {
                ingest_radio_iq(
                    &mut waveforms,
                    &mut radio_spectrum,
                    &mut radio_analyzers,
                    iq,
                    f32::from(radio_settings.trigger_level_dbfs_tenths) / 10.0,
                );
            }
            waveforms.ingest_frame(&frame, enabled_scope_channels);
        }
    }
    protocol_decode.finish();
    let completion = reader
        .completion_status()
        .ok_or_else(|| "recording has no completion status".to_owned())?;
    let has_logic = header
        .devices
        .iter()
        .any(|device| device.kind == InstrumentKind::LogicAnalyzer)
        || waveforms.logic_channels > 0;
    let has_scope = header
        .devices
        .iter()
        .any(|device| device.kind == InstrumentKind::Oscilloscope)
        || waveforms.scope_rate_hz > 0;
    let has_radio = header
        .devices
        .iter()
        .any(|device| device.kind == InstrumentKind::RadioReceiver)
        || waveforms.radio.newest_spectrum().is_some();
    if has_scope && !has_scope_channel_metadata {
        for (channel, samples) in scope_settings
            .channels
            .iter_mut()
            .zip(&waveforms.scope_channels)
        {
            channel.enabled = !samples.is_empty();
        }
        scope_settings
            .trigger
            .clamp_to_channels(scope_settings.channels.map(|channel| channel.enabled));
    }
    if !has_logic && !has_scope && !has_radio {
        return Err("recording does not contain a supported capture".to_owned());
    }
    if recording_capture_target(has_logic, has_scope, has_radio).is_none() {
        return Err("recording combines instruments that cannot be shown together".to_owned());
    }
    Ok(LoadedRecording {
        capture: CompletedCapture {
            waveforms,
            protocol_decode,
            has_logic,
            has_scope,
            has_radio,
        },
        logic_settings: has_logic.then_some(logic_settings),
        scope_settings: has_scope.then_some(scope_settings),
        radio_settings: has_radio.then_some(radio_settings),
        logic_channel_names,
        scope_channel_names,
        completion,
    })
}

pub(crate) fn load_gui_recording(
    path: &Path,
    analyzers: &[ProtocolAnalyzerInstance],
) -> Result<LoadedRecording, String> {
    let file = File::open(path)
        .map_err(|error| format!("failed to open recording {}: {error}", path.display()))?;
    let is_saleae = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sal"));
    let result = if is_saleae {
        read_gui_saleae_with_analyzers(BufReader::new(file), analyzers)
    } else {
        read_gui_recording_with_analyzers(BufReader::new(file), analyzers)
    };
    result.map_err(|error| format!("failed to read recording {}: {error}", path.display()))
}

pub(crate) fn load_gui_decoded_data(path: &Path) -> Result<LoadedRecording, String> {
    let file = File::open(path)
        .map_err(|error| format!("failed to open decoded data {}: {error}", path.display()))?;
    read_gui_decoded_data(BufReader::new(file))
        .map_err(|error| format!("failed to read decoded data {}: {error}", path.display()))
}

pub(crate) fn read_gui_decoded_data(input: impl BufRead) -> Result<LoadedRecording, String> {
    let mut analysis = RadioAnalysisStore::default();
    let mut fallback_sample_index = 0_u64;
    let (session, completion) = read_decoded_data(input, |record| {
        match record {
            DecodedDataRecord::RawMessage {
                channel_name,
                channel_frequency_offset_hz,
                sample_start,
                sample_end,
                uncertain_bits,
                preamble,
                data,
            } => {
                let preamble_bytes = preamble.len();
                let mut values = preamble;
                values.extend(data);
                let start = sample_start.unwrap_or(fallback_sample_index);
                let end = decoded_record_end(start, sample_end, values.len());
                fallback_sample_index = end;
                analysis.ingest_raw(
                    RawRadioOutput {
                        bytes: timed_decoded_bytes(values, start, end, uncertain_bits),
                        message_starts: vec![RawRadioMessageStart {
                            sample_index: start,
                            preamble_bytes,
                            maximum_uncertain_bits: usize::MAX,
                        }],
                        diagnostics: Vec::new(),
                        synchronized: true,
                    },
                    channel_frequency_offset_hz,
                    Arc::from(channel_name),
                );
                analysis.finish_channel_capture(channel_frequency_offset_hz);
            }
            DecodedDataRecord::Packet {
                channel_name,
                channel_frequency_offset_hz,
                sample_start,
                sample_end,
                confidence,
                uncertain_bits,
                preamble,
                data,
            } => {
                let preamble_bytes = preamble.len();
                let mut values = preamble;
                values.extend(data);
                let end = decoded_record_end(sample_start, Some(sample_end), values.len());
                fallback_sample_index = fallback_sample_index.max(end);
                let timed = timed_decoded_bytes(values, sample_start, end, uncertain_bits);
                analysis.ingest_packets(
                    RadioPacketPipelineOutput {
                        packets: vec![RadioPacket {
                            preamble: timed[..preamble_bytes].to_vec(),
                            bytes: timed[preamble_bytes..].to_vec(),
                            range: SampleRange {
                                start: sample_start,
                                end,
                            },
                            confidence,
                            diagnostics: Vec::new(),
                        }],
                        diagnostics: Vec::new(),
                    },
                    channel_frequency_offset_hz,
                    Arc::from(channel_name),
                );
            }
        }
        Ok(())
    })?;
    analysis.finish_raw_capture();
    let mut radio_settings = RadioSettings::default();
    radio_settings.capture.center_frequency_hz = session.center_frequency_hz;
    radio_settings.capture.sample_rate_hz = session.sample_rate_hz;
    radio_settings.capture.bandwidth_hz = session.bandwidth_hz;
    radio_settings.analyzer.data_mode = session.data_mode;
    if !session.channels.is_empty() {
        radio_settings.analyzer.channels = session
            .channels
            .into_iter()
            .map(|channel| RadioAnalyzerChannelSettings {
                name: channel.name,
                frequency_offset_hz: channel.frequency_offset_hz,
                sync_word: String::new(),
            })
            .collect();
    }
    let waveforms = WaveformStore {
        radio_analysis: analysis,
        ..WaveformStore::default()
    };
    Ok(LoadedRecording {
        capture: CompletedCapture {
            waveforms,
            protocol_decode: ProtocolCaptureDecoder::new(&[]),
            has_logic: false,
            has_scope: false,
            has_radio: true,
        },
        logic_settings: None,
        scope_settings: None,
        radio_settings: Some(radio_settings),
        logic_channel_names: empty_logic_channel_names(),
        scope_channel_names: empty_scope_channel_names(),
        completion,
    })
}

pub(crate) fn decoded_record_end(start: u64, end: Option<u64>, byte_count: usize) -> u64 {
    let minimum_span = u64::try_from(byte_count.max(1)).unwrap_or(u64::MAX);
    end.filter(|end| *end > start)
        .unwrap_or_else(|| start.saturating_add(minimum_span))
}

pub(crate) fn timed_decoded_bytes(
    values: Vec<u8>,
    start: u64,
    end: u64,
    mut uncertain_bits: u64,
) -> Vec<TimedByte> {
    let count = values.len() as u64;
    let span = end.saturating_sub(start).max(count);
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let index = index as u64;
            let range_start = start.saturating_add(span.saturating_mul(index) / count.max(1));
            let range_end = start
                .saturating_add(span.saturating_mul(index.saturating_add(1)) / count.max(1))
                .max(range_start.saturating_add(1));
            let low_confidence_bits = uncertain_bits.min(8) as u8;
            uncertain_bits = uncertain_bits.saturating_sub(u64::from(low_confidence_bits));
            TimedByte {
                value,
                range: SampleRange {
                    start: range_start,
                    end: range_end,
                },
                status: ByteStatus {
                    low_confidence_bits,
                    ..ByteStatus::default()
                },
            }
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn read_gui_saleae(input: impl Read + Seek) -> Result<LoadedRecording, String> {
    read_gui_saleae_with_analyzers(input, &[])
}

pub(crate) fn read_gui_saleae_with_analyzers(
    input: impl Read + Seek,
    analyzers: &[ProtocolAnalyzerInstance],
) -> Result<LoadedRecording, String> {
    let saleae = read_saleae(input).map_err(|error| error.to_string())?;
    let has_logic = saleae.digital.is_some();
    let has_scope = !saleae.analog.is_empty();
    if !has_logic && !has_scope {
        return Err("Saleae capture contains no supported channels".to_owned());
    }

    let mut logic_settings = LogicCaptureSettings {
        enabled_channels: [false; LOGIC_CHANNEL_COUNT],
        ..LogicCaptureSettings::default()
    };
    let mut logic_names = empty_logic_channel_names();
    let mut waveforms = WaveformStore::default();
    let mut protocol_decode = ProtocolCaptureDecoder::new(analyzers);
    if let Some(digital) = &saleae.digital {
        for channel in &digital.channels {
            let index = usize::from(channel.index);
            if index >= LOGIC_CHANNEL_COUNT {
                return Err(format!(
                    "Saleae digital channel {} exceeds the GUI's {} channels",
                    channel.index, LOGIC_CHANNEL_COUNT
                ));
            }
            logic_settings.enabled_channels[index] = true;
            logic_names[index] = normalized_channel_name(&channel.name);
        }
        if DSLOGIC_SAMPLE_RATES_HZ.contains(&digital.sample_rate_hz) {
            logic_settings.sample_rate_hz = digital.sample_rate_hz;
        }
        waveforms.configure_logic_capture(digital.samples.len() as u64);
        protocol_decode.ingest_logic(&digital.samples, digital.sample_rate_hz);
        waveforms.ingest(
            &CaptureBatch::Logic(wavecraft_core::LogicBatch {
                sample_rate_hz: digital.sample_rate_hz,
                channel_count: digital
                    .channels
                    .iter()
                    .map(|channel| channel.index + 1)
                    .max()
                    .unwrap_or(0),
                samples: digital.samples.clone(),
            }),
            [false; 2],
        );
        waveforms.logic_captured_at = Some(Duration::ZERO);
    }
    protocol_decode.finish();

    let mut scope_settings = ScopeCaptureSettings::default();
    for channel in &mut scope_settings.channels {
        channel.enabled = false;
    }
    let mut scope_names = empty_scope_channel_names();
    if has_scope {
        let rate = saleae
            .analog_sample_rate_hz
            .ok_or_else(|| "Saleae analog sample rate is missing".to_owned())?;
        if DSCOPE_SAMPLE_RATES_HZ.contains(&rate) {
            scope_settings.sample_rate_hz = rate;
        }
        let maximum_samples = saleae
            .analog
            .iter()
            .map(|channel| channel.samples.len())
            .max()
            .unwrap_or(0);
        waveforms.scope_capacity = maximum_samples.clamp(1, MAX_WAVEFORM_SAMPLES);
        let mut enabled = [false; 2];
        let mut channels = Vec::with_capacity(saleae.analog.len());
        for channel in &saleae.analog {
            let index = usize::from(channel.index);
            if index >= 2 {
                return Err(format!(
                    "Saleae analog channel {} exceeds the GUI's two scope channels",
                    channel.index
                ));
            }
            enabled[index] = true;
            scope_settings.channels[index].enabled = true;
            scope_names[index] = normalized_channel_name(&channel.name);
            channels.push(channel.samples.clone());
        }
        waveforms.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: rate,
                trigger_sample_index: None,
                channels,
            }),
            enabled,
        );
        waveforms.scope_captured_at = Some(Duration::ZERO);
    }

    Ok(LoadedRecording {
        capture: CompletedCapture {
            waveforms,
            protocol_decode,
            has_logic,
            has_scope,
            has_radio: false,
        },
        logic_settings: has_logic.then_some(logic_settings),
        scope_settings: has_scope.then_some(scope_settings),
        radio_settings: None,
        logic_channel_names: logic_names,
        scope_channel_names: scope_names,
        completion: CompletionStatus::Complete,
    })
}

pub(crate) fn saleae_capture_from_display(
    completed: &CompletedCapture,
    logic_settings: &LogicCaptureSettings,
    logic_names: &[String; LOGIC_CHANNEL_COUNT],
    scope_names: &[String; 2],
) -> Result<SaleaeCapture, String> {
    let waveforms = &completed.waveforms;
    let digital = if completed.has_logic && !waveforms.logic_samples.is_empty() {
        if waveforms.logic_sample_stride != 1 || waveforms.logic_sample_offset != 0 {
            return Err(
                "This logic capture was decimated for display; export its original .dsv file with wavecraft-cli to preserve every sample"
                    .to_owned(),
            );
        }
        if waveforms
            .logic_samples
            .iter()
            .any(|bucket| bucket.sample_count != 1)
        {
            return Err(
                "The displayed logic capture does not contain individual samples".to_owned(),
            );
        }
        let mut channels = (0..usize::from(waveforms.logic_channels).min(LOGIC_CHANNEL_COUNT))
            .filter(|index| logic_settings.enabled_channels[*index])
            .map(|index| SaleaeChannel {
                index: index as u8,
                name: normalized_channel_name(&logic_names[index]),
            })
            .collect::<Vec<_>>();
        if channels.is_empty() {
            channels = (0..usize::from(waveforms.logic_channels).min(LOGIC_CHANNEL_COUNT))
                .map(|index| SaleaeChannel {
                    index: index as u8,
                    name: normalized_channel_name(&logic_names[index]),
                })
                .collect();
        }
        Some(SaleaeDigitalCapture {
            sample_rate_hz: waveforms.logic_rate_hz,
            samples: waveforms
                .logic_samples
                .iter()
                .map(|bucket| bucket.first)
                .collect(),
            channels,
        })
    } else {
        None
    };

    if waveforms.scope_sample_offset != 0 {
        return Err(
            "This scope capture was trimmed for display; export its original .dsv file with wavecraft-cli to preserve every sample"
                .to_owned(),
        );
    }
    let analog = if completed.has_scope {
        waveforms
            .scope_channels
            .iter()
            .enumerate()
            .filter(|(_, samples)| !samples.is_empty())
            .map(|(index, samples)| {
                SaleaeAnalogChannel::with_automatic_range(
                    index as u8,
                    normalized_channel_name(&scope_names[index]),
                    samples.iter().copied().collect(),
                )
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if digital.is_none() && analog.is_empty() {
        return Err("There are no captured samples to export".to_owned());
    }
    Ok(SaleaeCapture {
        digital,
        analog_sample_rate_hz: (!analog.is_empty()).then_some(waveforms.scope_rate_hz),
        analog,
    })
}

pub(crate) fn recording_completion_label(completion: CompletionStatus) -> &'static str {
    match completion {
        CompletionStatus::Complete => "complete",
        CompletionStatus::Cancelled => "cancelled",
        CompletionStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waveform::radio::RadioAnalysisStore;
    use crate::*;
    use std::io::Cursor;
    use std::sync::Arc;
    use wavecraft_core::{Iq8, IqBatch};
    use wavecraft_decode::{
        ByteStatus, RawRadioMessageStart, RawRadioOutput, SampleRange, TimedByte,
    };

    #[test]
    fn recording_paths_use_the_dsv_extension() {
        assert_eq!(
            normalize_recording_path(PathBuf::from("capture")),
            PathBuf::from("capture.dsv")
        );
        assert_eq!(
            normalize_recording_path(PathBuf::from("capture.DSV")),
            PathBuf::from("capture.DSV")
        );
        assert_eq!(
            normalize_recording_path(PathBuf::from("capture.txt")),
            PathBuf::from("capture.dsv")
        );
    }

    #[test]
    fn saleae_paths_use_the_sal_extension() {
        assert_eq!(
            normalize_saleae_path(PathBuf::from("capture")),
            PathBuf::from("capture.sal")
        );
        assert_eq!(
            normalize_saleae_path(PathBuf::from("capture.SAL")),
            PathBuf::from("capture.SAL")
        );
        assert_eq!(
            normalize_saleae_path(PathBuf::from("capture.zip")),
            PathBuf::from("capture.sal")
        );
    }

    #[test]
    fn decoded_data_paths_use_the_jsonl_extension() {
        assert_eq!(
            normalize_decoded_data_path(PathBuf::from("packets")),
            PathBuf::from("packets.jsonl")
        );
        assert_eq!(
            normalize_decoded_data_path(PathBuf::from("packets.JSONL")),
            PathBuf::from("packets.JSONL")
        );
        assert_eq!(
            normalize_decoded_data_path(PathBuf::from("packets.csv")),
            PathBuf::from("packets.jsonl")
        );
    }

    #[test]
    fn decoded_data_recorder_streams_session_packets_and_footer() {
        let mut settings = RadioSettings::default();
        settings.analyzer.data_mode = RadioDataMode::Packets;
        let byte = |value, start| TimedByte {
            value,
            range: SampleRange {
                start,
                end: start + 8,
            },
            status: ByteStatus::default(),
        };
        let packet = RadioPacket {
            preamble: vec![byte(0xaa, 0)],
            bytes: vec![byte(0x12, 8), byte(0x34, 16)],
            range: SampleRange { start: 0, end: 24 },
            confidence: 0.875,
            diagnostics: Vec::new(),
        };
        let mut output = Vec::new();
        {
            let mut recorder = DecodedDataRecorder::new(&mut output, &settings).unwrap();
            recorder.write_packet(125_000, "Weather", &packet).unwrap();
            recorder.finish(CompletionStatus::Complete).unwrap();
        }

        let records = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["type"], "session");
        assert_eq!(records[1]["type"], "packet");
        assert_eq!(records[1]["channel_name"], "Weather");
        assert_eq!(records[1]["timestamp"], "00:00:00.000");
        assert_eq!(records[1]["data"], json!([0x12, 0x34]));
        assert_eq!(records[2]["type"], "footer");
        assert_eq!(records[2]["record_count"], 1);
        assert_eq!(records[2]["status"], "complete");
    }

    #[test]
    fn decoded_data_recorder_streams_completed_raw_messages() {
        let mut settings = RadioSettings::default();
        settings.capture.sample_rate_hz = 8;
        let mut store = RadioAnalysisStore::new(32).unwrap();
        store.ingest_raw(
            RawRadioOutput {
                bytes: [0xaa, 0x12, 0xaa, 0x34]
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| TimedByte {
                        value,
                        range: SampleRange {
                            start: index as u64 * 8,
                            end: index as u64 * 8 + 8,
                        },
                        status: ByteStatus::default(),
                    })
                    .collect(),
                message_starts: vec![
                    RawRadioMessageStart {
                        sample_index: 0,
                        preamble_bytes: 1,
                        maximum_uncertain_bits: 0,
                    },
                    RawRadioMessageStart {
                        sample_index: 16,
                        preamble_bytes: 1,
                        maximum_uncertain_bits: 0,
                    },
                ],
                diagnostics: Vec::new(),
                synchronized: true,
            },
            125_000,
            Arc::from("Weather"),
        );
        store.finish_raw_capture();
        let mut output = Vec::new();
        {
            let mut recorder = DecodedDataRecorder::new(&mut output, &settings).unwrap();
            for message in store.raw_messages() {
                recorder.write_raw_message(message).unwrap();
            }
            recorder.finish(CompletionStatus::Complete).unwrap();
        }

        let records = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(records.len(), 4);
        assert_eq!(records[1]["type"], "raw_message");
        assert_eq!(records[1]["timestamp"], "00:00:00.000");
        assert_eq!(records[1]["sample_start"], 0);
        assert_eq!(records[1]["preamble"], json!([0xaa]));
        assert_eq!(records[1]["data"], json!([0x12]));
        assert_eq!(records[2]["sample_start"], 16);
        assert_eq!(records[2]["timestamp"], "00:00:02.000");
        assert_eq!(records[2]["data"], json!([0x34]));
        assert_eq!(records[3]["record_count"], 2);
    }

    #[test]
    fn available_paths_skip_names_that_already_exist() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "wavecraft-available-paths-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();

        let capture = directory.join("capture.dsv");
        assert_eq!(
            available_recording_path(capture.clone()),
            Some(capture.clone())
        );
        std::fs::write(&capture, b"x").unwrap();
        assert_eq!(
            available_recording_path(capture.clone()),
            Some(directory.join("capture-2.dsv"))
        );
        std::fs::write(directory.join("capture-2.dsv"), b"x").unwrap();
        assert_eq!(
            available_recording_path(capture),
            Some(directory.join("capture-3.dsv"))
        );

        let decoded = directory.join("decoded.jsonl");
        assert_eq!(
            available_decoded_data_path(decoded.clone()),
            Some(decoded.clone())
        );
        std::fs::write(&decoded, b"x").unwrap();
        assert_eq!(
            available_decoded_data_path(decoded),
            Some(directory.join("decoded-2.jsonl"))
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn saleae_export_replaces_the_selected_file() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "wavecraft-saleae-export-{}-{unique}.sal",
            std::process::id()
        ));
        std::fs::write(&path, b"previous capture").unwrap();

        let file = create_saleae_export_file(&path).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 0);
        drop(file);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn recording_capture_target_covers_every_instrument_combination() {
        let expected = [
            ((false, false, false), None),
            ((true, false, false), Some(CaptureTarget::LogicAnalyzer)),
            ((false, true, false), Some(CaptureTarget::Oscilloscope)),
            ((false, false, true), Some(CaptureTarget::RadioReceiver)),
            ((true, true, false), Some(CaptureTarget::Combined)),
            ((true, true, true), Some(CaptureTarget::Combined)),
            // Combinations that include radio but not both wired instruments
            // cannot be shown together and must be rejected, not panic.
            ((true, false, true), None),
            ((false, true, true), None),
        ];
        for ((logic, scope, radio), target) in expected {
            assert_eq!(
                recording_capture_target(logic, scope, radio),
                target,
                "logic={logic} scope={scope} radio={radio}"
            );
        }
    }

    #[test]
    fn gui_recording_reader_restores_radio_iq_and_settings() {
        let descriptor = DeviceDescriptor {
            id: DeviceId::new("hackrf:test"),
            name: "HackRF One".to_owned(),
            kind: InstrumentKind::RadioReceiver,
        };
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "radio.center_frequency_hz".to_owned(),
            "433920000".to_owned(),
        );
        metadata.insert("radio.sample_rate_hz".to_owned(), "10000000".to_owned());
        metadata.insert("radio.bandwidth_hz".to_owned(), "8000000".to_owned());
        metadata.insert("radio.receive_channel".to_owned(), "0".to_owned());
        metadata.insert("radio.single_duration_millis".to_owned(), "2500".to_owned());
        metadata.insert("radio.trigger_enabled".to_owned(), "true".to_owned());
        metadata.insert(
            "radio.trigger_posttrigger_micros".to_owned(),
            "25000".to_owned(),
        );
        metadata.insert(
            "radio.trigger_level_dbfs_tenths".to_owned(),
            "-425".to_owned(),
        );
        metadata.insert(
            "radio.frequency_correction_milli_ppm".to_owned(),
            "-12500".to_owned(),
        );
        metadata.insert("radio.gain.lna_db".to_owned(), "24".to_owned());
        metadata.insert("radio.gain.vga_db".to_owned(), "20".to_owned());
        metadata.insert("radio.amplifier_enabled".to_owned(), "true".to_owned());
        metadata.insert("radio.iq_correction_enabled".to_owned(), "false".to_owned());
        metadata.insert("radio.antenna_power_enabled".to_owned(), "true".to_owned());
        let mut bytes = Vec::new();
        {
            let mut writer = RecordingWriter::new(&mut bytes, &[descriptor], metadata).unwrap();
            writer
                .write_event(&CaptureEvent::Frame(CaptureFrame {
                    device_id: DeviceId::new("hackrf:test"),
                    sequence: 0,
                    captured_at: Duration::ZERO,
                    payload: CaptureBatch::Iq(IqBatch {
                        sample_rate_hz: 10_000_000,
                        center_frequency_hz: 433_920_000,
                        first_sample_index: 0,
                        discontinuity: false,
                        samples: vec![Iq8 { i: 64, q: 0 }; RADIO_SPECTRUM_FFT_LENGTH],
                    }),
                }))
                .unwrap();
            writer.finish(CompletionStatus::Complete).unwrap();
        }

        let loaded = read_gui_recording(Cursor::new(bytes)).expect("radio recording should reopen");

        assert!(loaded.capture.has_radio);
        assert!(!loaded.capture.has_logic);
        assert!(!loaded.capture.has_scope);
        assert_eq!(loaded.capture.waveforms.radio.waterfall().len(), 1);
        let settings = loaded.radio_settings.expect("radio metadata should load");
        assert_eq!(settings.capture.center_frequency_hz, 433_920_000);
        assert_eq!(settings.capture.sample_rate_hz, 10_000_000);
        assert_eq!(settings.capture.bandwidth_hz, 8_000_000);
        assert_eq!(settings.single_duration_millis, 2_500);
        assert!(settings.trigger_enabled);
        assert_eq!(settings.trigger_posttrigger_micros, 25_000);
        assert_eq!(settings.trigger_level_dbfs_tenths, -425);
        assert_eq!(settings.frequency_correction_milli_ppm, -12_500);
        assert_eq!(settings.gains_db.get("lna"), Some(&24));
        assert_eq!(settings.gains_db.get("vga"), Some(&20));
        assert!(!settings.iq_correction_enabled);
        assert!(settings.amplifier_enabled);
        assert!(settings.antenna_power_enabled);
    }

    #[test]
    fn decoded_jsonl_opens_packets_in_the_rf_view() {
        let jsonl = concat!(
            "{\"type\":\"session\",\"format\":\"wavecraft-decoded-radio\",\"format_version\":1,\"center_frequency_hz\":433920000,\"sample_rate_hz\":2000000,\"bandwidth_hz\":1750000,\"data_mode\":\"packets\",\"channels\":[{\"name\":\"Weather\",\"frequency_offset_hz\":125000}]}\n",
            "{\"type\":\"packet\",\"sequence\":1,\"channel_name\":\"Weather\",\"channel_frequency_offset_hz\":125000,\"sample_start\":100,\"sample_end\":124,\"confidence\":0.9,\"uncertain_bits\":0,\"preamble\":[170],\"data\":[18,52]}\n",
            "{\"type\":\"footer\",\"status\":\"complete\",\"record_count\":1}\n",
        );

        let loaded = read_gui_decoded_data(Cursor::new(jsonl)).unwrap();

        assert!(loaded.capture.has_radio);
        assert!(!loaded.capture.has_logic);
        assert!(!loaded.capture.has_scope);
        assert_eq!(
            loaded.capture.waveforms.radio_analysis.raw_messages().len(),
            1
        );
        let message = loaded
            .capture
            .waveforms
            .radio_analysis
            .raw_messages()
            .front()
            .unwrap();
        assert_eq!(message.channel_name(), "Weather");
        assert_eq!(message.preamble_bytes(), 1);
        assert_eq!(
            message
                .data_bytes()
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0x12, 0x34]
        );
        let settings = loaded.radio_settings.unwrap();
        assert_eq!(settings.analyzer.data_mode, RadioDataMode::Packets);
        assert_eq!(settings.capture.center_frequency_hz, 433_920_000);
        assert_eq!(loaded.completion, CompletionStatus::Complete);
    }

    #[test]
    fn decoded_jsonl_open_requires_the_rf_view() {
        let path = Path::new("decoded-data.JSONL");

        assert_eq!(
            decoded_data_open_request(path, Some(WaveformTab::RadioReceiver)),
            Ok(true)
        );
        assert!(decoded_data_open_request(path, Some(WaveformTab::Oscilloscope)).is_err());
        assert_eq!(
            decoded_data_open_request(Path::new("capture.dsv"), Some(WaveformTab::Oscilloscope)),
            Ok(false)
        );
    }

    #[test]
    fn gui_saleae_reader_restores_channels_names_and_samples() {
        let capture = SaleaeCapture {
            digital: Some(SaleaeDigitalCapture {
                sample_rate_hz: 1_000_000,
                samples: vec![0, 1, 3, 2],
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
            analog_sample_rate_hz: Some(1_000_000),
            analog: vec![SaleaeAnalogChannel {
                index: 0,
                name: "Input".to_owned(),
                samples: vec![-1.0, 0.0, 1.0, 0.5],
                minimum_voltage: -2.0,
                maximum_voltage: 2.0,
            }],
        };
        let mut bytes = Cursor::new(Vec::new());
        write_saleae(&mut bytes, &capture).expect("Saleae capture should serialize");
        bytes.set_position(0);

        let loaded = read_gui_saleae(bytes).expect("Saleae capture should open");

        assert!(loaded.capture.has_logic);
        assert!(loaded.capture.has_scope);
        assert_eq!(loaded.logic_channel_names[0], "Clock");
        assert_eq!(loaded.logic_channel_names[1], "Data");
        assert_eq!(loaded.scope_channel_names[0], "Input");
        assert_eq!(loaded.capture.waveforms.logic_samples.len(), 4);
        assert_eq!(loaded.capture.waveforms.logic_samples[2].first, 3);
        assert_eq!(loaded.capture.waveforms.scope_channels[0].len(), 4);
        assert!((loaded.capture.waveforms.scope_channels[0][2] - 1.0).abs() < 0.002);
    }

    /// The settings `start_gui_recording` turns into session metadata. Tests
    /// change what they care about and leave the rest at the defaults.
    struct RecordingCase {
        logic_config: DsLogicCaptureConfig,
        logic_settings: LogicCaptureSettings,
        scope_config: DsCopeCaptureConfig,
        scope_settings: ScopeCaptureSettings,
        radio_settings: RadioSettings,
        logic_channel_names: [String; LOGIC_CHANNEL_COUNT],
        scope_channel_names: [String; 2],
    }

    impl RecordingCase {
        fn new() -> Self {
            Self {
                logic_config: DsLogicCaptureConfig::new(20_000_000, 65_536).unwrap(),
                logic_settings: LogicCaptureSettings::default(),
                scope_config: DsCopeCaptureConfig::new(1_000_000, 100_000).unwrap(),
                scope_settings: ScopeCaptureSettings::default(),
                radio_settings: RadioSettings::default(),
                logic_channel_names: empty_logic_channel_names(),
                scope_channel_names: empty_scope_channel_names(),
            }
        }

        fn start(&self, path: &Path) -> Result<FileRecorder, String> {
            let devices = [DeviceDescriptor {
                id: wavecraft_core::DeviceId::new("test-device"),
                name: "Test device".to_owned(),
                kind: InstrumentKind::LogicAnalyzer,
            }];
            start_gui_recording(
                path,
                &devices,
                GuiRecordingSettings {
                    logic_config: self.logic_config,
                    logic_settings: self.logic_settings,
                    scope_config: self.scope_config,
                    scope_settings: self.scope_settings,
                    radio_settings: &self.radio_settings,
                    logic_channel_names: &self.logic_channel_names,
                    scope_channel_names: &self.scope_channel_names,
                },
            )
        }

        /// Records these settings to a new file and returns the metadata read
        /// back from it.
        fn recorded_metadata(&self, name: &str) -> BTreeMap<String, String> {
            let path = crate::app::test_support::scratch_directory(name).join("capture.dsv");
            let mut recorder = self.start(&path).unwrap();
            recorder.finish(CompletionStatus::Complete).unwrap();
            let reader = RecordingReader::new(BufReader::new(File::open(&path).unwrap())).unwrap();
            reader.header().metadata.clone()
        }
    }

    #[track_caller]
    fn assert_recorded(metadata: &BTreeMap<String, String>, expected: &[(&str, &str)]) {
        for (key, value) in expected {
            assert_eq!(
                metadata.get(*key).map(String::as_str),
                Some(*value),
                "metadata key {key}"
            );
        }
    }

    #[track_caller]
    fn assert_not_recorded(metadata: &BTreeMap<String, String>, keys: &[&str]) {
        for key in keys {
            assert!(
                !metadata.contains_key(*key),
                "unexpected metadata key {key}"
            );
        }
    }

    #[test]
    fn a_new_recording_never_replaces_an_existing_file() {
        let directory = crate::app::test_support::scratch_directory("gui-recording-exists");
        let path = directory.join("capture.dsv");
        std::fs::write(&path, b"keep me").unwrap();

        let error = RecordingCase::new()
            .start(&path)
            .err()
            .expect("an existing file must not be replaced");

        assert!(
            error.contains("without overwriting an existing file"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
    }

    #[test]
    fn a_recording_names_the_gui_as_its_source_and_lists_the_devices() {
        let case = RecordingCase::new();
        let path =
            crate::app::test_support::scratch_directory("gui-recording-source").join("capture.dsv");
        let mut recorder = case.start(&path).unwrap();
        recorder.finish(CompletionStatus::Complete).unwrap();

        let reader = RecordingReader::new(BufReader::new(File::open(&path).unwrap())).unwrap();

        assert_eq!(reader.header().devices.len(), 1);
        assert_eq!(reader.header().devices[0].name, "Test device");
        assert_recorded(&reader.header().metadata, &[("capture.command", "gui")]);
    }

    #[test]
    fn logic_capture_options_are_recorded() {
        let mut case = RecordingCase::new();
        let mut channels = [false; 16];
        channels[0] = true;
        channels[3] = true;
        case.logic_config = DsLogicCaptureConfig::new(20_000_000, 65_536)
            .unwrap()
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_stop_option(DsLogicStopOption::StopImmediately)
            .with_threshold_voltage(1.8)
            .unwrap()
            .with_input_filter(DsLogicInputFilter::OneSampleClock)
            .with_rle_enabled(true)
            .with_external_clock(true)
            .with_falling_clock_edge(true)
            .with_enabled_channels(channels)
            .unwrap();
        case.logic_settings.maximum_heights[3] = 77;

        let metadata = case.recorded_metadata("gui-recording-logic");

        assert_recorded(
            &metadata,
            &[
                ("logic.sample_rate_hz", "20000000"),
                ("logic.sample_count", "65536"),
                ("logic.operation_mode", "stream"),
                ("logic.stop_option", "stop_immediately"),
                ("logic.threshold_voltage", "1.8"),
                ("logic.input_filter", "one_sample_clock"),
                ("logic.rle_enabled", "true"),
                ("logic.external_clock", "true"),
                ("logic.falling_clock_edge", "true"),
                ("logic.ch0.enabled", "true"),
                ("logic.ch1.enabled", "false"),
                ("logic.ch3.enabled", "true"),
                ("logic.ch3.maximum_height", "77"),
                ("logic.ch15.enabled", "false"),
                ("logic.trigger.enabled", "false"),
            ],
        );
        assert_not_recorded(
            &metadata,
            &[
                "logic.trigger.mode",
                "logic.trigger.channel",
                "logic.trigger.condition",
                "logic.trigger.stage_count",
                "logic.ch16.enabled",
            ],
        );
    }

    #[test]
    fn every_logic_option_is_recorded_under_its_own_name() {
        for (mode, name) in [
            (DsLogicOperationMode::Buffer, "buffer"),
            (DsLogicOperationMode::Stream, "stream"),
            (DsLogicOperationMode::InternalTest, "internal_test"),
        ] {
            let mut case = RecordingCase::new();
            case.logic_config = case.logic_config.with_operation_mode(mode);
            let metadata = case.recorded_metadata("gui-recording-logic-mode");
            assert_recorded(&metadata, &[("logic.operation_mode", name)]);
        }
        for (stop, name) in [
            (DsLogicStopOption::StopImmediately, "stop_immediately"),
            (
                DsLogicStopOption::UploadCapturedData,
                "upload_captured_data",
            ),
        ] {
            let mut case = RecordingCase::new();
            case.logic_config = case.logic_config.with_stop_option(stop);
            let metadata = case.recorded_metadata("gui-recording-logic-stop");
            assert_recorded(&metadata, &[("logic.stop_option", name)]);
        }
        for (filter, name) in [
            (DsLogicInputFilter::None, "none"),
            (DsLogicInputFilter::OneSampleClock, "one_sample_clock"),
        ] {
            let mut case = RecordingCase::new();
            case.logic_config = case.logic_config.with_input_filter(filter);
            let metadata = case.recorded_metadata("gui-recording-logic-filter");
            assert_recorded(&metadata, &[("logic.input_filter", name)]);
        }
        for (condition, name) in [
            (DsLogicTriggerCondition::Low, "low"),
            (DsLogicTriggerCondition::High, "high"),
            (DsLogicTriggerCondition::RisingEdge, "rising_edge"),
            (DsLogicTriggerCondition::FallingEdge, "falling_edge"),
            (DsLogicTriggerCondition::EitherEdge, "either_edge"),
        ] {
            let mut case = RecordingCase::new();
            let trigger = DsLogicSimpleTrigger::new(4, condition).unwrap();
            case.logic_config = case
                .logic_config
                .with_trigger(Some(DsLogicTriggerConfig::Simple(trigger)));
            let metadata = case.recorded_metadata("gui-recording-logic-condition");
            assert_recorded(&metadata, &[("logic.trigger.ch4.condition", name)]);
        }
    }

    #[test]
    fn a_simple_trigger_records_every_condition_and_keeps_the_one_channel_keys() {
        let mut case = RecordingCase::new();
        let trigger = DsLogicSimpleTrigger::new(5, DsLogicTriggerCondition::RisingEdge)
            .unwrap()
            .with_channel_condition(2, DsLogicTriggerCondition::Low)
            .unwrap()
            .with_channel_condition(9, DsLogicTriggerCondition::EitherEdge)
            .unwrap();
        case.logic_config = case
            .logic_config
            .with_trigger(Some(DsLogicTriggerConfig::Simple(trigger)));

        let metadata = case.recorded_metadata("gui-recording-simple-trigger");

        assert_recorded(
            &metadata,
            &[
                ("logic.trigger.enabled", "true"),
                ("logic.trigger.mode", "simple"),
                ("logic.trigger.ch2.condition", "low"),
                ("logic.trigger.ch5.condition", "rising_edge"),
                ("logic.trigger.ch9.condition", "either_edge"),
                // Older readers know only one condition: the lowest channel.
                ("logic.trigger.channel", "2"),
                ("logic.trigger.condition", "low"),
            ],
        );
        assert_not_recorded(
            &metadata,
            &[
                "logic.trigger.ch3.condition",
                "logic.trigger.stage_count",
                "logic.trigger.stage0.ch2.condition",
            ],
        );
    }

    #[test]
    fn a_smart_trigger_records_the_conditions_of_each_stage() {
        let mut case = RecordingCase::new();
        let first = DsLogicTriggerStage::new(1, DsLogicTriggerCondition::High).unwrap();
        let second = DsLogicTriggerStage::new(0, DsLogicTriggerCondition::FallingEdge)
            .unwrap()
            .with_channel_condition(4, DsLogicTriggerCondition::Low)
            .unwrap();
        let trigger = DsLogicSmartTrigger::new(first).with_stage(second).unwrap();
        case.logic_config = case
            .logic_config
            .with_trigger(Some(DsLogicTriggerConfig::Smart(trigger)));

        let metadata = case.recorded_metadata("gui-recording-smart-trigger");

        assert_recorded(
            &metadata,
            &[
                ("logic.trigger.enabled", "true"),
                ("logic.trigger.mode", "smart"),
                ("logic.trigger.stage_count", "2"),
                ("logic.trigger.stage0.ch1.condition", "high"),
                ("logic.trigger.stage1.ch0.condition", "falling_edge"),
                ("logic.trigger.stage1.ch4.condition", "low"),
            ],
        );
        assert_not_recorded(
            &metadata,
            &[
                "logic.trigger.stage0.ch0.condition",
                "logic.trigger.stage2.ch1.condition",
                "logic.trigger.channel",
                "logic.trigger.condition",
                "logic.trigger.ch1.condition",
            ],
        );
    }

    #[test]
    fn channel_names_are_recorded_trimmed() {
        let mut case = RecordingCase::new();
        case.logic_channel_names[2] = "  Clock  ".to_owned();
        case.scope_channel_names[1] = " Probe ".to_owned();

        let metadata = case.recorded_metadata("gui-recording-names");

        assert_recorded(
            &metadata,
            &[("logic.ch2.name", "Clock"), ("scope.ch1.name", "Probe")],
        );
    }

    #[test]
    fn the_scope_trigger_is_recorded() {
        let mut case = RecordingCase::new();
        let trigger = DsCopeTriggerConfig::new(
            DsCopeTriggerSource::ChannelZeroAndOne,
            DsCopeTriggerSlope::Falling,
        )
        .with_position_percent(30)
        .unwrap()
        .with_auto_channel(1)
        .unwrap()
        .with_holdoff_ns(2_500)
        .unwrap()
        .with_noise_sensitivity(7)
        .unwrap()
        .with_levels([100, 150])
        .unwrap();
        case.scope_config = DsCopeCaptureConfig::new(2_000_000, 50_000)
            .unwrap()
            .with_trigger_config(trigger)
            .unwrap();

        let metadata = case.recorded_metadata("gui-recording-scope-trigger");

        assert_recorded(
            &metadata,
            &[
                ("scope.sample_rate_hz", "2000000"),
                ("scope.sample_count", "50000"),
                ("scope.trigger.position_percent", "30"),
                ("scope.trigger.source", "ch0_and_ch1"),
                ("scope.trigger.auto_channel", "1"),
                ("scope.trigger.slope", "falling"),
                ("scope.trigger.holdoff_ns", "2500"),
                ("scope.trigger.noise_sensitivity", "7"),
                ("scope.trigger.level", "100"),
                ("scope.trigger.ch0.level", "100"),
                ("scope.trigger.ch1.level", "150"),
            ],
        );
    }

    #[test]
    fn every_scope_trigger_source_and_slope_is_recorded_under_its_own_name() {
        for (source, name) in [
            (DsCopeTriggerSource::Auto, "auto"),
            (DsCopeTriggerSource::ChannelZero, "ch0"),
            (DsCopeTriggerSource::ChannelOne, "ch1"),
            (DsCopeTriggerSource::ChannelZeroAndOne, "ch0_and_ch1"),
            (DsCopeTriggerSource::ChannelZeroOrOne, "ch0_or_ch1"),
        ] {
            let mut case = RecordingCase::new();
            let trigger = DsCopeTriggerConfig::new(source, DsCopeTriggerSlope::Rising);
            case.scope_config = case.scope_config.with_trigger_config(trigger).unwrap();
            let metadata = case.recorded_metadata("gui-recording-scope-source");
            assert_recorded(&metadata, &[("scope.trigger.source", name)]);
        }
        for (slope, name) in [
            (DsCopeTriggerSlope::Rising, "rising"),
            (DsCopeTriggerSlope::Falling, "falling"),
        ] {
            let mut case = RecordingCase::new();
            let trigger = DsCopeTriggerConfig::new(DsCopeTriggerSource::Auto, slope);
            case.scope_config = case.scope_config.with_trigger_config(trigger).unwrap();
            let metadata = case.recorded_metadata("gui-recording-scope-slope");
            assert_recorded(&metadata, &[("scope.trigger.slope", name)]);
        }
    }

    #[test]
    fn scope_channel_settings_are_recorded_per_channel() {
        let mut case = RecordingCase::new();
        case.scope_settings.channels[0].enabled = false;
        case.scope_settings.channels[0].volts_per_div_mv = 200;
        case.scope_settings.channels[0].coupling = DsCopeCoupling::Ac;
        case.scope_settings.channels[0].probe_attenuation = 10;
        case.scope_settings.channels[0].vertical_offset = 99;
        case.scope_settings.channels[1].volts_per_div_mv = 50;
        case.scope_settings.channels[1].coupling = DsCopeCoupling::Dc;

        let metadata = case.recorded_metadata("gui-recording-scope-channels");

        assert_recorded(
            &metadata,
            &[
                ("scope.ch0.enabled", "false"),
                ("scope.ch0.volts_per_div_mv", "200"),
                ("scope.ch0.coupling", "ac"),
                ("scope.ch0.probe_attenuation", "10"),
                ("scope.ch0.vertical_offset", "99"),
                ("scope.ch1.enabled", "true"),
                ("scope.ch1.volts_per_div_mv", "50"),
                ("scope.ch1.coupling", "dc"),
            ],
        );
    }

    #[test]
    fn radio_capture_settings_are_recorded() {
        let mut case = RecordingCase::new();
        let radio = &mut case.radio_settings;
        radio.capture.center_frequency_hz = 868_300_000;
        radio.capture.sample_rate_hz = 2_000_000;
        radio.capture.bandwidth_hz = 1_750_000;
        radio.capture.receive_channel = 1;
        radio.single_duration_millis = 1_500;
        radio.trigger_enabled = true;
        radio.trigger_posttrigger_micros = 250;
        radio.trigger_level_dbfs_tenths = -425;
        radio.frequency_correction_milli_ppm = -1_200;
        radio.iq_correction_enabled = true;
        radio.amplifier_enabled = true;
        radio.antenna_power_enabled = true;
        radio.gains_db.clear();
        radio.gains_db.insert("lna".to_owned(), 24);
        radio.gains_db.insert("vga".to_owned(), 16);

        let metadata = case.recorded_metadata("gui-recording-radio");

        assert_recorded(
            &metadata,
            &[
                ("radio.center_frequency_hz", "868300000"),
                ("radio.sample_rate_hz", "2000000"),
                ("radio.bandwidth_hz", "1750000"),
                ("radio.receive_channel", "1"),
                ("radio.single_duration_millis", "1500"),
                ("radio.trigger_enabled", "true"),
                ("radio.trigger_posttrigger_micros", "250"),
                ("radio.trigger_level_dbfs_tenths", "-425"),
                ("radio.frequency_correction_milli_ppm", "-1200"),
                ("radio.iq_correction_enabled", "true"),
                ("radio.amplifier_enabled", "true"),
                ("radio.antenna_power_enabled", "true"),
                ("radio.gain.lna_db", "24"),
                ("radio.gain.vga_db", "16"),
            ],
        );
        assert_not_recorded(&metadata, &["radio.gain.amp_db"]);
    }

    #[test]
    fn the_radio_analyzer_is_recorded_with_its_demodulation_and_whitening() {
        let mut case = RecordingCase::new();
        let analyzer = &mut case.radio_settings.analyzer;
        analyzer.demodulation = RadioDemodulation::BinaryGfsk;
        analyzer.channel_bandwidth_hz = 150_000;
        analyzer.output_sample_rate_hz = 400_000;
        analyzer.filter_tap_count = 129;
        analyzer.symbol_rate_hz = 38_400;
        analyzer.deviation_hz = 20_000;
        analyzer.gfsk_bt_milli = 300;
        analyzer.inverted = true;
        analyzer.adaptive_timing = false;
        analyzer.timing_loop_bandwidth_milli = 80;
        analyzer.max_clock_error_ppm = 5_000;
        analyzer.data_mode = RadioDataMode::Packets;
        analyzer.preamble_byte = 0xaa;
        analyzer.preamble_bytes = 4;
        analyzer.sync_word = 0x2d_d4;
        analyzer.sync_word_bytes = 2;
        analyzer.maximum_preamble_bit_errors = 1;
        analyzer.minimum_bit_confidence_milli = 600;
        analyzer.maximum_uncertain_bits = 3;
        analyzer.whitening_enabled = true;
        analyzer.whitening_preset = RadioWhiteningPreset::Pn9X9X5;
        analyzer.whitening_width = 9;
        analyzer.whitening_polynomial = 0x21;
        analyzer.whitening_seed = 0x1ff;
        analyzer.whitening_direction = WhiteningDirection::ShiftLeft;
        analyzer.whitening_invert_output = true;
        analyzer.whitening_start_bit = 8;
        analyzer.whitening_end_bit = Some(120);
        analyzer.whitening_advance_excluded_bits = true;

        let metadata = case.recorded_metadata("gui-recording-analyzer");

        assert_recorded(
            &metadata,
            &[
                ("radio.analyzer.enabled", "true"),
                ("radio.analyzer.demodulation", "gfsk2"),
                ("radio.analyzer.channel_bandwidth_hz", "150000"),
                ("radio.analyzer.output_sample_rate_hz", "400000"),
                ("radio.analyzer.filter_tap_count", "129"),
                ("radio.analyzer.symbol_rate_hz", "38400"),
                ("radio.analyzer.deviation_hz", "20000"),
                ("radio.analyzer.gfsk_bt_milli", "300"),
                ("radio.analyzer.inverted", "true"),
                ("radio.analyzer.adaptive_timing", "false"),
                ("radio.analyzer.timing_loop_bandwidth_milli", "80"),
                ("radio.analyzer.max_clock_error_ppm", "5000"),
                ("radio.analyzer.data_mode", "packets"),
                ("radio.analyzer.preamble_byte", "170"),
                ("radio.analyzer.preamble_bytes", "4"),
                ("radio.analyzer.sync_word", "11732"),
                ("radio.analyzer.sync_word_bytes", "2"),
                ("radio.analyzer.maximum_preamble_bit_errors", "1"),
                ("radio.analyzer.minimum_bit_confidence_milli", "600"),
                ("radio.analyzer.maximum_uncertain_bits", "3"),
                ("radio.analyzer.whitening_enabled", "true"),
                ("radio.analyzer.whitening_preset", "pn9_x9_x5"),
                ("radio.analyzer.whitening_width", "9"),
                ("radio.analyzer.whitening_polynomial", "33"),
                ("radio.analyzer.whitening_seed", "511"),
                ("radio.analyzer.whitening_direction", "left"),
                ("radio.analyzer.whitening_invert_output", "true"),
                ("radio.analyzer.whitening_start_bit", "8"),
                ("radio.analyzer.whitening_end_bit", "120"),
                ("radio.analyzer.whitening_advance_excluded_bits", "true"),
            ],
        );
    }

    #[test]
    fn radio_analyzer_options_are_recorded_under_their_own_names() {
        for (demodulation, name, enabled) in [
            (RadioDemodulation::None, "none", "false"),
            (RadioDemodulation::BinaryFsk, "fsk2", "true"),
            (RadioDemodulation::BinaryGfsk, "gfsk2", "true"),
        ] {
            let mut case = RecordingCase::new();
            case.radio_settings.analyzer.demodulation = demodulation;
            let metadata = case.recorded_metadata("gui-recording-analyzer-demodulation");
            assert_recorded(
                &metadata,
                &[
                    ("radio.analyzer.demodulation", name),
                    ("radio.analyzer.enabled", enabled),
                ],
            );
        }
        for (mode, name) in [
            (RadioDataMode::Raw, "raw"),
            (RadioDataMode::Packets, "packets"),
        ] {
            let mut case = RecordingCase::new();
            case.radio_settings.analyzer.data_mode = mode;
            let metadata = case.recorded_metadata("gui-recording-analyzer-mode");
            assert_recorded(&metadata, &[("radio.analyzer.data_mode", name)]);
        }
        for (direction, name) in [
            (WhiteningDirection::ShiftRight, "right"),
            (WhiteningDirection::ShiftLeft, "left"),
        ] {
            let mut case = RecordingCase::new();
            case.radio_settings.analyzer.whitening_direction = direction;
            let metadata = case.recorded_metadata("gui-recording-analyzer-direction");
            assert_recorded(&metadata, &[("radio.analyzer.whitening_direction", name)]);
        }
        for (preset, name) in [
            (RadioWhiteningPreset::Custom, "custom"),
            (RadioWhiteningPreset::Pn9X9X5, "pn9_x9_x5"),
        ] {
            let mut case = RecordingCase::new();
            case.radio_settings.analyzer.whitening_preset = preset;
            let metadata = case.recorded_metadata("gui-recording-analyzer-preset");
            assert_recorded(&metadata, &[("radio.analyzer.whitening_preset", name)]);
        }
        let mut case = RecordingCase::new();
        case.radio_settings.analyzer.whitening_end_bit = None;
        let metadata = case.recorded_metadata("gui-recording-analyzer-end-bit");
        assert_recorded(&metadata, &[("radio.analyzer.whitening_end_bit", "none")]);
    }

    #[test]
    fn the_radio_analyzer_channel_list_is_recorded_with_the_first_offset_as_the_legacy_key() {
        let mut case = RecordingCase::new();
        case.radio_settings.analyzer.channels = vec![
            RadioAnalyzerChannelSettings {
                name: "Uplink".to_owned(),
                frequency_offset_hz: -200_000,
                sync_word: "91 d3".to_owned(),
            },
            RadioAnalyzerChannelSettings {
                name: "Downlink".to_owned(),
                frequency_offset_hz: 200_000,
                sync_word: String::new(),
            },
        ];

        let metadata = case.recorded_metadata("gui-recording-analyzer-channels");

        assert_recorded(
            &metadata,
            &[
                ("radio.analyzer.channel_count", "2"),
                ("radio.analyzer.channel.0.offset_hz", "-200000"),
                ("radio.analyzer.channel.0.name", "Uplink"),
                ("radio.analyzer.channel.0.sync_word", "91 d3"),
                ("radio.analyzer.channel.1.offset_hz", "200000"),
                ("radio.analyzer.channel.1.name", "Downlink"),
                ("radio.analyzer.channel_offset_hz", "-200000"),
            ],
        );
        // A channel without a sync word of its own uses the general one, and
        // records nothing.
        assert_not_recorded(
            &metadata,
            &[
                "radio.analyzer.channel.1.sync_word",
                "radio.analyzer.channel.2.name",
            ],
        );

        case.radio_settings.analyzer.channels.clear();
        let metadata = case.recorded_metadata("gui-recording-analyzer-no-channels");
        assert_recorded(
            &metadata,
            &[
                ("radio.analyzer.channel_count", "0"),
                ("radio.analyzer.channel_offset_hz", "0"),
            ],
        );
        assert_not_recorded(&metadata, &["radio.analyzer.channel.0.name"]);
    }
}
