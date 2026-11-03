use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::path::PathBuf;
use std::time::Duration;
use wavecraft_core::{
    CaptureBatch, CaptureEvent, CaptureFrame, DeviceDescriptor, DeviceId, InstrumentKind,
    LogicBatch,
};
use wavecraft_recording::{CompletionStatus, RecordingWriter};

const SAMPLE_RATE_HZ: u64 = 1_152_000;
const SAMPLES_PER_BIT: usize = 10;
const SAMPLE_COUNT: usize = 4_096;
const DEVICE_ID: &str = "example-motor-uart";

fn main() -> Result<(), Box<dyn Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_output_path);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }

    let samples = example_samples();
    let device_id = DeviceId::new(DEVICE_ID);
    let devices = [DeviceDescriptor {
        id: device_id.clone(),
        name: "Motor Controller UART example".to_owned(),
        kind: InstrumentKind::LogicAnalyzer,
    }];
    let mut metadata = BTreeMap::from([
        ("capture.command".to_owned(), "example".to_owned()),
        (
            "example.protocol_schema".to_owned(),
            "motor-controller.wavecraft-protocol.yaml".to_owned(),
        ),
        (
            "logic.sample_rate_hz".to_owned(),
            SAMPLE_RATE_HZ.to_string(),
        ),
        ("logic.sample_count".to_owned(), samples.len().to_string()),
        ("logic.operation_mode".to_owned(), "buffer".to_owned()),
        (
            "logic.stop_option".to_owned(),
            "upload_captured_data".to_owned(),
        ),
    ]);
    for channel in 0..16 {
        metadata.insert(
            format!("logic.ch{channel}.enabled"),
            (channel == 0).to_string(),
        );
        metadata.insert(
            format!("logic.ch{channel}.name"),
            if channel == 0 { "UART RX" } else { "" }.to_owned(),
        );
    }

    let file = File::create(&output)?;
    let mut writer = RecordingWriter::new(file, &devices, metadata)?;
    writer.write_event(&CaptureEvent::Frame(CaptureFrame {
        device_id: device_id.clone(),
        sequence: 0,
        captured_at: Duration::ZERO,
        payload: CaptureBatch::Logic(LogicBatch {
            sample_rate_hz: SAMPLE_RATE_HZ,
            channel_count: 1,
            samples,
        }),
    }))?;
    writer.write_event(&CaptureEvent::SourceStopped { device_id })?;
    writer.finish(CompletionStatus::Complete)?;

    println!("{}", output.display());
    Ok(())
}

fn default_output_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/motor-controller-uart.dsv")
}

fn example_samples() -> Vec<u32> {
    let mut samples = vec![1; SAMPLES_PER_BIT * 4];
    for (motor, speed_rpm, flags) in [(1, 1_200, 0x01), (2, 2_000, 0x03), (3, 750, 0x00)] {
        let frame = motor_frame(motor, speed_rpm, flags);
        append_uart_8n1(&mut samples, &frame);
        samples.extend(std::iter::repeat(1).take(SAMPLES_PER_BIT * 10));
    }
    samples.resize(SAMPLE_COUNT, 1);
    samples
}

fn motor_frame(motor: u8, speed_rpm: u16, flags: u8) -> Vec<u8> {
    let [speed_low, speed_high] = speed_rpm.to_le_bytes();
    let mut frame = vec![0xaa, 0x55, 0x05, 0x10, motor, speed_low, speed_high, flags];
    frame.push(crc8_smbus(&frame[2..]));
    frame
}

fn append_uart_8n1(samples: &mut Vec<u32>, bytes: &[u8]) {
    for byte in bytes {
        samples.extend(std::iter::repeat(0).take(SAMPLES_PER_BIT));
        for bit in 0..8 {
            let level = u32::from(byte & (1 << bit) != 0);
            samples.extend(std::iter::repeat(level).take(SAMPLES_PER_BIT));
        }
        samples.extend(std::iter::repeat(1).take(SAMPLES_PER_BIT * 2));
    }
}

fn crc8_smbus(bytes: &[u8]) -> u8 {
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
