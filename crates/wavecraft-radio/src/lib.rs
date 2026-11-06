//! Device-independent radio demodulation and raw-frame acquisition.

use std::sync::Arc;
use wavecraft_core::IqBatch;
use wavecraft_decode::{
    RadioPacketPipeline, RadioPacketPipelineConfig, RadioPacketPipelineOutput, RawRadioConfig,
    RawRadioDecoder, RawRadioOutput, SampleRange, TimedBit, TimedBitBatch,
};
use wavecraft_dsp::{
    DemodulatedBatch, DemodulatorRegistry, PreparedIqBatch, SignalAnalyzer, SignalAnalyzerProfile,
    SpectrumFrame, SpectrumProcessor, TimedSoftSymbolBatch, WidebandIqPreprocessor,
};

const SQUELCH_RELEASE_FRAMES: usize = 3;
const AUTOMATIC_SQUELCH_MARGIN_DB: f32 = 8.0;
const NOISE_FLOOR_RISE_GAIN: f32 = 0.005;
const NOISE_FLOOR_FALL_GAIN: f32 = 0.2;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RadioAnalyzerTelemetry {
    pub burst_active: bool,
    pub channel_power_dbfs: Option<f32>,
    pub noise_floor_dbfs: Option<f32>,
    pub effective_squelch_dbfs: Option<f32>,
    pub timing_locked: bool,
    pub timing_lock_quality: f32,
    pub estimated_symbol_rate_hz: Option<f64>,
    pub estimated_frequency_offset_hz: Option<f64>,
    pub estimated_tone_separation_hz: Option<f64>,
}

pub struct RadioAnalyzerOutput {
    pub channel_frequency_offset_hz: i64,
    pub channel_name: Arc<str>,
    pub demodulated: Option<DemodulatedBatch>,
    pub raw: RawRadioOutput,
    pub packets: RadioPacketPipelineOutput,
    pub burst_ended: bool,
    pub telemetry: RadioAnalyzerTelemetry,
}

pub struct RadioDspOutput {
    pub spectrum: Vec<SpectrumFrame>,
    pub analyses: Vec<Result<RadioAnalyzerOutput, String>>,
}

/// Independent channel analyzers sharing the wideband IQ conversion and DC
/// tracker that precede per-channel frequency translation and filtering.
pub struct RadioAnalyzerBank {
    analyzers: Vec<RadioAnalyzerRuntime>,
    preprocessor: WidebandIqPreprocessor,
}

impl RadioAnalyzerBank {
    pub fn new(analyzers: Vec<RadioAnalyzerRuntime>) -> Self {
        Self {
            analyzers,
            preprocessor: WidebandIqPreprocessor::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.analyzers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.analyzers.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RadioDataDecoderConfig {
    Raw(RawRadioConfig),
    Packets(RadioPacketPipelineConfig),
}

enum RadioDataDecoder {
    Raw(RawRadioDecoder),
    Packets(Box<RadioPacketPipeline>),
}

impl RadioDataDecoder {
    fn new(config: RadioDataDecoderConfig) -> Result<Self, String> {
        match config {
            RadioDataDecoderConfig::Raw(config) => RawRadioDecoder::new(config)
                .map(Self::Raw)
                .map_err(|error| error.to_string()),
            RadioDataDecoderConfig::Packets(config) => RadioPacketPipeline::new(config)
                .map(Box::new)
                .map(Self::Packets)
                .map_err(|error| error.to_string()),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Raw(decoder) => decoder.reset(),
            Self::Packets(decoder) => decoder.reset(),
        }
    }

    fn push_demodulated(
        &mut self,
        demodulated: &DemodulatedBatch,
    ) -> Result<(RawRadioOutput, RadioPacketPipelineOutput), String> {
        match demodulated {
            DemodulatedBatch::SoftSymbols(symbols) => {
                let bits = soft_symbols_to_bits(symbols)?;
                self.push_bits(&bits)
            }
            DemodulatedBatch::SparseSymbols(_) => Err(
                "the selected radio data decoder does not accept sparse-symbol output".to_owned(),
            ),
            DemodulatedBatch::Analog(_) => Err(
                "the selected radio data decoder does not accept analog demodulator output"
                    .to_owned(),
            ),
        }
    }

    fn push_bits(
        &mut self,
        bits: &TimedBitBatch,
    ) -> Result<(RawRadioOutput, RadioPacketPipelineOutput), String> {
        match self {
            Self::Raw(decoder) => decoder
                .push(bits)
                .map(|output| (output, RadioPacketPipelineOutput::default()))
                .map_err(|error| error.to_string()),
            Self::Packets(decoder) => decoder
                .push(bits)
                .map(|output| (RawRadioOutput::default(), output))
                .map_err(|error| error.to_string()),
        }
    }

    fn finish(&mut self) -> (RawRadioOutput, RadioPacketPipelineOutput) {
        match self {
            Self::Raw(decoder) => (decoder.finish(), RadioPacketPipelineOutput::default()),
            Self::Packets(decoder) => (RawRadioOutput::default(), decoder.finish()),
        }
    }
}

/// Device-independent composition of channelization, FSK/GFSK demodulation,
/// binary symbol decisions, and raw or packet radio decoding.
pub struct RadioAnalyzerRuntime {
    signal: SignalAnalyzer,
    decoder: RadioDataDecoder,
    channel_frequency_offset_hz: i64,
    channel_name: Arc<str>,
    channel_bandwidth_hz: u64,
    burst_active: bool,
    quiet_spectrum_frames: usize,
    last_channel_power_dbfs: Option<f32>,
    noise_floor_dbfs: Option<f32>,
    effective_squelch_dbfs: Option<f32>,
}

impl RadioAnalyzerRuntime {
    pub fn new(
        profile: SignalAnalyzerProfile,
        decoder_config: RadioDataDecoderConfig,
    ) -> Result<Self, String> {
        let channel_name = format!("Channel {:+} Hz", profile.channel.frequency_offset_hz);
        Self::new_named(profile, decoder_config, channel_name)
    }

    pub fn new_named(
        profile: SignalAnalyzerProfile,
        decoder_config: RadioDataDecoderConfig,
        channel_name: impl Into<Arc<str>>,
    ) -> Result<Self, String> {
        let channel_frequency_offset_hz = profile.channel.frequency_offset_hz;
        let channel_bandwidth_hz = profile.channel.bandwidth_hz;
        let registry = DemodulatorRegistry::with_builtins()
            .map_err(|error| format!("failed to initialize demodulators: {error}"))?;
        let signal = SignalAnalyzer::new(profile, &registry)
            .map_err(|error| format!("invalid radio analyzer profile: {error}"))?;
        let decoder = RadioDataDecoder::new(decoder_config)?;
        Ok(Self {
            signal,
            decoder,
            channel_frequency_offset_hz,
            channel_name: channel_name.into(),
            channel_bandwidth_hz,
            burst_active: false,
            quiet_spectrum_frames: 0,
            last_channel_power_dbfs: None,
            noise_floor_dbfs: None,
            effective_squelch_dbfs: None,
        })
    }

    pub fn push(
        &mut self,
        input: &IqBatch,
        spectrum: &[SpectrumFrame],
        squelch_threshold_dbfs: f32,
    ) -> Result<RadioAnalyzerOutput, String> {
        if let Some(output) = self.begin_push(spectrum, squelch_threshold_dbfs) {
            return Ok(output);
        }
        let demodulated = self.signal.push(input).map_err(|error| error.to_string())?;
        self.finish_push(demodulated)
    }

    fn push_prepared(&mut self, input: &PreparedIqBatch) -> Result<RadioAnalyzerOutput, String> {
        let demodulated = self
            .signal
            .push_prepared(input)
            .map_err(|error| error.to_string())?;
        self.finish_push(demodulated)
    }

    fn begin_push(
        &mut self,
        spectrum: &[SpectrumFrame],
        squelch_threshold_dbfs: f32,
    ) -> Option<RadioAnalyzerOutput> {
        let powers = spectrum
            .iter()
            .filter_map(|frame| {
                channel_power_dbfs(
                    frame,
                    self.channel_frequency_offset_hz,
                    self.channel_bandwidth_hz,
                )
            })
            .collect::<Vec<_>>();
        self.last_channel_power_dbfs = powers.iter().copied().max_by(f32::total_cmp);
        if !self.burst_active && !powers.is_empty() {
            let mut sorted = powers.clone();
            sorted.sort_by(f32::total_cmp);
            let observation = sorted[sorted.len() / 5];
            self.noise_floor_dbfs = Some(self.noise_floor_dbfs.map_or(observation, |noise| {
                let gain = if observation < noise {
                    NOISE_FLOOR_FALL_GAIN
                } else {
                    NOISE_FLOOR_RISE_GAIN
                };
                noise + gain * (observation - noise)
            }));
        }
        let effective_squelch_dbfs = self
            .noise_floor_dbfs
            .map(|noise| squelch_threshold_dbfs.min(noise + AUTOMATIC_SQUELCH_MARGIN_DB))
            .unwrap_or(squelch_threshold_dbfs);
        self.effective_squelch_dbfs = Some(effective_squelch_dbfs);
        let signal_present = powers.iter().any(|power| *power >= effective_squelch_dbfs);
        if signal_present {
            if !self.burst_active {
                self.signal.reset();
                self.decoder.reset();
            }
            self.burst_active = true;
            self.quiet_spectrum_frames = 0;
        } else if self.burst_active && !powers.is_empty() {
            self.quiet_spectrum_frames = self.quiet_spectrum_frames.saturating_add(powers.len());
        }
        if self.burst_active && self.quiet_spectrum_frames >= SQUELCH_RELEASE_FRAMES {
            let (raw, packets) = self.decoder.finish();
            self.signal.reset();
            self.burst_active = false;
            self.quiet_spectrum_frames = 0;
            return Some(RadioAnalyzerOutput {
                channel_frequency_offset_hz: self.channel_frequency_offset_hz,
                channel_name: Arc::clone(&self.channel_name),
                demodulated: None,
                raw,
                packets,
                burst_ended: true,
                telemetry: self.telemetry(),
            });
        }
        if !self.burst_active {
            return Some(RadioAnalyzerOutput {
                channel_frequency_offset_hz: self.channel_frequency_offset_hz,
                channel_name: Arc::clone(&self.channel_name),
                demodulated: None,
                raw: RawRadioOutput::default(),
                packets: RadioPacketPipelineOutput::default(),
                burst_ended: false,
                telemetry: self.telemetry(),
            });
        }
        None
    }

    fn finish_push(
        &mut self,
        demodulated: DemodulatedBatch,
    ) -> Result<RadioAnalyzerOutput, String> {
        let (raw, packets) = self.decoder.push_demodulated(&demodulated)?;
        Ok(RadioAnalyzerOutput {
            channel_frequency_offset_hz: self.channel_frequency_offset_hz,
            channel_name: Arc::clone(&self.channel_name),
            demodulated: Some(demodulated),
            raw,
            packets,
            burst_ended: false,
            telemetry: self.telemetry(),
        })
    }

    fn telemetry(&self) -> RadioAnalyzerTelemetry {
        let demodulator = self.signal.telemetry();
        RadioAnalyzerTelemetry {
            burst_active: self.burst_active,
            channel_power_dbfs: self.last_channel_power_dbfs,
            noise_floor_dbfs: self.noise_floor_dbfs,
            effective_squelch_dbfs: self.effective_squelch_dbfs,
            timing_locked: demodulator.timing_locked,
            timing_lock_quality: demodulator.timing_lock_quality,
            estimated_symbol_rate_hz: demodulator.estimated_symbol_rate_hz,
            estimated_frequency_offset_hz: demodulator.estimated_frequency_offset_hz,
            estimated_tone_separation_hz: demodulator.estimated_tone_separation_hz,
        }
    }
}

/// Splits a capture batch at spectrum-window boundaries before burst detection.
pub fn process_radio_iq(
    spectrum: &mut SpectrumProcessor,
    analyzer_bank: &mut RadioAnalyzerBank,
    input: &IqBatch,
    squelch_threshold_dbfs: f32,
) -> RadioDspOutput {
    let chunk_samples = spectrum.fft_length();
    let mut output = RadioDspOutput {
        spectrum: Vec::new(),
        analyses: Vec::new(),
    };
    for (chunk_index, samples) in input.samples.chunks(chunk_samples).enumerate() {
        let first_sample_index = input
            .first_sample_index
            .saturating_add((chunk_index * chunk_samples) as u64);
        let chunk = IqBatch {
            sample_rate_hz: input.sample_rate_hz,
            center_frequency_hz: input.center_frequency_hz,
            first_sample_index,
            discontinuity: input.discontinuity && chunk_index == 0,
            samples: samples.to_vec(),
        };
        let frames = spectrum.push(&chunk);
        let preliminary = analyzer_bank
            .analyzers
            .iter_mut()
            .map(|analyzer| analyzer.begin_push(&frames, squelch_threshold_dbfs))
            .collect::<Vec<_>>();
        let prepared = preliminary.iter().any(Option::is_none).then(|| {
            analyzer_bank
                .preprocessor
                .push(&chunk)
                .map_err(|error| error.to_string())
        });
        let mut analyzer_index = 0;
        for preliminary in preliminary {
            let analysis = match preliminary {
                Some(output) => Ok(output),
                None => match prepared.as_ref() {
                    Some(Ok(prepared)) => {
                        analyzer_bank.analyzers[analyzer_index].push_prepared(prepared)
                    }
                    Some(Err(error)) => Err(error.clone()),
                    None => unreachable!("demodulation work requires prepared IQ"),
                },
            };
            let failed = analysis.is_err();
            output.analyses.push(analysis);
            if failed {
                analyzer_bank.analyzers.remove(analyzer_index);
            } else {
                analyzer_index += 1;
            }
        }
        if let Some(Ok(prepared)) = prepared {
            analyzer_bank.preprocessor.recycle(prepared);
        }
        output.spectrum.extend(frames);
    }
    output
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
    let lower_hz = channel_center_hz - channel_half_width_hz - bin_half_width_hz;
    let upper_hz = channel_center_hz + channel_half_width_hz + bin_half_width_hz;
    let first_bin = ((lower_hz - first_bin_hz) / frame.bin_width_hz).ceil() as isize;
    let last_bin = ((upper_hz - first_bin_hz) / frame.bin_width_hz).floor() as isize;
    let start = first_bin.clamp(0, frame.power_dbfs.len() as isize) as usize;
    let end = last_bin
        .saturating_add(1)
        .clamp(0, frame.power_dbfs.len() as isize) as usize;
    if start >= end {
        return None;
    }
    frame.power_dbfs[start..end]
        .iter()
        .copied()
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
    use std::f32::consts::TAU;
    use wavecraft_core::Iq8;
    use wavecraft_decode::{
        BitOrder, BitTransformConfig, RadioPacketFramerConfig, RadioPacketLength, SyncWordConfig,
    };
    use wavecraft_dsp::{
        BinaryFskProfile, ChannelizerConfig, DemodulationProfile, FskPolarity, FskTiming,
        SpectrumWindow, TimedSoftSymbol, TimedSparseSymbolBatch,
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

    fn raw_config() -> RawRadioConfig {
        RawRadioConfig {
            preamble_byte: 0xaa,
            preamble_bytes: 1,
            sync_word: 0,
            sync_word_bytes: 0,
            maximum_preamble_bit_errors: 0,
            bit_order: BitOrder::MostSignificantFirst,
            minimum_bit_confidence_milli: 500,
            maximum_uncertain_bits: 0,
        }
    }

    #[test]
    fn packet_decoder_mode_emits_completed_packets_separately_from_raw_data() {
        let config = RadioPacketPipelineConfig {
            bit_transform: BitTransformConfig::default(),
            framing: RadioPacketFramerConfig {
                preamble: Vec::new(),
                sync: SyncWordConfig {
                    bits: vec![true, false, true, false],
                    max_bit_errors: 0,
                },
                whitening: None,
                bit_order: BitOrder::MostSignificantFirst,
                length: RadioPacketLength::Fixed { packet_bytes: 1 },
                max_search_bits: 32,
            },
            crc: None,
        };
        let values = [
            true, false, true, false, true, false, true, false, false, true, false, true,
        ];
        let bits = TimedBitBatch {
            discontinuity: false,
            bits: values
                .into_iter()
                .enumerate()
                .map(|(index, value)| TimedBit {
                    value,
                    confidence: 0.9,
                    range: SampleRange {
                        start: index as u64,
                        end: index as u64 + 1,
                    },
                })
                .collect(),
        };
        let mut decoder = RadioDataDecoder::new(RadioDataDecoderConfig::Packets(config)).unwrap();

        let (raw, packets) = decoder.push_bits(&bits).unwrap();

        assert!(raw.bytes.is_empty());
        assert_eq!(packets.packets.len(), 1);
        assert_eq!(packets.packets[0].bytes[0].value, 0xA5);
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
    fn binary_data_decoders_reject_sparse_symbol_output_at_the_typed_boundary() {
        let mut decoder = RadioDataDecoder::new(RadioDataDecoderConfig::Raw(raw_config())).unwrap();
        let output = DemodulatedBatch::SparseSymbols(TimedSparseSymbolBatch {
            alphabet_size: 4_096,
            discontinuity: false,
            symbols: Vec::new(),
        });

        let error = decoder.push_demodulated(&output).unwrap_err();

        assert!(error.contains("does not accept sparse-symbol output"));
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
        assert_eq!(channel_power_dbfs(&frame, 20_000, 500), None);
    }

    #[test]
    fn dsp_processing_starts_analyzer_at_the_signal_fft_window() {
        let fft_length = 256;
        let mut samples = vec![Iq8 { i: 0, q: 0 }; fft_length];
        samples.extend((0..fft_length).map(|index| {
            let phase = TAU * 1_000.0 * index as f32 / 8_000.0;
            Iq8 {
                i: (phase.cos() * 64.0).round() as i8,
                q: (phase.sin() * 64.0).round() as i8,
            }
        }));
        let input = IqBatch {
            sample_rate_hz: 8_000,
            center_frequency_hz: 100_000_000,
            first_sample_index: 0,
            discontinuity: true,
            samples,
        };
        let mut spectrum = SpectrumProcessor::new(fft_length, SpectrumWindow::Hann).unwrap();
        let mut second_profile = profile();
        second_profile.channel.frequency_offset_hz = -1_000;
        let analyzers = vec![
            RadioAnalyzerRuntime::new_named(
                profile(),
                RadioDataDecoderConfig::Raw(raw_config()),
                "Primary",
            )
            .unwrap(),
            RadioAnalyzerRuntime::new(second_profile, RadioDataDecoderConfig::Raw(raw_config()))
                .unwrap(),
        ];
        let mut analyzers = RadioAnalyzerBank::new(analyzers);

        let output = process_radio_iq(&mut spectrum, &mut analyzers, &input, -40.0);

        assert_eq!(output.spectrum.len(), 2);
        assert_eq!(output.analyses.len(), 4);
        assert!(output.analyses[0].as_ref().unwrap().demodulated.is_none());
        assert!(output.analyses[1].as_ref().unwrap().demodulated.is_none());
        assert!(output.analyses[2].as_ref().unwrap().demodulated.is_some());
        assert_eq!(
            &*output.analyses[2].as_ref().unwrap().channel_name,
            "Primary"
        );
        assert_eq!(
            output
                .analyses
                .iter()
                .map(|analysis| analysis.as_ref().unwrap().channel_frequency_offset_hz)
                .collect::<Vec<_>>(),
            [1_000, -1_000, 1_000, -1_000]
        );
    }

    #[test]
    fn closes_a_synchronized_raw_message_below_the_channel_threshold() {
        let mut runtime =
            RadioAnalyzerRuntime::new(profile(), RadioDataDecoderConfig::Raw(raw_config()))
                .unwrap();
        let preamble = [true, false, true, false, true, false, true, false];
        let RadioDataDecoder::Raw(raw) = &mut runtime.decoder else {
            panic!("runtime should use raw decoding");
        };
        raw.push(&TimedBitBatch {
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
        let RadioDataDecoder::Raw(raw) = &runtime.decoder else {
            panic!("runtime should use raw decoding");
        };
        assert!(raw.is_synchronized());
        runtime.burst_active = true;
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
                &[
                    quiet_spectrum.clone(),
                    quiet_spectrum.clone(),
                    quiet_spectrum,
                ],
                -50.0,
            )
            .unwrap();

        assert!(output.burst_ended);
        assert!(output.demodulated.is_none());
        let RadioDataDecoder::Raw(raw) = &runtime.decoder else {
            panic!("runtime should use raw decoding");
        };
        assert!(!raw.is_synchronized());
    }
}
