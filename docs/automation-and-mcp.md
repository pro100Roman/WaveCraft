# Automation, Scenario, VISA/SCPI, and MCP Architecture

Status: proposed

WaveCraft should support direct unattended hardware operation, repeatable
diagnostic scenarios, conventional scripting, and AI-assisted debugging. These
use cases share the same device and capture lifecycle, but they require
different adapters. The canonical automation interface is therefore a typed,
Saleae-style device-and-capture service. Scenarios, the CLI, the GUI, a future
local daemon, SCPI, and MCP are clients of that service rather than independent
implementations of hardware control.

```text
Hardware/core --- device and capture service
                         |
                         +-- wavecraft CLI ----- humans, scripts, and CI
                         +-- scenario engine --- repeatable workflows
                         +-- GUI --------------- native integration
                         +-- local daemon ------ persistent external clients
                         +-- SCPI endpoint ----- VISA clients and test systems
                         `-- MCP adapter ------- AI agents
```

The service owns discovery, explicit selection, capabilities, preparation,
capture handles, recording, decoding, cancellation, and errors. Hardware
drivers remain in `wavecraft-usb`, independent of every user interface and
automation protocol. The service never depends on implicit GUI selection or
GUI view state; every operation resolves to an explicit device ID or handle and
a complete capture configuration.

### Current implementation

The first device-service milestone is implemented in
`crates/wavecraft-automation`. `AutomationManager` discovers USB instruments
and deterministic simulators, returns schema-versioned descriptions, resolves
selectors to exactly one connection, and acquires a process-wide exclusive
lease before returning `DeviceHandle`. Handles expose capabilities and
read-only probing, detect disconnect-after-selection, and release their lease
when explicitly closed or dropped. Structured errors distinguish no match,
ambiguous match, busy state, disconnect, discovery failure, and probe failure.

The initial end-to-end CLI surface is:

```text
wavecraft-cli devices list --format json
wavecraft-cli devices probe --id sim:logic-0 --format json
wavecraft-cli devices probe --id <usb-connection-id> --format json
```

USB persistent identities remain absent when the current non-opening discovery
path cannot read a serial number. Capture handles and direct automation capture
are the next milestone; they must build on this device service rather than GUI
selection state.

## Goals

- Run deterministic captures without the GUI.
- Control one explicitly selected device through a stable capture lifecycle.
- Select instruments explicitly and reject ambiguous matches.
- Express repeatable capture and analysis scenarios as versioned data.
- Produce stable machine-readable results and live progress events.
- Preserve the lossless `.dsv` recording as the source of truth.
- Give AI agents bounded, structured access to diagnostics and signal data.
- Keep the same operations and schemas across CLI, daemon, SCPI, and MCP
  adapters.
- Test automation against `wavecraft-sim` without connected USB hardware.

## Non-goals

- MCP is not the primary or only automation API.
- Scenarios are not the canonical hardware-control API.
- SCPI is an adapter for laboratory automation, not the canonical internal API.
- Scenarios do not embed arbitrary shell commands.
- Raw captures are not serialized into large JSON, SCPI, or MCP responses.
- The first version does not expose WaveCraft as a remotely accessible hardware
  service.
- Automation does not weaken device validation, transfer bounds, timeouts, or
  cancellation requirements.

## Device and capture service

Add a crate such as `crates/wavecraft-automation` that exposes typed operations
used by the CLI, GUI, and future adapters. It should coordinate existing core,
USB, simulator, recording, and decoder crates without containing device-model
specific transport code.

The initial object model follows a manager, device, capture, and completed
capture lifecycle:

```rust,ignore
pub struct AutomationManager { /* ... */ }

impl AutomationManager {
    fn list_devices(&self) -> Result<Vec<AutomationDevice>, AutomationError>;
    fn open_device(&self, selector: DeviceSelector)
        -> Result<DeviceHandle, AutomationError>;
    fn inspect_recording(
        &self,
        path: &Path,
    ) -> Result<RecordingSummary, AutomationError>;
}

impl DeviceHandle {
    fn descriptor(&self) -> &AutomationDevice;
    fn capabilities(&self) -> Result<DeviceCapabilities, AutomationError>;
    fn probe(&self) -> Result<DeviceProbeResult, AutomationError>;
    fn start_capture(
        &self,
        configuration: CaptureConfiguration,
    ) -> Result<CaptureHandle, AutomationError>;
}

impl CaptureHandle {
    fn id(&self) -> &CaptureId;
    fn status(&self) -> CaptureStatus;
    fn next_event(&self, timeout: Duration)
        -> Result<Option<AutomationEvent>, AutomationError>;
    fn request_stop(&self) -> Result<(), AutomationError>;
    fn wait(self) -> Result<CompletedCapture, AutomationError>;
}

impl CompletedCapture {
    fn summary(&self) -> &CaptureSummary;
    fn recording_path(&self) -> &Path;
}
```

This is illustrative rather than a fixed synchronous API. `CaptureHandle`
represents a running operation and must expose a bounded progress event stream,
cancellation, status, and a final result. A foreground CLI can own the handle
directly. A future daemon, SCPI server, or MCP server can place handles in a
bounded registry and expose their opaque capture IDs to external clients.

### Capture configuration and lifecycle

Starting a capture requires a complete device configuration. Existing GUI
settings, the currently displayed device, and previously configured decoders
are not part of the automation contract. The configuration contains enabled
channels, sample rate, sample count or duration, trigger, device-specific
settings, timeout, and recording destination.

```text
device.open
    |
    `-- capture.start
            |
            +-- preparing
            +-- armed
            +-- capturing -- wait or stop
            `-- finalizing
                    |
                    +-- completed
                    +-- failed
                    +-- cancelled
                    `-- timed_out
```

WaveCraft differs from Saleae in one deliberate way: the native `.dsv`
recording is written incrementally from capture start rather than being saved
only after capture completion. A terminal success is reported only after the
recording footer and structured result have been flushed. Cancellation,
timeout, source failure, and recording failure must produce distinct terminal
outcomes.

Capture events need a capture-global monotonically increasing sequence number.
Lossless frames flow to the recorder; progress events delivered to clients are
bounded and may be aggregated. A slow JSON, SCPI, or MCP consumer must not
stall USB acquisition or cause unbounded memory growth.

### Device discovery, selection, and ownership

Device selection is the first automation milestone and must be complete before
capture control is exposed. The required lifecycle is:

```text
discover devices
      |
      `-- describe identity and capabilities
              |
              `-- resolve selector to exactly one device
                      |
                      `-- acquire exclusive lease
                              |
                              `-- return DeviceHandle
```

The current device-independent `DeviceDescriptor` is intentionally small.
Automation needs a richer, schema-stable description containing at least:

- connection ID for the current USB topology;
- serial number or other persistent identity when available;
- instrument kind and model;
- physical USB location;
- negotiated USB link speed and warnings;
- supported channels, rates, buffers, triggers, and operation modes;
- firmware and FPGA HDL versions when probed;
- preparation and busy state.

The connection ID and persistent identity are separate concepts: a topology
identifier may change when an instrument moves to another port, while a serial
number identifies the physical instrument when the hardware exposes one.

Initial selectors should support an exact connection ID, a persistent serial
number when available, and model or capability predicates. Device selectors
must resolve to exactly one instrument. A selector matching no devices returns
`device_not_found`; one matching multiple devices returns
`ambiguous_device_match` together with bounded candidate descriptions.
Automation must never silently select the first matching physical instrument.

For example, three connected DSCope U3P100 instruments are three independent
devices even though their models and capabilities are identical:

```text
usb:1-2.1  DSCope U3P100
usb:1-2.2  DSCope U3P100
usb:1-3    DSCope U3P100
```

Selecting only `model = dscope_u3p100` is ambiguous. Selecting
`connection_id = usb:1-2.2` resolves exactly one instrument. Multi-device
capture is a separate, explicit operation that resolves every requested role
or selector independently; it is never inferred from an ambiguous single-
device selector.

Resolution and ownership are separate steps. A successful match does not imply
that the device is still available, so `open_device` must atomically acquire an
exclusive lease or return `device_busy`. The lease is held by `DeviceHandle`
and released when that handle is closed or dropped. A UI or SCPI connection may
retain a convenient per-client selected device, but internally every operation
uses its resolved handle. A future daemon makes leases authoritative across
multiple external clients; without it, the process-local lease and operating-
system USB interface claim provide the available protection.

Simulator discovery, selection, ambiguity handling, and leasing must use the
same public contract as USB devices. Tests for zero matches, one match, multiple
matches, busy devices, disconnect-after-selection, and lease release do not
require physical hardware except for opt-in transport coverage.

### Structured errors

Errors exposed outside the process need more than a display string. The public
error schema should contain:

```json
{
  "schema": "wavecraft.error/v1",
  "code": "device_busy",
  "message": "DSLogic U3Pro32 is already in use",
  "device_id": "usb:3-2",
  "operation": "capture.start",
  "retryable": true,
  "context": {}
}
```

Stable initial codes should cover missing or ambiguous devices, busy devices,
unsupported configurations, preparation failures, capture timeouts, transport
disconnects, cancellation, decoder failures, and recording I/O or integrity
failures. Human-readable messages may improve without changing the code.

## CLI contract

The CLI is the first public automation surface. It should use a conventional
argument parser and organize operations under stable command groups:

```text
wavecraft devices list --format json
wavecraft devices probe --id usb:3-2 --format json
wavecraft capture run --device usb:3-2 --sample-rate 50000000 \
  --samples 10000000 --channels 0,1,2,3 --output capture.dsv \
  --events ndjson
wavecraft capture run --device sim:logic-0 --sample-rate 100000 \
  --samples 10000 --channels 0,1,2,3 --output capture.dsv \
  --events ndjson
wavecraft scenario validate spi-boot.yaml
wavecraft scenario run spi-boot.yaml --bind logic=usb:3-2 \
  --events ndjson --artifacts runs/001
wavecraft recording inspect runs/001/capture.dsv --format json
wavecraft recording decode runs/001/capture.dsv --decoder spi
```

Direct `capture run` is implemented first and establishes the public device,
configuration, capture event, cancellation, recording, error, and result
contracts. `scenario run` is subsequently implemented as orchestration over
the same operations. The simulator and USB backends must accept the same
device-independent capture configuration where their capabilities overlap.

The machine interface follows these rules:

- final results use schema-versioned JSON;
- live events and progress use NDJSON;
- machine output is written to stdout and diagnostics to stderr;
- exit codes are documented and stable;
- raw samples are written to `.dsv` or an explicitly requested binary stream;
- capture output paths are never silently overwritten;
- interruption requests cancellation and writes a cancelled recording footer;
- unknown fields in compatible schema versions are tolerated by consumers.

Human-readable output remains the default for interactive commands, but it is
not an automation contract and must not be parsed by clients.

An initial Python integration should invoke direct CLI capture commands as a
subprocess and consume NDJSON incrementally. This avoids a platform-specific
native binding and works before a resident service exists. When persistent
capture handles or multiple external clients become necessary, a local daemon
can host the same `AutomationManager` rather than introducing a second API.

## Scenario format

Scenarios are declarative YAML or JSON clients of the device-and-capture
service. They have an explicit schema identifier and describe desired
WaveCraft operations, not implementation steps or shell scripts. Scenario
execution must resolve devices and call the same `start_capture`, wait/stop,
recording, decoder, and analysis operations used by direct clients.

```yaml
schema: wavecraft.scenario/v1
name: spi-boot-check

devices:
  logic:
    require:
      kind: logic_analyzer
      minimum_link: high_speed

capture:
  timeout: 15s
  output: capture.dsv
  sources:
    logic:
      sample_rate_hz: 50000000
      sample_count: 10000000
      channels: [0, 1, 2, 3]
      trigger:
        channel: 3
        edge: falling
```

Environment-specific identity is supplied as a binding when practical:

```text
wavecraft scenario run spi-boot.yaml --bind logic=sim:logic-0
wavecraft scenario run spi-boot.yaml --bind logic=usb:3-2
```

This lets one scenario express logical roles and capability requirements while
the caller explicitly selects the physical or simulated instruments. A
scenario may contain a persistent serial-number selector when binding to one
specific laboratory setup is intentional.

The first scenario version should reuse the already proven direct-capture
contract and initially support:

- explicit device matching and capability requirements;
- preparation and per-source capture configuration;
- trigger, duration, sample-count, and timeout termination;
- lossless recording;

Later additive versions may support:

- concurrent logic-analyzer and oscilloscope capture;
- first-party decoder configuration;
- structured measurements and checks;
- bounded retry policies;
- explicit variables supplied by the caller.

Scenario validation must be available without hardware. Static validation
checks document structure, value ranges, channel references, and any decoder
configuration present in the schema. Validation against connected hardware
additionally checks model capabilities and resource availability before making
state changes.

Arbitrary commands are excluded from the first schema. External test systems
can control the device under test and call WaveCraft at the appropriate time. If
integrated actions become necessary, add a manifest-based, explicitly enabled,
out-of-process action plugin contract with argument schemas, timeouts, output
limits, and cancellation.

## Local daemon

A resident `wavecraftd` service is useful after automation needs persistent device
ownership or sessions that outlive one CLI process. It can provide:

- exclusive device leases;
- long-running or triggered capture;
- multiple local clients;
- live status after the initiating client disconnects;
- explicit cancellation and bounded event buffering;
- GUI and automation access to one hardware manager;
- scheduled scenarios.

The daemon should host `AutomationManager`, acquire device leases, place active
`CaptureHandle` values in a bounded registry, and expose start, status, event,
wait, stop, and artifact operations through opaque capture IDs. Scenario run
IDs may refer to one or more capture IDs. Capture and run state remain explicit
in the API rather than hidden in a transport session. The daemon binds locally
by default. Remote access, authentication, and authorization are a separate
deployment feature rather than a consequence of enabling automation.

## VISA and SCPI adapter

WaveCraft may expose a SCPI endpoint for integration with PyVISA, LabVIEW,
MATLAB, and existing automated test systems. VISA is the client-side I/O API;
WaveCraft implements the instrument-side command protocol. The endpoint must be a
thin adapter over the application service or `wavecraftd`, not an automation layer
over GUI controls. GUI view state is outside the SCPI contract.

The initial transport should be a newline-delimited SCPI TCP socket bound to
the loopback interface by default. A client can address it as a VISA socket
resource such as:

```text
TCPIP0::127.0.0.1::5025::SOCKET
```

The port is configurable. VXI-11 or HiSLIP may be added later when discovery,
service requests, or `TCPIP::...::INSTR` compatibility justify the additional
protocol complexity. A raw socket endpoint must not advertise capabilities,
discovery, or VISA event semantics that it does not implement.

The adapter should support the applicable IEEE 488.2 common commands,
including `*IDN?`, `*CLS`, `*OPC?`, and status queries, together with
`SYSTem:ERRor?`. WaveCraft-specific commands should map directly to typed service
operations. An illustrative command tree is:

```text
WaveCraft:DEVice:LIST?
WaveCraft:DEVice:SELect "usb:3-2"
WaveCraft:DEVice:PROBe?
WaveCraft:CAPTure:CONFigure <bounded-configuration>
WaveCraft:CAPTure:STARt?
WaveCraft:CAPTure:STATus? "capture-id"
WaveCraft:CAPTure:STOP "capture-id"
WaveCraft:CAPTure:RESult? "capture-id"
WaveCraft:SCENario:VALidate? "spi-boot.yaml"
WaveCraft:SCENario:LOAD "spi-boot.yaml"
WaveCraft:RUN:STARt?
WaveCraft:RUN:STATus? "run-id"
WaveCraft:RUN:STOP "run-id"
WaveCraft:RUN:RESult? "run-id"
WaveCraft:RECording:INSPect? "capture.dsv"
```

Exact command names, abbreviations, response formats, and error mappings are a
versioned public contract. Direct capture commands map to the same explicit
device handle and complete capture configuration as the native service.
Commands that start long operations return opaque capture or scenario-run IDs.
Clients normally poll status or the standard operation status model; `*OPC?`
may wait for operations started by the same connection. Live NDJSON is not
multiplexed into SCPI query responses. Clients needing a rich event stream
should use the CLI or daemon API.

SCPI session state, such as the selected device and pending error queue, is per
connection. Device ownership and runs are service-level state shared with all
other adapters. The contract must define whether a run continues or is
cancelled when its initiating connection closes. `*RST` must have a bounded,
documented meaning and must not silently perform maintenance operations.

Responses contain bounded summaries and artifact references. Raw captures
remain in `.dsv` files. If bounded waveform transfer is later supported, it
uses IEEE binary blocks with explicit channel, sample, response-size, and
execution-time limits. Scenario and artifact paths are restricted to allowed
directories. The server binds locally by default; remote exposure requires a
separate authentication, authorization, and transport-security design.

A Python client may use a VISA implementation without a native WaveCraft binding:

```python
import pyvisa

resources = pyvisa.ResourceManager("@py")
instrument = resources.open_resource("TCPIP0::127.0.0.1::5025::SOCKET")
instrument.write_termination = "\n"
instrument.read_termination = "\n"
instrument.timeout = 20_000

print(instrument.query("*IDN?"))
instrument.write('WAVECRAFT:DEVICE:SELECT "usb:3-2"')
instrument.write('WAVECRAFT:CAPTURE:CONFIGURE 50000000,10000000,"0,1,2,3"')
capture_id = instrument.query("WAVECRAFT:CAPTURE:START?").strip()
print(instrument.query(f'WAVECRAFT:CAPTURE:STATUS? "{capture_id}"'))
```

## Saleae Logic 2 automation and MCP reference

Saleae Logic 2 provides a useful reference for separating deterministic
automation from AI-agent integration. The interfaces below were verified
against Logic 2.4.48 and Saleae's published documentation. They apply to the
Logic 8, Logic Pro 8, and Logic Pro 16 family; Logic MSO uses a separate API
and is not currently supported by the Logic 2 MCP server.

### Automation API

The supported Automation API is a gRPC service bound to `127.0.0.1:10430` by
default. Saleae publishes the `logic2-automation` Python package as the primary
client and the protobuf definition for clients in other languages. The server
can be enabled from Logic 2's Automation panel or at process startup:

```text
Logic --automation --automationPort 10430
```

The API can:

- enumerate physical and simulated instruments;
- configure analog and digital channels, sample rates, thresholds, glitch
  filters, capture buffers, and manual, timed, or digital-trigger modes;
- start, stop, and wait for captures;
- add and remove protocol analyzers and local high-level analyzers;
- load and save `.sal` capture files;
- export raw analog and digital data as CSV or per-channel binary files; and
- export analyzer results as a filtered data-table CSV or legacy analyzer
  output.

Automation creates a capture session from an explicit configuration. Existing
GUI channel settings and analyzers are not an implicit automation contract.
Large sample data is exported to files rather than returned by gRPC, and the
API does not provide continuous real-time access to raw samples. Clients are
expected to handle capture, USB, disconnect, and out-of-memory errors and to
discard failed captures before retrying.

The official starting points are the
[Automation API guide](https://docs.saleae.com/automation/guides/getting-started)
and the
[API reference](https://docs.saleae.com/automation/api/automation/).

### MCP server

Logic 2.4.48 also contains an experimental MCP server using Streamable HTTP on
`http://127.0.0.1:10530`. It can be enabled from the Automation panel and
connected to Codex with:

```text
codex mcp add --url http://127.0.0.1:10530 logic2
codex mcp list
```

The server exposes 15 tools:

```text
get_devices
start_capture
stop_capture
wait_capture
load_capture
save_capture
close_capture
add_analyzer
remove_analyzer
add_high_level_analyzer
remove_high_level_analyzer
export_raw_data_csv
export_raw_data_binary
export_data_table_csv
legacy_export_analyzer
```

These tools mirror the capture lifecycle and file-oriented exports of the
Automation API. Capture IDs and analyzer IDs are passed between calls, analyzer
configuration is schema-validated, and long-running capture completion is an
explicit `wait_capture` operation. MCP therefore adds an agent-friendly tool
surface without making GUI state or raw sample transport the protocol.

Saleae documents the server and its experimental status in the
[Logic 2 MCP guide](https://docs.saleae.com/mcp/guides/getting-started).
Codex's Streamable HTTP configuration model is described in the
[OpenAI MCP documentation](https://developers.openai.com/learn/docs-mcp).

### Implications for WaveCraft

The Saleae design supports several choices in this document:

- keep a deterministic automation interface usable without an AI client;
- expose an explicit manager, device configuration, capture handle, and
  completed-capture lifecycle;
- make MCP a thin, local adapter over the same capture and analysis service;
- represent captures as explicit stateful objects with opaque IDs;
- require complete capture configuration instead of scripting GUI controls;
- keep large raw data in recording or export artifacts;
- expose semantic analyzer and export operations as bounded tools; and
- test with simulated devices through the same public interface.

WaveCraft adopts this direct device-and-capture approach as its canonical
automation model, but should not copy the exact Saleae tool set. WaveCraft
scenarios are layered over direct capture operations, and concurrent
multi-source capture, native `.dsv` recordings, structured progress events,
and bounded recording inspection require a broader service contract. WaveCraft
also deliberately rejects ambiguous device matches rather than silently using
the first matching physical instrument.

## MCP adapter

MCP is an adapter for AI agents, not a replacement for the device-and-capture
service or CLI. The initial MCP server should use a local standard-I/O
transport and call the same typed service. A subprocess adapter may call the
structured CLI while that shared service is being extracted.

Useful MCP tools include:

- `list_devices`;
- `get_device_capabilities`;
- `probe_device`;
- `start_capture`;
- `get_capture_status`;
- `wait_capture`;
- `stop_capture`;
- `inspect_recording`;
- `decode_recording`;
- `measure_signal`;
- `compare_capture_regions`;
- `create_debug_bundle`;
- later, `validate_scenario` and `start_scenario`.

Read-only MCP resources can expose:

```text
wavecraft://devices/{device_id}
wavecraft://captures/{capture_id}/summary
wavecraft://captures/{capture_id}/diagnostics
wavecraft://runs/{run_id}/summary
wavecraft://runs/{run_id}/diagnostics
wavecraft://recordings/{recording_id}/metadata
wavecraft://recordings/{recording_id}/decoded/{decoder_id}
wavecraft://recordings/{recording_id}/window?...bounded parameters...
```

Start operations return explicit capture IDs that later calls pass back. A
scenario may create a separate run ID that refers to one or more captures.
Tool results contain summaries and artifact references rather than entire
captures. Waveform window, edge search, frequency measurement, pulse
statistics, and decoder tools must enforce sample, channel, response-size, and
execution-time limits.

The current official Rust MCP SDK may have a newer minimum supported Rust
version than WaveCraft. Adding it must not silently raise the workspace MSRV. The
implementation options are to begin with a Python MCP adapter over the stable
CLI, deliberately raise the WaveCraft MSRV, or build the MCP adapter as a
separately versioned component. Implementing an ad hoc subset of MCP is not a
preferred alternative.

### Agent permissions and safety

Server-side policy is authoritative; tool descriptions and client approval UI
are not security boundaries. Separate permissions into at least:

- observe: list devices and read existing artifacts;
- capture: perform model-approved preparation and run bounded captures;
- maintenance: load arbitrary FPGA or firmware data or perform other low-level
  state changes.

Enforce exclusive leases, path restrictions, capture-size limits, timeouts,
allowed resource directories, and cancellation for every client. Maintenance
operations should be distinct tools and should not be hidden inside a generic
scenario action.

## AI debugging artifacts

Each run may produce a self-contained bundle for human or agent analysis:

```text
run-001/
+-- manifest.json
+-- scenario.yaml
+-- result.json
+-- events.ndjson
+-- diagnostics.ndjson
+-- capture.dsv
+-- decoded/
|   +-- spi.ndjson
|   `-- i2c.ndjson
`-- environment.json
```

The bundle should record:

- WaveCraft build, schema versions, platform, and timestamps;
- device model, connection ID, persistent identity or redacted serial, USB
  topology, and link speed;
- firmware, HDL, calibration identity, and FPGA resource hashes;
- requested and effective capture configuration;
- trigger information and synchronization measurements;
- structured errors, warnings, and transport statistics;
- recording completion and integrity status;
- decoder output, signal measurements, and check results.

Sensitive identifiers should be redactable. Because an original recording may
itself contain those identifiers, a redacted bundle must either omit the
recording, contain a sanitized recording copy, or explicitly mark the original
recording as unredacted. The manifest contains hashes and relative paths so a
bundle can be moved and its contents verified.

AI-oriented analysis should prefer semantic operations over raw data transfer.
For example, an agent can request edge locations, frequency drift, pulse-width
outliers, decoder errors, or a bounded time window. The `.dsv` file remains the
complete evidence and can be reanalyzed locally when the initial summary is
insufficient.

## Compatibility and testing

Automation schemas are public contracts. Every output includes a schema name
and version. Breaking changes require a new major schema version; additive
fields are allowed when readers ignore unknown fields.

Tests should cover:

- direct capture configuration parsing, normalization, and validation;
- exact-zero and ambiguous device selection;
- device capability and configuration validation before state changes;
- successful, cancelled, timed-out, and failed captures;
- capture lifecycle transitions and terminal recording status;
- concurrent simulated logic and scope capture;
- ordering and bounded buffering of NDJSON events;
- stable error codes and exit status mapping;
- complete and interrupted `.dsv` artifacts;
- representative Python clients on every supported platform;
- scenario parsing, binding, normalization, validation, and translation into
  direct capture operations;
- MCP tool schema validation and response bounds;
- SCPI parsing, abbreviations, status registers, error queues, and response
  bounds;
- representative PyVISA clients over the loopback TCP socket;
- SCPI disconnect, cancellation, and cross-adapter device ownership behavior;
- exclusive device ownership and cancellation in the daemon.

Hardware tests remain opt-in. Every automation behavior that does not depend on
a physical USB transfer should be exercised with deterministic simulated
sources and recorded fixtures.

## Delivery plan

1. Define structured device identity, description, capability, selector, and
   device-resolution error types.
2. Add `AutomationManager` discovery for simulated and USB devices and expose
   schema-versioned device listings through the CLI.
3. Implement exact selector resolution with tests for zero, one, and multiple
   matches; remove implicit first-match selection from public automation paths.
4. Implement exclusive process-local leases and `DeviceHandle`, including busy,
   disconnect, and release behavior.
5. Expose capability inspection and read-only probing through the selected
   device handle.
6. Define capture configuration, capture error, event, status, result,
   `CaptureHandle`, and `CompletedCapture` types.
7. Implement deterministic, finite simulated capture through a selected
   simulated device.
8. Centralize capture execution, progress aggregation, cancellation, timeout,
   incremental recording, and terminal footer selection in the automation
   crate.
9. Add direct CLI `capture` commands plus schema-versioned JSON and NDJSON.
10. Add simulator end-to-end tests for successful, cancelled, timed-out,
    failed, and non-overwriting capture behavior.
11. Implement the same selected-device capture contract for USB instruments and
    keep hardware tests opt-in.
12. Extend `.dsv` metadata with requested and effective configuration, resolved
    device identity, build information, and structured completion context.
13. Implement `wavecraft.scenario/v1` as orchestration over the proven direct
    capture API, including explicit role-to-device bindings.
14. Add first-party decoders, bounded signal-analysis operations, and portable
    debug bundles.
15. Add MCP read-only tools, then bounded direct capture tools, over the same
    service.
16. Introduce `wavecraftd` when persistent ownership, shared capture handles,
    or background runs are required.
17. Add a loopback SCPI socket adapter after shared leases and persistent
    capture ownership are established.

The first milestone is deterministic discovery, exact selection, and exclusive
ownership of one simulated or USB device, with structured results for no match,
ambiguous match, and busy state. The second milestone is direct control of one
explicitly selected simulated logic analyzer: a CLI command supplies the
complete capture configuration, emits versioned NDJSON, supports cancellation
and timeout, writes a valid `.dsv`, and returns a structured result. The next
milestone runs that same operation against one explicitly selected USB
instrument. Scenarios and MCP then reuse the proven device-and-capture
lifecycle rather than defining a separate control path.
