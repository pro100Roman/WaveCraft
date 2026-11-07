# Code health review and improvement backlog

Status: in progress. Reviewed 2026-11-07 against commit `65f909f`; line numbers
in the findings below refer to that commit and will drift. See
[Progress](#progress) for what has been done since.

This document records a review of the workspace and a ranked list of
improvements. The findings describe what was verified and what was not, so that
each can be confirmed before work starts.

## Baseline

At the reviewed commit, `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` all pass (about 22 seconds locally). The Rust sources
contain no `TODO`, `FIXME`, or `HACK` markers.

Areas reviewed and found sound, which need no change:

- `wavecraft-core/src/session.rs`: bounded `sync_channel`, a failure-safe start
  gate, and `stop`/`Drop` that drop the receiver before joining workers, so a
  blocked `emit` cannot deadlock shutdown.
- `wavecraft-hackrf` receive callback: `catch_unwind` across the FFI boundary, a
  recycled buffer pool, and non-blocking `try_send` that reports overflow as a
  capture failure instead of silently dropping IQ. The FFI code carries SAFETY
  comments.

## Progress

Each item below was fixed in its own commit on the `chore/code-health-improvements`
branch. Formatting, Clippy, and the tests for the touched crates were run after
each step. At the end of the series the full CI command set passed: `cargo fmt
--all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo
test --workspace` (538 tests), `cargo build --workspace --release --locked`, the
Python client tests, and `cargo +1.79.0 check --workspace --locked`.

| Item | State |
| ---- | ----- |
| 1. Per-frame analyzer copies | Done. Custom protocols are shared through `Arc`; the decode cache compares its key in place and shares outcomes through `Arc`. Not profiled: frame time was not measured before or after. |
| 2. Large `lib.rs` | Started. The recording-metadata mapping moved to `recording_metadata.rs` (about 1,000 lines). `update` and the other long functions remain, along with migration steps 5-8 in [GUI modularization](gui-modularization.md). |
| 3. Hand-written metadata mapping | Partly done. The functions moved into one module, and a new test round-trips every `RadioSettings` field (logic and scope already did). The single key table that drives both directions was not built. |
| 4. Copied USB helpers | Done for the six identical helpers (`ds_driver_util.rs`). The queued-read bookkeeping in the two drivers was not compared or merged. |
| 5. Panics in production code | Done for `crates/`. About 45 sites were removed (the count of "about 20" below was low because the scan stopped at the first `#[cfg(test)]`), and `clippy::unwrap_used` and `clippy::expect_used` are now enforced there. Not done: the apps, which still have about a dozen `unwrap`/`expect` and a few `unreachable!` in `wavecraft-gui` and one `expect` in the CLI. |
| 6. Smaller items | `serde` and `serde_json` now come from `[workspace.dependencies]`. The CI changes (duplicate runs, cache, audit) were not made: they change release-pipeline behavior and cannot be verified locally, so they need a decision. |

Two fixes could not be fully covered by tests. The USB driver changes only convert
unreachable states into errors, so no test drives those states, and the
`discover_for_selector` retry loop in `wavecraft-automation` has no direct test;
its behavior (number of discovery calls and sleeps) was checked by reading.

## Improvements

Ordered by value for effort. Each entry lists the evidence and the intended fix.
These are the findings as originally written, before any of the work above.

### 1. Per-frame deep copies of protocol-analyzer state in the GUI

`WaveCraftApp::update` runs every frame and copies analyzer state that can be
large.

- `apps/wavecraft-gui/src/lib.rs:11691` clones all of `protocol_analyzer`
  (`ProtocolAnalyzerSettings`: analyzers, editor, search text) each frame.
- A `ProtocolAnalyzerInstance` may hold a `CustomProtocolAnalyzer` containing the
  YAML `source: String` and the parsed `ProtocolSchema`
  (`protocol_analyzer.rs:116-129`), so these clones copy schema data.
- `lib.rs:11696` clones `protocol_decode_snapshot`. Its `outcomes` are `Arc`
  (cheap), but its `analyzers: Vec<ProtocolAnalyzerInstance>` is copied deeply.
  `ProtocolDecodeSnapshot::matches_analyzers` (`protocol_analyzer.rs:277`) then
  compares the same data with a deep `==` every frame.
- `ProtocolDecodeCache::refresh` (`lib.rs:2091-2097`) builds its cache key with
  `analyzers.to_vec()` on every call, including cache hits.
- `lib.rs:11795` calls `.to_vec()` on all decoded outcomes each frame when the
  displayed analyzers differ from the streamed snapshot, for example after
  editing decoders on a finished capture.
- `lib.rs:11745-11747` also clones channel names, channel-settings drafts, and
  marker settings each frame. These are small.

Intended fix:

- Hold `custom_protocol` as `Option<Arc<CustomProtocolAnalyzer>>`.
- Give the analyzer list a revision counter (or share it as `Arc<[..]>`) and key
  the decode cache and snapshot comparison on it.
- Return `Arc<Vec<ProtocolDecodeOutcome>>` from the cache instead of copying.

Not verified: no profile was taken. The cost scales with custom-schema size and
decoded-event count, so measure frame time before and after the change.

### 2. `apps/wavecraft-gui/src/lib.rs` is too large

The file is 24,017 lines, of which about 14,000 are production code (tests start
near line 14,254 and 19,729). The longest functions are:

| Lines | Function |
| ----- | -------- |
| 1,895 | `eframe::App::update` (`lib.rs:10817`) |
| 816 | `paint_capture_settings` |
| 648 | `paint_radio_analyzer_configuration` |
| 488 | `poll_worker` |
| 436 | `start_gui_recording` |
| 398 | `paint_scope_time_domain` |
| 384 | `poll_capture_target` |

It is also the most frequently changed file (about 80 commits since August 2026),
so its size raises review cost and merge-conflict risk.

[GUI modularization](gui-modularization.md) already defines the destination.
Migration steps 1-4 appear done, judging by the existence of `snapshot.rs`,
`sessions.rs`, `recording.rs`, and `devices.rs`; the contents of each were not
audited against the plan. Steps 5-8 remain: `waveform::store` and `waveform::viewport`, the
logic and scope renderers, and splitting `update` into panels that return typed
actions. Continue those steps as small, behavior-preserving changes.

That document is stale: it names `main.rs` where the code now lives in `lib.rs`,
and its status is still "proposed" although the first four steps landed. Update
it as part of the next migration step.

### 3. Hand-written recording-metadata mapping

Recording metadata for capture settings is written and read by separate,
hand-written functions that must stay in sync key by key:

- `saved_radio_settings_metadata` (226 lines, `lib.rs:13005`)
- `radio_settings_from_recording_metadata` (205 lines, `lib.rs:14050`)
- `logic_settings_from_recording_metadata` (169 lines, `lib.rs:13755`)

A field added to one side and not the other is silently lost on reopen.

Intended fix: move these into `recording.rs` (consistent with the modularization
plan), describe each key once in a table that drives both directions, and add a
round-trip test that covers every field. The metadata keys are part of the
`.dsv` format (`recording-format.md`), so key names and legacy fallbacks, such as
the `radio.analyzer.enabled` compatibility path, must be preserved exactly.

### 4. Copied helpers between the USB drivers

`crates/wavecraft-usb/src/dslogic.rs` and `dscope.rs` contain byte-identical
copies of `capture_error`, `combine_results`, `push_u16`, `push_u32`, `read_u16`,
and `read_u32`. Both drivers also keep a `VecDeque` of queued stream reads with
similar cleanup logic; the DSLogic version
(`dslogic.rs:1008-1110, 1320`) was read in full, the DSCope one was not compared
line by line.

Intended fix: move the identical helpers into a shared private module, then
evaluate whether the queued-read bookkeeping can share an implementation.
`dscope.rs` (3,712 lines) can later be split by concern (configuration,
calibration, capture).

### 5. `expect`/`unreachable!` in production code

`AGENTS.md` requires that production crates not panic on device input. About 20
non-test sites use `.expect(...)` or `unreachable!(...)`. Every site that was read
is guarded by an earlier check in the same function, so these are latent hazards
and none was shown to be reachable. Those fed by external data are the riskiest:

- `wavecraft-decode/src/runtime.rs:195, 279, 357, 363, 382, 490` (segmented
  packets from YAML schemas and decoded streams).
- `wavecraft-recording/src/saleae.rs:369, 884, 888, 894, 1090, 1129` (analog
  sample rate and pyramid levels; the importer reads user-supplied `.sal` files).
- Lower risk: `wavecraft-usb/src/dscope.rs:535, 1274, 1549, 1577, 1938`,
  `wavecraft-usb/src/transport.rs:163, 188`, `wavecraft-decode/src/decode.rs:593,
  597`, `wavecraft-decode/src/transport.rs:563`.

Intended fix: carry the already-checked reference or value forward instead of
looking it up again, or return a diagnostic or `CaptureError`. Then enforce the
rule with `#![warn(clippy::unwrap_used, clippy::expect_used)]` on library crates,
allowed in `#[cfg(test)]`, so new sites are caught in review.

### 6. Smaller items

- `serde_json` is declared with different version requirements across crates
  (`1.0.154` in the CLI and GUI, `1.0.145` in `wavecraft-recording`). A
  `[workspace.dependencies]` table would keep them aligned.
- `.github/workflows/ci.yml` triggers on both `push` and `pull_request`, so
  pull-request branches run twice. It has no Cargo cache and no dependency audit
  (`cargo audit` or `cargo deny`). Only the workflow file was read; adding these
  was not tried.

## Suggested order

1. Item 1, with a before/after frame-time measurement.
2. Items 4 and 5, which are low risk and independent of each other.
3. Items 2 and 3 as a series of small changes following the migration plan; item
   3 fits naturally into the `recording.rs` step.
4. Item 6 opportunistically.

Each change should keep `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` green, per `AGENTS.md`.
