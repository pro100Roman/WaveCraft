# DSView

A Rust reimplementation of DSView designed to capture from a logic analyzer and an oscilloscope concurrently. The original C/C++ application in `../DSView` is retained as a protocol and behavior reference.

## Current milestone

The repository contains the first vertical slice of the new capture engine:

- independent capture workers for every instrument;
- a shared monotonic timeline across workers;
- typed logic and oscilloscope sample batches;
- coordinated cancellation and worker cleanup;
- simulated instruments and a CLI demonstrating simultaneous capture;
- finite buffered capture from DSLogic U3Pro16 at supported 16-channel rates;
- finite two-channel capture from DSCope U3P100 at 1 MHz;
- concurrent physical capture from one DSLogic and one DSCope on a shared timeline;
- a native desktop capture workbench with device selection and waveform views.

Broader DSCope ranges and sample rates, interactive timeline navigation,
recording, protocol decoders, and automation interfaces are the next
milestones.

The required end state includes a cross-platform decoder plugin system. SPI
and I2C will be first-party decoders, while third-party decoders run through a
versioned out-of-process protocol so they can be implemented independently in
Rust, Python, or another language without sharing the application's native ABI.
Decoded events remain aligned to source sample ranges and the shared capture
timeline.

Hardware discovery, configuration, capture, recording, and decoding must also
be controllable without the GUI. The CLI will provide stable JSON/NDJSON output
and complete raw capture recording suitable for `subprocess` automation. The
recording path receives every captured frame and is independent of the GUI's
bounded waveform display cache. A local service API and thin Python client can
reuse the same application command layer for long-lived and streaming
workflows; automation must never depend on parsing human-readable status text.

Initial USB discovery supports DSLogic U3Pro16 (`2a0e:002a`) and DSCope U3P100 (`2a0e:002b`) at HighSpeed or SuperSpeed. Discovery is read-only; explicit preparation and capture commands perform the state-changing operations.

The device listing reports the negotiated USB link speed. USB 3 SuperSpeed is recommended; USB 2 HighSpeed produces a reduced-performance warning, and slower links are reported as unsupported.

The hardware probe validates descriptors, claims interface 0, and reads firmware
and FPGA status. A bounded, idempotent FPGA loader can configure each model from
the original GPL-compatible bitstream resources. DSLogic and DSCope finite
capture use the same device-independent `CaptureSource` interface as the
simulators.

## Run the simulator

Install Rust 1.76 or newer, then run:

```sh
cargo run -p dsview-cli -- 2
cargo run -p dsview-cli -- list
cargo run -p dsview-cli -- probe
cargo run -p dsview-cli -- load-fpga resources/fpga
cargo run -p dsview-cli -- prepare resources/fpga
cargo run -p dsview-cli -- capture-logic resources/fpga 1
cargo run -p dsview-cli -- capture-scope resources/fpga
cargo run -p dsview-cli -- capture-usb resources/fpga
```

Here `<resource-directory>` contains `DSLogicU3Pro16.bin` and
`DSCopeU3P100.bin`. Unmodified copies from the original DSView project are
included under `resources/fpga`; only preparation and hardware-capture commands
need them.

`list` performs read-only discovery. `probe` opens each supported instrument,
validates its interface and bulk endpoints, reads the firmware version, and
reports the FPGA hardware status through a reusable claimed-device transport.
It also verifies that interface 0 can be claimed and released. It does not
upload firmware or FPGA data and does not start acquisition.

`load-fpga <resource-directory>` loads the model-specific bitstream only when
the device reports that its FPGA is not configured. The loader validates the
raw Xilinx header, bounds every status wait, checks exact transfer lengths, and
restores safe control-line values if configuration fails.

`prepare <resource-directory>` performs the complete idempotent initialization
path and requires compatible firmware `2.x`, a configured FPGA, and HDL version
`0x0e`. A successful result means the instrument is ready for acquisition.

`capture-logic <resource-directory> [seconds]` prepares the first connected
DSLogic U3Pro16 and performs a finite, trigger-free, 16-channel capture at
1 MHz. The default duration is one second. The driver sends the packed FPGA
settings, validates the 1 KiB trigger header, transposes the channel-major
64-sample wire blocks, and emits typed `LogicBatch` frames.

`capture-scope <resource-directory>` prepares the first connected DSCope
U3P100 and performs a finite two-channel capture at 1 MHz with 1 V/div scaling.
The driver configures the 1 GHz front-end clock and ADC, validates and applies
each channel's factory VGA and offset calibration from flash, queues both USB
reads before starting acquisition, validates the trigger header and measurement
footer, and emits typed `ScopeBatch` frames. The device-reported valid length is
authoritative and can be smaller than the requested one-million sample buffer.

`capture-usb <resource-directory>` prepares one instrument of each model. Each
worker initializes and arms its device independently, then waits at a
failure-safe gate immediately before issuing the hardware start command. The
command reports per-device frame and sample totals, voltage ranges, timelines,
and the observed software start skew. This skew is not a hardware
synchronization guarantee; an external trigger or clock is still needed for
exact sample alignment.

## Run the desktop application

Launch the native capture workbench with:

```sh
cargo run -p dsview-gui
```

The application discovers supported USB instruments without blocking the UI,
lets you select any detected devices and edit the FPGA resource directory, then
prepares and captures from the selected sources concurrently. It displays all
16 digital channels, both calibrated scope channels, per-device sample totals,
errors, and shared-timeline start skew. Waveform buffers retain the newest 8,192
samples so rendering remains bounded; this display limit does not truncate data
written to a capture file.

The GUI uses native window backends on macOS and Windows and enables both
Wayland and X11 on Linux. Vendored libusb keeps the USB transport consistent
across the three platforms. CI builds and tests the workspace on macOS, Ubuntu,
and Windows, with a separate Rust 1.76 compatibility check.

The USB transport also provides cancellable bulk-IN reads. It polls with a
50 ms timeout, preserves partial transfers reported by libusb, retries timeout
and interrupted operations, and distinguishes cancellation from USB failures.
The opt-in hardware check is:

```sh
cargo test -p dsview-usb --test hardware -- --ignored --nocapture
```

The optional numeric argument is the simulated capture duration in seconds.
Run all checks with:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Workspace layout

- `crates/dsview-core`: device-independent capture types and concurrency.
- `crates/dsview-sim`: deterministic simulated logic and scope sources.
- `crates/dsview-usb`: USB discovery, claimed sessions, control transport, FPGA loading, and DSLogic/DSCope capture.
- `apps/dsview-cli`: simultaneous-capture development harness.
- `apps/dsview-gui`: native device selection, capture controls, status, and waveform rendering.
- `docs/architecture.md`: design constraints and planned hardware integration.

The project is licensed under GPL-3.0-or-later to remain compatible with the original DSView codebase.
