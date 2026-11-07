# GUI modularization and multi-SDR integration plan

Status: the file split (migration steps 1-3 and 5-7) is done and `lib.rs` is
down to 688 lines; the RF/SDR integration for HackRF One is implemented. What
remains is the long functions inside the moved code, narrowing what the panels
can reach, and the LimeSDR and AntSDR adapters. See
[Current status](#current-status) and the [Migration sequence](#migration-sequence).

This document describes a behavior-preserving split of the GUI application
(originally all in `apps/wavecraft-gui/src/main.rs`, now mostly in
`apps/wavecraft-gui/src/lib.rs`) and reserves explicit integration seams for
HackRF One, LimeSDR, and AntSDR. It complements
[Capture Architecture](architecture.md) and
[RF/SDR capture and signal-decoding architecture](rf-sdr-architecture.md).
The RF document remains authoritative for IQ batches, bounded streaming, DSP,
recording, and decoding. This document focuses on how those capabilities enter
the GUI without coupling hardware drivers to rendering code.

No device is considered supported merely because a placeholder described here
exists. A backend becomes supported only after discovery, configuration,
capture, cancellation, error handling, and opt-in hardware tests are
implemented for an identified model and transport.

## Current status

Reviewed 2026-11-07 on branch `chore/code-health-improvements` at commit
`b3222cc`. Sizes below were measured, not estimated: "before" is `e74c2bc`,
the commit before the GUI split began.

`apps/wavecraft-gui/src/main.rs` is only the native entry point
(`wavecraft_gui::run()`). The application lives in the library crate.

| Measure | Before | Now |
| ------- | ------ | --- |
| `lib.rs` total lines | 23,218 | 688 |
| `lib.rs` production lines | 18,778 | 688 |
| tests in `lib.rs` | 157 tests, 4,440 lines | none (see below) |
| `eframe::App::update` lines | 1,874 | 3 (it calls `run_frame`, 143 lines) |

An earlier version of this document said `lib.rs` held "roughly 13,000 lines of
production code". That was wrong; it was about 18,800.

`lib.rs` now holds the module declarations, the `WaveCraftApp` struct with its
constructors and `run_frame`, a few small shared helpers, and `run()`. The code
moved into these modules, mostly unchanged apart from `pub(crate)` visibility:

- `acquisition/{capture,logic,scope,radio}.rs`: capture settings, session
  settings, and the capture lifecycle types (`AcquisitionMode`, `CaptureTarget`,
  `ActiveCapture`, `start_capture`).
- `app/{devices,calibration,files,view_state,captures,worker}.rs`: the methods
  of `WaveCraftApp`, grouped by what they do. They are separate `impl` blocks,
  so the state still lives in one struct.
- `ui/`: every panel, window, and dialog that `run_frame` paints, plus
  `frame_state.rs` and `frame_actions.rs` (below) and `view_tabs.rs`.
- `waveform/`: store, viewport, logic, scope (with `scope_markers`, `scope_fft`,
  `scope_lissajous`), shared `chrome`, the `combined` view layout, and `radio`.
- `format.rs`, `process_stats.rs`, `protocol_decode_cache.rs`, `recording.rs`,
  `recording_metadata.rs`, `sessions.rs`, `devices.rs`, `snapshot.rs`.

Moving code did not shorten the long functions inside it. The longest ones now
are `paint_capture_settings` (815 lines), `paint_radio_analyzer_configuration`
(647), `paint_central_panel` (511, moved verbatim out of `update`), `poll_worker`
(487), `start_gui_recording` (435), `paint_scope_time_domain` (397), and
`poll_capture_target` (383). They are in the right module but still need to be
split.

Tests: 116 of the 157 tests that were in `lib.rs` moved next to the single
module they cover. The other 41 cover more than one module or only the
application root, and are in `src/tests.rs`. The test names are unchanged
(255 listed, 253 run, 2 ignored). `src/frame_tests.rs` adds 15 headless tests
(see below).

| Step | State | Notes |
| ---- | ----- | ----- |
| 1. Snapshot utilities | Done | `snapshot.rs` holds image cropping and PNG persistence with its tests. `finish_pending_snapshot` is in `app/files.rs`. |
| 2. Generic session tabs | Done | `sessions.rs` holds `SessionTab`, the combined, logic, and scope session types, and their tests. The tab strip and its select/add/close/rename handling are in `ui/session_tabs.rs`. |
| 3. Recording and Saleae | Done | `recording.rs` holds path selection, decoded-data JSON Lines I/O, Saleae export file handling, `start_gui_recording`, `read_gui_recording*`, and `LoadedRecording`. `recording_metadata.rs` holds the table-driven mapping between capture settings and `.dsv` metadata. The methods that open, export, and attach recordings are in `app/files.rs`. |
| 4. Devices | Partial | `devices.rs` holds selection helpers, the resource directory lookup, and the USB speed warning state. Discovery itself is `wavecraft_usb::discover_supported_devices`; the GUI's refresh and selection methods are in `app/devices.rs`. There is no `devices::radio`. |
| 5. Waveform store and viewport | Done | `waveform/store.rs` and `waveform/viewport.rs`, with their tests. |
| 6. Logic rendering, markers, protocol lanes | Done | `waveform/logic.rs` holds the signal lanes, marker state and drawing, timing measurements, and protocol-decode lanes. |
| 7. Scope rendering, markers, FFT, measurements UI | Done | `waveform/scope.rs`, `scope_markers.rs`, `scope_fft.rs`, `scope_lissajous.rs`, and `chrome.rs` (cards, grid, and time ruler shared by every view). The measurement settings UI is in `ui/scope_settings.rs`; `measurements.rs` is still only the engine. |
| 8. Split `update` into panels returning typed actions | Partial | Every panel is a method in `ui/`. Enablement flags are computed once in `FrameState` (`ui/frame_state.rs`); requests are collected in `FrameActions` (`ui/frame_actions.rs`) and applied after painting. See the deviations below. |
| 9. Core IQ model, recording, simulator | Done | See [RF/SDR architecture](rf-sdr-architecture.md). |
| 10. Radio GUI | Partial | `waveform/radio.rs`, `radio_analyzer.rs`, `radio_trigger.rs`, `acquisition/radio.rs`, and `ui/radio_settings.rs` exist and `WaveformTab::RadioReceiver` is a live tab. There is no `devices::radio` module. |
| 11. HackRF One adapter | Done | `wavecraft-hackrf` is a receive-only adapter. Hardware tests must remain opt-in. |
| 12. LimeSDR-USB and AntSDR E200 adapters | Not started | `wavecraft-sdr` names `RadioProvider::LimeSuiteNg`, `RadioProvider::Libiio`, `RadioModel::LimeSdrUsbTypeA`, and `RadioModel::AntSdrE200`, but no adapter crates exist. These names are not support claims. |

Differences between this plan and the code:

- The panels do not follow the "narrow editor model in, typed action out"
  pattern described in
  [Application ownership and UI communication](#application-ownership-and-ui-communication).
  They are `impl WaveCraftApp` methods that take `&mut self`, a shared
  `&FrameState` (read-only flags and before-painting snapshots), and
  `&mut FrameActions` to record what the user asked for. This kept each move
  verbatim and borrow-checker friendly, but the UI modules still have
  unrestricted access to application state. Narrowing that is future work.
- `run_frame` is 143 lines. It sequences the panels and applies a few small
  actions inline (calibration requests, device-selection changes, radio runtime
  updates). The rest of the apply code is in `FrameActions` and
  `scope_frontend_changes.rs`.
- The planned `DeviceProvider` enum is implemented for radios as `RadioProvider`
  and `RadioModel` in `wavecraft-sdr`, alongside `ConnectionBackend` and
  `RadioCapabilities` (with `ValueDomain`). There is no combined
  `InstrumentCapabilities` enum or `RadioBackendSettings` enum yet; HackRF
  settings are the concrete `HackRfSettings`.
- `WaveformTab` has `LogicAnalyzer`, `Oscilloscope`, `Lissajous`,
  `RadioReceiver`, and `Combined`, rather than the four-variant sketch below.
- The modules are flat files, not the directories in the layout below:
  `waveform/logic.rs` and `waveform/scope.rs` instead of `waveform/logic/` and
  `waveform/scope/`; `app/` has `devices`, `calibration`, `files`, `view_state`,
  `captures`, and `worker` instead of `actions`, `state`, and `workers`; and
  `ui/` is organized by panel (see above) instead of the listed names.
  `WaveCraftApp` itself is still in `lib.rs`, not `app/mod.rs`.

Verification of the split, and its limits:

- `src/frame_tests.rs` runs `run_frame` on a bare `egui::Context` with fake
  DSLogic, DSCope, and HackRF entries (no window, no hardware). It paints every
  view, both capture settings tabs, and the settings, calibration, and
  USB-speed dialogs, and calls `apply_toolbar_actions` and
  `apply_scope_frontend_changes` directly. A panic inserted into each paint
  method, into several inner branches, and into the apply methods made these
  tests fail.
- The tests do not click anything, so button and drag handling inside the
  panels is not covered. Hardware tests exercise the drivers, not the GUI.
- The app was also started by hand for 10 seconds after the larger steps and
  did not panic.

## Why split the GUI now

The GUI application (originally in `apps/wavecraft-gui/src/main.rs`, now mostly
in `src/lib.rs`) contains application state, capture orchestration, device
selection, settings panels, dialogs, waveform storage, viewport calculations,
logic rendering, scope rendering, FFT processing, markers, and their tests.
These responsibilities change for different reasons and already have useful
internal boundaries.

The refactor should improve ownership and test locality, not only reduce the
line count of `main.rs`. In particular:

- the executable entry point should not contain domain or rendering logic;
- hardware-specific settings must not spread through general-purpose panels;
- logic, scope, and RF views should share only genuinely common viewport and
  panel infrastructure;
- recording and capture workers must remain independent of display buffers;
- the application remains the owner of mutable GUI state; and
- focused tests should live beside the module whose behavior they verify.

## Source layout

This is the layout the code has now. Items marked *(planned)* do not exist yet.

```text
apps/wavecraft-gui/src/
├── main.rs                       # native startup only
├── lib.rs                        # module declarations, WaveCraftApp struct,
│                                 # constructors, run_frame, run()
├── app/                          # WaveCraftApp methods, by responsibility
│   ├── mod.rs
│   ├── devices.rs                # refresh, device and radio selection
│   ├── captures.rs               # begin/stop/poll captures, completed captures
│   ├── worker.rs                 # poll_worker: results of background work
│   ├── calibration.rs            # DSCope calibration and idle configuration
│   ├── files.rs                  # open/export/attach recordings, snapshots
│   └── view_state.rs             # saved settings, session settings, active tabs
├── acquisition/
│   ├── mod.rs
│   ├── capture.rs                # AcquisitionMode, CaptureTarget, ActiveCapture,
│   │                             # start_capture
│   ├── logic.rs                  # DSLogic capture/session settings
│   ├── scope.rs                  # DSCope capture/session settings
│   └── radio.rs                  # radio capture settings and requests
├── devices.rs                    # selection helpers, USB speed warning,
│                                 # resource directory
├── sessions.rs                   # generic session tabs and session settings
├── recording.rs                  # native/Saleae loading, saving
├── recording_metadata.rs         # settings <-> .dsv metadata
├── capture_settings.rs           # saved per-user capture setting profiles
├── snapshot.rs                   # screenshot crop and PNG persistence
├── theme.rs                      # colors and style
├── format.rs                     # presentation-only value formatting
├── process_stats.rs              # CPU and memory sampling for the status bar
├── protocol_decode_cache.rs      # decode results for the displayed capture
├── ui/
│   ├── mod.rs
│   ├── frame_state.rs            # FrameState: flags computed once per frame
│   ├── frame_actions.rs          # FrameActions: what the user asked for
│   ├── toolbar.rs, waveform_tabs.rs, status_bar.rs, session_tabs.rs
│   ├── capture_setup_panel.rs, capture_settings_panel.rs, radio_settings.rs,
│   │   scope_settings.rs, scope_tools_panel.rs     # settings panels
│   ├── central_panel.rs, decoded_panel.rs          # waveform area
│   ├── app_windows.rs, dialogs.rs, protocol_analyzer_dialog.rs
│   ├── scope_frontend_changes.rs # applies scope frontend edits after painting
│   └── view_tabs.rs              # WaveformTab, CaptureSettingsTab
├── waveform/
│   ├── mod.rs
│   ├── store.rs                  # bounded display data and revisions
│   ├── viewport.rs               # pan, zoom, time axes, combined timeline
│   ├── chrome.rs                 # cards, grid, time ruler shared by all views
│   ├── combined.rs               # logic + scope pane layout
│   ├── logic.rs                  # logic lanes, markers, protocol lanes
│   ├── scope.rs, scope_markers.rs, scope_fft.rs, scope_lissajous.rs
│   └── radio.rs                  # RF Spectrum view state and composition
├── measurements.rs               # scope measurement engine
├── protocol_analyzer.rs          # logic protocol engine
├── radio_analyzer.rs, radio_trigger.rs
├── tests.rs                      # tests that cross modules
└── frame_tests.rs                # headless whole-frame tests

app/mod.rs                        # (planned) WaveCraftApp itself; see step 15
devices/{logic,scope,radio}.rs    # (planned) capability mapping per instrument
```

Compared with the first version of this plan, `waveform/logic/` and
`waveform/scope/` are single files (and `scope_markers.rs`, `scope_fft.rs`, and
`scope_lissajous.rs` split off), `app/` is organized by responsibility rather
than `actions`/`state`/`workers`, and `ui/` is organized by panel. Modules are
introduced when code and tests move into them; empty modules add navigation
cost without establishing a real contract.

## Application ownership and UI communication

`WaveCraftApp` remains the composition root. It owns selected devices, active
captures, session tabs, display stores, workers, dialogs, and user-visible
errors. Submodules should not gain their own disconnected copies of that
state.

Panels receive narrow editor or view models and return typed actions. For
example:

```rust
pub(crate) struct RadioCaptureEditor<'a> {
    pub settings: &'a mut RadioCaptureSettings,
    pub capabilities: &'a RadioCapabilities,
    pub availability: CaptureAvailability,
}

pub(crate) enum RadioCaptureAction {
    None,
    ApplyIdleConfiguration,
    Start,
    Stop,
}
```

The top-level update cycle applies actions after painting. This follows the
pattern already used by settings editor structs and calibration actions, and
avoids giving every UI module unrestricted `&mut WaveCraftApp` access. It also
prevents borrow-checker pressure from turning the new modules into one large
web of shared mutable references.

`eframe::App::update` now only calls `run_frame`, which reads as a sequence of
orchestration operations:

```text
poll workers and capture events
reconcile selected devices and tabs
compute FrameState (enablement flags and before-painting snapshots)
paint toolbar, view tabs, and settings panels
paint the active instrument view, session tabs, and status bar
paint dialogs and warnings
apply returned actions (FrameActions, scope frontend changes)
schedule repaint when work remains
```

Two parts of that picture are not met yet. The panels are `impl WaveCraftApp`
methods that take `&mut self`, so they can reach all 77 fields of the
application; narrowing that is step 14. And a few small actions (calibration
requests, device-selection changes, radio runtime updates) are still applied
inline in `run_frame`.

## Device-independent instrument model

The GUI should select instruments through device-independent descriptors and
capabilities. It must not extend the existing GUI-local `DeviceRole` with a
growing list of concrete models.

The planned core RF work adds `InstrumentKind::RadioReceiver` and IQ capture as
specified by `rf-sdr-architecture.md`. Discovery should additionally expose a
stable provider identity and typed capabilities. Provider identity is distinct
from connection kind: HackRF is a provider/driver family, while USB is a
connection backend.

This distinction preserves the existing automation API, where
`DeviceBackend` currently means `Usb` or `Simulator`. That public field must not
silently change meaning. A future automation schema may add a separate
`provider` field while retaining `backend` for connection/discovery class. The
exact shared type placement can be settled while implementing the core RF
slice, but the internal registry contract should be equivalent to:

```rust
pub enum DeviceProvider {
    DsLogic,
    DsCope,
    Simulator,
    HackRf,
    LimeSdr,
    AntSdr,
}

pub enum ConnectionBackend {
    Usb,
    Network,
    Simulator,
}

pub enum InstrumentCapabilities {
    Logic(LogicCapabilities),
    Scope(ScopeCapabilities),
    Radio(RadioCapabilities),
}

pub struct RadioCapabilities {
    pub frequency_range_hz: RangeInclusive<u64>,
    pub sample_rates: ValueDomain<u64>,
    pub bandwidths_hz: ValueDomain<u64>,
    pub receive_channels: u8,
    pub gain_controls: Vec<GainControlDescriptor>,
    pub supports_hardware_timestamps: bool,
    pub supports_external_clock: bool,
}
```

These are illustrative shapes, not committed public APIs. `Network` is
reserved for a backend that actually requires it; it does not assert that
every AntSDR uses network transport. Capabilities should describe ranges and
controls reported by the selected backend rather than encoding HackRF
assumptions into generic settings. For example, the generic GUI may display a
list of named gain controls; it should not require every SDR to expose controls
named `LNA`, `VGA`, or `AMP`.

Settings have two levels:

- `RadioCaptureSettings` contains portable intent such as center frequency,
  sample rate, bandwidth, channel, and stream duration.
- A backend-specific settings enum contains optional controls that cannot be
  represented honestly by the portable settings.

```rust
pub enum RadioBackendSettings {
    HackRf(HackRfSettings),
    LimeSdrUsb(LimeSdrUsbSettings),
    AntSdrE200(AntSdrE200Settings),
}
```

The enum is an application/configuration boundary, not permission for the GUI
to call vendor APIs. Backends validate and apply their own settings. Unknown
newer settings in recordings or projects must produce an actionable
compatibility diagnostic rather than a panic.

## SDR backend placeholders

The workspace should reserve a device-independent radio facade plus one adapter
per backend:

```text
crates/
├── wavecraft-sdr/                 # shared discovery/configuration contracts
├── wavecraft-hackrf/              # HackRF One adapter
├── wavecraft-limesdr/             # LimeSDR family adapter
└── wavecraft-antsdr/              # AntSDR family adapter
```

The facade owns no vendor transport. It aggregates discovered devices, maps
backend errors into stable WaveCraft diagnostics, and constructs a
`CaptureSource` from validated generic and backend settings. Each adapter owns
its native library or transport integration, model probing, settings
translation, receive worker, cancellation, and hardware tests.

The three adapter crates are placeholders in this architecture; they should be
added to the workspace only when implementation starts. Until then, simulator
and recorded IQ data should drive RF GUI development.

### HackRF One

`wavecraft-hackrf` is the first physical backend and is implemented, following
the detailed sequence in `rf-sdr-architecture.md`. Its initial scope is
receive-only, single-channel IQ streaming. HackRF-specific gain and amplifier
controls remain in `HackRfSettings` and are surfaced through capabilities.

### LimeSDR-USB Type-A

`wavecraft-limesdr` is a separate adapter using the same raw IQ and capture
contracts. The placeholder allows multiple receive channels, model-dependent
frequency/sample-rate domains, named gain stages, and optional timing/clock
capabilities. The first target is the LimeSDR-USB board with the integrated USB
Type-A plug, using the native Lime Suite NG device API. Lime Suite NG lists
LimeSDR USB as a supported USB device and is the recommended API for new
applications. The generic GUI must not depend on the Lime Suite NG API.

References:

- [Lime Suite NG documentation](https://github.com/myriadrf/LimeSuiteNG/blob/develop/docs/index.rst)
- [LimeSDR-USB hardware description](https://wiki.myriadrf.org/LimeSDR-USB_hardware_description)

Initial LimeSDR support may deliberately expose one receive channel even when
the selected hardware can provide more. Multi-channel coherent capture should
be enabled only after `IqBatch`, recording, synchronization, and DSP semantics
for channel identity are specified and tested.

### AntSDR E200

`wavecraft-antsdr` is also a separate adapter. AntSDR is treated as a device
family, not one assumed USB protocol. The first target is the AntSDR E200 using
libiio/IIOD over its Gigabit Ethernet interface. The USB Type-C interface is a
UART connection and is not the RF sample transport. Discovery may use bounded
libiio network scanning, but an explicit device URI/IP must also be supported.

The E200 can contain an AD9361 or AD9363. The adapter must probe the RFIC and
derive tuning and bandwidth limits from its IIO attributes rather than assuming
the wider AD9361 ranges. Initial support is receive-only and may expose one
channel even when the detected hardware/firmware exposes two.

References:

- [AntSDR E200 hardware manual](https://antsdr-docs.microphase.cn/en/latest/device_and_usage_manual/ANTSDR_E_Series_Module/ANTSDR_E200_Reference_Manual/AntsdrE200_Reference_Manual.html)
- [AntSDR E200 RF parameters](https://antsdr-docs.microphase.cn/en/latest/device_and_usage_manual/ANTSDR_E_Series_Module/ANTSDR_E200_Reference_Manual/AntsdrE200_RF_parameters.html)
- [libiio](https://github.com/analogdevicesinc/libiio)

This adapter boundary prevents E200 assumptions from leaking into
`wavecraft-core`, `wavecraft-sdr`, or the RF view. Network discovery, if needed
by the E200, belongs in this adapter and must be opt-in or bounded so a GUI
refresh cannot block the render loop.

## Discovery and selection flow

Device refresh becomes an aggregation operation outside the UI thread:

```text
DSLogic/DSCope discovery ───────────────┐
HackRF discovery ───────────────────────┤
LimeSDR discovery ──────────────────────┼─> discovered device registry
AntSDR discovery ───────────────────────┤      │
simulated instruments ──────────────────┘      v
                                      stable GUI selection
```

Every entry has a stable connection ID, instrument kind, provider, connection
backend, display name, availability, and capabilities. A failure in one
provider is reported with that provider's identity and does not hide devices
returned successfully by other providers.

The GUI filters the registry by `InstrumentKind`, then renders settings from
the selected entry's capabilities. Session settings store stable IDs but
handle a missing device explicitly. Opening a recording does not require the
original hardware to be connected.

## RF view integration

The new `waveform::radio` module renders only device-independent derived data:

- latest spectrum frame and bounded averaging/peak state;
- bounded waterfall rows or texture data;
- frequency viewport and markers;
- overrun, dropped-row, discontinuity, and decoder diagnostics; and
- optional compact I/Q traces.

It never opens a device, invokes a vendor API, or performs unbounded DSP in the
render loop. Hardware configuration stays under `acquisition::radio`, IQ
capture runs through `CaptureSource`, DSP runs in bounded workers, and
recording consumes the capture event stream directly.

The initial active view set becomes:

```rust
enum WaveformTab {
    Logic,
    Scope,
    Combined,
    Radio,
}
```

`Radio` is available for live use when a radio receiver is selected, and for
offline use when a loaded recording contains IQ data. It is not added to the
existing Combined view initially. Cross-instrument RF/logic/scope views require
explicit clock and timestamp semantics and should be designed separately.

## Recording and project boundaries

The existing RF plan requires a versioned IQ payload in native recordings.
The GUI `recording` module should map recording data into device-independent
loaded sessions; it must not switch on HackRF, LimeSDR, or AntSDR merely to
display IQ.

Raw recordings contain capture facts and sufficient hardware metadata to
interpret the samples. Project/session state contains display and analysis
choices such as waterfall palette, frequency viewport, markers, channelizers,
and decoder stacks. Backend-specific capture settings are versioned metadata
and remain inspectable when their adapter is unavailable.

Saleae import/export continues to apply only to compatible logic/analog data.
It must reject RF-only export clearly rather than dropping IQ silently.

## Migration sequence

The refactor should remain reviewable and keep the workspace buildable after
each step. The state of each step is in [Current status](#current-status).

1. Move snapshot utilities and tests to `snapshot.rs`. *(done)*
2. Move generic session tabs and their tests to `sessions.rs`. *(done)*
3. Move recording/Saleae functions and tests to `recording.rs`. *(done)*
4. Introduce `devices` and move current DSLogic/DSCope selection without
   changing discovery behavior. *(partial: no `devices::{logic,scope,radio}`
   capability modules)*
5. Move bounded data, timeline, and viewport code into `waveform::store` and
   `waveform::viewport`. *(done)*
6. Move logic rendering, markers, and protocol lanes into `waveform::logic`.
   *(done)*
7. Move scope rendering, markers, FFT, and measurement UI into
   `waveform::scope`. *(done)*
8. Split the top-level update loop into panels that return typed actions.
   *(partial: panels are methods and record requests in `FrameActions`, but
   take `&mut self`; see step 14)*
9. Add the core IQ model, bounded streaming, recording changes, and simulator
   in the order required by `rf-sdr-architecture.md`. *(done)*
10. Add `acquisition::radio`, `devices::radio`, and `waveform::radio` against
    simulated and recorded IQ. *(partial: no `devices::radio`)*
11. Implement the HackRF One adapter and opt-in hardware tests. *(done)*
12. Implement the LimeSDR-USB Type-A adapter with Lime Suite NG and the AntSDR
    E200 adapter with libiio over Gigabit Ethernet as independent changes.
    *(not started)*

Follow-up steps for the GUI structure. They are independent of steps 11 and 12
and should not change behavior:

13. Split the long functions inside the moved code, one behavior-preserving
    change each: `paint_capture_settings` (815 lines),
    `paint_radio_analyzer_configuration` (647), `paint_central_panel` (511),
    `poll_worker` (487, one handler per `WorkerSuccess` variant),
    `start_gui_recording` (435), `paint_scope_time_domain` (397), and
    `poll_capture_target` (383). Where a piece turns out to be pure logic,
    give it a unit test.
14. Narrow what the panels can reach. Group the 77 fields of `WaveCraftApp`
    into per-area state structs (for example logic, scope, radio, capture,
    dialogs) and make each panel take the group it edits, plus `&FrameState`
    and `&mut FrameActions`, instead of `&mut self`. Start with the panels that
    touch few fields: the status bar, view tabs, and scope tools panel.
15. Move `WaveCraftApp`, its constructors, and `run_frame` into `app/mod.rs`.
    Do this after step 14, not before: see below.
16. Add widget-level tests that feed pointer and key events to `run_frame` (or
    use an egui test harness) for the toolbar, session tabs, and settings
    panels. `frame_tests.rs` covers painting and the apply methods but does not
    click anything.

Why step 15 comes after step 14. A Rust field is private to the module that
defines the struct and to that module's descendants. `WaveCraftApp` is defined
in the crate root, so every module in the crate can read and write every field.
Moving the struct into `app/` would hide its private fields from `ui/`, `lib.rs`,
and the tests, which is the point of the move, but those modules use them: 64 of
the 77 fields are accessed from `ui/` (about 240 accesses), plus about 40
accesses in the tests and about 50 in `lib.rs` (counted by searching for
`self.<field>` and `app.<field>`, so the numbers are approximate). Making the
fields `pub(crate)` to keep the code compiling would leave the move purely
cosmetic: it would relocate the struct, its constructors, and `run_frame`
(roughly 400 lines) and leave `lib.rs` as module declarations and `run()`, but
it would not change who can touch the state. The move only restricts access
once step 14 has given the panels something narrower to use.

Steps 1 through 8 and 13 through 15 are structural and should not change
behavior. RF placeholder types may be introduced when step 9 begins; creating
unused device enums or empty backend crates earlier would imply support that
does not yet exist.

## Test boundaries

Each moved module carries its focused tests. Cross-module tests remain at the
application or crate boundary. The default workspace suite must remain
hardware-independent.

Required RF-facing coverage includes:

- registry aggregation when one backend fails and others succeed;
- stable selection of identical devices by connection ID;
- capability-driven settings validation for each backend fixture;
- missing-backend and disconnected-device session restoration;
- bounded IQ display, waterfall, and worker queues;
- reset behavior at overruns, discontinuities, and retunes;
- identical live and offline DSP results for deterministic IQ;
- native recording round trips without an installed physical backend; and
- opt-in discovery, cancellation, and streaming tests per supported physical
  device model.

GUI rendering tests should use capability fixtures named for HackRF One,
LimeSDR, and AntSDR. These fixtures verify generic UI behavior only and must not
be described as hardware support.

## Completion criteria

The modularization is complete when:

- `main.rs` contains only native startup and construction of `WaveCraftApp`;
- the top-level app module coordinates features without implementing waveform
  algorithms or file formats;
- logic, scope, and RF views have isolated state, rendering, and tests;
- adding an SDR backend does not require editing logic or scope modules;
- device-specific APIs are confined to their backend adapters;
- all capture sources still use the core coordinator and typed batches; and
- formatting, Clippy, the default workspace tests, and release builds remain
  clean after the migration.

Against these, as of commit `b3222cc`: `main.rs` only starts the app, and
formatting, Clippy, the workspace tests, and the release build pass. `lib.rs`
is 688 lines of module declarations, the state struct, and the frame loop, but
rendering and file formats are in their own modules while the state is still one
struct, so "isolated state" is not met (step 14). The GUI still registers
`HackRfAdapter` in `begin_refresh` and uses HackRF settings types, so "device
APIs confined to adapters" is only partly met. The other two criteria were not
assessed.
