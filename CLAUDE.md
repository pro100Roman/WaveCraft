# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

WaveCraft is a Rust rewrite of DSView (logic analyzer + oscilloscope + SDR radio capture, concurrently on one timeline). `../DSView` is a read-only reference for behavior and USB protocol; keep GPL-3.0-or-later compatibility when adapting its code. See `AGENTS.md` for style, testing, and commit/PR conventions (Conventional Commits, e.g. `fix(usb): ...`).

## Commands

Run from the workspace root (Rust 1.79+). CI runs these exact steps:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release --locked
python -m unittest discover -s python/tests -v   # Python automation client
```

- Single test: `cargo test -p wavecraft-decode rejects_duplicate_ids` (or add `-- --nocapture`).
- No hardware needed: `cargo run -p wavecraft-cli -- 2` (simulated capture), `cargo run -p wavecraft-cli -- simulate 1 --output x.dsv`.
- Hardware commands (`list`, `probe`, `prepare`, `capture run ...`) need `resources/fpga` (original DSView firmware/bitstreams) and a connected device. Hardware tests must be opt-in.
- GUI: `cargo run -p wavecraft-gui`. Its `build.rs` derives the version from git/CI env vars (logic in `build_version.rs`, tested in `apps/wavecraft-gui/tests`).
- macOS packaging: `scripts/package-macos.sh --universal`; HackRF on macOS: `scripts/run-wavecraft-hackrf-macos.sh`.

## Architecture

Every instrument is a `CaptureSource` (trait in `wavecraft-core/src/model.rs`) that emits typed batches (`LogicBatch`, `ScopeBatch`, `IqBatch`) through a `CaptureContext`. `CaptureCoordinator` (`wavecraft-core/src/session.rs`) runs one worker per source, gates them at a failure-safe barrier just before the hardware start, and stamps all frames with a monotonic shared timeline; consumers read a `CaptureSession` event stream. There is deliberately no global "active device". Hardware drivers must never depend on the UI.

Crate roles:
- `wavecraft-core` – model + coordinator/session (no hardware/UI deps).
- `wavecraft-usb` – DSLogic (`dslogic.rs`) / DSCope (`dscope.rs`) drivers, discovery, probe, FPGA prepare, transport.
- `wavecraft-hackrf` – receive-only HackRF via runtime-loaded `libhackrf`; `wavecraft-sdr` – device-independent SDR abstractions; `wavecraft-sim` – deterministic simulated devices.
- `wavecraft-dsp` – channelizer, FSK/GFSK demodulation (incremental, source sample ranges preserved across batch boundaries).
- `wavecraft-decode` – layered decoding: UART/SPI/I2C transport decoders → byte framing → declarative YAML custom-protocol schemas (`schema.rs`); plus radio packet/CRC/whitening pipeline. Decoders are incremental and independent of the GUI's bounded display buffers; decoded events carry authoritative sample ranges.
- `wavecraft-radio` – radio settings/trigger types; `wavecraft-recording` – lossless CRC-checked `.dsv` recording and Saleae `.sal` import/export (never overwrites existing files).
- `wavecraft-automation` – shared command layer behind the CLI JSON/NDJSON contract and the MCP/automation surface; `python/` is a first-party `subprocess` client over the CLI. Automation must not parse human-readable text.
- `apps/wavecraft-cli`, `apps/wavecraft-gui` (egui-style native app). In the GUI, `src/lib.rs` holds the `WaveCraftApp` struct and `run_frame`; capture settings are in `acquisition/`, the `WaveCraftApp` methods in `app/`, panels and dialogs in `ui/`, waveform views in `waveform/`. Tests sit next to the module they cover; `src/tests.rs` and `src/frame_tests.rs` hold the cross-module and headless-frame tests.

Key rules: YAML protocol schemas never touch hardware samples or implement DSP; GUI sessions are independent per instrument; production code must return `CaptureError` rather than panic on device input.

Design docs in `docs/` (read before changing the relevant area): `architecture.md`, `rf-sdr-architecture.md`, `custom-protocols.md`, `custom-protocol-yaml.md`, `recording-format.md`, `automation-and-mcp.md`, `gui-modularization.md`, `saleae-logic-device-support.md`.
