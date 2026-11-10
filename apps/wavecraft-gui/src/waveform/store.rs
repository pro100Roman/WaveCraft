//! Bounded display buffers for captured logic, scope, and radio data.
//!
//! The store is a display cache. Capture workers and recording do not read from
//! it, so evicting old samples here never loses recorded data. The protocol
//! decode cache can still decode from its raw logic samples when no streamed
//! decode is available, which is why streamed decoding keeps its own history.

use crate::{
    process_radio_iq, Arc, CaptureBatch, CaptureFrame, CaptureTimeRange, CombinedTimeline,
    DsCopeCaptureConfig, DsLogicCaptureConfig, Duration, FileDecodedDataRecorder, Ordering,
    RadioAnalysisStore, RadioAnalyzerBank, RadioDisplayStore, RadioDspOutput, SpectrumProcessor,
    VecDeque, LOGIC_WAVEFORM_REVISION, MAX_WAVEFORM_SAMPLES, SCOPE_WAVEFORM_REVISION,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LogicSampleBucket {
    pub(crate) first: u32,
    pub(crate) last: u32,
    pub(crate) activity: u32,
    pub(crate) multiple_transitions: u32,
    pub(crate) sample_count: u32,
}

impl LogicSampleBucket {
    pub(crate) fn new(sample: u32) -> Self {
        Self {
            first: sample,
            last: sample,
            activity: 0,
            multiple_transitions: 0,
            sample_count: 1,
        }
    }

    pub(crate) fn from_samples(samples: &[u32]) -> Option<Self> {
        let first = *samples.first()?;
        let mut bucket = Self::new(first);
        bucket.push_samples(&samples[1..]);
        Some(bucket)
    }

    pub(crate) fn push_samples(&mut self, samples: &[u32]) {
        for sample in samples {
            let transitions = self.last ^ sample;
            self.multiple_transitions |= self.activity & transitions;
            self.activity |= self.first ^ sample;
            self.last = *sample;
        }
        self.sample_count += samples.len() as u32;
    }
}

pub(crate) struct WaveformStore {
    pub(crate) logic_samples: VecDeque<LogicSampleBucket>,
    pub(crate) logic_channels: u8,
    pub(crate) logic_rate_hz: u64,
    pub(crate) logic_revision: u64,
    pub(crate) logic_captured_at: Option<Duration>,
    pub(crate) logic_sample_offset: u64,
    pub(crate) logic_sample_count: usize,
    pub(crate) logic_sample_stride: usize,
    pub(crate) logic_capacity: usize,
    pub(crate) scope_channels: Vec<VecDeque<f32>>,
    pub(crate) scope_rate_hz: u64,
    pub(crate) scope_captured_at: Option<Duration>,
    pub(crate) scope_sample_offset: u64,
    pub(crate) scope_trigger_sample_index: Option<u64>,
    pub(crate) scope_capture_boundaries: VecDeque<u64>,
    pub(crate) scope_capacity: usize,
    pub(crate) scope_rolling: bool,
    pub(crate) scope_revision: u64,
    pub(crate) radio: RadioDisplayStore,
    pub(crate) radio_analysis: RadioAnalysisStore,
}

impl Default for WaveformStore {
    fn default() -> Self {
        Self {
            logic_samples: VecDeque::new(),
            logic_channels: 0,
            logic_rate_hz: 0,
            logic_revision: 0,
            logic_captured_at: None,
            logic_sample_offset: 0,
            logic_sample_count: 0,
            logic_sample_stride: 1,
            logic_capacity: MAX_WAVEFORM_SAMPLES,
            scope_channels: Vec::new(),
            scope_rate_hz: 0,
            scope_captured_at: None,
            scope_sample_offset: 0,
            scope_trigger_sample_index: None,
            scope_capture_boundaries: VecDeque::new(),
            scope_capacity: MAX_WAVEFORM_SAMPLES,
            scope_rolling: false,
            scope_revision: 0,
            radio: RadioDisplayStore::default(),
            radio_analysis: RadioAnalysisStore::default(),
        }
    }
}

impl WaveformStore {
    pub(crate) fn logic_capture_layout(expected_sample_count: u64) -> (usize, usize) {
        let expected_sample_count = usize::try_from(expected_sample_count).unwrap_or(usize::MAX);
        let stride = expected_sample_count
            .div_ceil(MAX_WAVEFORM_SAMPLES)
            .clamp(1, u32::MAX as usize);
        let capacity = expected_sample_count
            .div_ceil(stride)
            .clamp(1, MAX_WAVEFORM_SAMPLES);
        (capacity, stride)
    }

    pub(crate) fn logic_time_range(&self) -> Option<CaptureTimeRange> {
        let captured_at = self.logic_captured_at?.as_secs_f64();
        if self.logic_rate_hz == 0 || self.logic_sample_count == 0 {
            return None;
        }
        let rate = self.logic_rate_hz as f64;
        let start_seconds = captured_at + self.logic_sample_offset as f64 / rate;
        Some(CaptureTimeRange {
            start_seconds,
            end_seconds: start_seconds + self.logic_sample_count as f64 / rate,
        })
    }

    pub(crate) fn scope_time_range(&self) -> Option<CaptureTimeRange> {
        let captured_at = self.scope_captured_at?.as_secs_f64();
        if self.scope_rate_hz == 0 {
            return None;
        }
        let sample_count = self
            .scope_channels
            .iter()
            .filter(|samples| !samples.is_empty())
            .map(VecDeque::len)
            .min()?;
        let rate = self.scope_rate_hz as f64;
        let start_seconds = captured_at + self.scope_sample_offset as f64 / rate;
        Some(CaptureTimeRange {
            start_seconds,
            end_seconds: start_seconds + sample_count as f64 / rate,
        })
    }

    pub(crate) fn combined_timeline(&self) -> Option<CombinedTimeline> {
        let logic = self.logic_time_range()?;
        let scope = self.scope_time_range()?;
        let shared = CaptureTimeRange {
            start_seconds: logic.start_seconds.max(scope.start_seconds),
            end_seconds: logic.end_seconds.min(scope.end_seconds),
        };
        (shared.duration_seconds() > 0.0).then_some(CombinedTimeline {
            shared,
            logic,
            scope,
        })
    }

    pub(crate) fn configure_logic_capture(&mut self, expected_sample_count: u64) {
        (self.logic_capacity, self.logic_sample_stride) =
            Self::logic_capture_layout(expected_sample_count);
        self.logic_samples.clear();
        self.logic_revision = LOGIC_WAVEFORM_REVISION.fetch_add(1, Ordering::Relaxed);
        self.logic_captured_at = None;
        self.logic_sample_offset = 0;
        self.logic_sample_count = 0;
    }

    pub(crate) fn can_continue_logic_loop(&self, config: DsLogicCaptureConfig) -> bool {
        let (capacity, stride) = Self::logic_capture_layout(config.sample_count());
        self.logic_rate_hz == config.sample_rate_hz()
            && self.logic_capacity == capacity
            && self.logic_sample_stride == stride
    }

    pub(crate) fn ingest_logic_samples(&mut self, samples: &[u32]) {
        self.logic_revision = LOGIC_WAVEFORM_REVISION.fetch_add(1, Ordering::Relaxed);
        let mut remaining = samples;
        if let Some(bucket) = self.logic_samples.back_mut() {
            let available = self
                .logic_sample_stride
                .saturating_sub(bucket.sample_count as usize);
            let append_count = available.min(remaining.len());
            bucket.push_samples(&remaining[..append_count]);
            remaining = &remaining[append_count..];
        }
        for chunk in remaining.chunks(self.logic_sample_stride) {
            if let Some(bucket) = LogicSampleBucket::from_samples(chunk) {
                self.logic_samples.push_back(bucket);
            }
        }
        self.logic_sample_count = self.logic_sample_count.saturating_add(samples.len());

        while self.logic_samples.len() > self.logic_capacity {
            if let Some(removed) = self.logic_samples.pop_front() {
                let removed_count = removed.sample_count as usize;
                self.logic_sample_count = self.logic_sample_count.saturating_sub(removed_count);
                self.logic_sample_offset = self
                    .logic_sample_offset
                    .saturating_add(removed_count as u64);
            }
        }
    }

    pub(crate) fn can_continue_scope_roll(
        &self,
        config: DsCopeCaptureConfig,
        scope_capacity: usize,
    ) -> bool {
        self.scope_rolling
            && self.scope_rate_hz == config.sample_rate_hz()
            && self.scope_capacity == scope_capacity
            && self
                .scope_channels
                .iter()
                .map(|samples| !samples.is_empty())
                .eq(config.channels().iter().map(|channel| channel.enabled()))
    }

    pub(crate) fn mark_scope_capture_start(&mut self, scope_channels: [bool; 2]) {
        let sample_count = self
            .scope_channels
            .iter()
            .zip(scope_channels)
            .filter_map(|(samples, enabled)| enabled.then_some(samples.len() as u64))
            .min()
            .unwrap_or(0);
        if sample_count > 0 {
            self.scope_capture_boundaries
                .push_back(self.scope_sample_offset + sample_count);
        }
    }

    /// Starts a frame-based continuous scope sweep. Auto and Update both use
    /// one long-lived USB session, but the display contains only the current
    /// hardware frame so its trigger stays at a stable horizontal position.
    pub(crate) fn begin_scope_frame(&mut self) {
        for samples in &mut self.scope_channels {
            samples.clear();
        }
        self.scope_captured_at = None;
        self.scope_sample_offset = 0;
        self.scope_trigger_sample_index = None;
        self.scope_capture_boundaries.clear();
    }

    pub(crate) fn ingest_frame(&mut self, frame: &CaptureFrame, scope_channels: [bool; 2]) {
        match &frame.payload {
            CaptureBatch::Logic(_) => {
                self.logic_captured_at.get_or_insert(frame.captured_at);
            }
            CaptureBatch::Scope(_) => {
                self.scope_captured_at.get_or_insert(frame.captured_at);
            }
            CaptureBatch::Iq(_) => {}
        }
        self.ingest(&frame.payload, scope_channels);
    }

    pub(crate) fn ingest(&mut self, batch: &CaptureBatch, scope_channels: [bool; 2]) {
        match batch {
            CaptureBatch::Logic(logic) => {
                self.logic_channels = logic.channel_count;
                self.logic_rate_hz = logic.sample_rate_hz;
                self.ingest_logic_samples(&logic.samples);
            }
            CaptureBatch::Scope(scope) => {
                self.scope_revision = SCOPE_WAVEFORM_REVISION.fetch_add(1, Ordering::Relaxed);
                self.scope_rate_hz = scope.sample_rate_hz;
                if self.scope_channels.len() != scope_channels.len() {
                    self.scope_channels = vec![VecDeque::new(); scope_channels.len()];
                    self.scope_sample_offset = 0;
                    self.scope_capture_boundaries.clear();
                }
                let capture_start = self.scope_sample_offset
                    + self
                        .scope_channels
                        .iter()
                        .zip(scope_channels)
                        .filter_map(|(samples, enabled)| enabled.then_some(samples.len() as u64))
                        .min()
                        .unwrap_or(0);
                if let Some(trigger_sample_index) = scope.trigger_sample_index {
                    self.scope_trigger_sample_index =
                        Some(capture_start.saturating_add(trigger_sample_index));
                }
                let mut removed_samples = 0;
                for (source, physical_channel) in scope.channels.iter().zip(
                    scope_channels
                        .into_iter()
                        .enumerate()
                        .filter_map(|(index, enabled)| enabled.then_some(index)),
                ) {
                    let destination = &mut self.scope_channels[physical_channel];
                    removed_samples = removed_samples.max(extend_bounded_to(
                        destination,
                        source.iter().copied(),
                        self.scope_capacity,
                    ));
                }
                self.scope_sample_offset += removed_samples as u64;
                while self
                    .scope_capture_boundaries
                    .front()
                    .is_some_and(|boundary| *boundary <= self.scope_sample_offset)
                {
                    self.scope_capture_boundaries.pop_front();
                }
            }
            CaptureBatch::Iq(_) => {}
        }
    }
}

pub(crate) fn extend_bounded_to<T>(
    destination: &mut VecDeque<T>,
    values: impl IntoIterator<Item = T>,
    capacity: usize,
) -> usize {
    destination.extend(values);
    let excess = destination.len().saturating_sub(capacity);
    destination.drain(..excess);
    excess
}

pub(crate) fn ingest_radio_iq(
    waveforms: &mut WaveformStore,
    spectrum: &mut SpectrumProcessor,
    analyzer_bank: &mut RadioAnalyzerBank,
    iq: &wavecraft_core::IqBatch,
    squelch_threshold_dbfs: f32,
) {
    let output = process_radio_iq(spectrum, analyzer_bank, iq, squelch_threshold_dbfs);
    ingest_radio_dsp_output(waveforms, output);
}

pub(crate) fn ingest_radio_dsp_output(waveforms: &mut WaveformStore, output: RadioDspOutput) {
    for frame in output.spectrum {
        waveforms.radio.ingest(frame);
    }
    for analysis in output.analyses {
        let analysis = analysis.and_then(|output| {
            let channel_offset_hz = output.channel_frequency_offset_hz;
            let channel_name = output.channel_name;
            waveforms
                .radio_analysis
                .set_channel_sync_word(channel_offset_hz, output.sync_word);
            waveforms.radio_analysis.set_telemetry(output.telemetry);
            if let Some(demodulated) = output.demodulated {
                waveforms
                    .radio_analysis
                    .ingest(demodulated, channel_offset_hz)?;
            }
            waveforms.radio_analysis.ingest_raw(
                output.raw,
                channel_offset_hz,
                Arc::clone(&channel_name),
            );
            waveforms.radio_analysis.ingest_packets(
                output.packets,
                channel_offset_hz,
                channel_name,
            );
            if output.burst_ended {
                waveforms
                    .radio_analysis
                    .finish_channel_capture(channel_offset_hz);
            }
            Ok(())
        });
        if let Err(error) = analysis {
            waveforms.radio_analysis.set_error(error);
        }
    }
}

pub(crate) fn record_decoded_radio_packets(
    recorder: &mut Option<FileDecodedDataRecorder>,
    output: &RadioDspOutput,
) -> Result<(), String> {
    let Some(recorder) = recorder
        .as_mut()
        .filter(|recorder| recorder.records_packets())
    else {
        return Ok(());
    };
    for analysis in &output.analyses {
        let analysis = analysis
            .as_ref()
            .map_err(|error| format!("failed to decode packet data for recording: {error}"))?;
        for packet in &analysis.packets.packets {
            recorder.write_packet(
                analysis.channel_frequency_offset_hz,
                &analysis.channel_name,
                packet,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn record_decoded_raw_messages(
    recorder: &mut Option<FileDecodedDataRecorder>,
    analysis: &RadioAnalysisStore,
) -> Result<(), String> {
    let Some(recorder) = recorder
        .as_mut()
        .filter(|recorder| recorder.records_raw_messages())
    else {
        return Ok(());
    };
    for message in analysis.raw_messages() {
        recorder.write_raw_message(message)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    use wavecraft_core::{Iq8, IqBatch, LogicBatch, ScopeBatch};

    #[test]
    fn radio_iq_pipeline_runs_the_selected_analyzer() {
        let settings = RadioAnalyzerSettings {
            demodulation: RadioDemodulation::BinaryFsk,
            channels: vec![RadioAnalyzerChannelSettings::numbered(0, 10_000)],
            channel_bandwidth_hz: 4_000,
            output_sample_rate_hz: 8_000,
            filter_tap_count: 65,
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            adaptive_timing: false,
            ..RadioAnalyzerSettings::default()
        };
        let profile = settings.signal_profiles().unwrap().remove(0);
        let mut analyzers = RadioAnalyzerBank::new(vec![RadioAnalyzerRuntime::new(
            profile,
            settings.decoder_config().unwrap(),
        )
        .unwrap()]);
        // Keep the FFT shorter than this focused synthetic burst so the
        // analyzer's burst gate receives a channel-power observation.
        let mut spectrum = SpectrumProcessor::new(256, SpectrumWindow::Hann).unwrap();
        // The carrier keeps going after the symbols checked below: the channel
        // is only handed to the demodulator a few symbols behind the newest
        // sample, so that a change of level is seen before the samples around
        // it are decoded.
        let bits = [
            true, false, true, true, false, false, true, true, false, true, false,
        ];
        let mut phase: f64 = 0.0;
        let samples = bits
            .into_iter()
            .flat_map(|bit| {
                let frequency_hz = 10_000.0 + if bit { 1_000.0 } else { -1_000.0 };
                (0..64).map(move |_| frequency_hz)
            })
            .map(|frequency_hz| {
                let sample = Iq8 {
                    i: (100.0 * phase.cos()).round() as i8,
                    q: (100.0 * phase.sin()).round() as i8,
                };
                phase += std::f64::consts::TAU * frequency_hz / 64_000.0;
                sample
            })
            .collect();
        let mut waveforms = WaveformStore::default();

        ingest_radio_iq(
            &mut waveforms,
            &mut spectrum,
            &mut analyzers,
            &IqBatch {
                sample_rate_hz: 64_000,
                center_frequency_hz: 100_000_000,
                first_sample_index: 0,
                discontinuity: false,
                samples,
            },
            -50.0,
        );

        assert_eq!(analyzers.len(), 1);
        assert_eq!(
            waveforms.radio_analysis.soft_symbol_alphabet_size(),
            Some(2)
        );
        assert_eq!(waveforms.radio_analysis.soft_symbols().len(), 6);
        assert_eq!(
            waveforms
                .radio_analysis
                .soft_symbols()
                .iter()
                .map(|symbol| symbol.decision())
                .collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(1), Some(0), Some(0), Some(1)]
        );
        assert_eq!(waveforms.radio_analysis.error(), None);
    }

    #[test]
    fn the_sync_word_of_an_analyzer_output_reaches_the_messages_of_its_channel() {
        use crate::waveform::radio::ChannelSyncWord;
        use wavecraft_decode::{
            ByteStatus, RadioPacketPipelineOutput, RawRadioMessageStart, RawRadioOutput,
            SampleRange, TimedByte,
        };
        use wavecraft_radio::{RadioAnalyzerOutput, RadioAnalyzerTelemetry};
        let output = |offset_hz: i64, name: &str, sync_word: Option<&str>| RadioAnalyzerOutput {
            channel_frequency_offset_hz: offset_hz,
            channel_name: Arc::from(name),
            sync_word: sync_word.map(Arc::from),
            demodulated: None,
            raw: RawRadioOutput {
                bytes: vec![TimedByte {
                    value: 0xaa,
                    range: SampleRange { start: 0, end: 8 },
                    status: ByteStatus::default(),
                }],
                message_starts: vec![RawRadioMessageStart {
                    sample_index: 0,
                    preamble_bytes: 1,
                    maximum_uncertain_bits: usize::MAX,
                }],
                diagnostics: Vec::new(),
                synchronized: false,
            },
            packets: RadioPacketPipelineOutput::default(),
            burst_ended: false,
            telemetry: RadioAnalyzerTelemetry::default(),
        };
        let mut waveforms = WaveformStore::default();

        ingest_radio_dsp_output(
            &mut waveforms,
            RadioDspOutput {
                spectrum: Vec::new(),
                analyses: vec![
                    Ok(output(-200_000, "Lower", Some("91 d3"))),
                    Ok(output(200_000, "Upper", None)),
                ],
            },
        );

        let words = waveforms
            .radio_analysis
            .raw_messages()
            .iter()
            .map(|message| {
                (
                    message.channel_name().to_owned(),
                    message.channel_sync_word().clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            words,
            vec![
                (
                    "Lower".to_owned(),
                    ChannelSyncWord::Word(Arc::from("91 d3"))
                ),
                ("Upper".to_owned(), ChannelSyncWord::None),
            ]
        );
    }

    #[test]
    fn waveform_store_keeps_newest_samples_within_bound() {
        let mut store = WaveformStore::default();
        store.ingest(
            &CaptureBatch::Logic(LogicBatch {
                sample_rate_hz: 1_000,
                channel_count: 2,
                samples: (0..MAX_WAVEFORM_SAMPLES as u32 + 10).collect(),
            }),
            [true, true],
        );

        assert_eq!(store.logic_samples.len(), MAX_WAVEFORM_SAMPLES);
        assert_eq!(
            store.logic_samples.front(),
            Some(&LogicSampleBucket::new(10))
        );
        assert_eq!(store.logic_sample_count, MAX_WAVEFORM_SAMPLES);
        assert_eq!(store.logic_sample_offset, 10);
    }

    #[test]
    fn waveform_store_preserves_a_one_second_logic_domain_with_bounded_buckets() {
        let mut store = WaveformStore::default();
        store.configure_logic_capture(1_000_000_000);

        assert_eq!(store.logic_capacity, MAX_WAVEFORM_SAMPLES);
        assert_eq!(store.logic_sample_stride, 500);

        let mut samples = vec![0_u32; 1_000];
        samples[123] = 1;
        store.ingest(
            &CaptureBatch::Logic(LogicBatch {
                sample_rate_hz: 1_000_000_000,
                channel_count: 3,
                samples,
            }),
            [true, true],
        );

        assert_eq!(store.logic_sample_count, 1_000);
        assert_eq!(store.logic_samples.len(), 2);
        assert_eq!(store.logic_samples[0].sample_count, 500);
        assert_eq!(store.logic_samples[0].activity & 1, 1);
        assert_eq!(store.logic_sample_offset, 0);
    }

    #[test]
    fn loop_waveform_capacity_covers_the_configured_two_second_span() {
        let sample_rate_hz = 10_000_000_u64;
        let expected_sample_count = aligned_logic_sample_count(2.0, sample_rate_hz, u64::MAX)
            .expect("two seconds should fit the stream capture depth");
        let mut store = WaveformStore::default();

        store.configure_logic_capture(expected_sample_count);

        let represented_samples = store.logic_capacity * store.logic_sample_stride;
        assert!(store.logic_capacity <= MAX_WAVEFORM_SAMPLES);
        assert_eq!(store.logic_sample_stride, 11);
        assert!(represented_samples >= expected_sample_count as usize);
        assert!(represented_samples - (expected_sample_count as usize) < store.logic_sample_stride);
    }

    #[test]
    fn loop_restart_reuses_only_a_compatible_waveform_window() {
        let sample_rate_hz = 10_000_000_u64;
        let two_second_count = aligned_logic_sample_count(2.0, sample_rate_hz, u64::MAX).unwrap();
        let one_second_count = aligned_logic_sample_count(1.0, sample_rate_hz, u64::MAX).unwrap();
        let two_second_config = DsLogicCaptureConfig::new(sample_rate_hz, two_second_count)
            .unwrap()
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_loop_mode(true);
        let one_second_config = DsLogicCaptureConfig::new(sample_rate_hz, one_second_count)
            .unwrap()
            .with_operation_mode(DsLogicOperationMode::Stream)
            .with_loop_mode(true);
        let mut store = WaveformStore {
            logic_rate_hz: sample_rate_hz,
            ..WaveformStore::default()
        };
        store.configure_logic_capture(two_second_count);

        assert!(store.can_continue_logic_loop(two_second_config));
        assert!(!store.can_continue_logic_loop(one_second_config));
    }

    #[test]
    fn waveform_store_tracks_scope_channels() {
        let mut store = WaveformStore::default();
        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000,
                trigger_sample_index: Some(1),
                channels: vec![vec![0.1, 0.2], vec![-0.1, -0.2]],
            }),
            [true, true],
        );

        assert_eq!(store.scope_rate_hz, 1_000_000);
        assert_eq!(store.scope_trigger_sample_index, Some(1));
        assert_eq!(store.scope_channels[0].back(), Some(&0.2));
        assert_eq!(store.scope_channels[1].back(), Some(&-0.2));
    }

    #[test]
    fn a_new_continuous_scope_frame_replaces_the_previous_sweep() {
        let mut store = WaveformStore::default();
        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000,
                trigger_sample_index: Some(1),
                channels: vec![vec![0.1, 0.2], vec![-0.1, -0.2]],
            }),
            [true, true],
        );

        store.begin_scope_frame();
        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000,
                trigger_sample_index: Some(0),
                channels: vec![vec![0.3, 0.4], vec![-0.3, -0.4]],
            }),
            [true, true],
        );

        assert_eq!(store.scope_channels[0], VecDeque::from([0.3, 0.4]));
        assert_eq!(store.scope_trigger_sample_index, Some(0));
        assert_eq!(store.scope_sample_offset, 0);
    }

    #[test]
    fn combined_timeline_uses_timestamped_sample_overlap() {
        let mut store = WaveformStore::default();
        store.ingest_frame(
            &CaptureFrame {
                device_id: DeviceId::new("logic"),
                sequence: 0,
                captured_at: Duration::from_millis(10),
                payload: CaptureBatch::Logic(LogicBatch {
                    sample_rate_hz: 1_000,
                    channel_count: 1,
                    samples: vec![0; 100],
                }),
            },
            [true, true],
        );
        store.ingest_frame(
            &CaptureFrame {
                device_id: DeviceId::new("scope"),
                sequence: 0,
                captured_at: Duration::from_millis(20),
                payload: CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 2_000,
                    trigger_sample_index: None,
                    channels: vec![vec![0.0; 200], vec![0.0; 200]],
                }),
            },
            [true, true],
        );

        let timeline = store
            .combined_timeline()
            .expect("logic and scope ranges should overlap");
        assert!((timeline.logic.start_seconds - 0.010).abs() < f64::EPSILON);
        assert!((timeline.logic.end_seconds - 0.110).abs() < f64::EPSILON);
        assert!((timeline.scope.start_seconds - 0.020).abs() < f64::EPSILON);
        assert!((timeline.scope.end_seconds - 0.120).abs() < f64::EPSILON);
        assert!((timeline.shared.start_seconds - 0.020).abs() < f64::EPSILON);
        assert!((timeline.shared.end_seconds - 0.110).abs() < f64::EPSILON);
    }

    #[test]
    fn waveform_store_keeps_trigger_coordinates_across_appended_captures() {
        let mut store = WaveformStore {
            scope_capacity: 3,
            ..WaveformStore::default()
        };
        for (capture_index, (trigger_sample_index, samples)) in
            [(Some(1), vec![0.0, 1.0]), (Some(0), vec![2.0, 3.0])]
                .into_iter()
                .enumerate()
        {
            if capture_index > 0 {
                store.mark_scope_capture_start([true, true]);
            }
            store.ingest(
                &CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 1_000_000,
                    trigger_sample_index,
                    channels: vec![samples.clone(), samples],
                }),
                [true, true],
            );
        }

        assert_eq!(store.scope_channels[0], VecDeque::from(vec![1.0, 2.0, 3.0]));
        assert_eq!(store.scope_trigger_sample_index, Some(2));
        assert_eq!(store.scope_sample_offset, 1);
        assert_eq!(store.scope_capture_boundaries, VecDeque::from([2]));
    }

    #[test]
    fn rolling_scope_discards_boundaries_evicted_from_the_ring() {
        let mut store = WaveformStore {
            scope_capacity: 3,
            scope_rolling: true,
            ..WaveformStore::default()
        };
        for samples in [vec![0.0, 1.0], vec![2.0, 3.0], vec![4.0, 5.0]] {
            store.mark_scope_capture_start([true, true]);
            store.ingest(
                &CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 1_000,
                    trigger_sample_index: None,
                    channels: vec![samples.clone(), samples],
                }),
                [true, true],
            );
        }

        assert_eq!(store.scope_sample_offset, 3);
        assert_eq!(store.scope_capture_boundaries, VecDeque::from([4]));
    }

    #[test]
    fn waveform_store_preserves_single_scope_channel_identity() {
        let mut store = WaveformStore::default();
        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000_000,
                trigger_sample_index: None,
                channels: vec![vec![0.1, 0.2]],
            }),
            [false, true],
        );

        assert!(store.scope_channels[0].is_empty());
        assert_eq!(store.scope_channels[1].back(), Some(&0.2));
    }
}
