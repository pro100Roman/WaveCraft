//! Device-independent signal processing for radio IQ captures.

mod channelizer;
mod fsk;

pub use channelizer::{ChannelizedBatch, Channelizer, ChannelizerConfig, TimedComplexSample};
pub use fsk::{FskConfig, FskDemodulator, FskPolarity, TimedSoftBit};

use std::collections::VecDeque;
use std::error::Error;
use std::f64::consts::TAU;
use std::fmt::{Display, Formatter};
use std::ops::Range;
use wavecraft_core::IqBatch;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DspError {
    message: String,
}

impl DspError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for DspError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl Error for DspError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpectrumWindow {
    Rectangular,
    Hann,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpectrumFrame {
    pub source_range: Range<u64>,
    pub center_frequency_hz: u64,
    pub bin_width_hz: f64,
    /// FFT-shifted power bins ordered from negative to positive frequency.
    pub power_dbfs: Vec<f32>,
}

pub struct SpectrumProcessor {
    fft_length: usize,
    window: SpectrumWindow,
    samples: VecDeque<(f64, f64)>,
    first_sample_index: Option<u64>,
    expected_next_index: Option<u64>,
    sample_rate_hz: Option<u64>,
    center_frequency_hz: Option<u64>,
}

impl SpectrumProcessor {
    pub fn new(fft_length: usize, window: SpectrumWindow) -> Result<Self, String> {
        if fft_length < 2 || !fft_length.is_power_of_two() {
            return Err("spectrum FFT length must be a power of two of at least two".to_owned());
        }
        Ok(Self {
            fft_length,
            window,
            samples: VecDeque::with_capacity(fft_length),
            first_sample_index: None,
            expected_next_index: None,
            sample_rate_hz: None,
            center_frequency_hz: None,
        })
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.first_sample_index = None;
        self.expected_next_index = None;
        self.sample_rate_hz = None;
        self.center_frequency_hz = None;
    }

    pub fn push(&mut self, batch: &IqBatch) -> Vec<SpectrumFrame> {
        if batch.discontinuity
            || self.expected_next_index != Some(batch.first_sample_index)
            || self
                .sample_rate_hz
                .is_some_and(|rate| rate != batch.sample_rate_hz)
            || self
                .center_frequency_hz
                .is_some_and(|frequency| frequency != batch.center_frequency_hz)
        {
            self.reset();
        }
        if self.first_sample_index.is_none() {
            self.first_sample_index = Some(batch.first_sample_index);
        }
        self.samples.extend(
            batch
                .samples
                .iter()
                .map(|sample| (f64::from(sample.i) / 128.0, f64::from(sample.q) / 128.0)),
        );
        self.sample_rate_hz = Some(batch.sample_rate_hz);
        self.center_frequency_hz = Some(batch.center_frequency_hz);
        self.expected_next_index = Some(
            batch
                .first_sample_index
                .saturating_add(batch.samples.len() as u64),
        );

        let mut frames = Vec::new();
        while self.samples.len() >= self.fft_length {
            let start = self.first_sample_index.unwrap_or(batch.first_sample_index);
            let mut values = self
                .samples
                .drain(..self.fft_length)
                .enumerate()
                .map(|(index, value)| {
                    let scale = match self.window {
                        SpectrumWindow::Rectangular => 1.0,
                        SpectrumWindow::Hann => {
                            0.5 - 0.5 * (TAU * index as f64 / (self.fft_length - 1) as f64).cos()
                        }
                    };
                    (value.0 * scale, value.1 * scale)
                })
                .collect::<Vec<_>>();
            fft_in_place(&mut values);
            let normalization = self.fft_length as f64;
            let half = self.fft_length / 2;
            let power_dbfs = values
                .iter()
                .cycle()
                .skip(half)
                .take(self.fft_length)
                .map(|(real, imaginary)| {
                    let magnitude = (real * real + imaginary * imaginary).sqrt() / normalization;
                    (20.0 * magnitude.max(1.0e-12).log10()) as f32
                })
                .collect();
            frames.push(SpectrumFrame {
                source_range: start..start.saturating_add(self.fft_length as u64),
                center_frequency_hz: batch.center_frequency_hz,
                bin_width_hz: batch.sample_rate_hz as f64 / self.fft_length as f64,
                power_dbfs,
            });
            self.first_sample_index = Some(start + self.fft_length as u64);
        }
        frames
    }
}

fn fft_in_place(values: &mut [(f64, f64)]) {
    let length = values.len();
    let mut reversed = 0_usize;
    for index in 1..length {
        let mut bit = length >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            values.swap(index, reversed);
        }
    }
    let mut size = 2;
    while size <= length {
        let angle = -TAU / size as f64;
        let root = (angle.cos(), angle.sin());
        for start in (0..length).step_by(size) {
            let mut twiddle = (1.0, 0.0);
            for offset in 0..size / 2 {
                let even = values[start + offset];
                let odd = values[start + offset + size / 2];
                let rotated = (
                    odd.0 * twiddle.0 - odd.1 * twiddle.1,
                    odd.0 * twiddle.1 + odd.1 * twiddle.0,
                );
                values[start + offset] = (even.0 + rotated.0, even.1 + rotated.1);
                values[start + offset + size / 2] = (even.0 - rotated.0, even.1 - rotated.1);
                twiddle = (
                    twiddle.0 * root.0 - twiddle.1 * root.1,
                    twiddle.0 * root.1 + twiddle.1 * root.0,
                );
            }
        }
        size *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_core::{Iq8, IqBatch};

    fn tone_batch(first_sample_index: u64, discontinuity: bool) -> IqBatch {
        IqBatch {
            sample_rate_hz: 8_000,
            center_frequency_hz: 100_000_000,
            first_sample_index,
            discontinuity,
            samples: (0..8)
                .map(|index| {
                    let phase = TAU * 2.0 * index as f64 / 8.0;
                    Iq8 {
                        i: (100.0 * phase.cos()).round() as i8,
                        q: (100.0 * phase.sin()).round() as i8,
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn spectrum_places_a_complex_tone_in_the_expected_shifted_bin() {
        let mut processor = SpectrumProcessor::new(8, SpectrumWindow::Rectangular).unwrap();
        let frame = processor.push(&tone_batch(0, false)).remove(0);

        let peak = frame
            .power_dbfs
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(index, _)| index)
            .unwrap();
        assert_eq!(peak, 6);
        assert_eq!(frame.source_range, 0..8);
        assert_eq!(frame.bin_width_hz, 1_000.0);
    }

    #[test]
    fn discontinuity_drops_an_incomplete_fft_window() {
        let mut processor = SpectrumProcessor::new(8, SpectrumWindow::Rectangular).unwrap();
        let mut partial = tone_batch(0, false);
        partial.samples.truncate(4);
        assert!(processor.push(&partial).is_empty());

        let frame = processor.push(&tone_batch(100, true)).remove(0);
        assert_eq!(frame.source_range, 100..108);
    }
}
