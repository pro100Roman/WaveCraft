// Integration tests may unwrap; the library lint exists for production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Bit-error-rate regression tests for adaptive FSK/GFSK decoding in noise.
//!
//! The other demodulator tests use noise-free signals, so they cannot see
//! failures that only appear when the discriminator output is dominated by
//! channel noise: symbol-clock slips, tone acquisition on noise, and decoding
//! that depends on how much noise precedes a burst. These tests decode
//! seeded, additive-white-Gaussian-noise bursts through the full channelizer
//! and demodulator and bound the bit error rate.
//!
//! The scenarios are the 1/10-rate equivalents of the configurations used to
//! tune the receiver, so the dimensionless ratios (samples per symbol,
//! deviation per symbol rate, channel width per deviation) are unchanged and
//! the debug-build tests stay fast. Thresholds sit roughly an order of
//! magnitude above the measured error rate and far below the rate of a
//! demodulator that lacks the post-discriminator filter.

use std::f64::consts::{LN_2, PI, TAU};
use wavecraft_core::{Iq8, IqBatch};
use wavecraft_dsp::{
    BinaryFskProfile, BinaryGfskProfile, ChannelizerConfig, DemodulatedBatch, DemodulationProfile,
    DemodulatorRegistry, FskPolarity, FskTiming, SignalAnalyzer, SignalAnalyzerProfile,
};

const PREAMBLE_BITS: usize = 32;
const SIGNAL_AMPLITUDE: f64 = 20.0;
/// Bits at the start of a burst that are excluded from the error count: the
/// channel filter has not yet filled and clock recovery is still settling.
const SETTLING_BITS: usize = 40;

/// Seeded generator so a failure reproduces exactly.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1_u64 << 53) as f64
    }

    fn gaussian(&mut self) -> f64 {
        let (first, second) = (self.uniform(), self.uniform());
        (-2.0 * first.ln()).sqrt() * (TAU * second).cos()
    }
}

#[derive(Clone, Copy)]
struct Link {
    sample_rate_hz: u64,
    symbol_rate_hz: u64,
    deviation_hz: u64,
    /// `None` is unshaped FSK.
    bt: Option<f64>,
    channel_offset_hz: i64,
    channel_bandwidth_hz: u64,
    channel_rate_hz: u64,
}

impl Link {
    /// 1/10-rate equivalent of a 100 kbit/s, 50 kHz deviation, BT 0.5 link
    /// (modulation index 1) sampled at 20 samples per symbol.
    fn fast_gfsk() -> Self {
        Self {
            sample_rate_hz: 200_000,
            symbol_rate_hz: 10_000,
            deviation_hz: 5_000,
            bt: Some(0.5),
            channel_offset_hz: 20_000,
            channel_bandwidth_hz: 25_000,
            channel_rate_hz: 50_000,
        }
    }

    /// The same link without Gaussian pulse shaping (plain 2-FSK).
    fn fast_fsk() -> Self {
        Self {
            bt: None,
            ..Self::fast_gfsk()
        }
    }

    /// 1/10-rate equivalent of the GUI's default 19.2 kbit/s, 35 kHz
    /// deviation link, whose 200 kHz channel is wide for the signal.
    fn default_gfsk() -> Self {
        Self {
            sample_rate_hz: 200_000,
            symbol_rate_hz: 1_920,
            deviation_hz: 3_500,
            bt: Some(0.5),
            channel_offset_hz: -20_000,
            channel_bandwidth_hz: 20_000,
            channel_rate_hz: 51_200,
        }
    }

    fn samples_per_symbol(&self) -> f64 {
        self.sample_rate_hz as f64 / self.symbol_rate_hz as f64
    }

    fn analyzer(&self, timing: FskTiming) -> SignalAnalyzer {
        let demodulation = match self.bt {
            Some(bt) => DemodulationProfile::BinaryGfsk(BinaryGfskProfile {
                symbol_rate_hz: self.symbol_rate_hz,
                deviation_hz: self.deviation_hz,
                bt,
                polarity: FskPolarity::Normal,
                timing,
            }),
            None => DemodulationProfile::BinaryFsk(BinaryFskProfile {
                symbol_rate_hz: self.symbol_rate_hz,
                deviation_hz: self.deviation_hz,
                polarity: FskPolarity::Normal,
                timing,
            }),
        };
        SignalAnalyzer::new(
            SignalAnalyzerProfile {
                channel: ChannelizerConfig {
                    frequency_offset_hz: self.channel_offset_hz,
                    bandwidth_hz: self.channel_bandwidth_hz,
                    output_sample_rate_hz: self.channel_rate_hz,
                    filter_tap_count: 257,
                },
                demodulation,
            },
            &DemodulatorRegistry::with_builtins().unwrap(),
        )
        .unwrap()
    }
}

/// The GUI's default timing loop: adaptive, 0.1 loop bandwidth, 1% clock error.
fn adaptive_timing() -> FskTiming {
    FskTiming::Adaptive {
        loop_bandwidth: 0.1,
        max_clock_error_ppm: 10_000,
    }
}

struct Burst {
    /// A 32-bit alternating preamble followed by random payload bits.
    bits: Vec<bool>,
    iq: Vec<Iq8>,
}

/// Builds one burst with `lead_symbols` of noise before it. `eb_n0_db` is the
/// energy per bit over the noise density, so the same value means the same
/// link margin whatever the sample rate.
fn burst(link: Link, rng: &mut Rng, eb_n0_db: f64, lead_symbols: usize, payload: usize) -> Burst {
    let mut bits = (0..PREAMBLE_BITS)
        .map(|index| index % 2 == 0)
        .collect::<Vec<_>>();
    bits.extend((0..payload).map(|_| rng.next_u64() & 1 == 1));

    let samples_per_symbol = link.samples_per_symbol();
    let signal_samples = (bits.len() as f64 * samples_per_symbol).ceil() as usize;
    let levels = (0..signal_samples)
        .map(|index| {
            let symbol = ((index as f64 / samples_per_symbol) as usize).min(bits.len() - 1);
            if bits[symbol] {
                1.0
            } else {
                -1.0
            }
        })
        .collect::<Vec<_>>();
    let shaped = match link.bt {
        Some(bt) => gaussian_shape(&levels, bt, samples_per_symbol),
        None => levels,
    };

    // Complex noise power over the whole sample rate: A^2 * fs / (Eb/N0 * Rb).
    let eb_n0 = 10_f64.powf(eb_n0_db / 10.0);
    let noise_power = SIGNAL_AMPLITUDE.powi(2) * link.sample_rate_hz as f64
        / (eb_n0 * link.symbol_rate_hz as f64);
    let sigma = (noise_power / 2.0).sqrt();
    let quantize = |value: f64| value.round().clamp(-127.0, 127.0) as i8;

    let lead_samples = (lead_symbols as f64 * samples_per_symbol) as usize;
    let mut iq = Vec::with_capacity(lead_samples + signal_samples);
    for _ in 0..lead_samples {
        iq.push(Iq8 {
            i: quantize(sigma * rng.gaussian()),
            q: quantize(sigma * rng.gaussian()),
        });
    }
    let mut phase = 0.0_f64;
    for level in shaped {
        iq.push(Iq8 {
            i: quantize(SIGNAL_AMPLITUDE * phase.cos() + sigma * rng.gaussian()),
            q: quantize(SIGNAL_AMPLITUDE * phase.sin() + sigma * rng.gaussian()),
        });
        let frequency_hz = link.channel_offset_hz as f64 + link.deviation_hz as f64 * level;
        phase += TAU * frequency_hz / link.sample_rate_hz as f64;
    }
    Burst { bits, iq }
}

/// Convolves NRZ levels with the Gaussian pulse of a transmitter with the given BT.
fn gaussian_shape(levels: &[f64], bt: f64, samples_per_symbol: f64) -> Vec<f64> {
    let half_span = (2.0 * samples_per_symbol).ceil() as isize;
    let mut taps = (-half_span..=half_span)
        .map(|offset| (-2.0 * (PI * bt * offset as f64 / samples_per_symbol).powi(2) / LN_2).exp())
        .collect::<Vec<_>>();
    let sum = taps.iter().sum::<f64>();
    taps.iter_mut().for_each(|tap| *tap /= sum);
    (0..levels.len())
        .map(|index| {
            taps.iter()
                .enumerate()
                .map(|(tap, weight)| {
                    let source = (index as isize + tap as isize - half_span)
                        .clamp(0, levels.len() as isize - 1)
                        as usize;
                    levels[source] * weight
                })
                .sum()
        })
        .collect()
}

fn decode(analyzer: &mut SignalAnalyzer, link: Link, burst: &Burst) -> Vec<bool> {
    let output = analyzer
        .push(&IqBatch {
            sample_rate_hz: link.sample_rate_hz,
            center_frequency_hz: 868_300_000,
            first_sample_index: 0,
            discontinuity: true,
            samples: burst.iq.clone(),
        })
        .unwrap();
    let DemodulatedBatch::SoftSymbols(symbols) = output else {
        panic!("binary FSK/GFSK must emit soft symbols");
    };
    symbols
        .symbols
        .iter()
        .map(|symbol| symbol.decision() == Some(1))
        .collect()
}

/// Alignment of `decoded` against `truth` with the fewest bit errors, as
/// `(lag, errors, compared bits)` where `decoded[i + lag]` corresponds to
/// `truth[i]`.
fn best_alignment(truth: &[bool], decoded: &[bool], lead_symbols: usize) -> (isize, usize, usize) {
    let mut best: Option<(isize, usize, usize)> = None;
    for lag in -64_isize..=(64 + lead_symbols as isize) {
        let (mut errors, mut compared) = (0, 0);
        for (index, expected) in truth.iter().enumerate().skip(SETTLING_BITS) {
            let Some(actual) = usize::try_from(index as isize + lag)
                .ok()
                .and_then(|decoded_index| decoded.get(decoded_index))
            else {
                continue;
            };
            compared += 1;
            errors += usize::from(actual != expected);
        }
        let better = best.map_or(true, |(_, best_errors, best_compared)| {
            compared > 0 && errors * best_compared.max(1) < best_errors * compared
        });
        if better {
            best = Some((lag, errors, compared));
        }
    }
    best.unwrap()
}

/// Bit errors and compared bits over the best fixed alignment of `decoded`
/// against `truth`. A symbol slip changes the alignment part-way through, so
/// the bits after it count as errors; that is the failure being guarded.
fn bit_errors(truth: &[bool], decoded: &[bool], lead_symbols: usize) -> (usize, usize) {
    let (_, errors, compared) = best_alignment(truth, decoded, lead_symbols);
    (errors, compared)
}

/// Bit error rate over `bursts` bursts, plus how many bursts decoded at all.
fn error_rate(
    link: Link,
    timing: FskTiming,
    eb_n0_db: f64,
    lead_symbols: usize,
    bursts: usize,
    payload: usize,
    seed: u64,
) -> (f64, usize) {
    let mut rng = Rng(seed);
    let (mut errors, mut compared, mut decoded_bursts) = (0, 0, 0);
    for _ in 0..bursts {
        let burst = burst(link, &mut rng, eb_n0_db, lead_symbols, payload);
        let decoded = decode(&mut link.analyzer(timing), link, &burst);
        if decoded.len() * 2 < burst.bits.len() {
            continue;
        }
        decoded_bursts += 1;
        let (burst_errors, burst_compared) = bit_errors(&burst.bits, &decoded, lead_symbols);
        errors += burst_errors;
        compared += burst_compared;
    }
    (errors as f64 / compared.max(1) as f64, decoded_bursts)
}

#[test]
fn adaptive_gfsk_decodes_noisy_bursts_whatever_noise_precedes_them() {
    // Measured: about 4e-4 at every lead-in. Without the post-discriminator
    // receive filter the clock loop slips and the rate is about 1.7e-1; a
    // receiver that acquires tones on the lead-in noise also fails here.
    for lead_symbols in [0, 32, 128] {
        let (rate, decoded) = error_rate(
            Link::fast_gfsk(),
            adaptive_timing(),
            12.0,
            lead_symbols,
            12,
            1_000,
            0x5eed_0001,
        );
        assert_eq!(decoded, 12, "every burst must decode (lead {lead_symbols})");
        assert!(
            rate < 4e-3,
            "bit error rate {rate:.2e} with {lead_symbols} symbols of lead-in noise"
        );
    }
}

#[test]
fn adaptive_fsk_decodes_noisy_bursts_whatever_noise_precedes_them() {
    // Plain FSK has no Gaussian pulse, but its discriminator output is just
    // as noisy. Measured: about 1e-3, equal to ideal fixed timing; without the
    // one-symbol receive filter the rate is about 1.3e-1.
    for lead_symbols in [0, 64] {
        let (rate, decoded) = error_rate(
            Link::fast_fsk(),
            adaptive_timing(),
            12.0,
            lead_symbols,
            12,
            1_000,
            0x5eed_0006,
        );
        assert_eq!(decoded, 12, "every burst must decode (lead {lead_symbols})");
        assert!(
            rate < 8e-3,
            "bit error rate {rate:.2e} with {lead_symbols} symbols of lead-in noise"
        );
    }
}

#[test]
fn adaptive_gfsk_decodes_a_strong_burst_without_errors() {
    // At high SNR any remaining error is a decoder defect, not channel noise.
    // Measured: no errors, with or without lead-in noise.
    for lead_symbols in [0, 64] {
        let (rate, decoded) = error_rate(
            Link::fast_gfsk(),
            adaptive_timing(),
            20.0,
            lead_symbols,
            12,
            1_000,
            0x5eed_0002,
        );
        assert_eq!(decoded, 12);
        assert!(
            rate < 1e-3,
            "bit error rate {rate:.2e} with {lead_symbols} symbols of lead-in noise"
        );
    }
}

#[test]
fn adaptive_gfsk_decodes_with_a_channel_much_wider_than_the_signal() {
    // The GUI's default link: the channel filter passes about twice the
    // occupied bandwidth, so the raw discriminator is noisy. Measured: about
    // 2e-5 or better; without the receive filter 2.7e-2 to 7e-2.
    let (rate, decoded) = error_rate(
        Link::default_gfsk(),
        adaptive_timing(),
        20.0,
        16,
        8,
        500,
        0x5eed_0003,
    );
    assert_eq!(decoded, 8, "every burst must decode");
    assert!(rate < 5e-3, "bit error rate {rate:.2e}");
}

#[test]
fn adaptive_gfsk_acquires_a_weak_signal_in_a_wide_channel() {
    // At 12 dB the unfiltered discriminator spread of this link is too wide
    // to accept as a signal, so acquisition used to fail for most bursts.
    // The bound is loose because the channel noise itself costs errors.
    let (rate, decoded) = error_rate(
        Link::default_gfsk(),
        adaptive_timing(),
        12.0,
        16,
        8,
        500,
        0x5eed_0004,
    );
    assert_eq!(decoded, 8, "every burst must decode");
    assert!(rate < 0.15, "bit error rate {rate:.2e}");
}

#[test]
fn noise_without_a_burst_does_not_decode_as_tones() {
    // A noise-only stream must not be taken for a signal: the tone tracker
    // would then normalise later bursts by the noise statistics.
    let link = Link::fast_gfsk();
    let mut rng = Rng(0x5eed_0005);
    let mut analyzer = link.analyzer(adaptive_timing());
    let noise = burst(link, &mut rng, 12.0, 512, 0);
    let silent = Burst {
        bits: Vec::new(),
        iq: noise.iq[..(512.0 * link.samples_per_symbol()) as usize].to_vec(),
    };
    let decoded = decode(&mut analyzer, link, &silent);
    assert!(
        decoded.is_empty(),
        "decoded {} symbols from noise",
        decoded.len()
    );
}

/// Symbols from the start of the burst to the first decoded symbol.
fn first_symbol_start(analyzer: &mut SignalAnalyzer, link: Link, burst: &Burst) -> f64 {
    let output = analyzer
        .push(&IqBatch {
            sample_rate_hz: link.sample_rate_hz,
            center_frequency_hz: 868_300_000,
            first_sample_index: 0,
            discontinuity: true,
            samples: burst.iq.clone(),
        })
        .unwrap();
    let DemodulatedBatch::SoftSymbols(symbols) = output else {
        panic!("binary FSK/GFSK must emit soft symbols");
    };
    symbols.symbols.first().map_or(f64::INFINITY, |symbol| {
        symbol.source_range.start as f64 / link.samples_per_symbol()
    })
}

#[test]
fn adaptive_gfsk_acquires_its_tones_from_the_first_preamble_symbols() {
    // At 5 samples per symbol the tone centres come from a window of a few
    // dozen samples and jitter with the noise. Requiring consecutive windows
    // to agree within 10% restarted acquisition so often that 22 of 100
    // bursts at 10 dB lost one or more preamble symbols and 8 lost eight or
    // more; at 20% only 2 lost one.
    let link = Link::fast_gfsk();
    let mut rng = Rng(0x5eed_00a1);
    let on_time = (0..60)
        .filter(|_| {
            let burst = burst(link, &mut rng, 10.0, 0, 100);
            first_symbol_start(&mut link.analyzer(adaptive_timing()), link, &burst) < 1.5
        })
        .count();
    assert!(
        on_time >= 55,
        "only {on_time} of 60 bursts decoded from the first symbols"
    );
}

/// Whether the last 24 bits of the preamble were decoded with at most one
/// error, at the alignment the rest of the burst has. A preamble detector
/// needs them intact; the first 8 settle the clock loop.
fn preamble_intact(burst: &Burst, decoded: &[bool], lead_symbols: usize) -> bool {
    let (lag, _, _) = best_alignment(&burst.bits, decoded, lead_symbols);
    let errors = (PREAMBLE_BITS - 24..PREAMBLE_BITS)
        .filter(|&index| {
            usize::try_from(index as isize + lag)
                .ok()
                .and_then(|decoded_index| decoded.get(decoded_index))
                .map_or(true, |&actual| actual != burst.bits[index])
        })
        .count();
    errors <= 1
}

#[test]
fn adaptive_gfsk_keeps_the_preamble_after_a_long_noise_lead_in() {
    // The burst's first chunk usually starts in noise: a 2048-sample chunk is
    // about 25 symbols of a 100 kbit/s link at 8 MS/s. Starting the clock loop
    // on a symbol grid chosen from that noise as well left the alternating
    // preamble (on which Mueller-and-Muller sees no timing error) sampled at
    // the wrong phase: 53 of 80 bursts kept it at a 128-symbol lead-in, 71 at
    // 32. Taking the phase from the last two acquisition windows keeps all 80.
    let link = Link::fast_gfsk();
    for lead_symbols in [32, 128] {
        let mut rng = Rng(0x5eed_00b1);
        let intact = (0..40)
            .filter(|_| {
                let burst = burst(link, &mut rng, 12.0, lead_symbols, 200);
                let decoded = decode(&mut link.analyzer(adaptive_timing()), link, &burst);
                preamble_intact(&burst, &decoded, lead_symbols)
            })
            .count();
        assert!(
            intact >= 38,
            "only {intact} of 40 preambles intact after {lead_symbols} symbols of noise"
        );
    }
}
