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
each step. After the last code change (`b3222cc`, the GUI split and its tests) the full
CI command set passed: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace` (559 passed, 6 ignored),
`cargo build --workspace --release --locked`, the Python client tests (9), and
`cargo +1.79.0 check --workspace --locked`.

| Item | State |
| ---- | ----- |
| 1. Per-frame analyzer copies | Done and measured; see [Measurements](#measurements-for-item-1). Custom protocols are shared through `Arc`; the decode cache compares its key in place and shares outcomes through `Arc`. The measured saving is small: under 0.1 ms per frame except with about 10,000 or more decoded events in the cache path. |
| 2. Large `lib.rs` | The file split is done; the long functions inside the moved code are not. `lib.rs` went from 23,218 lines to 688, and `eframe::App::update` from 1,874 lines to a 3-line call into `run_frame` (143 lines). The code now lives in `acquisition/` (settings and capture lifecycle), `app/` (the `WaveCraftApp` methods by responsibility), `ui/` (every panel, window, and dialog, plus `FrameState` and `FrameActions`), and `waveform/`, plus `format.rs`, `recording.rs`, `sessions.rs`, `process_stats.rs`, and `protocol_decode_cache.rs`. 116 of the 157 tests in `lib.rs` moved next to the code they cover; the other 41 are in `tests.rs`, and the set of test names is unchanged. A new `frame_tests.rs` runs whole frames headlessly with fake devices (15 tests); inserting a panic into each paint method and into the apply methods made them fail. They do not click widgets. The longest functions are still long: `paint_capture_settings` 815 lines, `paint_radio_analyzer_configuration` 647, `paint_central_panel` 511, `poll_worker` 487, `start_gui_recording` 435, `paint_scope_time_domain` 397, `poll_capture_target` 383. Panels still take `&mut self` rather than narrow editor models. Details and deviations from the plan are in [GUI modularization](gui-modularization.md). |
| 3. Hand-written metadata mapping | Done for the one-key-per-setting fields. `recording_metadata.rs` now describes each such setting once, in a `MetadataField` or `ChannelField` table entry that drives both saving and loading (logic, scope and radio). Still hand-written, and marked as such in the file: the logic trigger (mode, stage count and per-stage conditions depend on each other), the radio analyzer channel list and gains, and the legacy radio analyzer keys. The file is not shorter (about 1,100 lines, up from 1,040, because rustfmt expands the table entries). Checked against the old code by saving and loading about 19,000 cases (every key removed, set to 34 odd values, plus legacy-key combinations) and comparing the output byte for byte; that harness was temporary and is not in the repository. A new test checks that table keys are unique. |
| 4. Copied USB helpers | Done for the six identical helpers (`ds_driver_util.rs`). The queued-read bookkeeping in the two drivers was not compared or merged. |
| 5. Panics in production code | Done. About 45 sites in `crates/` were removed (the count of "about 20" below was low because the scan stopped at the first `#[cfg(test)]`), plus the remaining `expect`/`unreachable!` sites in `wavecraft-gui` (one looks reachable from the code: a recording holding logic and radio, or scope and radio, but not all three, passed the reader and then hit `unreachable!`; this was not reproduced with a real file). `clippy::unwrap_used` and `clippy::expect_used` are now enforced for every crate and both apps. The CLI had no production sites; its one `expect` is in test code. |
| 6. Smaller items | `serde` and `serde_json` now come from `[workspace.dependencies]`. CI changes were made but have **not been run on GitHub**; the YAML parses, nothing more was checked. (a) `ci.yml` builds every branch push and every `v*` tag, with or without a pull request. A pull request from a branch in this repository is covered by that push, so the `pull_request` event only runs for pull requests from forks (each job has an `if`); this avoids running every job twice. A newer push to a branch cancels the run still in progress for it; `main` and tags are never cancelled. The check on a same-repository pull request therefore tests the branch head, not its merge with `main`. (b) `Swatinem/rust-cache` was added to the `msrv` and `workspace` jobs; it is skipped on release tags so release binaries still build from a clean tree, and only `main` writes the cache. (c) A separate `audit.yml` runs `cargo deny check advisories` when `Cargo.lock` or a manifest changes, weekly, and on demand. It is not a dependency of the `release` job. `cargo audit` and `cargo deny` are not installed here, so it is unknown whether the current `Cargo.lock` has advisories that would fail its first run. |

Two fixes could not be fully covered by unit tests. The USB driver changes only
convert unreachable states into errors, so no test drives those states, and the
`discover_for_selector` retry loop in `wavecraft-automation` has no direct test;
its behavior (number of discovery calls and sleeps) was checked by reading.

The opt-in hardware tests were run at commit `ee7b325` (after the USB driver
changes) and again at `b3222cc` (after the GUI split) with a DSLogic U3Pro16, a
DSCope U3P100, and a HackRF One connected and idle, and all passed both times:

- `cargo test -p wavecraft-usb --test hardware -- --ignored` (idle bulk reads cancel promptly)
- `cargo test -p wavecraft-automation --test hardware -- --ignored` (DSLogic and DSCope capture contract)
- `cargo test -p wavecraft-hackrf --test hardware -- --ignored` (HackRF discovery and finite IQ capture)

That shows the changed driver code still prepares, captures, and cancels on real
devices. It does not show that the new error paths behave correctly, since the
hardware never reaches those states.

## Measurements for item 1

A temporary release-mode micro-benchmark (not in the repository) replayed the
protocol-analyzer work that `update()` does on every frame while the logic tab
is shown, once against the code before the fix (`7225710`) and once against the
code after it. It ran on one machine (macOS, Apple M5 Pro), single threaded, 1.5 s per case.

Per-frame cost of cloning the analyzer settings and the decode snapshot, matching
them, and looking up the outcomes. Event counts are the streamed events; the
analyzers were two UART analyzers and one I²C analyzer:

| Custom schemas attached | Snapshot matches analyzers (before → after) | Decoders edited after capture (before → after) |
| ----------------------- | ------------------------------------------- | ---------------------------------------------- |
| none | 0.15 → 0.10 µs | 0.5–1.5 → 0.11 µs |
| motor controller (1 KB YAML) | 1.1 → 0.10 µs | 2.0–2.7 → 0.12 µs |
| motor controller + BNO08x (39 KB YAML) | 31 → 0.10 µs | 54–56 → 0.12 µs |

Cost of the outcome copy the old code made every frame when the displayed
analyzers differed from the streamed snapshot (`Vec<ProtocolDecodeOutcome>::to_vec()`),
against sharing the list through an `Arc`, for one analyzer's events:

| Decoded events | `to_vec()` (before) | `Arc::clone` (after) |
| -------------- | ------------------- | -------------------- |
| 100 | 1.7 µs | 0.02 µs |
| 1,000 | 18 µs | 0.02 µs |
| 10,000 | 206 µs | 0.02 µs |
| 50,000 | 1,038 µs | 0.02 µs |

A frame at 60 Hz is 16,667 µs, so the worst measured saving (50,000 events) is
about 6% of a frame and the schema-clone saving (39 KB of YAML) is about 0.3%.
In practice the fix removes a real but small cost.

Limits of this measurement:

- It times only the operations listed, not `update()` or a real frame; no
  whole-application profile was taken.
- The synthetic capture (UART bytes) produced only 44-66 events through the
  one-shot decode path that the cache uses, so the large `to_vec()` rows come from
  copying hand-built event lists of the stated size, not from a decoded capture.
  Whether a real capture reaches 10,000 events in that path was not checked. The
  streamed path is capped at 10,000 events per analyzer.
- The old and new code were compared on the same synthetic data only.

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

When this review was written the file was 24,017 lines. Measured again on the
commit before the split began (`e74c2bc`, after the metadata mapping had moved
out), it was 23,218 lines: 18,778 of production code, and one `mod tests` of
4,440 lines starting at line 18,780. An earlier version of this section said
"about 14,000 production lines"; that was wrong. The longest functions were:

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

Since then `update` has become a 3-line call into `run_frame` (143 lines), and
the file split is done: the functions above moved into `ui/`, `recording.rs`,
`waveform/`, and `app/` without changes to their bodies, so the table above
still gives their sizes. Only `update` itself was broken up. `poll_worker` and
`poll_capture_target` (now in `app/`) and the paint functions are as long as
before.

[GUI modularization](gui-modularization.md) defines the destination, and its
status table is now current. Of its migration steps, 1, 2, 3, 5, 6 and 7 are
done, and 4 (devices) and 8 (typed actions from panels) are partial. Remaining
work is splitting the long functions listed above, narrowing what the panels can
reach, and moving `WaveCraftApp` itself into `app/`.

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
