# Automation, Scenario, VISA/SCPI, and MCP Architecture

Status: proposed

DSView should support unattended hardware operation, repeatable diagnostic
scenarios, conventional scripting, and AI-assisted debugging. These use cases
share the same device and capture operations, but they require different
adapters. The CLI, a future local service, a SCPI endpoint, and an MCP server
must therefore be thin clients of one application service rather than
independent implementations of hardware control.

```text
                 +-- dsview CLI ----- humans, scripts, and CI
Hardware/core ---+-- application ---- GUI and native integrations
                 +-- local daemon --- persistent sessions and Python clients
                 +-- SCPI endpoint -- VISA clients and test systems
                 `-- MCP adapter ---- AI agents
```

The application service owns discovery, selection, preparation, capture,
recording, decoding, cancellation, and errors. Hardware drivers remain in
`dsview-usb`, independent of every user interface and automation protocol.

## Goals

- Run deterministic captures without the GUI.
- Select instruments explicitly and reject ambiguous matches.
- Express repeatable capture and analysis scenarios as versioned data.
- Produce stable machine-readable results and live progress events.
- Preserve the lossless `.dsv` recording as the source of truth.
- Give AI agents bounded, structured access to diagnostics and signal data.
- Keep the same operations and schemas across CLI, daemon, SCPI, and MCP
  adapters.
- Test automation against `dsview-sim` without connected USB hardware.

## Non-goals

- MCP is not the primary or only automation API.
- SCPI is an adapter for laboratory automation, not the canonical internal API.
- Scenarios do not embed arbitrary shell commands.
- Raw captures are not serialized into large JSON, SCPI, or MCP responses.
- The first version does not expose DSView as a remotely accessible hardware
  service.
- Automation does not weaken device validation, transfer bounds, timeouts, or
  cancellation requirements.

## Application service

Add a crate such as `crates/dsview-automation` that exposes typed operations
used by the CLI, GUI, and future adapters. It should coordinate existing core,
USB, simulator, recording, and decoder crates without containing device-model
specific transport code.

An initial interface may resemble:

```rust,ignore
pub trait DsViewService {
    fn list_devices(&self) -> Result<Vec<AutomationDevice>, AutomationError>;
    fn validate_scenario(&self, scenario: Scenario) -> ValidationReport;
    fn run_scenario(&self, scenario: Scenario) -> Result<ScenarioRun, AutomationError>;
    fn stop_run(&self, run_id: &RunId) -> Result<(), AutomationError>;
    fn inspect_recording(
        &self,
        path: &Path,
    ) -> Result<RecordingSummary, AutomationError>;
}
```

This is illustrative rather than a fixed synchronous API. Long-running capture
operations must expose progress, cancellation, and a final result. A future
daemon may represent them as persistent jobs, while the CLI can consume the
same event stream in its foreground process.

### Automation device description

The current device-independent `DeviceDescriptor` is intentionally small.
Automation needs a richer, schema-stable description containing at least:

- stable device ID;
- instrument kind and model;
- serial number when available;
- physical USB location;
- negotiated USB link speed and warnings;
- supported channels, rates, buffers, triggers, and operation modes;
- firmware and FPGA HDL versions when probed;
- preparation and busy state.

Device selectors must resolve to exactly one instrument. A selector matching no
devices returns `device_not_found`; one matching multiple devices returns
`ambiguous_device_match`. Automation must never silently select the first
matching physical instrument.

### Structured errors

Errors exposed outside the process need more than a display string. The public
error schema should contain:

```json
{
  "schema": "dsview.error/v1",
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
dsview devices list --format json
dsview devices probe --id usb:3-2 --format json
dsview scenario validate spi-boot.yaml
dsview scenario run spi-boot.yaml --events ndjson --artifacts runs/001
dsview recording inspect runs/001/capture.dsv --format json
dsview recording decode runs/001/capture.dsv --decoder spi
```

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

An initial Python integration should invoke the CLI as a subprocess and consume
NDJSON incrementally. This avoids a platform-specific native binding and works
before a resident service exists.

## Scenario format

Scenarios are declarative YAML or JSON documents with an explicit schema
identifier. They describe desired DSView operations, not implementation steps
or shell scripts.

```yaml
schema: dsview.scenario/v1
name: spi-boot-check

devices:
  logic:
    match:
      kind: logic_analyzer
      serial: DSL123456
    require:
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

analysis:
  decoders:
    - id: spi
      channels:
        clock: 0
        mosi: 1
        miso: 2
        chip_select: 3

checks:
  - decoder_has_events:
      decoder: spi
      minimum: 1
```

Version 1 should support:

- explicit device matching and capability requirements;
- preparation and per-source capture configuration;
- concurrent logic-analyzer and oscilloscope capture;
- trigger, duration, sample-count, and timeout termination;
- lossless recording;
- first-party decoder configuration;
- structured measurements and checks;
- bounded retry policies;
- explicit variables supplied by the caller.

Scenario validation must be available without hardware. Static validation
checks document structure, value ranges, channel references, and decoder
configuration. Validation against connected hardware additionally checks model
capabilities and resource availability before making state changes.

Arbitrary commands are excluded from the first schema. External test systems
can control the device under test and call DSView at the appropriate time. If
integrated actions become necessary, add a manifest-based, explicitly enabled,
out-of-process action plugin contract with argument schemas, timeouts, output
limits, and cancellation.

## Local daemon

A resident `dsviewd` service is useful after automation needs persistent device
ownership or sessions that outlive one CLI process. It can provide:

- exclusive device leases;
- long-running or triggered capture;
- multiple local clients;
- live status after the initiating client disconnects;
- explicit cancellation and bounded event buffering;
- GUI and automation access to one hardware manager;
- scheduled scenarios.

The daemon should mint opaque run IDs and expose start, status, event, stop, and
artifact operations. Run state is explicit in the API rather than hidden in a
transport session. It binds locally by default. Remote access, authentication,
and authorization are a separate deployment feature rather than a consequence
of enabling automation.

## VISA and SCPI adapter

DSView may expose a SCPI endpoint for integration with PyVISA, LabVIEW,
MATLAB, and existing automated test systems. VISA is the client-side I/O API;
DSView implements the instrument-side command protocol. The endpoint must be a
thin adapter over the application service or `dsviewd`, not an automation layer
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
`SYSTem:ERRor?`. DSView-specific commands should map directly to typed service
operations. An illustrative command tree is:

```text
DSView:DEVice:LIST?
DSView:DEVice:SELect "usb:3-2"
DSView:DEVice:PROBe?
DSView:SCENario:VALidate? "spi-boot.yaml"
DSView:SCENario:LOAD "spi-boot.yaml"
DSView:RUN:STARt?
DSView:RUN:STATus? "run-id"
DSView:RUN:STOP "run-id"
DSView:RUN:RESult? "run-id"
DSView:RECording:INSPect? "capture.dsv"
```

Exact command names, abbreviations, response formats, and error mappings are a
versioned public contract. Commands that start long operations return opaque
run IDs. Clients normally poll status or the standard operation status model;
`*OPC?` may wait for operations started by the same connection. Live NDJSON is
not multiplexed into SCPI query responses. Clients needing a rich event stream
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

A Python client may use a VISA implementation without a native DSView binding:

```python
import pyvisa

resources = pyvisa.ResourceManager("@py")
instrument = resources.open_resource("TCPIP0::127.0.0.1::5025::SOCKET")
instrument.write_termination = "\n"
instrument.read_termination = "\n"
instrument.timeout = 20_000

print(instrument.query("*IDN?"))
instrument.write('DSVIEW:DEVICE:SELECT "usb:3-2"')
instrument.write('DSVIEW:SCENARIO:LOAD "spi-boot.yaml"')
run_id = instrument.query("DSVIEW:RUN:START?").strip()
print(instrument.query(f'DSVIEW:RUN:STATUS? "{run_id}"'))
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

### Implications for DSView

The Saleae design supports several choices in this document:

- keep a deterministic automation interface usable without an AI client;
- make MCP a thin, local adapter over the same capture and analysis service;
- represent captures as explicit stateful jobs with opaque IDs;
- require complete capture configuration instead of scripting GUI controls;
- keep large raw data in recording or export artifacts;
- expose semantic analyzer and export operations as bounded tools; and
- test with simulated devices through the same public interface.

DSView should not copy the exact Saleae tool set. DSView scenarios, concurrent
multi-source capture, native `.dsv` recordings, structured progress events,
and bounded recording inspection require a broader application-service
contract. The Saleae interface is most useful as evidence that deterministic
automation and an AI-facing adapter can share one lifecycle without coupling
hardware drivers to either client protocol.

## MCP adapter

MCP is an adapter for AI agents, not a replacement for the application service
or CLI. The initial MCP server should use a local standard-I/O transport and
call the same typed service. A subprocess adapter may call the structured CLI
while that shared service is being extracted.

Useful MCP tools include:

- `list_devices`;
- `probe_device`;
- `validate_scenario`;
- `start_scenario`;
- `get_run_status`;
- `stop_run`;
- `inspect_recording`;
- `decode_recording`;
- `measure_signal`;
- `compare_capture_regions`;
- `create_debug_bundle`.

Read-only MCP resources can expose:

```text
dsview://devices/{device_id}
dsview://runs/{run_id}/summary
dsview://runs/{run_id}/diagnostics
dsview://recordings/{recording_id}/metadata
dsview://recordings/{recording_id}/decoded/{decoder_id}
dsview://recordings/{recording_id}/window?...bounded parameters...
```

Start operations return explicit run IDs that later calls pass back. Tool
results contain summaries and artifact references rather than entire captures.
Waveform window, edge search, frequency measurement, pulse statistics, and
decoder tools must enforce sample, channel, response-size, and execution-time
limits.

The current official Rust MCP SDK may have a newer minimum supported Rust
version than DSView. Adding it must not silently raise the workspace MSRV. The
implementation options are to begin with a Python MCP adapter over the stable
CLI, deliberately raise the DSView MSRV, or build the MCP adapter as a
separately versioned component. Implementing an ad hoc subset of MCP is not a
preferred alternative.

### Agent permissions and safety

Server-side policy is authoritative; tool descriptions and client approval UI
are not security boundaries. Separate permissions into at least:

- observe: list devices and read existing artifacts;
- capture: prepare devices and run bounded captures;
- maintenance: load FPGA data or perform other low-level state changes.

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

- DSView build, schema versions, platform, and timestamps;
- device model, stable ID or redacted serial, USB topology, and link speed;
- firmware, HDL, calibration identity, and FPGA resource hashes;
- requested and effective capture configuration;
- trigger information and synchronization measurements;
- structured errors, warnings, and transport statistics;
- recording completion and integrity status;
- decoder output, signal measurements, and check results.

Sensitive identifiers should be redactable without changing the original
recording. The manifest contains hashes and relative paths so a bundle can be
moved and its contents verified.

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

- scenario parsing, normalization, and validation;
- exact-zero and ambiguous device selection;
- successful, cancelled, timed-out, and failed runs;
- concurrent simulated logic and scope capture;
- ordering and bounded buffering of NDJSON events;
- stable error codes and exit status mapping;
- complete and interrupted `.dsv` artifacts;
- representative Python clients on every supported platform;
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

1. Define structured automation device, error, result, and event types.
2. Extract shared discovery, selection, capture, and recording operations into
   `dsview-automation`.
3. Replace development-only CLI parsing with command groups and JSON/NDJSON.
4. Implement `dsview.scenario/v1`, validation, and simulator integration tests.
5. Add first-party decoders and bounded signal-analysis operations.
6. Generate portable debug bundles.
7. Add a loopback SCPI socket adapter over the application service or daemon.
8. Add a thin local MCP adapter over the CLI or application service.
9. Introduce `dsviewd` when persistent ownership and background runs are
   required.

The first milestone is a YAML scenario that runs against `dsview-sim`, emits
versioned NDJSON, writes a valid `.dsv`, and then runs unchanged against one
explicitly selected USB instrument.
