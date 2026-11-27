//! Device-independent capture primitives for WaveCraft.

mod model;
mod session;

pub use model::{
    CaptureBatch, CaptureError, CaptureEvent, CaptureFrame, CaptureSource, DeviceDescriptor,
    DeviceId, InstrumentKind, Iq8, IqBatch, LogicBatch, PowerSample, ScopeBatch,
};
pub use session::{CaptureContext, CaptureCoordinator, CaptureSession};
