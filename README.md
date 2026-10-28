# My DSView

A Rust reimplementation of DSView designed to capture from a logic analyzer and an oscilloscope concurrently. The original C/C++ application in `../DSView` is retained as a protocol and behavior reference.

## Current milestone

The repository contains the first vertical slice of the new capture engine:

- independent capture workers for every instrument;
- a shared monotonic timeline across workers;
- typed logic and oscilloscope sample batches;
- coordinated cancellation and worker cleanup;
- simulated instruments and a CLI demonstrating simultaneous capture;
- finite buffered capture from DSLogic U3Pro16 at supported 16-channel rates.

DSCope acquisition and the graphical interface are the next milestones.

Initial USB discovery supports DSLogic U3Pro16 (`2a0e:002a`) and DSCope U3P100 (`2a0e:002b`) at HighSpeed or SuperSpeed. Discovery is read-only; explicit preparation and capture commands perform the state-changing operations.

The device listing reports the negotiated USB link speed. USB 3 SuperSpeed is recommended; USB 2 HighSpeed produces a reduced-performance warning, and slower links are reported as unsupported.

The hardware probe validates descriptors, claims interface 0, and reads firmware
and FPGA status. A bounded, idempotent FPGA loader can configure each model from
the original GPL-compatible bitstream resources. DSLogic finite capture uses the
same device-independent `CaptureSource` interface as the simulators.

## Run the simulator

Install Rust 1.75 or newer, then run:

```sh
cargo run -p dsview-cli -- 2
cargo run -p dsview-cli -- list
cargo run -p dsview-cli -- probe
cargo run -p dsview-cli -- load-fpga ../DSView/DSView/res
cargo run -p dsview-cli -- prepare ../DSView/DSView/res
cargo run -p dsview-cli -- capture-logic ../DSView/DSView/res 1
```

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
- `crates/dsview-usb`: USB discovery, claimed sessions, control transport, FPGA loading, and DSLogic capture.
- `apps/dsview-cli`: simultaneous-capture development harness.
- `docs/architecture.md`: design constraints and planned hardware integration.

The project is licensed under GPL-3.0-or-later to remain compatible with the original DSView codebase.
