# Capture Architecture

My DSView models every physical instrument as an independent `CaptureSource`. The `CaptureCoordinator` starts one worker per source behind a shared barrier and assigns all frames to one monotonic process timeline. This removes the original application's global “active device” restriction.

## Data flow

```text
DSLogic worker ── logic batches ──┐
                                 ├── CaptureSession ── UI / recorder / decoders
DSCope worker  ── scope batches ──┘
```

Each frame contains a device ID, monotonically increasing sequence number, capture timestamp, sample rate, and typed payload. Digital samples are bit-packed into `u32`; scope samples are channel-major `f32` voltages.

## Synchronization

The start barrier minimizes software start skew, while timestamps make remaining skew visible. USB completion time is not a hardware sample timestamp. Exact cross-device alignment will require either:

- a shared external trigger recorded by both instruments; or
- a supported hardware clock/trigger link.

The synchronization layer will therefore preserve raw device timing metadata instead of pretending host arrival times are exact.

## Hardware integration plan

The `dsview-usb` crate owns model profiles, read-only discovery, claimed device
sessions, and the vendor control transport. It initially targets DSLogic U3Pro16
(VID/PID `2a0e:002a`) and DSCope U3P100 (`2a0e:002b`) on interface 0, with bulk
OUT endpoint `0x02` and bulk IN endpoint `0x86` reserved for the acquisition
layer. The current transport reads firmware and hardware status and loads the
model-specific FPGA bitstream with bounded waits and exact transfer checks.

The next driver step is HDL-version validation followed by cancellable bulk
transfers. DSLogic and DSCope protocol implementations will translate USB
buffers into the existing core batches. Driver code must not depend on the UI
and must stop promptly when the capture context is cancelled.

The original GPL DSView implementation under `../DSView/libsigrok4DSL/hardware/DSL` is the behavior reference for vendor IDs, firmware loading, command transfers, and bulk endpoint framing.
