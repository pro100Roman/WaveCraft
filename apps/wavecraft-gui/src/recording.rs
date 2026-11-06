use crate::{waveform::radio::RawRadioMessage, RadioDataMode, RadioSettings};
use serde_json::json;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
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

fn available_recording_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("capture");
    for index in 2_u32.. {
        let candidate = parent.join(format!("{stem}-{index}.dsv"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("the recording file suffix cannot be exhausted")
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
        .map(available_recording_path)
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

fn available_decoded_data_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("decoded-data");
    for index in 2_u32.. {
        let candidate = parent.join(format!("{stem}-{index}.jsonl"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("the decoded data file suffix cannot be exhausted")
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
        .map(available_decoded_data_path)
}

pub(crate) struct DecodedDataRecorder<W: Write> {
    output: W,
    data_mode: RadioDataMode,
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
        write_json_line(
            &mut self.output,
            &json!({
                "type": "packet",
                "sequence": sequence,
                "channel_name": channel_name,
                "channel_frequency_offset_hz": channel_frequency_offset_hz,
                "sample_start": packet.range.start,
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
        let sample_start = message.bytes().first().map(|byte| byte.range.start);
        let sample_end = message.bytes().last().map(|byte| byte.range.end);
        write_json_line(
            &mut self.output,
            &json!({
                "type": "raw_message",
                "sequence": sequence,
                "capture_sequence": message.sequence(),
                "channel_name": message.channel_name(),
                "channel_frequency_offset_hz": message.channel_frequency_offset_hz(),
                "sample_start": sample_start,
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

pub(crate) fn choose_recording_to_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open capture")
        .add_filter("Supported captures", &["dsv", "sal"])
        .add_filter("WaveCraft capture", &["dsv"])
        .add_filter("Saleae Logic 2 capture", &["sal"])
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waveform::radio::RadioAnalysisStore;
    use std::sync::Arc;
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
        assert_eq!(records[1]["data"], json!([0x12, 0x34]));
        assert_eq!(records[2]["type"], "footer");
        assert_eq!(records[2]["record_count"], 1);
        assert_eq!(records[2]["status"], "complete");
    }

    #[test]
    fn decoded_data_recorder_streams_completed_raw_messages() {
        let settings = RadioSettings::default();
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
        assert_eq!(records[1]["preamble"], json!([0xaa]));
        assert_eq!(records[1]["data"], json!([0x12]));
        assert_eq!(records[2]["data"], json!([0x34]));
        assert_eq!(records[3]["record_count"], 2);
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
}
