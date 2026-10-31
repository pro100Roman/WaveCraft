# Repository Guidelines

## Project Structure & Module Organization

This is a Cargo workspace for a Rust rewrite of DSView. Device-independent types and capture coordination live in `crates/dsview-core`; USB profiles and discovery live in `crates/dsview-usb`; deterministic test devices live in `crates/dsview-sim`; executable applications belong under `apps/`. Architecture decisions are documented in `docs/`. The original implementation in `../DSView` is a read-only behavior and USB-protocol reference.

Keep hardware drivers independent of the UI. New device backends should implement `CaptureSource` and emit typed batches through `CaptureContext`.

## Build, Test, and Development Commands

- `cargo run -p dsview-cli -- 2` — run simultaneous simulated capture for two seconds.
- `cargo run -p dsview-cli -- list` — list supported connected USB instruments.
- `cargo test --workspace` — run all unit and integration tests.
- `cargo fmt --all --check` — verify standard Rust formatting.
- `cargo clippy --workspace --all-targets -- -D warnings` — reject lint warnings.
- `cargo build --workspace --release` — produce optimized binaries.

Rust 1.79 or newer is required. Run commands from the workspace root.

## Coding Style & Naming Conventions

Use `rustfmt` defaults and keep Clippy clean. Follow Rust naming conventions: `snake_case` for modules and functions, `PascalCase` for types and traits, and `UPPER_SNAKE_CASE` for constants. Prefer explicit domain names such as `CaptureCoordinator` over abbreviations. Production crates must not panic on device input; return `CaptureError` with actionable context instead.

## Testing Guidelines

Place focused unit tests beside the code under `#[cfg(test)]`; put cross-crate behavior in a crate's `tests/` directory. Name tests after behavior, for example `rejects_duplicate_device_ids`. Any capture or synchronization change should test multiple sources, cancellation, error propagation, and ordering. Hardware-dependent tests must be opt-in so the default suite works without USB devices.

## Commit & Pull Request Guidelines

The new repository has minimal history, so use concise Conventional Commit subjects such as `feat(core): coordinate multiple capture sources` or `fix(usb): cancel pending transfers`. Keep commits focused and include tests with behavior changes.

Pull requests should explain the problem, summarize the solution, list verification performed, and link relevant issues. Include screenshots for UI changes and capture logs for driver changes. Call out tested device models, firmware versions, protocol assumptions, and compatibility risks. Retain GPL-3.0-or-later compatibility when adapting code from the original DSView project.
