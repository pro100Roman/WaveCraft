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
