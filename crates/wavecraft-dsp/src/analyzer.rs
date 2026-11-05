use crate::{
    Channelizer, ChannelizerConfig, DemodulatedBatch, Demodulator, DemodulatorRegistry,
    DemodulatorTelemetry, DspError, FskConfig, FskPolarity, FskTiming, BINARY_FSK_DEMODULATOR_ID,
    BINARY_GFSK_DEMODULATOR_ID,
};
use wavecraft_core::IqBatch;

#[derive(Clone, Debug, PartialEq)]
pub struct BinaryFskProfile {
    pub symbol_rate_hz: u64,
    pub deviation_hz: u64,
    pub polarity: FskPolarity,
    pub timing: FskTiming,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BinaryGfskProfile {
    pub symbol_rate_hz: u64,
    pub deviation_hz: u64,
    pub bt: f64,
    pub polarity: FskPolarity,
    pub timing: FskTiming,
}

/// Built-in, modulation-tagged analyzer configuration.
///
/// Every variant owns only parameters that apply to that modulation. New
/// modulation families add variants without widening unrelated profiles.
#[derive(Clone, Debug, PartialEq)]
pub enum DemodulationProfile {
    BinaryFsk(BinaryFskProfile),
    BinaryGfsk(BinaryGfskProfile),
}

impl DemodulationProfile {
    pub fn demodulator_id(&self) -> &'static str {
        match self {
            Self::BinaryFsk(_) => BINARY_FSK_DEMODULATOR_ID,
            Self::BinaryGfsk(_) => BINARY_GFSK_DEMODULATOR_ID,
        }
    }

    pub fn validate(&self) -> Result<(), DspError> {
        self.fsk_config().validate()
    }

    fn fsk_config(&self) -> FskConfig {
        match self {
            Self::BinaryFsk(profile) => FskConfig {
                symbol_rate_hz: profile.symbol_rate_hz,
                deviation_hz: profile.deviation_hz,
                modulation: crate::FskModulation::Fsk,
                polarity: profile.polarity,
                timing: profile.timing,
            },
            Self::BinaryGfsk(profile) => FskConfig {
                symbol_rate_hz: profile.symbol_rate_hz,
                deviation_hz: profile.deviation_hz,
                modulation: crate::FskModulation::Gfsk { bt: profile.bt },
                polarity: profile.polarity,
                timing: profile.timing,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignalAnalyzerProfile {
    pub channel: ChannelizerConfig,
    pub demodulation: DemodulationProfile,
}

impl SignalAnalyzerProfile {
    pub fn validate(&self) -> Result<(), DspError> {
        self.channel.validate()?;
        self.demodulation.validate()
    }
}

/// Stateful channelization and demodulation pipeline built from one profile.
pub struct SignalAnalyzer {
    profile: SignalAnalyzerProfile,
    channelizer: Channelizer,
    demodulator: Box<dyn Demodulator>,
}

impl SignalAnalyzer {
    pub fn new(
        profile: SignalAnalyzerProfile,
        registry: &DemodulatorRegistry,
    ) -> Result<Self, DspError> {
        profile.validate()?;
        let channelizer = Channelizer::new(profile.channel.clone())?;
        let demodulator_config = profile.demodulation.fsk_config();
        let demodulator =
            registry.create(profile.demodulation.demodulator_id(), &demodulator_config)?;
        Ok(Self {
            profile,
            channelizer,
            demodulator,
        })
    }

    pub fn profile(&self) -> &SignalAnalyzerProfile {
        &self.profile
    }

    pub fn reset(&mut self) {
        self.channelizer.reset();
        self.demodulator.reset();
    }

    pub fn push(&mut self, input: &IqBatch) -> Result<DemodulatedBatch, DspError> {
        let channelized = self.channelizer.push(input)?;
        self.demodulator.push(&channelized)
    }

    pub fn telemetry(&self) -> DemodulatorTelemetry {
        self.demodulator.telemetry()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TimedSoftSymbol, TimedSoftSymbolBatch};
    use std::f64::consts::TAU;
    use wavecraft_core::{Iq8, IqBatch};

    const SOURCE_SAMPLE_RATE_HZ: u64 = 64_000;
    const SAMPLES_PER_SYMBOL: usize = 64;

    fn profile(demodulation: DemodulationProfile) -> SignalAnalyzerProfile {
        SignalAnalyzerProfile {
            channel: ChannelizerConfig {
                frequency_offset_hz: 10_000,
                bandwidth_hz: 4_000,
                output_sample_rate_hz: 8_000,
                filter_tap_count: 65,
            },
            demodulation,
        }
    }

    fn fsk_profile() -> DemodulationProfile {
        DemodulationProfile::BinaryFsk(BinaryFskProfile {
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            polarity: FskPolarity::Normal,
            timing: FskTiming::Fixed,
        })
    }

    fn modulated_iq(bits: &[bool]) -> Vec<Iq8> {
        let mut phase: f64 = 0.0;
        let mut samples = Vec::with_capacity(bits.len() * SAMPLES_PER_SYMBOL);
        for bit in bits {
            let deviation_hz = if *bit { 1_000.0 } else { -1_000.0 };
            for _ in 0..SAMPLES_PER_SYMBOL {
                samples.push(Iq8 {
                    i: (100.0 * phase.cos()).round() as i8,
                    q: (100.0 * phase.sin()).round() as i8,
                });
                phase += TAU * (10_000.0 + deviation_hz) / SOURCE_SAMPLE_RATE_HZ as f64;
            }
        }
        samples
    }

    fn soft_symbols(output: DemodulatedBatch) -> TimedSoftSymbolBatch {
        let DemodulatedBatch::SoftSymbols(symbols) = output else {
            panic!("binary FSK profile must emit soft symbols");
        };
        symbols
    }

    #[test]
    fn tagged_profiles_select_distinct_stable_demodulator_ids() {
        assert_eq!(fsk_profile().demodulator_id(), "fsk2");
        let gfsk = DemodulationProfile::BinaryGfsk(BinaryGfskProfile {
            symbol_rate_hz: 100_000,
            deviation_hz: 50_000,
            bt: 0.5,
            polarity: FskPolarity::Normal,
            timing: FskTiming::Adaptive {
                loop_bandwidth: 0.1,
                max_clock_error_ppm: 10_000,
            },
        });
        assert_eq!(gfsk.demodulator_id(), "gfsk2");
        assert!(gfsk.validate().is_ok());

        let mut invalid_gfsk = gfsk;
        let DemodulationProfile::BinaryGfsk(profile) = &mut invalid_gfsk else {
            unreachable!();
        };
        profile.bt = 0.0;
        assert!(invalid_gfsk.validate().is_err());
    }

    #[test]
    fn profile_builds_a_chunk_independent_end_to_end_analyzer() {
        let transmitted = [true, false, true, true, false, false, true];
        let samples = modulated_iq(&transmitted);
        let registry = DemodulatorRegistry::with_builtins().unwrap();

        let mut whole = SignalAnalyzer::new(profile(fsk_profile()), &registry).unwrap();
        let whole_symbols = soft_symbols(
            whole
                .push(&IqBatch {
                    sample_rate_hz: SOURCE_SAMPLE_RATE_HZ,
                    center_frequency_hz: 100_000_000,
                    first_sample_index: 0,
                    discontinuity: false,
                    samples: samples.clone(),
                })
                .unwrap(),
        )
        .symbols;

        let mut chunked = SignalAnalyzer::new(profile(fsk_profile()), &registry).unwrap();
        let mut first_sample_index = 0;
        let mut chunked_symbols = Vec::new();
        for length in [37, 91, 113, 79, 128] {
            let end = first_sample_index + length;
            chunked_symbols.extend(
                soft_symbols(
                    chunked
                        .push(&IqBatch {
                            sample_rate_hz: SOURCE_SAMPLE_RATE_HZ,
                            center_frequency_hz: 100_000_000,
                            first_sample_index: first_sample_index as u64,
                            discontinuity: false,
                            samples: samples[first_sample_index..end].to_vec(),
                        })
                        .unwrap(),
                )
                .symbols,
            );
            first_sample_index = end;
        }

        assert_eq!(first_sample_index, samples.len());
        assert_eq!(chunked_symbols, whole_symbols);
        assert_eq!(
            whole_symbols
                .iter()
                .map(TimedSoftSymbol::decision)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(1), Some(0), Some(0)]
        );
    }
}
