use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use wavecraft_core::IqBatch;
use wavecraft_decode::{
    RawRadioConfig, RawRadioDecoder, RawRadioOutput, SampleRange, TimedBit, TimedBitBatch,
};
use wavecraft_dsp::{
    DemodulatedBatch, DemodulatorRegistry, SignalAnalyzer, SignalAnalyzerProfile, SpectrumFrame,
    SpectrumProcessor, TimedSoftSymbolBatch,
};

const SQUELCH_RELEASE_FRAMES: usize = 3;
const AUTOMATIC_SQUELCH_MARGIN_DB: f32 = 8.0;
const NOISE_FLOOR_RISE_GAIN: f32 = 0.005;
const NOISE_FLOOR_FALL_GAIN: f32 = 0.2;
const RADIO_DSP_INPUT_CAPACITY: usize = 8;

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
    pub demodulated: Option<DemodulatedBatch>,
    pub raw: RawRadioOutput,
    pub burst_ended: bool,
    pub telemetry: RadioAnalyzerTelemetry,
}

pub struct RadioDspOutput {
    pub spectrum: Vec<SpectrumFrame>,
    pub analysis: Option<Result<RadioAnalyzerOutput, String>>,
}

struct RadioDspInput {
    iq: IqBatch,
    squelch_threshold_dbfs: f32,
}

/// Runs spectrum generation, channelization, and demodulation away from the UI thread.
pub struct RadioDspWorker {
    sender: Option<SyncSender<RadioDspInput>>,
    receiver: Receiver<RadioDspOutput>,
    worker: Option<JoinHandle<()>>,
}

impl RadioDspWorker {
    pub fn start(
        mut spectrum: SpectrumProcessor,
        mut analyzer: Option<RadioAnalyzerRuntime>,
    ) -> Self {
        let (input_sender, input_receiver) =
            mpsc::sync_channel::<RadioDspInput>(RADIO_DSP_INPUT_CAPACITY);
        let (output_sender, output_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok(input) = input_receiver.recv() {
                let frames = spectrum.push(&input.iq);
                let analysis = analyzer
                    .as_mut()
                    .map(|runtime| runtime.push(&input.iq, &frames, input.squelch_threshold_dbfs));
                if analysis.as_ref().is_some_and(Result::is_err) {
                    analyzer = None;
                }
                if output_sender
                    .send(RadioDspOutput {
                        spectrum: frames,
                        analysis,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            sender: Some(input_sender),
            receiver: output_receiver,
            worker: Some(worker),
        }
    }

    pub fn push(&self, iq: IqBatch, squelch_threshold_dbfs: f32) -> Result<(), String> {
        self.sender
            .as_ref()
            .ok_or_else(|| "radio DSP worker is stopped".to_owned())?
            .send(RadioDspInput {
                iq,
                squelch_threshold_dbfs,
            })
            .map_err(|_| "radio DSP worker disconnected".to_owned())
    }

    pub fn try_recv(&self) -> Result<RadioDspOutput, TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn finish(&mut self) -> Result<Vec<RadioDspOutput>, String> {
        self.sender.take();
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            return Err("radio DSP worker panicked".to_owned());
        }
        Ok(self.receiver.try_iter().collect())
    }
}

impl Drop for RadioDspWorker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Composition of the configured signal analyzer and raw-byte acquisition.
pub struct RadioAnalyzerRuntime {
    signal: SignalAnalyzer,
    raw: RawRadioDecoder,
    channel_frequency_offset_hz: i64,
    channel_bandwidth_hz: u64,
    burst_active: bool,
    quiet_spectrum_frames: usize,
    last_channel_power_dbfs: Option<f32>,
    noise_floor_dbfs: Option<f32>,
    effective_squelch_dbfs: Option<f32>,
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
                self.raw.reset();
            }
            self.burst_active = true;
            self.quiet_spectrum_frames = 0;
        } else if self.burst_active && !powers.is_empty() {
            self.quiet_spectrum_frames = self.quiet_spectrum_frames.saturating_add(powers.len());
        }
        if self.burst_active && self.quiet_spectrum_frames >= SQUELCH_RELEASE_FRAMES {
            let raw = self.raw.finish();
            self.signal.reset();
            self.burst_active = false;
            self.quiet_spectrum_frames = 0;
            return Ok(RadioAnalyzerOutput {
                demodulated: None,
                raw,
                burst_ended: true,
                telemetry: self.telemetry(),
            });
        }
        if !self.burst_active {
            return Ok(RadioAnalyzerOutput {
                demodulated: None,
                raw: RawRadioOutput::default(),
                burst_ended: false,
                telemetry: self.telemetry(),
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
    fn radio_dsp_worker_processes_all_queued_iq_before_finishing() {
        let spectrum = SpectrumProcessor::new(8, wavecraft_dsp::SpectrumWindow::Hann).unwrap();
        let mut worker = RadioDspWorker::start(spectrum, None);
        for first_sample_index in [0, 8] {
            worker
                .push(
                    IqBatch {
                        sample_rate_hz: 8_000,
                        center_frequency_hz: 100_000_000,
                        first_sample_index,
                        discontinuity: first_sample_index == 0,
                        samples: vec![Iq8 { i: 64, q: 0 }; 8],
                    },
                    -50.0,
                )
                .unwrap();
        }

        let outputs = worker.finish().unwrap();

        assert_eq!(outputs.len(), 2);
        assert!(outputs.iter().all(|output| output.spectrum.len() == 1));
        assert!(outputs.iter().all(|output| output.analysis.is_none()));
        assert_eq!(outputs[0].spectrum[0].source_range, 0..8);
        assert_eq!(outputs[1].spectrum[0].source_range, 8..16);
    }

    #[test]
    fn closes_a_synchronized_raw_message_below_the_channel_threshold() {
        let raw_config = RawRadioConfig {
            preamble_byte: 0xaa,
            preamble_bytes: 1,
            sync_word: 0,
            sync_word_bytes: 0,
            maximum_preamble_bit_errors: 0,
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
        assert!(!runtime.raw.is_synchronized());
    }
}
