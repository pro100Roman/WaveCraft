# Capture Architecture

My DSView models every physical instrument as an independent `CaptureSource`.
The `CaptureCoordinator` starts one worker per source, synchronizes prepared
workers immediately before acquisition, and assigns all frames to one monotonic
process timeline. This removes the original application's global “active
device” restriction.

## Data flow

```text
DSLogic worker ── logic batches ──┐
                                 ├── CaptureSession ── UI / recorder / decoders
DSCope worker  ── scope batches ──┘
```

Each frame contains a device ID, monotonically increasing sequence number, capture timestamp, sample rate, and typed payload. Digital samples are bit-packed into `u32`; scope samples are channel-major `f32` voltages.

## Synchronization

Capture workers first pass a launch barrier, then initialize and arm their
devices independently. A cancellable second-phase gate releases every worker
only after all devices are ready to issue their hardware start commands. This
minimizes software start skew without allowing a preparation failure to strand
another worker at the gate. Timestamps make the remaining skew visible. USB
completion time is not a hardware sample timestamp. Exact cross-device
alignment will require either:

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
The idempotent preparation path then validates FPGA HDL version `0x0e` before
declaring an instrument ready for acquisition.

Bulk-IN acquisition uses queued asynchronous libusb transfers so reads are
already pending when an instrument starts. The DSLogic U3Pro16 driver implements
finite 16-channel buffered capture, including packed FPGA settings,
arm/start/stop commands, trigger-header validation, and conversion of
channel-major 64-sample bitmaps into `LogicBatch` sample words. Cancellation
forces the FPGA read path ready before stopping it.

The DSCope U3P100 driver implements an initial finite two-channel mode at 1 MHz.
It configures the 1 GHz front-end clock and ADC, validates and applies the
per-channel 1 V/div VGA and offset calibration stored in flash, validates the
trigger header and measurement footer, and converts interleaved ADC bytes into
channel-major `ScopeBatch` voltages. It trusts the footer's valid sample length
rather than padding to the requested buffer size. Broader sample-rate/range
support remains future work; driver code stays independent of the UI.

The `capture-usb` development command places one physical DSLogic source and one
physical DSCope source in the same `CaptureSession`. Both workers prepare and
arm independently, synchronize immediately before their USB start commands,
emit on the common monotonic timeline, and stop independently. The reported
software start skew remains distinct from exact hardware sample alignment.

The original GPL DSView implementation under `../DSView/libsigrok4DSL/hardware/DSL` is the behavior reference for vendor IDs, firmware loading, command transfers, and bulk endpoint framing.
