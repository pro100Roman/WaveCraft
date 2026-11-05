use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use wavecraft_core::{Iq8, IqBatch};
use wavecraft_dsp::{
    BinaryGfskProfile, ChannelizerConfig, DemodulatedBatch, DemodulationProfile,
    DemodulatorRegistry, FskPolarity, FskTiming, SignalAnalyzer, SignalAnalyzerProfile,
};

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = env::args().collect::<Vec<_>>();
    if arguments.len() != 10 {
        return Err("usage: decode_hackrf_gfsk <iq8-file> <sample-rate-hz> <center-hz> <target-hz> <start-seconds> <duration-seconds> <symbol-rate-hz> <deviation-hz> <bt>".into());
    }
    let sample_rate_hz = arguments[2].parse::<u64>()?;
    let center_frequency_hz = arguments[3].parse::<u64>()?;
    let target_frequency_hz = arguments[4].parse::<u64>()?;
    let start_seconds = arguments[5].parse::<f64>()?;
    let duration_seconds = arguments[6].parse::<f64>()?;
    let symbol_rate_hz = arguments[7].parse::<u64>()?;
    let deviation_hz = arguments[8].parse::<u64>()?;
    let bt = arguments[9].parse::<f64>()?;
    let first_sample_index = (start_seconds * sample_rate_hz as f64).round() as u64;
    let sample_count = (duration_seconds * sample_rate_hz as f64).round() as usize;
    let mut bytes = vec![0_u8; sample_count.saturating_mul(2)];
    let mut file = File::open(&arguments[1])?;
    file.seek(SeekFrom::Start(first_sample_index.saturating_mul(2)))?;
    file.read_exact(&mut bytes)?;
    let samples = bytes
        .chunks_exact(2)
        .map(|sample| Iq8 {
            i: sample[0] as i8,
            q: sample[1] as i8,
        })
        .collect::<Vec<_>>();

    for polarity in [FskPolarity::Normal, FskPolarity::Inverted] {
        let profile = SignalAnalyzerProfile {
            channel: ChannelizerConfig {
                frequency_offset_hz: target_frequency_hz as i64 - center_frequency_hz as i64,
                bandwidth_hz: 150_000,
                output_sample_rate_hz: 512_000,
                filter_tap_count: 257,
            },
            demodulation: DemodulationProfile::BinaryGfsk(BinaryGfskProfile {
                symbol_rate_hz,
                deviation_hz,
                bt,
                polarity,
                timing: FskTiming::Adaptive {
                    loop_bandwidth: 0.1,
                    max_clock_error_ppm: 20_000,
                },
            }),
        };
        let registry = DemodulatorRegistry::with_builtins()?;
        let mut analyzer = SignalAnalyzer::new(profile, &registry)?;
        let output = analyzer.push(&IqBatch {
            sample_rate_hz,
            center_frequency_hz,
            first_sample_index,
            discontinuity: true,
            samples: samples.clone(),
        })?;
        let telemetry = analyzer.telemetry();
        let DemodulatedBatch::SoftSymbols(symbols) = output else {
            return Err("GFSK demodulator did not return soft symbols".into());
        };
        let bits = symbols
            .symbols
            .iter()
            .map(|symbol| symbol.decision() == Some(1))
            .collect::<Vec<_>>();
        println!(
            "polarity={polarity:?} symbols={} locked={} quality={:.3} rate={:?}Hz cfo={:?}Hz separation={:?}Hz",
            bits.len(),
            telemetry.timing_locked,
            telemetry.timing_lock_quality,
            telemetry.estimated_symbol_rate_hz,
            telemetry.estimated_frequency_offset_hz,
            telemetry.estimated_tone_separation_hz,
        );
        for phase in 0..8 {
            let encoded = bits[phase.min(bits.len())..]
                .chunks_exact(8)
                .take(48)
                .map(|chunk| {
                    chunk
                        .iter()
                        .fold(0_u8, |value, bit| (value << 1) | u8::from(*bit))
                })
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!("  phase={phase}: {encoded}");
        }
    }
    Ok(())
}
