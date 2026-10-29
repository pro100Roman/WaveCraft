//! Deterministic simulated instruments used while USB drivers are developed.

use dsview_core::{
    CaptureBatch, CaptureContext, CaptureError, CaptureSource, DeviceDescriptor, DeviceId,
    InstrumentKind, LogicBatch, ScopeBatch,
};
use std::f32::consts::TAU;
use std::time::{Duration, Instant};

pub struct SimulatedLogicAnalyzer {
    descriptor: DeviceDescriptor,
    sample_rate_hz: u64,
    channel_count: u8,
    batch_size: usize,
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
        })
    }
}

impl CaptureSource for SimulatedLogicAnalyzer {
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
            let captured_at = context.elapsed();
            let mut samples = Vec::with_capacity(self.batch_size);
            for _ in 0..self.batch_size {
                let mut sample = 0_u32;
                for channel in 0..self.channel_count {
                    let half_period =
                        (self.sample_rate_hz / (2_000 * (u64::from(channel) + 1))).max(1);
                    if (sample_index / half_period) % 2 == 1 {
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
            deadline += batch_period;
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
        })
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
            let captured_at = context.elapsed();
            let mut channels = (0..self.channel_count)
                .map(|_| Vec::with_capacity(self.batch_size))
                .collect::<Vec<_>>();
            for offset in 0..self.batch_size {
                let time = (sample_index + offset as u64) as f32 / self.sample_rate_hz as f32;
                for (channel_index, channel) in channels.iter_mut().enumerate() {
                    let frequency = 1_000.0 + 500.0 * channel_index as f32;
                    let phase = channel_index as f32 * 0.25;
                    channel.push((TAU * frequency * time + phase).sin());
                }
            }
            sample_index += self.batch_size as u64;

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
            deadline += batch_period;
            context.wait_until(deadline);
        }
        Ok(())
    }
}
