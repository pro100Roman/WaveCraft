//! Generates matching ASCII, binary and CSV UART samples for `docs/examples`.

use std::{error::Error, fs, io, path::PathBuf};
use wavecraft_serial::{
    BinaryDecoder, BinarySchema, ByteOrder, CrcConfiguration, CsvDelimiter, CsvReader, TextDecoder,
};

const SCHEMA: &str = include_str!("../../../docs/examples/serial-schema.json");
const RECORDS: usize = 512;

fn triangle(index: usize, period: usize) -> usize {
    let phase = index % period;
    phase.min(period - phase)
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .unwrap_or_else(|| "docs/examples".into()),
    );
    let schema = BinarySchema::from_json(SCHEMA)?;
    let fields = schema.channel_names();
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
    let mut ascii = String::new();
    let mut csv = String::from("timestamp,temperature,speed,enabled,fault,mode\n");
    let mut binary = Vec::new();
    let mut expected = Vec::new();
    for index in 0..RECORDS {
        let temperature = 18.0 + triangle(index, 128) as f32 * 0.25;
        let speed = triangle(index, 192) as u16;
        let enabled = ((index / 96) % 2) as u8;
        let fault = if (320..352).contains(&index) { 3 } else { 0 };
        let mode = ((index / 128) % 4) as u8;
        ascii.push_str(&format!(
            "temperature={temperature:.2} speed={speed} enabled={enabled} fault={fault} mode={mode}\r\n"
        ));
        csv.push_str(&format!(
            "{:.2},{temperature:.2},{speed},{enabled},{fault},{mode}\n",
            index as f64 / 100.0
        ));
        let mut payload = temperature.to_le_bytes().to_vec();
        payload.extend_from_slice(&speed.to_le_bytes());
        payload.push(enabled | (fault << 1) | (mode << 4));
        binary.extend_from_slice(&[0xaa, 0x55]);
        binary.extend_from_slice(&payload);
        binary.extend_from_slice(&(crc.checksum(&payload)? as u16).to_le_bytes());
        binary.extend_from_slice(b"\r\n");
        expected.push(vec![
            f64::from(temperature),
            f64::from(speed),
            f64::from(enabled),
            f64::from(fault),
            f64::from(mode),
        ]);
    }

    // Validate with the actual streaming decoders, including split records.
    let mut binary_decoder = BinaryDecoder::new(schema, vec![0xaa, 0x55], vec![13, 10], crc)?;
    let decoded_binary: Vec<_> = binary
        .chunks(17)
        .flat_map(|chunk| binary_decoder.feed(chunk))
        .collect();
    let mut text_decoder = TextDecoder::default();
    let decoded_ascii: Vec<_> = ascii
        .as_bytes()
        .chunks(17)
        .flat_map(|chunk| text_decoder.feed(chunk, &fields))
        .collect();
    let expected_ascii: Vec<Vec<_>> = expected
        .iter()
        .map(|record| record.iter().copied().map(Some).collect())
        .collect();
    let decoded_csv: Vec<_> = CsvReader::new(csv.as_bytes(), CsvDelimiter::Auto, true)?
        .records(&fields)?
        .collect::<Result<_, _>>()?;
    if decoded_binary != expected
        || decoded_ascii != expected_ascii
        || decoded_csv != expected_ascii
        || binary_decoder.rejected_frames != 0
        || binary_decoder.discarded_bytes != 0
    {
        return Err(io::Error::other("Generated UART samples did not round-trip").into());
    }

    fs::create_dir_all(&output)?;
    fs::write(output.join("serial-ascii.txt"), &ascii)?;
    fs::write(output.join("serial-binary.bin"), &binary)?;
    fs::write(output.join("serial-samples.csv"), &csv)?;
    fs::write(output.join("serial-schema.json"), SCHEMA)?;
    println!(
        "Wrote and verified {RECORDS} matching records: {} ASCII bytes, {} binary bytes, {} CSV bytes in {}",
        ascii.len(),
        binary.len(),
        csv.len(),
        output.display()
    );
    Ok(())
}
