# GUI modularization and multi-SDR integration plan

Status: proposed.

This document describes a behavior-preserving split of the current
`apps/wavecraft-gui/src/main.rs` and reserves explicit integration seams for
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

## Why split the GUI now

`apps/wavecraft-gui/src/main.rs` currently contains the executable entry point,
application state, capture orchestration, device selection, session tabs,
recording import and export, settings panels, dialogs, waveform storage,
viewport calculations, logic rendering, scope rendering, FFT processing,
markers, and their tests. These responsibilities change for different reasons
and already have useful internal boundaries.

The refactor should improve ownership and test locality, not only reduce the
line count of `main.rs`. In particular:

- the executable entry point should not contain domain or rendering logic;
- hardware-specific settings must not spread through general-purpose panels;
- logic, scope, and RF views should share only genuinely common viewport and
  panel infrastructure;
- recording and capture workers must remain independent of display buffers;
- the application remains the owner of mutable GUI state; and
- focused tests should live beside the module whose behavior they verify.

## Proposed source layout

```text
apps/wavecraft-gui/src/
├── main.rs                       # native startup only
├── app/
│   ├── mod.rs                    # WaveCraftApp and top-level update cycle
│   ├── actions.rs                # typed actions returned by panels/dialogs
│   ├── state.rs                  # cross-view application state
│   └── workers.rs                # polling and background-task coordination
├── acquisition/
│   ├── mod.rs                    # instrument-independent capture lifecycle
│   ├── logic.rs                  # DSLogic capture/session settings
│   ├── scope.rs                  # DSCope capture/session settings
│   └── radio.rs                  # generic RF capture/session settings
├── devices/
│   ├── mod.rs                    # discovery aggregation and stable selection
│   ├── logic.rs                  # logic capability mapping
│   ├── scope.rs                  # scope capability mapping
│   └── radio.rs                  # SDR capability mapping and backend labels
├── sessions.rs                   # generic session tabs and saved view state
├── recording.rs                  # native/Saleae loading, saving, metadata
├── snapshot.rs                   # screenshot crop and PNG persistence
├── theme.rs                      # colors, spacing, and common widgets
├── format.rs                     # presentation-only value formatting
├── ui/
│   ├── mod.rs
│   ├── toolbar.rs
│   ├── status.rs
│   ├── capture_settings.rs       # tab shell; delegates instrument controls
│   ├── device_selector.rs
│   ├── decoded_panel.rs
│   └── dialogs/
│       ├── mod.rs
│       ├── calibration.rs
│       ├── protocol_analyzer.rs
│       └── settings.rs
├── waveform/
│   ├── mod.rs                    # waveform cards and shared pane layout
│   ├── store.rs                  # bounded display data and revisions
│   ├── viewport.rs               # pan, zoom, time axes, combined timeline
│   ├── logic/
│   │   ├── mod.rs
│   │   ├── markers.rs
│   │   ├── protocol_lanes.rs
│   │   └── render.rs
│   ├── scope/
│   │   ├── mod.rs
│   │   ├── fft.rs
│   │   ├── markers.rs
│   │   ├── measurements.rs
│   │   └── render.rs
│   └── radio/
│       ├── mod.rs                # RF Spectrum view state and composition
│       ├── spectrum.rs
│       ├── waterfall.rs
│       └── markers.rs
├── measurements.rs               # existing scope measurement engine
└── protocol_analyzer.rs           # existing logic protocol engine
```

This is a destination layout, not a requirement to create every empty file in
the first change. Modules should be introduced when code and tests move into
them. Empty Rust modules add navigation cost without establishing a real
contract.

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

The `eframe::App::update` implementation should eventually read as a sequence
of orchestration operations:

```text
poll workers and capture events
reconcile selected devices and tabs
paint toolbar and settings panels
paint the active instrument view
paint dialogs and warnings
apply returned actions
schedule repaint when work remains
```

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

`wavecraft-hackrf` remains the first planned physical backend and follows the
detailed sequence in `rf-sdr-architecture.md`. Its initial scope is
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
each step:

1. Move snapshot utilities and tests to `snapshot.rs`.
2. Move generic session tabs and their tests to `sessions.rs`.
3. Move recording/Saleae functions and tests to `recording.rs`.
4. Introduce `devices` and move current DSLogic/DSCope selection without
   changing discovery behavior.
5. Move bounded data, timeline, and viewport code into `waveform::store` and
   `waveform::viewport`.
6. Move logic rendering, markers, and protocol lanes into `waveform::logic`.
7. Move scope rendering, markers, FFT, and measurement UI into
   `waveform::scope`.
8. Split the top-level update loop into panels that return typed actions.
9. Add the core IQ model, bounded streaming, recording changes, and simulator
   in the order required by `rf-sdr-architecture.md`.
10. Add `acquisition::radio`, `devices::radio`, and `waveform::radio` against
    simulated and recorded IQ.
11. Implement the HackRF One adapter and opt-in hardware tests.
12. Implement the LimeSDR-USB Type-A adapter with Lime Suite NG and the AntSDR
    E200 adapter with libiio over Gigabit Ethernet as independent changes.

Steps 1 through 8 are structural and should not change behavior. RF placeholder
types may be introduced when step 9 begins; creating unused device enums or
empty backend crates earlier would imply support that does not yet exist.

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
