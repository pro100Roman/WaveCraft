use std::f64::consts::TAU;
use wavecraft_core::{Iq8, IqBatch};
use wavecraft_dsp::{
    Channelizer, ChannelizerConfig, FskConfig, FskDemodulator, FskModulation, FskPolarity,
    TimedSoftBit,
};

const SOURCE_SAMPLE_RATE_HZ: u64 = 64_000;
const SYMBOL_RATE_HZ: u64 = 1_000;
const SAMPLES_PER_SYMBOL: usize = (SOURCE_SAMPLE_RATE_HZ / SYMBOL_RATE_HZ) as usize;
const CHANNEL_OFFSET_HZ: f64 = 10_000.0;
const CARRIER_ERROR_HZ: f64 = 100.0;
const DEVIATION_HZ: f64 = 1_000.0;

fn modulated_iq(bits: &[bool]) -> Vec<Iq8> {
    let mut phase: f64 = 0.0;
    let mut samples = Vec::with_capacity(bits.len() * SAMPLES_PER_SYMBOL);
    for bit in bits {
        let deviation = if *bit { DEVIATION_HZ } else { -DEVIATION_HZ };
        let frequency = CHANNEL_OFFSET_HZ + CARRIER_ERROR_HZ + deviation;
        for _ in 0..SAMPLES_PER_SYMBOL {
            samples.push(Iq8 {
                i: (100.0 * phase.cos()).round() as i8,
                q: (100.0 * phase.sin()).round() as i8,
            });
            phase += TAU * frequency / SOURCE_SAMPLE_RATE_HZ as f64;
        }
    }
    samples
}

fn decode_with_chunks(samples: &[Iq8], chunk_lengths: &[usize]) -> Vec<TimedSoftBit> {
    let mut channelizer = Channelizer::new(ChannelizerConfig {
        frequency_offset_hz: CHANNEL_OFFSET_HZ as i64,
        bandwidth_hz: 4_000,
        output_sample_rate_hz: 8_000,
        filter_tap_count: 65,
    })
    .unwrap();
    let mut demodulator = FskDemodulator::new(FskConfig {
        symbol_rate_hz: SYMBOL_RATE_HZ,
        deviation_hz: DEVIATION_HZ as u64,
        modulation: FskModulation::Fsk,
        polarity: FskPolarity::Normal,
    })
    .unwrap();
    let mut first_sample_index = 0_usize;
    let mut decoded = Vec::new();
    for &chunk_length in chunk_lengths {
        let end = first_sample_index + chunk_length;
        let channelized = channelizer
            .push(&IqBatch {
                sample_rate_hz: SOURCE_SAMPLE_RATE_HZ,
                center_frequency_hz: 100_000_000,
                first_sample_index: first_sample_index as u64,
                discontinuity: false,
                samples: samples[first_sample_index..end].to_vec(),
            })
            .unwrap();
        decoded.extend(demodulator.push(&channelized).unwrap());
        first_sample_index = end;
    }
    assert_eq!(first_sample_index, samples.len());
    decoded
}

#[test]
fn channelizer_and_fsk_demodulator_decode_across_arbitrary_iq_chunks() {
    let transmitted = [true, false, true, true, false, false, true];
    let samples = modulated_iq(&transmitted);
    let whole = decode_with_chunks(&samples, &[samples.len()]);
    let chunked = decode_with_chunks(&samples, &[37, 91, 113, 79, 128]);

    assert_eq!(chunked, whole);
    assert_eq!(
        whole
            .iter()
            .map(|bit| bit.likelihood.is_sign_positive())
            .collect::<Vec<_>>(),
        transmitted[1..6]
    );
    assert!(whole.iter().all(|bit| bit.likelihood.abs() > 0.65));
    assert!(whole.windows(2).all(|bits| {
        bits[0].source_range.start < bits[1].source_range.start
            && bits[0].source_range.end < bits[1].source_range.end
    }));
}
