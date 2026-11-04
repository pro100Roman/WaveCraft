use crate::DspError;
use std::collections::VecDeque;
use std::f64::consts::{PI, TAU};
use std::ops::Range;
use wavecraft_core::IqBatch;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelizerConfig {
    /// Signed channel offset from the input batch center frequency.
    pub frequency_offset_hz: i64,
    /// Full complex channel bandwidth.
    pub bandwidth_hz: u64,
    /// Rate after bounded rational downsampling. Upsampling is not supported.
    pub output_sample_rate_hz: u64,
    /// Odd windowed-sinc FIR length.
    pub filter_tap_count: usize,
}

impl ChannelizerConfig {
    pub fn validate(&self) -> Result<(), DspError> {
        if self.bandwidth_hz == 0 {
            return Err(DspError::new("radio channel bandwidth must be nonzero"));
        }
        if self.output_sample_rate_hz == 0 {
            return Err(DspError::new(
                "radio channel output sample rate must be nonzero",
            ));
        }
        if self.bandwidth_hz > self.output_sample_rate_hz {
            return Err(DspError::new(
                "radio channel bandwidth must not exceed its output sample rate",
            ));
        }
        if self.filter_tap_count < 3
            || self.filter_tap_count % 2 == 0
            || self.filter_tap_count > 4_095
        {
            return Err(DspError::new(
                "radio channel filter must have an odd tap count between 3 and 4095",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedComplexSample {
    pub i: f32,
    pub q: f32,
    /// Half-open source IQ range that contributed to this filtered sample.
    pub source_range: Range<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelizedBatch {
    pub source_sample_rate_hz: u64,
    pub sample_rate_hz: u64,
    pub center_frequency_hz: u64,
    /// First input sample index after the latest gap, retune, or rate change.
    pub segment_start_sample_index: u64,
    /// FIR latency in input-sample units.
    pub filter_delay_input_samples: u64,
    pub discontinuity: bool,
    pub samples: Vec<TimedComplexSample>,
}

/// Incremental frequency translator, low-pass filter, and rate reducer.
pub struct Channelizer {
    config: ChannelizerConfig,
    filter: Vec<f64>,
    filter_sample_rate_hz: Option<u64>,
    history: VecDeque<(f64, f64, u64)>,
    expected_next_index: Option<u64>,
    input_sample_rate_hz: Option<u64>,
    input_center_frequency_hz: Option<u64>,
    segment_start_sample_index: Option<u64>,
    mixer_phase: f64,
    mixer_step: f64,
    resample_accumulator: u64,
    pending_discontinuity: bool,
}

impl Channelizer {
    pub fn new(config: ChannelizerConfig) -> Result<Self, DspError> {
        config.validate()?;
        Ok(Self {
            history: VecDeque::with_capacity(config.filter_tap_count),
            config,
            filter: Vec::new(),
            filter_sample_rate_hz: None,
            expected_next_index: None,
            input_sample_rate_hz: None,
            input_center_frequency_hz: None,
            segment_start_sample_index: None,
            mixer_phase: 0.0,
            mixer_step: 0.0,
            resample_accumulator: 0,
            pending_discontinuity: false,
        })
    }

    pub fn config(&self) -> &ChannelizerConfig {
        &self.config
    }

    pub fn reset(&mut self) {
        self.filter.clear();
        self.filter_sample_rate_hz = None;
        self.history.clear();
        self.expected_next_index = None;
        self.input_sample_rate_hz = None;
        self.input_center_frequency_hz = None;
        self.segment_start_sample_index = None;
        self.mixer_phase = 0.0;
        self.mixer_step = 0.0;
        self.resample_accumulator = 0;
        self.pending_discontinuity = true;
    }

    pub fn push(&mut self, batch: &IqBatch) -> Result<ChannelizedBatch, DspError> {
        self.validate_batch(batch)?;
        let reset = batch.discontinuity
            || self.expected_next_index != Some(batch.first_sample_index)
            || self.input_sample_rate_hz != Some(batch.sample_rate_hz)
            || self.input_center_frequency_hz != Some(batch.center_frequency_hz);
        if reset {
            self.reset_for_batch(batch);
        }

        let selected_frequency =
            selected_frequency(batch.center_frequency_hz, self.config.frequency_offset_hz)?;
        let mut output = Vec::new();
        for (offset, sample) in batch.samples.iter().enumerate() {
            let source_index = batch.first_sample_index.saturating_add(offset as u64);
            let oscillator = (self.mixer_phase.cos(), self.mixer_phase.sin());
            let input = (f64::from(sample.i) / 128.0, f64::from(sample.q) / 128.0);
            let mixed = (
                input.0 * oscillator.0 - input.1 * oscillator.1,
                input.0 * oscillator.1 + input.1 * oscillator.0,
            );
            self.mixer_phase = wrap_phase(self.mixer_phase + self.mixer_step);

            self.history.push_back((mixed.0, mixed.1, source_index));
            if self.history.len() > self.config.filter_tap_count {
                self.history.pop_front();
            }
            if self.history.len() != self.config.filter_tap_count {
                continue;
            }

            self.resample_accumulator = self
                .resample_accumulator
                .saturating_add(self.config.output_sample_rate_hz);
            if self.resample_accumulator < batch.sample_rate_hz {
                continue;
            }
            self.resample_accumulator -= batch.sample_rate_hz;

            let (i, q) = self.history.iter().zip(&self.filter).fold(
                (0.0, 0.0),
                |sum, (sample, coefficient)| {
                    (
                        sum.0 + sample.0 * coefficient,
                        sum.1 + sample.1 * coefficient,
                    )
                },
            );
            let first = self.history.front().map_or(source_index, |sample| sample.2);
            let end = self
                .history
                .back()
                .map_or(source_index.saturating_add(1), |sample| {
                    sample.2.saturating_add(1)
                });
            output.push(TimedComplexSample {
                i: i as f32,
                q: q as f32,
                source_range: first..end,
            });
        }

        self.expected_next_index = Some(
            batch
                .first_sample_index
                .saturating_add(batch.samples.len() as u64),
        );
        let discontinuity = self.pending_discontinuity;
        if !output.is_empty() {
            self.pending_discontinuity = false;
        }
        Ok(ChannelizedBatch {
            source_sample_rate_hz: batch.sample_rate_hz,
            sample_rate_hz: self.config.output_sample_rate_hz,
            center_frequency_hz: selected_frequency,
            segment_start_sample_index: self
                .segment_start_sample_index
                .unwrap_or(batch.first_sample_index),
            filter_delay_input_samples: (self.config.filter_tap_count / 2) as u64,
            discontinuity,
            samples: output,
        })
    }

    fn validate_batch(&self, batch: &IqBatch) -> Result<(), DspError> {
        if batch.sample_rate_hz == 0 {
            return Err(DspError::new("IQ sample rate must be nonzero"));
        }
        if self.config.output_sample_rate_hz > batch.sample_rate_hz {
            return Err(DspError::new(
                "radio channel output rate must not exceed the IQ sample rate",
            ));
        }
        let occupied_edge = i128::from(self.config.frequency_offset_hz.unsigned_abs())
            + i128::from(self.config.bandwidth_hz.div_ceil(2));
        if occupied_edge.saturating_mul(2) > i128::from(batch.sample_rate_hz) {
            return Err(DspError::new(
                "radio channel extends outside the input Nyquist range",
            ));
        }
        selected_frequency(batch.center_frequency_hz, self.config.frequency_offset_hz)?;
        Ok(())
    }

    fn reset_for_batch(&mut self, batch: &IqBatch) {
        self.history.clear();
        if self.filter_sample_rate_hz != Some(batch.sample_rate_hz) {
            self.filter = design_low_pass(
                batch.sample_rate_hz,
                self.config.bandwidth_hz,
                self.config.filter_tap_count,
            );
            self.filter_sample_rate_hz = Some(batch.sample_rate_hz);
        }
        self.expected_next_index = None;
        self.input_sample_rate_hz = Some(batch.sample_rate_hz);
        self.input_center_frequency_hz = Some(batch.center_frequency_hz);
        self.segment_start_sample_index = Some(batch.first_sample_index);
        self.mixer_step =
            -TAU * self.config.frequency_offset_hz as f64 / batch.sample_rate_hz as f64;
        let reduced_index = batch.first_sample_index % batch.sample_rate_hz;
        self.mixer_phase = wrap_phase(self.mixer_step * reduced_index as f64);
        self.resample_accumulator = 0;
        self.pending_discontinuity = true;
    }
}

fn selected_frequency(center_frequency_hz: u64, offset_hz: i64) -> Result<u64, DspError> {
    let frequency = i128::from(center_frequency_hz) + i128::from(offset_hz);
    u64::try_from(frequency)
        .map_err(|_| DspError::new("radio channel center frequency is outside the valid range"))
}

fn wrap_phase(phase: f64) -> f64 {
    (phase + PI).rem_euclid(TAU) - PI
}

fn design_low_pass(sample_rate_hz: u64, bandwidth_hz: u64, tap_count: usize) -> Vec<f64> {
    let cutoff = bandwidth_hz as f64 / (2.0 * sample_rate_hz as f64);
    let midpoint = (tap_count / 2) as isize;
    let mut taps = (0..tap_count)
        .map(|index| {
            let offset = index as isize - midpoint;
            let sinc = if offset == 0 {
                2.0 * cutoff
            } else {
                (2.0 * PI * cutoff * offset as f64).sin() / (PI * offset as f64)
            };
            let window = 0.54 - 0.46 * (TAU * index as f64 / (tap_count - 1) as f64).cos();
            sinc * window
        })
        .collect::<Vec<_>>();
    let sum = taps.iter().sum::<f64>();
    for tap in &mut taps {
        *tap /= sum;
    }
    taps
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;
    use wavecraft_core::Iq8;

    fn tone_batch(first_sample_index: u64, count: usize, discontinuity: bool) -> IqBatch {
        tone_batch_at(first_sample_index, count, discontinuity, 1_000.0)
    }

    fn tone_batch_at(
        first_sample_index: u64,
        count: usize,
        discontinuity: bool,
        frequency_hz: f64,
    ) -> IqBatch {
        IqBatch {
            sample_rate_hz: 8_000,
            center_frequency_hz: 100_000_000,
            first_sample_index,
            discontinuity,
            samples: (0..count)
                .map(|offset| {
                    let index = first_sample_index + offset as u64;
                    let phase = TAU * frequency_hz * index as f64 / 8_000.0;
                    Iq8 {
                        i: (100.0 * phase.cos()).round() as i8,
                        q: (100.0 * phase.sin()).round() as i8,
                    }
                })
                .collect(),
        }
    }

    fn config() -> ChannelizerConfig {
        ChannelizerConfig {
            frequency_offset_hz: 1_000,
            bandwidth_hz: 1_000,
            output_sample_rate_hz: 2_000,
            filter_tap_count: 9,
        }
    }

    #[test]
    fn channelization_is_independent_of_input_batch_boundaries() {
        let mut whole = Channelizer::new(config()).unwrap();
        let expected = whole.push(&tone_batch(0, 64, false)).unwrap();

        let mut chunked = Channelizer::new(config()).unwrap();
        let first = chunked.push(&tone_batch(0, 13, false)).unwrap();
        let second = chunked.push(&tone_batch(13, 17, false)).unwrap();
        let third = chunked.push(&tone_batch(30, 34, false)).unwrap();
        let actual = first
            .samples
            .into_iter()
            .chain(second.samples)
            .chain(third.samples)
            .collect::<Vec<_>>();

        assert_eq!(actual, expected.samples);
        assert_eq!(expected.center_frequency_hz, 100_001_000);
        assert_eq!(expected.filter_delay_input_samples, 4);
        assert!(expected.discontinuity);
        assert!(actual
            .iter()
            .all(|sample| sample.source_range.end - sample.source_range.start == 9));
        assert!(actual
            .iter()
            .skip(2)
            .all(|sample| { sample.i > 0.70 && sample.q.abs() < 0.03 }));
    }

    #[test]
    fn discontinuity_discards_filter_history_and_restarts_source_mapping() {
        let mut channelizer = Channelizer::new(config()).unwrap();
        let first = channelizer.push(&tone_batch(0, 12, false)).unwrap();
        assert!(!first.samples.is_empty());

        let reset = channelizer.push(&tone_batch(100, 16, true)).unwrap();
        assert!(reset.discontinuity);
        assert!(reset
            .samples
            .iter()
            .all(|sample| sample.source_range.start >= 100));
    }

    #[test]
    fn rejects_a_channel_outside_the_input_nyquist_range() {
        let mut invalid = config();
        invalid.frequency_offset_hz = 3_750;
        let mut channelizer = Channelizer::new(invalid).unwrap();
        assert!(channelizer.push(&tone_batch(0, 16, false)).is_err());
    }

    #[test]
    fn channel_filter_rejects_a_tone_outside_the_selected_bandwidth() {
        let mut filter_config = config();
        filter_config.filter_tap_count = 65;
        let mut desired_channel = Channelizer::new(filter_config.clone()).unwrap();
        let desired = desired_channel
            .push(&tone_batch_at(0, 512, false, 1_000.0))
            .unwrap();
        let mut rejected_channel = Channelizer::new(filter_config).unwrap();
        let rejected = rejected_channel
            .push(&tone_batch_at(0, 512, false, -2_500.0))
            .unwrap();
        let mean_magnitude = |batch: &ChannelizedBatch| {
            batch
                .samples
                .iter()
                .map(|sample| f64::from(sample.i).hypot(f64::from(sample.q)))
                .sum::<f64>()
                / batch.samples.len() as f64
        };

        assert!(mean_magnitude(&desired) > 0.70);
        assert!(mean_magnitude(&rejected) < mean_magnitude(&desired) * 0.02);
    }
}
