use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufReader, Read};
use wavecraft_core::{Iq8, IqBatch};
use wavecraft_dsp::{SpectrumFrame, SpectrumProcessor, SpectrumWindow};

const FFT_LENGTH: usize = 4_096;
const READ_BYTES: usize = 1 << 20;
const ANALYSIS_HALF_WIDTH_HZ: f64 = 100_000.0;
const BURST_MARGIN_DB: f32 = 8.0;

#[derive(Clone, Copy, Debug)]
struct Observation {
    sample_index: u64,
    power_dbfs: f32,
    peak_frequency_hz: f64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = env::args().collect::<Vec<_>>();
    if arguments.len() < 5 {
        return Err(
            "usage: analyze_hackrf_iq <iq8-file> <sample-rate-hz> <center-hz> <target-hz>..."
                .into(),
        );
    }
    let sample_rate_hz = arguments[2].parse::<u64>()?;
    let center_frequency_hz = arguments[3].parse::<u64>()?;
    let targets = arguments[4..]
        .iter()
        .map(|argument| argument.parse::<u64>())
        .collect::<Result<Vec<_>, _>>()?;
    let mut observations = vec![Vec::new(); targets.len()];
    let mut processor = SpectrumProcessor::new(FFT_LENGTH, SpectrumWindow::Hann)?;
    let mut reader = BufReader::new(File::open(&arguments[1])?);
    let mut bytes = vec![0_u8; READ_BYTES];
    let mut first_sample_index = 0_u64;
    let mut sample_count = 0_u64;
    let mut clipped_components = 0_u64;
    let mut i_sum = 0_i128;
    let mut q_sum = 0_i128;

    loop {
        let read = reader.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        let usable = read - read % 2;
        let samples = bytes[..usable]
            .chunks_exact(2)
            .map(|sample| {
                let i = sample[0] as i8;
                let q = sample[1] as i8;
                sample_count += 1;
                i_sum += i128::from(i);
                q_sum += i128::from(q);
                clipped_components += u64::from(i == i8::MIN || i == i8::MAX);
                clipped_components += u64::from(q == i8::MIN || q == i8::MAX);
                Iq8 { i, q }
            })
            .collect::<Vec<_>>();
        let frames = processor.push(&IqBatch {
            sample_rate_hz,
            center_frequency_hz,
            first_sample_index,
            discontinuity: first_sample_index == 0,
            samples,
        });
        for frame in &frames {
            for (target_index, target) in targets.iter().enumerate() {
                if let Some(observation) = observe(frame, *target as f64) {
                    observations[target_index].push(observation);
                }
            }
        }
        first_sample_index = first_sample_index.saturating_add((usable / 2) as u64);
    }

    println!(
        "samples={sample_count} duration={:.3}s dc_i={:.3} dc_q={:.3} clipped={:.6}%",
        sample_count as f64 / sample_rate_hz as f64,
        i_sum as f64 / sample_count.max(1) as f64,
        q_sum as f64 / sample_count.max(1) as f64,
        clipped_components as f64 * 100.0 / (sample_count.max(1) * 2) as f64,
    );
    for (target, target_observations) in targets.iter().zip(&observations) {
        report_target(*target, target_observations, sample_rate_hz);
    }
    Ok(())
}

fn observe(frame: &SpectrumFrame, target_hz: f64) -> Option<Observation> {
    let span_hz = frame.bin_width_hz * frame.power_dbfs.len() as f64;
    let first_bin_hz = frame.center_frequency_hz as f64 - span_hz / 2.0;
    let (bin, power_dbfs) = frame
        .power_dbfs
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, _)| {
            let frequency_hz = first_bin_hz + *index as f64 * frame.bin_width_hz;
            (frequency_hz - target_hz).abs() <= ANALYSIS_HALF_WIDTH_HZ
        })
        .max_by(|left, right| left.1.total_cmp(&right.1))?;
    Some(Observation {
        sample_index: frame.source_range.start,
        power_dbfs,
        peak_frequency_hz: first_bin_hz + bin as f64 * frame.bin_width_hz,
    })
}

fn report_target(target_hz: u64, observations: &[Observation], sample_rate_hz: u64) {
    if observations.is_empty() {
        println!("target={target_hz}Hz no spectrum observations");
        return;
    }
    let mut powers = observations
        .iter()
        .map(|observation| observation.power_dbfs)
        .collect::<Vec<_>>();
    powers.sort_by(f32::total_cmp);
    let noise_dbfs = powers[powers.len() / 2];
    let threshold_dbfs = noise_dbfs + BURST_MARGIN_DB;
    println!(
        "target={target_hz}Hz median_peak={noise_dbfs:.2}dBFS burst_threshold={threshold_dbfs:.2}dBFS"
    );

    let mut burst = Vec::new();
    let mut previous_frame = None;
    for observation in observations
        .iter()
        .copied()
        .filter(|observation| observation.power_dbfs >= threshold_dbfs)
    {
        let contiguous = previous_frame.is_some_and(|previous: u64| {
            observation.sample_index.saturating_sub(previous) <= (FFT_LENGTH as u64 * 3)
        });
        if !contiguous && !burst.is_empty() {
            report_burst(&burst, sample_rate_hz);
            burst.clear();
        }
        burst.push(observation);
        previous_frame = Some(observation.sample_index);
    }
    if !burst.is_empty() {
        report_burst(&burst, sample_rate_hz);
    }
}

fn report_burst(burst: &[Observation], sample_rate_hz: u64) {
    let strongest = burst
        .iter()
        .max_by(|left, right| left.power_dbfs.total_cmp(&right.power_dbfs))
        .unwrap_or(&burst[0]);
    let start = burst[0].sample_index as f64 / sample_rate_hz as f64;
    let end = burst[burst.len() - 1]
        .sample_index
        .saturating_add(FFT_LENGTH as u64) as f64
        / sample_rate_hz as f64;
    println!(
        "  burst {start:.6}..{end:.6}s peak={:.2}dBFS @ {:.0}Hz frames={}",
        strongest.power_dbfs,
        strongest.peak_frequency_hz,
        burst.len(),
    );
}
