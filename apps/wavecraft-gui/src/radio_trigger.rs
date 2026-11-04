use std::collections::VecDeque;
use std::ops::Range;
use std::time::Duration;

use wavecraft_core::{CaptureBatch, CaptureFrame, IqBatch};
use wavecraft_dsp::{
    SpectrumPowerTrigger, SpectrumPowerTriggerConfig, SpectrumPowerTriggerEvent, SpectrumWindow,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadioSingleTriggerConfig {
    pub threshold_dbfs: f32,
    pub pretrigger_samples: u64,
    pub posttrigger_samples: u64,
    pub fft_length: usize,
    pub iq_correction_enabled: bool,
}

#[derive(Debug)]
pub struct RadioSingleTriggerOutput {
    pub frames: Vec<CaptureFrame>,
    pub event: Option<SpectrumPowerTriggerEvent>,
    pub complete: bool,
}

pub struct RadioSingleTrigger {
    detector: SpectrumPowerTrigger,
    pretrigger: VecDeque<CaptureFrame>,
    pretrigger_samples: u64,
    posttrigger_samples: u64,
    retained_samples: u64,
    target_end_sample: Option<u64>,
    complete: bool,
}

impl RadioSingleTrigger {
    pub fn new(config: RadioSingleTriggerConfig) -> Result<Self, String> {
        if config.posttrigger_samples == 0 {
            return Err(
                "RF trigger post-trigger sample count must be greater than zero".to_owned(),
            );
        }
        Ok(Self {
            detector: SpectrumPowerTrigger::new(SpectrumPowerTriggerConfig {
                fft_length: config.fft_length,
                window: SpectrumWindow::Hann,
                threshold_dbfs: config.threshold_dbfs,
                iq_correction_enabled: config.iq_correction_enabled,
            })?,
            pretrigger: VecDeque::new(),
            pretrigger_samples: config.pretrigger_samples,
            posttrigger_samples: config.posttrigger_samples,
            retained_samples: 0,
            target_end_sample: None,
            complete: false,
        })
    }

    pub fn push(&mut self, frame: CaptureFrame) -> Result<RadioSingleTriggerOutput, String> {
        let CaptureBatch::Iq(iq) = &frame.payload else {
            return Err("RF trigger received a non-IQ capture frame".to_owned());
        };
        if self.complete {
            return Ok(RadioSingleTriggerOutput {
                frames: Vec::new(),
                event: None,
                complete: true,
            });
        }

        if let Some(target_end_sample) = self.target_end_sample {
            let frame_end = iq_end(iq)?;
            let frames = trim_frame(frame, 0..target_end_sample)
                .into_iter()
                .collect();
            self.complete = frame_end >= target_end_sample;
            return Ok(RadioSingleTriggerOutput {
                frames,
                event: None,
                complete: self.complete,
            });
        }

        if iq.discontinuity {
            self.clear_pretrigger();
        }
        let event = self.detector.push(iq);
        let Some(event) = event else {
            self.retain_pretrigger(frame)?;
            return Ok(RadioSingleTriggerOutput {
                frames: Vec::new(),
                event: None,
                complete: false,
            });
        };

        let retain_start = event
            .source_range
            .start
            .saturating_sub(self.pretrigger_samples);
        let target_end_sample = event
            .source_range
            .start
            .saturating_add(self.posttrigger_samples);
        let frame_end = iq_end(iq)?;
        let mut frames = self
            .pretrigger
            .drain(..)
            .filter_map(|queued| trim_frame(queued, retain_start..target_end_sample))
            .collect::<Vec<_>>();
        if let Some(frame) = trim_frame(frame, retain_start..target_end_sample) {
            frames.push(frame);
        }
        if let Some(CaptureFrame {
            payload: CaptureBatch::Iq(iq),
            ..
        }) = frames.first_mut()
        {
            iq.discontinuity = true;
        }
        self.retained_samples = 0;
        self.complete = frame_end >= target_end_sample;
        if !self.complete {
            self.target_end_sample = Some(target_end_sample);
        }
        Ok(RadioSingleTriggerOutput {
            frames,
            event: Some(event),
            complete: self.complete,
        })
    }

    pub fn is_armed(&self) -> bool {
        self.target_end_sample.is_none() && !self.complete
    }

    pub fn set_threshold_dbfs(&mut self, threshold_dbfs: f32) -> Result<(), String> {
        self.detector.set_threshold_dbfs(threshold_dbfs)
    }

    fn retain_pretrigger(&mut self, frame: CaptureFrame) -> Result<(), String> {
        let samples = iq_sample_count(&frame)?;
        self.retained_samples = self.retained_samples.saturating_add(samples);
        self.pretrigger.push_back(frame);
        let mut excess = self
            .retained_samples
            .saturating_sub(self.pretrigger_samples);
        while excess > 0 {
            let Some(front) = self.pretrigger.pop_front() else {
                break;
            };
            let front_samples = iq_sample_count(&front)?;
            if excess >= front_samples {
                self.retained_samples = self.retained_samples.saturating_sub(front_samples);
                excess -= front_samples;
                continue;
            }
            let CaptureBatch::Iq(iq) = &front.payload else {
                unreachable!("pre-trigger queue only accepts IQ frames");
            };
            let start = iq.first_sample_index.saturating_add(excess);
            if let Some(trimmed) = trim_frame(front, start..u64::MAX) {
                self.pretrigger.push_front(trimmed);
            }
            self.retained_samples = self.retained_samples.saturating_sub(excess);
            break;
        }
        Ok(())
    }

    fn clear_pretrigger(&mut self) {
        self.pretrigger.clear();
        self.retained_samples = 0;
    }
}

fn iq_sample_count(frame: &CaptureFrame) -> Result<u64, String> {
    let CaptureBatch::Iq(iq) = &frame.payload else {
        return Err("RF trigger received a non-IQ capture frame".to_owned());
    };
    u64::try_from(iq.samples.len()).map_err(|_| "IQ frame is too large".to_owned())
}

fn iq_end(iq: &IqBatch) -> Result<u64, String> {
    let samples =
        u64::try_from(iq.samples.len()).map_err(|_| "IQ frame is too large".to_owned())?;
    iq.first_sample_index
        .checked_add(samples)
        .ok_or_else(|| "IQ sample range overflowed".to_owned())
}

fn trim_frame(mut frame: CaptureFrame, keep: Range<u64>) -> Option<CaptureFrame> {
    let CaptureBatch::Iq(iq) = &mut frame.payload else {
        return None;
    };
    let frame_end = iq
        .first_sample_index
        .saturating_add(iq.samples.len() as u64);
    let start = iq.first_sample_index.max(keep.start);
    let end = frame_end.min(keep.end);
    if start >= end {
        return None;
    }
    let prefix = usize::try_from(start - iq.first_sample_index).ok()?;
    let retained = usize::try_from(end - start).ok()?;
    iq.samples = iq.samples[prefix..prefix + retained].to_vec();
    iq.first_sample_index = start;
    if prefix > 0 && iq.sample_rate_hz > 0 {
        frame.captured_at += Duration::from_secs_f64(prefix as f64 / iq.sample_rate_hz as f64);
    }
    Some(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_core::{DeviceId, Iq8};

    const SAMPLE_RATE_HZ: u64 = 8_000;
    const CENTER_FREQUENCY_HZ: u64 = 868_500_000;

    fn trigger() -> RadioSingleTrigger {
        RadioSingleTrigger::new(RadioSingleTriggerConfig {
            threshold_dbfs: -10.0,
            pretrigger_samples: 8,
            posttrigger_samples: 12,
            fft_length: 8,
            iq_correction_enabled: false,
        })
        .unwrap()
    }

    fn iq_frame(first_sample_index: u64, samples: Vec<Iq8>) -> CaptureFrame {
        CaptureFrame {
            device_id: DeviceId::new("radio0"),
            sequence: first_sample_index / 8,
            captured_at: Duration::from_secs_f64(first_sample_index as f64 / SAMPLE_RATE_HZ as f64),
            payload: CaptureBatch::Iq(IqBatch {
                sample_rate_hz: SAMPLE_RATE_HZ,
                center_frequency_hz: CENTER_FREQUENCY_HZ,
                first_sample_index,
                discontinuity: first_sample_index == 0,
                samples,
            }),
        }
    }

    fn quiet(count: usize) -> Vec<Iq8> {
        vec![Iq8 { i: 0, q: 0 }; count]
    }

    fn tone(count: usize) -> Vec<Iq8> {
        (0..count)
            .map(|index| match index % 4 {
                0 => Iq8 { i: 100, q: 0 },
                1 => Iq8 { i: 0, q: 100 },
                2 => Iq8 { i: -100, q: 0 },
                _ => Iq8 { i: 0, q: -100 },
            })
            .collect()
    }

    fn range(frame: &CaptureFrame) -> Range<u64> {
        let CaptureBatch::Iq(iq) = &frame.payload else {
            unreachable!();
        };
        iq.first_sample_index..iq.first_sample_index + iq.samples.len() as u64
    }

    #[test]
    fn waits_then_flushes_pretrigger_and_completes_after_posttrigger() {
        let mut trigger = trigger();
        let waiting = trigger.push(iq_frame(0, quiet(8))).unwrap();
        assert!(waiting.frames.is_empty());
        assert!(waiting.event.is_none());
        assert!(!waiting.complete);

        let fired = trigger.push(iq_frame(8, tone(8))).unwrap();
        assert!(fired.event.is_some());
        assert_eq!(
            fired.frames.iter().map(range).collect::<Vec<_>>(),
            [0..8, 8..16]
        );
        assert!(!fired.complete);
        let CaptureBatch::Iq(first) = &fired.frames[0].payload else {
            unreachable!();
        };
        assert!(first.discontinuity);

        let complete = trigger.push(iq_frame(16, quiet(8))).unwrap();
        assert_eq!(complete.frames.len(), 1);
        assert_eq!(range(&complete.frames[0]), 16..20);
        assert!(complete.complete);
    }

    #[test]
    fn bounds_pretrigger_to_configured_sample_count() {
        let mut trigger = trigger();
        assert!(trigger
            .push(iq_frame(0, quiet(16)))
            .unwrap()
            .frames
            .is_empty());
        let fired = trigger.push(iq_frame(16, tone(8))).unwrap();
        assert_eq!(
            fired.frames.iter().map(range).collect::<Vec<_>>(),
            [8..16, 16..24]
        );
    }

    #[test]
    fn rejects_zero_posttrigger_length() {
        let result = RadioSingleTrigger::new(RadioSingleTriggerConfig {
            threshold_dbfs: -10.0,
            pretrigger_samples: 8,
            posttrigger_samples: 0,
            fft_length: 8,
            iq_correction_enabled: false,
        });
        assert!(result.is_err());
    }
}
