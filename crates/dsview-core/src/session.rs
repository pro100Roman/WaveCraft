use crate::{CaptureBatch, CaptureError, CaptureEvent, CaptureFrame, CaptureSource, DeviceId};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Barrier};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Services available to one capture worker.
pub struct CaptureContext {
    device_id: DeviceId,
    timeline_origin: Instant,
    cancelled: Arc<AtomicBool>,
    sender: Sender<CaptureEvent>,
}

impl CaptureContext {
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn elapsed(&self) -> Duration {
        self.timeline_origin.elapsed()
    }

    pub fn emit(
        &self,
        sequence: u64,
        captured_at: Duration,
        payload: CaptureBatch,
    ) -> Result<(), CaptureError> {
        self.sender
            .send(CaptureEvent::Frame(CaptureFrame {
                device_id: self.device_id.clone(),
                sequence,
                captured_at,
                payload,
            }))
            .map_err(|_| CaptureError::new("capture event receiver was closed"))
    }

    /// Sleep in short intervals so stop requests remain responsive.
    pub fn wait_until(&self, deadline: Instant) {
        const MAX_SLEEP: Duration = Duration::from_millis(5);

        while !self.is_cancelled() {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            thread::sleep(remaining.min(MAX_SLEEP));
        }
    }
}

pub struct CaptureCoordinator;

impl CaptureCoordinator {
    /// Start all sources behind one barrier and place their data on one timeline.
    pub fn start(sources: Vec<Box<dyn CaptureSource>>) -> Result<CaptureSession, CaptureError> {
        if sources.is_empty() {
            return Err(CaptureError::new("at least one capture source is required"));
        }

        let descriptors = sources
            .iter()
            .map(|source| source.descriptor())
            .collect::<Vec<_>>();
        let unique_ids = descriptors
            .iter()
            .map(|descriptor| descriptor.id.clone())
            .collect::<HashSet<_>>();
        if unique_ids.len() != descriptors.len() {
            return Err(CaptureError::new(
                "capture source identifiers must be unique",
            ));
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let timeline_origin = Instant::now();
        let start_barrier = Arc::new(Barrier::new(sources.len() + 1));
        let (sender, receiver) = mpsc::channel();
        let mut workers = Vec::with_capacity(sources.len());

        for (source, descriptor) in sources.into_iter().zip(descriptors.iter()) {
            let worker_cancelled = Arc::clone(&cancelled);
            let worker_barrier = Arc::clone(&start_barrier);
            let worker_sender = sender.clone();
            let device_id = descriptor.id.clone();

            workers.push(thread::spawn(move || {
                let context = CaptureContext {
                    device_id: device_id.clone(),
                    timeline_origin,
                    cancelled: Arc::clone(&worker_cancelled),
                    sender: worker_sender.clone(),
                };

                worker_barrier.wait();
                if let Err(error) = source.run(context) {
                    if !worker_cancelled.load(Ordering::Acquire) {
                        let _ = worker_sender.send(CaptureEvent::SourceFailed {
                            device_id: device_id.clone(),
                            error,
                        });
                    }
                }
                let _ = worker_sender.send(CaptureEvent::SourceStopped { device_id });
            }));
        }
        drop(sender);

        start_barrier.wait();
        Ok(CaptureSession {
            descriptors,
            receiver,
            cancelled,
            workers,
        })
    }
}

pub struct CaptureSession {
    descriptors: Vec<crate::DeviceDescriptor>,
    receiver: Receiver<CaptureEvent>,
    cancelled: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl CaptureSession {
    pub fn descriptors(&self) -> &[crate::DeviceDescriptor] {
        &self.descriptors
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<CaptureEvent, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    pub fn try_recv(&self) -> Result<CaptureEvent, TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn request_stop(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn stop(mut self) -> Result<(), CaptureError> {
        self.request_stop();
        self.join_workers()
    }

    fn join_workers(&mut self) -> Result<(), CaptureError> {
        let mut worker_panicked = false;
        for worker in self.workers.drain(..) {
            worker_panicked |= worker.join().is_err();
        }
        if worker_panicked {
            Err(CaptureError::new("a capture worker panicked"))
        } else {
            Ok(())
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.join_workers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceDescriptor, InstrumentKind, LogicBatch, ScopeBatch};

    struct OneFrameSource {
        descriptor: DeviceDescriptor,
    }

    impl CaptureSource for OneFrameSource {
        fn descriptor(&self) -> DeviceDescriptor {
            self.descriptor.clone()
        }

        fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
            let payload = match self.descriptor.kind {
                InstrumentKind::LogicAnalyzer => CaptureBatch::Logic(LogicBatch {
                    sample_rate_hz: 1_000,
                    channel_count: 1,
                    samples: vec![0, 1],
                }),
                InstrumentKind::Oscilloscope => CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 1_000,
                    channels: vec![vec![0.0, 1.0]],
                }),
            };
            context.emit(0, context.elapsed(), payload)
        }
    }

    fn source(id: &str, kind: InstrumentKind) -> Box<dyn CaptureSource> {
        Box::new(OneFrameSource {
            descriptor: DeviceDescriptor {
                id: DeviceId::new(id),
                name: id.to_owned(),
                kind,
            },
        })
    }

    #[test]
    fn runs_logic_and_scope_sources_in_one_session() {
        let session = CaptureCoordinator::start(vec![
            source("logic", InstrumentKind::LogicAnalyzer),
            source("scope", InstrumentKind::Oscilloscope),
        ])
        .expect("session should start");

        let mut frame_ids = HashSet::new();
        while frame_ids.len() < 2 {
            if let CaptureEvent::Frame(frame) = session
                .recv_timeout(Duration::from_secs(1))
                .expect("source should emit a frame")
            {
                frame_ids.insert(frame.device_id);
            }
        }

        assert!(frame_ids.contains(&DeviceId::new("logic")));
        assert!(frame_ids.contains(&DeviceId::new("scope")));
        session.stop().expect("workers should stop cleanly");
    }

    #[test]
    fn rejects_duplicate_device_ids() {
        let result = CaptureCoordinator::start(vec![
            source("duplicate", InstrumentKind::LogicAnalyzer),
            source("duplicate", InstrumentKind::Oscilloscope),
        ]);

        assert!(result.is_err());
    }
}
