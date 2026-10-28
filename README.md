# My DSView

A Rust reimplementation of DSView designed to capture from a logic analyzer and an oscilloscope concurrently. The original C/C++ application in `../DSView` is retained as a protocol and behavior reference.

## Current milestone

The repository contains the first vertical slice of the new capture engine:

- independent capture workers for every instrument;
- a shared monotonic timeline across workers;
- typed logic and oscilloscope sample batches;
- coordinated cancellation and worker cleanup;
- simulated instruments and a CLI demonstrating simultaneous capture.

USB capture support and the graphical interface are the next milestones.

Initial USB discovery supports DSLogic U3Pro16 (`2a0e:002a`) and DSCope U3P100 (`2a0e:002b`) at HighSpeed or SuperSpeed. Discovery is read-only; firmware upload and acquisition are not enabled yet.

The device listing reports the negotiated USB link speed. USB 3 SuperSpeed is recommended; USB 2 HighSpeed produces a reduced-performance warning, and slower links are reported as unsupported.

## Run the simulator

Install Rust 1.75 or newer, then run:

```sh
cargo run -p dsview-cli -- 2
cargo run -p dsview-cli -- list
cargo run -p dsview-cli -- probe
```

`list` performs read-only discovery. `probe` opens each supported instrument,
validates its interface and bulk endpoints, reads the firmware version, and
verifies that interface 0 can be claimed and released. It does not upload
firmware or FPGA data and does not start acquisition.

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
- `crates/dsview-usb`: target-device profiles and read-only USB discovery.
- `apps/dsview-cli`: simultaneous-capture development harness.
- `docs/architecture.md`: design constraints and planned hardware integration.

The project is licensed under GPL-3.0-or-later to remain compatible with the original DSView codebase.
