use crate::{CaptureBatch, CaptureError, CaptureEvent, CaptureFrame, CaptureSource, DeviceId};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Services available to one capture worker.
pub struct CaptureContext {
    device_id: DeviceId,
    timeline_origin: Instant,
    cancelled: Arc<AtomicBool>,
    start_gate: Arc<StartGate>,
    start_synchronized: Arc<AtomicBool>,
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

    /// Mark this source ready and wait until every source is ready to issue its
    /// hardware start command.
    pub fn synchronize_start(&self) -> Result<(), CaptureError> {
        if self.start_synchronized.swap(true, Ordering::AcqRel) {
            return Err(CaptureError::new(format!(
                "capture source {} synchronized its start more than once",
                self.device_id
            )));
        }
        self.start_gate.wait(&self.cancelled)
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

struct StartGate {
    state: Mutex<StartGateState>,
    wake: Condvar,
}

struct StartGateState {
    expected: usize,
    ready: usize,
    released: bool,
    aborted: bool,
}

impl StartGate {
    fn new(expected: usize) -> Self {
        Self {
            state: Mutex::new(StartGateState {
                expected,
                ready: 0,
                released: false,
                aborted: false,
            }),
            wake: Condvar::new(),
        }
    }

    fn wait(&self, cancelled: &AtomicBool) -> Result<(), CaptureError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CaptureError::new("capture start gate was poisoned"))?;
        if state.aborted || cancelled.load(Ordering::Acquire) {
            return Err(CaptureError::new("capture start was cancelled"));
        }

        state.ready += 1;
        if state.ready == state.expected {
            state.released = true;
            self.wake.notify_all();
            return Ok(());
        }

        while !state.released && !state.aborted {
            if cancelled.load(Ordering::Acquire) {
                state.aborted = true;
                self.wake.notify_all();
                break;
            }
            let (next_state, _) = self
                .wake
                .wait_timeout(state, Duration::from_millis(5))
                .map_err(|_| CaptureError::new("capture start gate was poisoned"))?;
            state = next_state;
        }

        if state.released {
            Ok(())
        } else {
            Err(CaptureError::new("capture start was cancelled"))
        }
    }

    fn abort(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.released {
            state.aborted = true;
            self.wake.notify_all();
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
        let worker_barrier = Arc::new(Barrier::new(sources.len() + 1));
        let start_gate = Arc::new(StartGate::new(sources.len()));
        let (sender, receiver) = mpsc::channel();
        let mut workers = Vec::with_capacity(sources.len());

        for (source, descriptor) in sources.into_iter().zip(descriptors.iter()) {
            let worker_cancelled = Arc::clone(&cancelled);
            let worker_barrier = Arc::clone(&worker_barrier);
            let worker_start_gate = Arc::clone(&start_gate);
            let start_synchronized = Arc::new(AtomicBool::new(false));
            let worker_start_synchronized = Arc::clone(&start_synchronized);
            let worker_sender = sender.clone();
            let device_id = descriptor.id.clone();

            workers.push(thread::spawn(move || {
                let context = CaptureContext {
                    device_id: device_id.clone(),
                    timeline_origin,
                    cancelled: Arc::clone(&worker_cancelled),
                    start_gate: Arc::clone(&worker_start_gate),
                    start_synchronized,
                    sender: worker_sender.clone(),
                };

                worker_barrier.wait();
                let mut result = source.run(context);
                if result.is_ok()
                    && !worker_start_synchronized.load(Ordering::Acquire)
                    && !worker_cancelled.load(Ordering::Acquire)
                {
                    result = Err(CaptureError::new(format!(
                        "capture source {device_id} returned without synchronizing its hardware start"
                    )));
                }
                if let Err(error) = result {
                    let first_failure = !worker_cancelled.swap(true, Ordering::AcqRel);
                    worker_start_gate.abort();
                    if first_failure {
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

        worker_barrier.wait();
        Ok(CaptureSession {
            descriptors,
            receiver,
            cancelled,
            start_gate,
            workers,
        })
    }
}

pub struct CaptureSession {
    descriptors: Vec<crate::DeviceDescriptor>,
    receiver: Receiver<CaptureEvent>,
    cancelled: Arc<AtomicBool>,
    start_gate: Arc<StartGate>,
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
        self.start_gate.abort();
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
        preparation_delay: Duration,
    }

    impl CaptureSource for OneFrameSource {
        fn descriptor(&self) -> DeviceDescriptor {
            self.descriptor.clone()
        }

        fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
            thread::sleep(self.preparation_delay);
            let payload = match self.descriptor.kind {
                InstrumentKind::LogicAnalyzer => CaptureBatch::Logic(LogicBatch {
                    sample_rate_hz: 1_000,
                    channel_count: 1,
                    samples: vec![0, 1],
                }),
                InstrumentKind::Oscilloscope => CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 1_000,
                    trigger_sample_index: None,
                    channels: vec![vec![0.0, 1.0]],
                }),
            };
            context.synchronize_start()?;
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
            preparation_delay: Duration::ZERO,
        })
    }

    fn delayed_source(
        id: &str,
        kind: InstrumentKind,
        preparation_delay: Duration,
    ) -> Box<dyn CaptureSource> {
        Box::new(OneFrameSource {
            descriptor: DeviceDescriptor {
                id: DeviceId::new(id),
                name: id.to_owned(),
                kind,
            },
            preparation_delay,
        })
    }

    struct PreparationFailureSource {
        descriptor: DeviceDescriptor,
    }

    impl CaptureSource for PreparationFailureSource {
        fn descriptor(&self) -> DeviceDescriptor {
            self.descriptor.clone()
        }

        fn run(self: Box<Self>, _context: CaptureContext) -> Result<(), CaptureError> {
            Err(CaptureError::new("device preparation failed"))
        }
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

    #[test]
    fn synchronizes_sources_after_independent_preparation() {
        let session = CaptureCoordinator::start(vec![
            delayed_source(
                "fast",
                InstrumentKind::LogicAnalyzer,
                Duration::from_millis(1),
            ),
            delayed_source(
                "slow",
                InstrumentKind::Oscilloscope,
                Duration::from_millis(100),
            ),
        ])
        .expect("session should start");

        let mut timestamps = Vec::new();
        while timestamps.len() < 2 {
            if let CaptureEvent::Frame(frame) = session
                .recv_timeout(Duration::from_secs(1))
                .expect("source should emit a frame")
            {
                timestamps.push(frame.captured_at);
            }
        }
        let skew = timestamps[0].max(timestamps[1]) - timestamps[0].min(timestamps[1]);

        assert!(skew < Duration::from_millis(50), "start skew was {skew:?}");
        session.stop().expect("workers should stop cleanly");
    }

    #[test]
    fn preparation_failure_releases_sources_waiting_at_start_gate() {
        let failing = Box::new(PreparationFailureSource {
            descriptor: DeviceDescriptor {
                id: DeviceId::new("failing"),
                name: "failing".to_owned(),
                kind: InstrumentKind::LogicAnalyzer,
            },
        });
        let session = CaptureCoordinator::start(vec![
            failing,
            source("waiting", InstrumentKind::Oscilloscope),
        ])
        .expect("session should start");

        let failure = loop {
            if let CaptureEvent::SourceFailed { device_id, error } = session
                .recv_timeout(Duration::from_secs(1))
                .expect("preparation failure should be reported")
            {
                break (device_id, error);
            }
        };

        assert_eq!(failure.0, DeviceId::new("failing"));
        assert_eq!(failure.1, CaptureError::new("device preparation failed"));
        session
            .stop()
            .expect("the waiting worker should leave the start gate");
    }
}
