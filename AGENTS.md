# Repository Guidelines

## Project Structure & Module Organization

This is a Cargo workspace for WaveCraft, a Rust rewrite of DSView. Device-independent types and capture coordination live in `crates/wavecraft-core`; USB profiles and discovery live in `crates/wavecraft-usb`; deterministic test devices live in `crates/wavecraft-sim`; executable applications belong under `apps/`. Architecture decisions are documented in `docs/`. The original DSView implementation in `../DSView` is a read-only behavior and USB-protocol reference.

Keep hardware drivers independent of the UI. New device backends should implement `CaptureSource` and emit typed batches through `CaptureContext`.

## Shared GUI View Pattern

Every workspace view (LA, OSC, RF, Combined, VNA, System Trace, Serial, and future views) must reuse the instrument chrome in `apps/wavecraft-gui/src/ui/view_shell.rs`. Use `view_toolbar` for the application menu and control row, `SetupPanel` for left settings, `settings_card` for inset settings groups, `workspace_panel` and `view_card` for plots/tables, and `tools_panel` for collapsible auxiliary settings. Keep primary acquisition controls at the right end of the toolbar. Extend these shared components when a layout capability is missing instead of copying panel frames, margins, typography, or collapse controls into a view.

Operational status and errors belong in the common bottom message area in `ui/status_bar.rs`, including view-specific connection and Serial errors. Adapt a view's state through `active_view_status`; do not paint operational messages in toolbars, setup panels, or over plots. All informational text (including status, errors, labels, and tooltips) must support text selection and native keyboard copying (Cmd+C / Ctrl+C); keep `style.interaction.selectable_labels` enabled and do not add dedicated message-copy buttons. Keep errors visible when settings are collapsed. Plot annotations, data quality counters, empty-state guidance, and actionable confirmation dialogs remain local to their content.

The shell must stay independent of `WaveCraftApp`, capture lifecycle, and hardware drivers. Views supply their controls/content and retain domain state; apply requested actions through the existing application orchestration. See `docs/gui-view-pattern.md` for the component contract, extension recipe, and verification expectations.

## Build, Test, and Development Commands

- `cargo run -p wavecraft-cli -- 2` — run simultaneous simulated capture for two seconds.
- `cargo run -p wavecraft-cli -- list` — list supported connected USB instruments.
- `cargo test --workspace` — run all unit and integration tests.
- `cargo fmt --all --check` — verify standard Rust formatting.
- `cargo clippy --workspace --all-targets -- -D warnings` — reject lint warnings.
- `cargo build --workspace --release` — produce optimized binaries.

Rust 1.79 or newer is required. Run commands from the workspace root.

## Coding Style & Naming Conventions

Use `rustfmt` defaults and keep Clippy clean. Follow Rust naming conventions: `snake_case` for modules and functions, `PascalCase` for types and traits, and `UPPER_SNAKE_CASE` for constants. Prefer explicit domain names such as `CaptureCoordinator` over abbreviations. Production crates must not panic on device input; return `CaptureError` with actionable context instead. Crates under `crates/` and the apps under `apps/` enforce this with the `clippy::unwrap_used` and `clippy::expect_used` workspace lints; test code is exempt through `clippy.toml`, and integration tests opt out with a file-level `#![allow]`.

## Testing Guidelines

Place focused unit tests beside the code under `#[cfg(test)]`; put cross-crate behavior in a crate's `tests/` directory. Name tests after behavior, for example `rejects_duplicate_device_ids`. Any capture or synchronization change should test multiple sources, cancellation, error propagation, and ordering. Hardware-dependent tests must be opt-in so the default suite works without USB devices.

## Commit & Pull Request Guidelines

The new repository has minimal history, so use concise Conventional Commit subjects such as `feat(core): coordinate multiple capture sources` or `fix(usb): cancel pending transfers`. Keep commits focused and include tests with behavior changes.

Pull requests should explain the problem, summarize the solution, list verification performed, and link relevant issues. Include screenshots for UI changes and capture logs for driver changes. Call out tested device models, firmware versions, protocol assumptions, and compatibility risks. Retain GPL-3.0-or-later compatibility when adapting code from the original DSView project.
