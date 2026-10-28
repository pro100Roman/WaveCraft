# Capture Architecture

DSView models every physical instrument as an independent `CaptureSource`.
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

The native `dsview-gui` application consumes the same `CaptureSession` event
stream as the CLI. USB discovery and device preparation run on background
threads, while the UI polls typed capture events without blocking the window
loop. It keeps bounded display buffers and paints digital and scope traces
without introducing UI dependencies into the core or hardware crates.

## Protocol decoding and plugins

Protocol decoding is a consumer of capture data, never a hardware-driver
responsibility. A decoder receives channel mappings, sample-rate metadata, and
ordered logic chunks, then emits typed events containing a decoder ID, sample
range, summary, and structured fields. The sample range is authoritative so
the GUI, recorder, CLI, and automation API can all place the same decoded event
on the capture timeline.

First-party SPI and I2C decoders will implement the same decoder contract as
third-party extensions. External plugins use a versioned, out-of-process
protocol rather than Rust dynamic libraries. This avoids an unstable Rust ABI,
works consistently on macOS, Linux, and Windows, permits Python plugins, and
isolates plugin crashes from capture workers and the GUI. The transport uses
framed messages with JSON metadata and bounded binary logic chunks; protocol
version negotiation, capabilities, configuration schemas, cancellation,
timeouts, and maximum message sizes are mandatory parts of the contract.

Plugin discovery is manifest-based. Manifests declare the executable, protocol
version, decoder IDs, channel requirements, and configuration schema. The host
validates manifests before launch, requires explicit enablement, captures
plugin diagnostics separately from protocol output, and terminates plugins
that violate framing, resource, or timeout limits. Decoders must be testable
against deterministic recorded logic without USB hardware.

## CLI and automation API

GUI actions call an application service layer shared with automation clients;
the GUI must not own device-control policy. The CLI exposes discovery,
preparation, capture, recording, and decoding with stable command names,
explicit device IDs, documented exit codes, and schema-versioned JSON or
NDJSON. Machine output goes to stdout and diagnostics go to stderr. Raw sample
data is written to a versioned recording or explicitly requested stream rather
than embedded in a final human-readable summary.

Recording subscribes directly to the capture event stream and writes every
frame incrementally; it never reads back from the GUI's bounded display
buffers. The primary recording format preserves all logic sample words, all
scope channel samples, device descriptors, sample rates, frame sequences,
shared-timeline timestamps, capture configuration, calibration identity, start
skew, errors, and clean or interrupted completion state. Streaming writes keep
memory bounded, and per-chunk lengths and checksums make truncated or corrupted
files detectable. Unknown metadata fields remain skippable for forward
compatibility.

The native recording is the lossless source of truth. Optional VCD, CSV, or
other interchange exporters operate from that recording and may intentionally
represent only compatible signal types; they must never silently replace or
truncate the complete native data set.

The initial Python integration invokes the CLI as a subprocess and consumes
NDJSON incrementally, which works without a resident service or platform-
specific bindings. A later local daemon can expose the same service operations
for long-running sessions and live streaming, with a thin Python package as a
convenience client. The daemon must bind locally by default, support explicit
cancellation and backpressure, and preserve the same device, capture, decoder,
and error schemas as the CLI.

Automation compatibility is a public contract: schema changes require a
version bump, unknown fields must be tolerated, and integration tests must run
representative Python clients against simulated devices on all supported
operating systems.

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
