use wavecraft_core::IqBatch;
use wavecraft_decode::{
    RawRadioConfig, RawRadioDecoder, RawRadioOutput, SampleRange, TimedBit, TimedBitBatch,
};
use wavecraft_dsp::{
    DemodulatedBatch, DemodulatorRegistry, SignalAnalyzer, SignalAnalyzerProfile, SpectrumFrame,
    TimedSoftSymbolBatch,
};

pub struct RadioAnalyzerOutput {
    pub demodulated: Option<DemodulatedBatch>,
    pub raw: RawRadioOutput,
    pub burst_ended: bool,
}

/// Composition of the configured signal analyzer and raw-byte acquisition.
pub struct RadioAnalyzerRuntime {
    signal: SignalAnalyzer,
    raw: RawRadioDecoder,
    channel_frequency_offset_hz: i64,
    channel_bandwidth_hz: u64,
}

impl RadioAnalyzerRuntime {
    pub fn new(profile: SignalAnalyzerProfile, raw_config: RawRadioConfig) -> Result<Self, String> {
        let channel_frequency_offset_hz = profile.channel.frequency_offset_hz;
        let channel_bandwidth_hz = profile.channel.bandwidth_hz;
        let registry = DemodulatorRegistry::with_builtins()
            .map_err(|error| format!("failed to initialize demodulators: {error}"))?;
        let signal = SignalAnalyzer::new(profile, &registry)
            .map_err(|error| format!("invalid radio analyzer profile: {error}"))?;
        let raw = RawRadioDecoder::new(raw_config).map_err(|error| error.to_string())?;
        Ok(Self {
            signal,
            raw,
            channel_frequency_offset_hz,
            channel_bandwidth_hz,
        })
    }

    pub fn push(
        &mut self,
        input: &IqBatch,
        spectrum: &[SpectrumFrame],
        squelch_threshold_dbfs: f32,
    ) -> Result<RadioAnalyzerOutput, String> {
        if self.raw.is_synchronized()
            && !spectrum.is_empty()
            && !spectrum.iter().any(|frame| {
                channel_power_dbfs(
                    frame,
                    self.channel_frequency_offset_hz,
                    self.channel_bandwidth_hz,
                )
                .is_some_and(|power| power >= squelch_threshold_dbfs)
            })
        {
            let raw = self.raw.finish();
            self.signal.reset();
            return Ok(RadioAnalyzerOutput {
                demodulated: None,
                raw,
                burst_ended: true,
            });
        }
        let demodulated = self.signal.push(input).map_err(|error| error.to_string())?;
        let DemodulatedBatch::SoftSymbols(symbols) = &demodulated else {
            return Err("raw radio bytes require digital soft-symbol output".to_owned());
        };
        let bits = soft_symbols_to_bits(symbols)?;
        let raw = self.raw.push(&bits).map_err(|error| error.to_string())?;
        Ok(RadioAnalyzerOutput {
            demodulated: Some(demodulated),
            raw,
            burst_ended: false,
        })
    }
}

fn channel_power_dbfs(
    frame: &SpectrumFrame,
    frequency_offset_hz: i64,
    bandwidth_hz: u64,
) -> Option<f32> {
    if frame.power_dbfs.is_empty() || !frame.bin_width_hz.is_finite() || frame.bin_width_hz <= 0.0 {
        return None;
    }
    let span_hz = frame.bin_width_hz * frame.power_dbfs.len() as f64;
    let first_bin_hz = frame.center_frequency_hz as f64 - span_hz / 2.0;
    let channel_center_hz = frame.center_frequency_hz as f64 + frequency_offset_hz as f64;
    let channel_half_width_hz = bandwidth_hz as f64 / 2.0;
    let bin_half_width_hz = frame.bin_width_hz / 2.0;
    frame
        .power_dbfs
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, _)| {
            let bin_hz = first_bin_hz + *index as f64 * frame.bin_width_hz;
            (bin_hz - channel_center_hz).abs() <= channel_half_width_hz + bin_half_width_hz
        })
        .map(|(_, power)| power)
        .max_by(f32::total_cmp)
}

fn soft_symbols_to_bits(symbols: &TimedSoftSymbolBatch) -> Result<TimedBitBatch, String> {
    symbols.validate().map_err(|error| error.to_string())?;
    if symbols.alphabet_size != 2 {
        return Err(format!(
            "raw binary decoding requires a two-symbol alphabet, got {}",
            symbols.alphabet_size
        ));
    }
    Ok(TimedBitBatch {
        discontinuity: symbols.discontinuity,
        bits: symbols
            .symbols
            .iter()
            .map(|symbol| TimedBit {
                value: symbol.decision() == Some(1),
                confidence: symbol.confidence().unwrap_or(0.0),
                range: SampleRange {
                    start: symbol.source_range.start,
                    end: symbol.source_range.end,
                },
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_core::Iq8;
    use wavecraft_decode::BitOrder;
    use wavecraft_dsp::{
        BinaryFskProfile, ChannelizerConfig, DemodulationProfile, FskPolarity, FskTiming,
        TimedSoftSymbol,
    };

    fn profile() -> SignalAnalyzerProfile {
        SignalAnalyzerProfile {
            channel: ChannelizerConfig {
                frequency_offset_hz: 1_000,
                bandwidth_hz: 2_000,
                output_sample_rate_hz: 8_000,
                filter_tap_count: 65,
            },
            demodulation: DemodulationProfile::BinaryFsk(BinaryFskProfile {
                symbol_rate_hz: 1_000,
                deviation_hz: 1_000,
                polarity: FskPolarity::Normal,
                timing: FskTiming::Fixed,
            }),
        }
    }

    #[test]
    fn maps_binary_soft_symbols_to_timed_bits() {
        let bits = soft_symbols_to_bits(&TimedSoftSymbolBatch {
            alphabet_size: 2,
            discontinuity: true,
            symbols: vec![
                TimedSoftSymbol {
                    likelihoods: vec![0.9, 0.1],
                    source_range: 10..20,
                },
                TimedSoftSymbol {
                    likelihoods: vec![0.2, 0.8],
                    source_range: 20..30,
                },
            ],
        })
        .unwrap();

        assert!(bits.discontinuity);
        assert_eq!(
            bits.bits.iter().map(|bit| bit.value).collect::<Vec<_>>(),
            [false, true]
        );
        assert_eq!(bits.bits[1].range, SampleRange { start: 20, end: 30 });
        assert!((bits.bits[1].confidence - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn rejects_non_binary_soft_symbols() {
        let error = soft_symbols_to_bits(&TimedSoftSymbolBatch {
            alphabet_size: 4,
            discontinuity: false,
            symbols: vec![],
        })
        .unwrap_err();

        assert!(error.contains("two-symbol alphabet"));
    }

    #[test]
    fn channel_power_ignores_stronger_bins_outside_the_analyzer_band() {
        let mut power_dbfs = vec![-100.0; 8];
        power_dbfs[1] = -10.0;
        power_dbfs[5] = -42.0;
        let frame = SpectrumFrame {
            source_range: 0..8,
            center_frequency_hz: 100_000_000,
            bin_width_hz: 1_000.0,
            power_dbfs,
        };

        assert_eq!(channel_power_dbfs(&frame, 1_000, 500), Some(-42.0));
    }

    #[test]
    fn closes_a_synchronized_raw_message_below_the_channel_threshold() {
        let raw_config = RawRadioConfig {
            preamble_byte: 0xaa,
            preamble_bytes: 1,
            bit_order: BitOrder::MostSignificantFirst,
            minimum_bit_confidence_milli: 500,
            maximum_uncertain_bits: 0,
        };
        let mut runtime = RadioAnalyzerRuntime::new(profile(), raw_config).unwrap();
        let preamble = [true, false, true, false, true, false, true, false];
        runtime
            .raw
            .push(&TimedBitBatch {
                discontinuity: false,
                bits: preamble
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| TimedBit {
                        value,
                        confidence: 1.0,
                        range: SampleRange {
                            start: index as u64,
                            end: index as u64 + 1,
                        },
                    })
                    .collect(),
            })
            .unwrap();
        assert!(runtime.raw.is_synchronized());
        let quiet_spectrum = SpectrumFrame {
            source_range: 0..8,
            center_frequency_hz: 100_000_000,
            bin_width_hz: 1_000.0,
            power_dbfs: vec![-90.0; 8],
        };

        let output = runtime
            .push(
                &IqBatch {
                    sample_rate_hz: 64_000,
                    center_frequency_hz: 100_000_000,
                    first_sample_index: 8,
                    discontinuity: false,
                    samples: vec![Iq8 { i: 0, q: 0 }; 8],
                },
                &[quiet_spectrum],
                -50.0,
            )
            .unwrap();

        assert!(output.burst_ended);
        assert!(output.demodulated.is_none());
        assert!(!runtime.raw.is_synchronized());
    }
}
