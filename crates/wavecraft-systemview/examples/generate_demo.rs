//! Generates the SystemView demo recording checked into `docs/examples`.

use std::io;
use std::path::PathBuf;

const SYNC_PACKET: [u8; 10] = [0; 10];
const FILE_HEADER: &str = ";\r\n\
; Version     SEGGER SystemView V4.10b\r\n\
; RecordTime  01 Jan 2026 00:00:00\r\n\
; Author      WaveCraft\r\n\
; Title       nRF52832 FreeRTOS demo\r\n\
; Description Deterministic System Trace demo recording\r\n\
;\r\n";

fn append_varint(output: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn append_string(output: &mut Vec<u8>, value: &str) -> io::Result<()> {
    let length = u8::try_from(value.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "demo strings must fit the compact SystemView encoding",
        )
    })?;
    output.push(length);
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn compact_packet(output: &mut Vec<u8>, event_id: u64, payload: &[u8], delta: u64) {
    append_varint(output, event_id);
    output.extend_from_slice(payload);
    append_varint(output, delta);
}

fn length_packet(output: &mut Vec<u8>, event_id: u64, payload: &[u8], delta: u64) {
    append_varint(output, event_id);
    append_varint(output, payload.len() as u64);
    output.extend_from_slice(payload);
    append_varint(output, delta);
}

fn values(values: &[u64]) -> Vec<u8> {
    let mut payload = Vec::new();
    for value in values {
        append_varint(&mut payload, *value);
    }
    payload
}

fn task_info(
    output: &mut Vec<u8>,
    task_id: u64,
    priority: u64,
    name: &str,
    delta: u64,
) -> io::Result<()> {
    let mut payload = values(&[task_id, priority]);
    append_string(&mut payload, name)?;
    compact_packet(output, 9, &payload, delta);
    Ok(())
}

fn formatted_print(
    output: &mut Vec<u8>,
    format: &str,
    arguments: &[u64],
    delta: u64,
) -> io::Result<()> {
    let mut payload = Vec::new();
    append_string(&mut payload, format)?;
    append_varint(&mut payload, 0);
    append_varint(&mut payload, arguments.len() as u64);
    for argument in arguments {
        append_varint(&mut payload, *argument);
    }
    length_packet(output, 26, &payload, delta);
    Ok(())
}

fn demo_stream() -> io::Result<Vec<u8>> {
    let mut output = SYNC_PACKET.to_vec();

    compact_packet(&mut output, 10, &[], 1); // Trace started.
    length_packet(
        &mut output,
        24,
        &values(&[1_000_000, 64_000_000, 0x2000_0000, 0]),
        0,
    );
    let mut description = Vec::new();
    append_string(
        &mut description,
        "N=nRF52832;O=FreeRTOS;D=WaveCraft simulated wristband",
    )?;
    compact_packet(&mut output, 14, &description, 0);
    task_info(&mut output, 1, 2, "app_main", 0)?;
    task_info(&mut output, 2, 4, "sensor", 0)?;
    task_info(&mut output, 3, 3, "ble_radio", 0)?;

    formatted_print(&mut output, "application initialized", &[], 1_000)?;
    for cycle in 0_u64..64 {
        compact_packet(&mut output, 4, &values(&[1]), 620 + cycle % 7 * 15);
        if cycle % 16 == 0 {
            formatted_print(
                &mut output,
                "battery=%u mV",
                &[3_986_u64.saturating_sub(cycle * 3)],
                80,
            )?;
        }
        compact_packet(&mut output, 2, &values(&[15]), 240);
        compact_packet(&mut output, 3, &[], 35);
        compact_packet(&mut output, 19, &values(&[4]), 110);
        compact_packet(&mut output, 20, &[], 45);
        compact_packet(&mut output, 7, &values(&[1, 1]), 190);
        compact_packet(&mut output, 5, &[], 1);

        compact_packet(&mut output, 4, &values(&[2]), 40);
        let temperature = 22.0_f32 + (cycle % 12) as f32 * 0.25;
        compact_packet(
            &mut output,
            23,
            &values(&[1, u64::from(temperature.to_bits())]),
            135,
        );
        compact_packet(&mut output, 2, &values(&[17]), 90);
        compact_packet(&mut output, 3, &[], 25);
        if cycle % 8 == 0 {
            formatted_print(
                &mut output,
                "temperature=%u C",
                &[temperature.round() as u64],
                30,
            )?;
        }
        compact_packet(&mut output, 7, &values(&[2, 2]), 160);
        compact_packet(&mut output, 5, &[], 1);

        compact_packet(&mut output, 4, &values(&[3]), 55);
        compact_packet(&mut output, 15, &values(&[cycle % 4 + 1]), 80);
        compact_packet(&mut output, 2, &values(&[16]), 120);
        compact_packet(&mut output, 3, &[], 30);
        compact_packet(&mut output, 16, &values(&[cycle % 4 + 1]), 95);
        if cycle % 10 == 0 {
            formatted_print(
                &mut output,
                "BLE packet sent, channel=%u",
                &[37 + cycle % 3],
                20,
            )?;
        }
        compact_packet(&mut output, 7, &values(&[3, 3]), 140);
        compact_packet(&mut output, 5, &[], 1);
        compact_packet(&mut output, 17, &[], 65);
    }
    compact_packet(&mut output, 11, &[], 900);

    Ok(output)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("docs/examples/nrf52832-freertos.SVDat"));
    let mut recording = FILE_HEADER.as_bytes().to_vec();
    recording.extend(demo_stream()?);
    std::fs::write(&path, recording)?;
    println!("generated {}", path.display());
    Ok(())
}
