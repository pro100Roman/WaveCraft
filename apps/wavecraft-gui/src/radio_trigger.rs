use std::ops::Range;
use std::time::Duration;

use wavecraft_core::{CaptureBatch, CaptureFrame, IqBatch};
use wavecraft_dsp::{
    SpectrumPowerTrigger, SpectrumPowerTriggerConfig, SpectrumPowerTriggerEvent, SpectrumWindow,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadioSingleTriggerConfig {
    pub threshold_dbfs: f32,
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
    posttrigger_samples: u64,
    target_end_sample: Option<u64>,
    complete: bool,
}

impl RadioSingleTrigger {
    pub fn new(config: RadioSingleTriggerConfig) -> Result<Self, String> {
        Ok(Self {
            detector: SpectrumPowerTrigger::new(SpectrumPowerTriggerConfig {
                fft_length: config.fft_length,
                window: SpectrumWindow::Hann,
                threshold_dbfs: config.threshold_dbfs,
                iq_correction_enabled: config.iq_correction_enabled,
            })?,
            posttrigger_samples: config.posttrigger_samples,
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

        let event = self.detector.push(iq);
        let Some(event) = event else {
            return Ok(RadioSingleTriggerOutput {
                frames: Vec::new(),
                event: None,
                complete: false,
            });
        };

        let target_end_sample = event
            .source_range
            .end
            .saturating_add(self.posttrigger_samples);
        let frame_end = iq_end(iq)?;
        let mut frames = trim_frame(frame, event.source_range.start..target_end_sample)
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(CaptureFrame {
            payload: CaptureBatch::Iq(iq),
            ..
        }) = frames.first_mut()
        {
            iq.discontinuity = true;
        }
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

    pub fn set_posttrigger_samples(&mut self, posttrigger_samples: u64) {
        if self.is_armed() {
            self.posttrigger_samples = posttrigger_samples;
        }
    }
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
    use crate::*;

    use wavecraft_core::{DeviceId, Iq8, IqBatch};
    use wavecraft_sdr::RadioProviderAdapter;

    const SAMPLE_RATE_HZ: u64 = 8_000;
    const CENTER_FREQUENCY_HZ: u64 = 868_500_000;

    fn trigger() -> RadioSingleTrigger {
        RadioSingleTrigger::new(RadioSingleTriggerConfig {
            threshold_dbfs: -10.0,
            posttrigger_samples: 4,
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
    fn waits_for_trigger_then_keeps_the_configured_posttrigger_interval() {
        let mut trigger = trigger();
        let waiting = trigger.push(iq_frame(0, quiet(8))).unwrap();
        assert!(waiting.frames.is_empty());
        assert!(waiting.event.is_none());
        assert!(!waiting.complete);

        let fired = trigger.push(iq_frame(8, tone(8))).unwrap();
        assert!(fired.event.is_some());
        assert_eq!(fired.frames.len(), 1);
        assert_eq!(range(&fired.frames[0]), 8..16);
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
    fn does_not_buffer_iq_while_waiting_for_the_trigger() {
        let mut trigger = trigger();
        assert!(trigger
            .push(iq_frame(0, quiet(16)))
            .unwrap()
            .frames
            .is_empty());
        let fired = trigger.push(iq_frame(16, tone(8))).unwrap();
        assert_eq!(fired.frames.len(), 1);
        assert_eq!(range(&fired.frames[0]), 16..24);
    }

    #[test]
    fn trims_the_triggering_input_after_the_posttrigger_interval() {
        let mut trigger = trigger();

        let output = trigger
            .push(iq_frame(0, [tone(8), quiet(8)].concat()))
            .unwrap();

        assert!(output.complete);
        assert_eq!(output.frames.len(), 1);
        assert_eq!(range(&output.frames[0]), 0..12);
    }

    #[test]
    fn posttrigger_interval_can_change_while_armed() {
        let mut trigger = trigger();
        trigger.set_posttrigger_samples(0);

        let output = trigger.push(iq_frame(0, tone(8))).unwrap();

        assert!(output.complete);
        assert_eq!(range(&output.frames[0]), 0..8);
    }

    #[test]
    #[ignore = "requires a connected HackRF One and installed libhackrf"]
    fn gui_hackrf_trigger_gate_stops_an_unbounded_single_capture() {
        let adapter = wavecraft_hackrf::HackRfAdapter::new();
        let device = adapter
            .discover()
            .expect("GUI HackRF discovery should succeed")
            .into_iter()
            .next()
            .expect("a HackRF One should be connected");
        let request = RadioCaptureRequest {
            device,
            settings: RadioCaptureSettings {
                center_frequency_hz: 100_000_000,
                sample_rate_hz: 2_000_000,
                bandwidth_hz: 1_750_000,
                receive_channel: 0,
                sample_limit: None,
            },
            provider_settings: RadioProviderSettings::HackRf(
                wavecraft_sdr::HackRfSettings::default(),
            ),
            iq_correction_enabled: true,
            analyzer_profiles: Vec::new(),
            analyzer_channel_names: Vec::new(),
            decoder_configs: Vec::new(),
        };
        let started = start_capture(
            Vec::new(),
            Some(request),
            Path::new("."),
            LogicCaptureSettings::default().capture_config().unwrap(),
            ScopeCaptureSettings::default().capture_config().unwrap(),
            None,
        )
        .expect("GUI radio capture should start");
        let mut trigger = RadioSingleTrigger::new(RadioSingleTriggerConfig {
            threshold_dbfs: -120.0,
            posttrigger_samples: RADIO_SPECTRUM_FFT_LENGTH as u64,
            fft_length: RADIO_SPECTRUM_FFT_LENGTH,
            iq_correction_enabled: true,
        })
        .unwrap();
        let session = started.session;
        let mut retained_samples = 0;
        loop {
            match session
                .recv_timeout(Duration::from_secs(5))
                .expect("triggered HackRF capture should produce an event")
            {
                CaptureEvent::Frame(frame) => {
                    let output = trigger.push(frame).expect("IQ trigger gate should run");
                    retained_samples += output
                        .frames
                        .iter()
                        .map(|frame| frame.payload.sample_count())
                        .sum::<usize>();
                    if output.complete {
                        break;
                    }
                }
                CaptureEvent::SourceFailed { error, .. } => {
                    panic!("GUI HackRF capture failed: {error}")
                }
                CaptureEvent::SourceStopped { .. } => {
                    panic!("unbounded HackRF capture stopped before the RF trigger completed")
                }
            }
        }

        session
            .stop()
            .expect("triggered HackRF capture should stop");
        assert_eq!(retained_samples, RADIO_SPECTRUM_FFT_LENGTH * 2);
    }
}
