//! Generates the raw SystemView demo stream checked into `docs/examples`.

use std::io;
use std::path::PathBuf;

const SYNC_PACKET: [u8; 10] = [0; 10];

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

    compact_packet(&mut output, 4, &values(&[1]), 1_000);
    formatted_print(&mut output, "application initialized", &[], 450)?;
    compact_packet(&mut output, 2, &values(&[15]), 620);
    compact_packet(&mut output, 3, &[], 180);
    compact_packet(&mut output, 19, &values(&[4]), 520);
    compact_packet(&mut output, 20, &[], 120);
    compact_packet(&mut output, 7, &values(&[1, 1]), 400);
    compact_packet(&mut output, 5, &[], 1);

    compact_packet(&mut output, 4, &values(&[2]), 90);
    compact_packet(
        &mut output,
        23,
        &values(&[1, u64::from(24.7_f32.to_bits())]),
        350,
    );
    compact_packet(&mut output, 2, &values(&[17]), 400);
    compact_packet(&mut output, 3, &[], 75);
    formatted_print(&mut output, "temperature=%u C", &[25], 260)?;
    compact_packet(&mut output, 7, &values(&[2, 2]), 300);
    compact_packet(&mut output, 5, &[], 1);

    compact_packet(&mut output, 4, &values(&[3]), 80);
    compact_packet(&mut output, 15, &values(&[1]), 120);
    compact_packet(&mut output, 2, &values(&[16]), 180);
    compact_packet(&mut output, 3, &[], 90);
    compact_packet(&mut output, 16, &values(&[1]), 250);
    formatted_print(&mut output, "BLE packet sent, channel=%u", &[37], 100)?;
    compact_packet(&mut output, 7, &values(&[3, 3]), 310);
    compact_packet(&mut output, 5, &[], 1);

    compact_packet(&mut output, 17, &[], 100);
    compact_packet(&mut output, 2, &values(&[15]), 850);
    compact_packet(&mut output, 18, &[], 120);
    compact_packet(&mut output, 4, &values(&[1]), 90);
    formatted_print(&mut output, "battery=%u mV", &[3_986], 340)?;
    compact_packet(&mut output, 11, &[], 900);

    Ok(output)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("docs/examples/nrf52832-freertos.SVDat"));
    let stream = demo_stream()?;
    std::fs::write(&path, stream)?;
    println!("generated {}", path.display());
    Ok(())
}
