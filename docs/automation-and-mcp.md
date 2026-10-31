# Automation, Scenario, and MCP Architecture

Status: proposed

DSView should support unattended hardware operation, repeatable diagnostic
scenarios, conventional scripting, and AI-assisted debugging. These use cases
share the same device and capture operations, but they require different
adapters. The CLI, a future local service, and an MCP server must therefore be
thin clients of one application service rather than independent
implementations of hardware control.

```text
                 +-- dsview CLI ----- humans, scripts, and CI
Hardware/core ---+-- application ---- GUI and native integrations
                 +-- local daemon --- persistent sessions and Python clients
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
- Keep the same operations and schemas across CLI, daemon, and MCP adapters.
- Test automation against `dsview-sim` without connected USB hardware.

## Non-goals

- MCP is not the primary or only automation API.
- Scenarios do not embed arbitrary shell commands.
- Raw captures are not serialized into large JSON or MCP responses.
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
7. Add a thin local MCP adapter over the CLI or application service.
8. Introduce `dsviewd` when persistent ownership and background runs are
   required.

The first milestone is a YAML scenario that runs against `dsview-sim`, emits
versioned NDJSON, writes a valid `.dsv`, and then runs unchanged against one
explicitly selected USB instrument.
