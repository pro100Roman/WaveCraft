use crate::CaptureContext;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;

/// Stable identifier for one connected instrument.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for DeviceId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstrumentKind {
    LogicAnalyzer,
    Oscilloscope,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceDescriptor {
    pub id: DeviceId,
    pub name: String,
    pub kind: InstrumentKind,
}

/// One time-ordered batch of packed digital samples.
#[derive(Clone, Debug, PartialEq)]
pub struct LogicBatch {
    pub sample_rate_hz: u64,
    pub channel_count: u8,
    /// One word per sampling instant; bit N contains logic channel N.
    pub samples: Vec<u32>,
}

/// One time-ordered batch of voltage samples.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeBatch {
    pub sample_rate_hz: u64,
    /// Channel-major voltage samples. All channels must have equal length.
    pub channels: Vec<Vec<f32>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CaptureBatch {
    Logic(LogicBatch),
    Scope(ScopeBatch),
}

impl CaptureBatch {
    pub fn sample_count(&self) -> usize {
        match self {
            Self::Logic(batch) => batch.samples.len(),
            Self::Scope(batch) => batch.channels.first().map_or(0, Vec::len),
        }
    }
}

/// A batch positioned on the coordinator's shared monotonic timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureFrame {
    pub device_id: DeviceId,
    pub sequence: u64,
    pub captured_at: Duration,
    pub payload: CaptureBatch,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CaptureEvent {
    Frame(CaptureFrame),
    SourceFailed {
        device_id: DeviceId,
        error: CaptureError,
    },
    SourceStopped {
        device_id: DeviceId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureError {
    message: String,
}

impl CaptureError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for CaptureError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl Error for CaptureError {}

impl From<&str> for CaptureError {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for CaptureError {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Blocking capture producer run on its own worker thread.
///
/// Implementations should periodically check `context.is_cancelled()` and use
/// the shared context timestamp for every emitted frame.
pub trait CaptureSource: Send + 'static {
    fn descriptor(&self) -> DeviceDescriptor;

    fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError>;
}
