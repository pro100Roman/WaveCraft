//! Deterministic simulated instruments used while USB drivers are developed.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};
use wavecraft_core::{
    CaptureBatch, CaptureContext, CaptureError, CaptureSource, DeviceDescriptor, DeviceId,
    InstrumentKind, Iq8, IqBatch, LogicBatch, ScopeBatch,
};

pub const SIMULATED_LOGIC_ANALYZER_ID: &str = "sim:logic-0";
pub const SIMULATED_OSCILLOSCOPE_ID: &str = "sim:scope-0";
pub const SIMULATED_RADIO_RECEIVER_ID: &str = "sim:radio-0";

/// Static discovery information for one deterministic simulated instrument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SimulatedDeviceProfile {
    pub connection_id: &'static str,
    pub display_name: &'static str,
    pub model: &'static str,
    pub kind: InstrumentKind,
    pub channel_count: u8,
    pub sample_rates_hz: &'static [u64],
}

const SIMULATED_LOGIC_SAMPLE_RATES_HZ: &[u64] = &[100_000, 1_000_000, 10_000_000];
const SIMULATED_SCOPE_SAMPLE_RATES_HZ: &[u64] = &[50_000, 500_000, 5_000_000];
const SIMULATED_RADIO_SAMPLE_RATES_HZ: &[u64] = &[1_000_000, 2_000_000, 10_000_000];

pub const SIMULATED_DEVICE_PROFILES: &[SimulatedDeviceProfile] = &[
    SimulatedDeviceProfile {
        connection_id: SIMULATED_LOGIC_ANALYZER_ID,
        display_name: "Simulated Logic Analyzer",
        model: "simulated_logic_analyzer",
        kind: InstrumentKind::LogicAnalyzer,
        channel_count: 8,
        sample_rates_hz: SIMULATED_LOGIC_SAMPLE_RATES_HZ,
    },
    SimulatedDeviceProfile {
        connection_id: SIMULATED_OSCILLOSCOPE_ID,
        display_name: "Simulated Oscilloscope",
        model: "simulated_oscilloscope",
        kind: InstrumentKind::Oscilloscope,
        channel_count: 2,
        sample_rates_hz: SIMULATED_SCOPE_SAMPLE_RATES_HZ,
    },
    SimulatedDeviceProfile {
        connection_id: SIMULATED_RADIO_RECEIVER_ID,
        display_name: "Simulated Radio Receiver",
        model: "simulated_radio_receiver",
        kind: InstrumentKind::RadioReceiver,
        channel_count: 1,
        sample_rates_hz: SIMULATED_RADIO_SAMPLE_RATES_HZ,
    },
];

pub struct SimulatedLogicAnalyzer {
    descriptor: DeviceDescriptor,
    sample_rate_hz: u64,
    channel_count: u8,
    batch_size: usize,
    sample_limit: Option<u64>,
    enabled_channel_mask: u32,
}

impl SimulatedLogicAnalyzer {
    pub fn new(
        id: impl Into<String>,
        sample_rate_hz: u64,
        channel_count: u8,
        batch_size: usize,
    ) -> Result<Self, CaptureError> {
        if sample_rate_hz == 0 || channel_count == 0 || channel_count > 32 || batch_size == 0 {
            return Err(CaptureError::new(
                "invalid simulated logic analyzer configuration",
            ));
        }
        Ok(Self {
            descriptor: DeviceDescriptor {
                id: DeviceId::new(id),
                name: "Simulated Logic Analyzer".to_owned(),
                kind: InstrumentKind::LogicAnalyzer,
            },
            sample_rate_hz,
            channel_count,
            batch_size,
            sample_limit: None,
            enabled_channel_mask: if channel_count == 32 {
                u32::MAX
            } else {
                (1_u32 << channel_count) - 1
            },
        })
    }

    /// Stop after emitting exactly `sample_count` samples.
    pub fn with_sample_limit(mut self, sample_count: u64) -> Result<Self, CaptureError> {
        if sample_count == 0 {
            return Err(CaptureError::new(
                "simulated capture sample count must be greater than zero",
            ));
        }
        self.sample_limit = Some(sample_count);
        Ok(self)
    }

    /// Generate data only for the selected physical channels.
    pub fn with_enabled_channel_mask(
        mut self,
        enabled_channel_mask: u32,
    ) -> Result<Self, CaptureError> {
        let available_mask = if self.channel_count == 32 {
            u32::MAX
        } else {
            (1_u32 << self.channel_count) - 1
        };
        if enabled_channel_mask == 0 || enabled_channel_mask & !available_mask != 0 {
            return Err(CaptureError::new(
                "invalid simulated logic analyzer channel selection",
            ));
        }
        self.enabled_channel_mask = enabled_channel_mask;
        Ok(self)
    }
}

impl CaptureSource for SimulatedLogicAnalyzer {
    fn descriptor(&self) -> DeviceDescriptor {
        self.descriptor.clone()
    }

    fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
        context.synchronize_start()?;
        let mut deadline = Instant::now();
        let mut sample_index = 0_u64;
        let mut sequence = 0_u64;

        while !context.is_cancelled() {
            let remaining = self
                .sample_limit
                .map(|limit| limit.saturating_sub(sample_index));
            if remaining == Some(0) {
                break;
            }
            let batch_size = remaining.map_or(self.batch_size, |remaining| {
                usize::try_from(remaining)
                    .unwrap_or(usize::MAX)
                    .min(self.batch_size)
            });
            let captured_at = context.elapsed();
            let mut samples = Vec::with_capacity(batch_size);
            for _ in 0..batch_size {
                let mut sample = 0_u32;
                for channel in 0..self.channel_count {
                    let half_period =
                        (self.sample_rate_hz / (2_000 * (u64::from(channel) + 1))).max(1);
                    if self.enabled_channel_mask & (1 << channel) != 0
                        && (sample_index / half_period) % 2 == 1
                    {
                        sample |= 1 << channel;
                    }
                }
                samples.push(sample);
                sample_index += 1;
            }

            context.emit(
                sequence,
                captured_at,
                CaptureBatch::Logic(LogicBatch {
                    sample_rate_hz: self.sample_rate_hz,
                    channel_count: self.channel_count,
                    samples,
                }),
            )?;
            sequence += 1;
            deadline += Duration::from_secs_f64(batch_size as f64 / self.sample_rate_hz as f64);
            context.wait_until(deadline);
        }
        Ok(())
    }
}

pub struct SimulatedOscilloscope {
    descriptor: DeviceDescriptor,
    sample_rate_hz: u64,
    channel_count: usize,
    batch_size: usize,
    sample_limit: Option<u64>,
}

impl SimulatedOscilloscope {
    pub fn new(
        id: impl Into<String>,
        sample_rate_hz: u64,
        channel_count: usize,
        batch_size: usize,
    ) -> Result<Self, CaptureError> {
        if sample_rate_hz == 0 || channel_count == 0 || batch_size == 0 {
            return Err(CaptureError::new(
                "invalid simulated oscilloscope configuration",
            ));
        }
        Ok(Self {
            descriptor: DeviceDescriptor {
                id: DeviceId::new(id),
                name: "Simulated Oscilloscope".to_owned(),
                kind: InstrumentKind::Oscilloscope,
            },
            sample_rate_hz,
            channel_count,
            batch_size,
            sample_limit: None,
        })
    }

    /// Stop after emitting exactly `sample_count` samples per channel.
    pub fn with_sample_limit(mut self, sample_count: u64) -> Result<Self, CaptureError> {
        if sample_count == 0 {
            return Err(CaptureError::new(
                "simulated capture sample count must be greater than zero",
            ));
        }
        self.sample_limit = Some(sample_count);
        Ok(self)
    }
}

impl CaptureSource for SimulatedOscilloscope {
    fn descriptor(&self) -> DeviceDescriptor {
        self.descriptor.clone()
    }

    fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
        let batch_period =
            Duration::from_secs_f64(self.batch_size as f64 / self.sample_rate_hz as f64);
        context.synchronize_start()?;
        let mut deadline = Instant::now();
        let mut sample_index = 0_u64;
        let mut sequence = 0_u64;

        while !context.is_cancelled() {
            let remaining = self
                .sample_limit
                .map_or(self.batch_size as u64, |limit| limit - sample_index);
            if remaining == 0 {
                break;
            }
            let batch_size = remaining.min(self.batch_size as u64) as usize;
            let captured_at = context.elapsed();
            let mut channels = (0..self.channel_count)
                .map(|_| Vec::with_capacity(batch_size))
                .collect::<Vec<_>>();
            for offset in 0..batch_size {
                let time = (sample_index + offset as u64) as f32 / self.sample_rate_hz as f32;
                for (channel_index, channel) in channels.iter_mut().enumerate() {
                    let frequency = 1_000.0 + 500.0 * channel_index as f32;
                    let phase = channel_index as f32 * 0.25;
                    channel.push((TAU * frequency * time + phase).sin());
                }
            }
            sample_index += batch_size as u64;

            context.emit(
                sequence,
                captured_at,
                CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: self.sample_rate_hz,
                    trigger_sample_index: None,
                    channels,
                }),
            )?;
            sequence += 1;
            deadline += if batch_size == self.batch_size {
                batch_period
            } else {
                Duration::from_secs_f64(batch_size as f64 / self.sample_rate_hz as f64)
            };
            context.wait_until(deadline);
        }
        Ok(())
    }
}

/// Deterministic complex tone source for RF display, recording, and DSP tests.
pub struct SimulatedRadioReceiver {
    descriptor: DeviceDescriptor,
    sample_rate_hz: u64,
    center_frequency_hz: u64,
    tone_offset_hz: i64,
    amplitude: i8,
    batch_size: usize,
    sample_limit: Option<u64>,
}

impl SimulatedRadioReceiver {
    pub fn new(
        id: impl Into<String>,
        sample_rate_hz: u64,
        center_frequency_hz: u64,
        tone_offset_hz: i64,
        batch_size: usize,
    ) -> Result<Self, CaptureError> {
        if sample_rate_hz == 0 || batch_size == 0 {
            return Err(CaptureError::new(
                "invalid simulated radio receiver configuration",
            ));
        }
        if tone_offset_hz.unsigned_abs().saturating_mul(2) >= sample_rate_hz {
            return Err(CaptureError::new(
                "simulated radio tone must remain inside the Nyquist range",
            ));
        }
        Ok(Self {
            descriptor: DeviceDescriptor {
                id: DeviceId::new(id),
                name: "Simulated Radio Receiver".to_owned(),
                kind: InstrumentKind::RadioReceiver,
            },
            sample_rate_hz,
            center_frequency_hz,
            tone_offset_hz,
            amplitude: 100,
            batch_size,
            sample_limit: None,
        })
    }

    pub fn with_amplitude(mut self, amplitude: i8) -> Result<Self, CaptureError> {
        if amplitude <= 0 {
            return Err(CaptureError::new(
                "simulated radio amplitude must be positive",
            ));
        }
        self.amplitude = amplitude;
        Ok(self)
    }

    pub fn with_sample_limit(mut self, sample_count: u64) -> Result<Self, CaptureError> {
        if sample_count == 0 {
            return Err(CaptureError::new(
                "simulated capture sample count must be greater than zero",
            ));
        }
        self.sample_limit = Some(sample_count);
        Ok(self)
    }
}

impl CaptureSource for SimulatedRadioReceiver {
    fn descriptor(&self) -> DeviceDescriptor {
        self.descriptor.clone()
    }

    fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
        context.synchronize_start()?;
        let mut deadline = Instant::now();
        let mut sample_index = 0_u64;
        let mut sequence = 0_u64;

        while !context.is_cancelled() {
            let remaining = self
                .sample_limit
                .map_or(self.batch_size as u64, |limit| limit - sample_index);
            if remaining == 0 {
                break;
            }
            let batch_size = remaining.min(self.batch_size as u64) as usize;
            let captured_at = context.elapsed();
            let mut samples = Vec::with_capacity(batch_size);
            for offset in 0..batch_size {
                let index = sample_index + offset as u64;
                let phase = std::f64::consts::TAU * self.tone_offset_hz as f64 * index as f64
                    / self.sample_rate_hz as f64;
                let amplitude = f64::from(self.amplitude);
                samples.push(Iq8 {
                    i: (amplitude * phase.cos()).round() as i8,
                    q: (amplitude * phase.sin()).round() as i8,
                });
            }

            context.emit(
                sequence,
                captured_at,
                CaptureBatch::Iq(IqBatch {
                    sample_rate_hz: self.sample_rate_hz,
                    center_frequency_hz: self.center_frequency_hz,
                    first_sample_index: sample_index,
                    discontinuity: false,
                    samples,
                }),
            )?;
            sample_index += batch_size as u64;
            sequence += 1;
            deadline += Duration::from_secs_f64(batch_size as f64 / self.sample_rate_hz as f64);
            context.wait_until(deadline);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_core::{CaptureCoordinator, CaptureEvent};

    #[test]
    fn radio_tone_is_deterministic_across_batch_boundaries() {
        let source = SimulatedRadioReceiver::new("radio", 1_000, 100_000_000, 250, 3)
            .unwrap()
            .with_sample_limit(8)
            .unwrap();
        let session = CaptureCoordinator::start(vec![Box::new(source)]).unwrap();
        let mut batches = Vec::new();
        loop {
            match session.recv_timeout(Duration::from_secs(1)).unwrap() {
                CaptureEvent::Frame(frame) => {
                    let CaptureBatch::Iq(batch) = frame.payload else {
                        panic!("simulated radio should emit IQ");
                    };
                    batches.push(batch);
                }
                CaptureEvent::SourceStopped { .. } => break,
                CaptureEvent::SourceFailed { error, .. } => {
                    panic!("simulated radio failed: {error}")
                }
            }
        }
        session.stop().unwrap();

        assert_eq!(
            batches
                .iter()
                .map(|batch| batch.first_sample_index)
                .collect::<Vec<_>>(),
            [0, 3, 6]
        );
        assert!(batches.iter().all(|batch| !batch.discontinuity));
        assert_eq!(
            batches
                .into_iter()
                .flat_map(|batch| batch.samples)
                .collect::<Vec<_>>(),
            [
                Iq8 { i: 100, q: 0 },
                Iq8 { i: 0, q: 100 },
                Iq8 { i: -100, q: 0 },
                Iq8 { i: 0, q: -100 },
                Iq8 { i: 100, q: 0 },
                Iq8 { i: 0, q: 100 },
                Iq8 { i: -100, q: 0 },
                Iq8 { i: 0, q: -100 },
            ]
        );
    }

    #[test]
    fn radio_rejects_a_tone_at_or_beyond_nyquist() {
        assert!(SimulatedRadioReceiver::new("radio", 1_000, 100_000_000, 500, 16).is_err());
        assert!(SimulatedRadioReceiver::new("radio", 1_000, 100_000_000, -500, 16).is_err());
    }
}
