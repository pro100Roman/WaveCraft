# Custom protocol decoding architecture

Status: implementation in progress. The strict YAML loader, schema validator,
generic packet model, incremental byte-stream framing, and fixed-layout field
decoder exist in `wavecraft-decode`. Stateful UART, SPI, and I2C decoders retain
transport state across arbitrary sample chunks, reject discontinuities until
explicitly reset, and report incomplete state when finished. A persistent
custom-protocol runtime likewise retains framing state across timed-byte
batches. The GUI owns one persistent runtime per configured analyzer during
live capture and while opening native or Saleae recordings. It consumes every
logic batch before display retention, bounds decoded history independently,
and renders the results in the waveform and decoded-data panel. The stateless
display-window path remains as a compatibility fallback when an analyzer is
added only after a capture has completed. Project persistence remains to be
implemented.

WaveCraft custom protocols describe the meaning of bytes that a first-party
transport decoder has already recovered from logic samples. Version 1 uses a
bounded, declarative YAML format. Kaitai Struct and executable decoder plugins
are outside the first implementation and may be added later without changing
the transport event contract.

## Goals

The first version must:

- stack a custom protocol on an existing UART, SPI, or I2C analyzer;
- decode live capture and saved `.dsv` recordings with the same engine;
- preserve the sample range of every transport byte and decoded field;
- support framing, typed fields, enums, bit fields, conditional fields, and
  bounded validation without running user code;
- produce one generic result model for the waveform, decoded-data panel, CLI,
  exports, and automation;
- keep raw recordings independent from analyzer and UI state;
- save reproducible analyzer configuration in a separate project file; and
- reject malformed schemas and hostile input with actionable diagnostics
  instead of panicking or allocating without a bound.

The first version does not provide arbitrary expressions, scripts, network or
filesystem access from a schema, packet generation, or Kaitai Struct support.
Stateful request/response correlation across multiple frames is a later
addition. The version 1 data model must leave room for it.

## Layered data flow

```text
packed logic samples
        |
        v
first-party transport decoder (UART / SPI / I2C)
        |
        v
timed transport events and byte streams
        |
        v
custom-protocol framer
        |
        v
compiled YAML field parser and validator
        |
        v
generic decoded messages and fields
        |
        +---- waveform lanes
        +---- decoded-data panel
        +---- CLI / NDJSON / automation
        `---- optional exports
```

Transport decoding and custom-protocol decoding are separate operations. A
UART decoder knows where a byte starts and ends and whether framing or parity
failed. It does not know that the byte is a command opcode. Conversely, a
custom-protocol schema consumes bytes and transport attributes; it never reads
logic channel levels directly.

Protocol decoding is a capture-stream consumer, not a view operation. Live
decoding consumes the ordered logic batches alongside recording. Offline
decoding consumes ordered frames from the recording reader. It must not use the
GUI's bounded or downsampled waveform display buffer, because that buffer may
no longer contain every raw sample.

## Continuous and Loop capture

The GUI separates decoding from display retention:

```text
CaptureFrame
    |-- native .dsv recorder
    |-- bounded waveform display buffer
    `-- persistent analyzer runtime
            |-- incremental UART / SPI / I2C decoder
            `-- persistent custom-protocol framer and field parser
                    `-- bounded decoded-event history
```

Each configured analyzer instance owns an incremental runtime. It consumes
every ordered raw logic batch before display downsampling or eviction, retains
partial symbols, transport transactions, and framed bytes across chunk
boundaries, and emits events with absolute sample ranges. A normal capture stop
flushes incomplete state with an explicit diagnostic. Decoded timestamps are
derived from those absolute ranges, so rolling-display eviction does not reset
their time origin. A source discontinuity,
new acquisition, channel remap, sample-rate change, or analyzer configuration
change resets the affected stages rather than joining unrelated samples.

Decoded-event retention is bounded separately from raw waveform retention.
The GUI currently keeps the newest 10,000 events per analyzer; older UI events
may be discarded without changing recorder or decoder input. Loop-mode decode
therefore continues across the rolling waveform's left boundary. Native `.dsv`
loading streams every `CaptureFrame` through the same runtime, and Saleae
loading streams its complete digital sample set before building the display
store.

Analyzer configuration is captured when acquisition or recording loading
starts. Adding or changing an analyzer only after a capture has completed uses
the stateless `ProtocolDecodeCache` compatibility path over the retained raw
display window. Reopening the recording with that analyzer configured runs the
complete offline stream. Project persistence will remove the need for this
manual ordering by restoring analyzer settings and schema snapshots before the
recording pass begins.

## Crate boundaries

The implementation adds a UI-independent `wavecraft-decode` crate with:

- generic sample ranges, transport events, and decoded result types;
- incremental UART, SPI, and I2C decoder interfaces;
- YAML schema loading, validation, and compilation;
- bounded framing and field parsing; and
- deterministic tests that operate without hardware or a windowing backend.

The first-party sample decoders live in `wavecraft-decode`; the GUI adapts their
transport results to its current presentation model. Formatting remains a GUI
concern, while sampling edges, bytes, acknowledgements, baud detection, and
sample ranges are defined and tested outside the application.

A later `wavecraft-project` crate may own project-file serialization when both
the GUI and CLI need to create or modify projects. Initially the versioned
project data-transfer types may live beside the application service layer, but
they must not contain `egui` types.

`wavecraft-core`, hardware drivers, and `wavecraft-recording` do not depend on
the decoder crate. Applications and automation depend on both capture and
decoding. This keeps hardware drivers independent from protocol interpretation.

## Transport event model

All positions are absolute sample indices in the source logic stream. An end
sample is exclusive. Implementations may use different internal containers,
but the public model must preserve the following information. Transport
decoders emit events; the custom framer turns those events into packets:

```rust
pub struct SampleRange {
    pub start: u64,
    pub end: u64,
}

pub struct TimedByte {
    pub value: u8,
    pub range: SampleRange,
    pub status: ByteStatus,
}

pub enum TransportEvent {
    Begin {
        transport: TransportKind,
        start_sample: u64,
        attributes: BTreeMap<String, TransportValue>,
    },
    Bytes {
        stream: StreamId,
        bytes: Vec<TimedByte>,
    },
    Gap {
        range: SampleRange,
    },
    End {
        end_sample: u64,
        attributes: BTreeMap<String, TransportValue>,
    },
    Error {
        range: SampleRange,
        code: TransportErrorCode,
    },
}

pub struct TransportPacket {
    pub transport: TransportKind,
    pub range: SampleRange,
    pub streams: BTreeMap<StreamId, Vec<TimedByte>>,
    pub attributes: BTreeMap<String, TransportValue>,
    pub completeness: PacketCompleteness,
}
```

The stream map supports transport-specific lanes without embedding them in the
custom parser:

| Transport | Version 1 stream names | Relevant attributes |
| --- | --- | --- |
| UART/USART | `data` | baud rate, parity/framing status, optional direction |
| SPI | `mosi`, `miso` | mode, chip-select identity and assertion state |
| I2C | `data` | address, read/write direction, ACK/NACK, repeated START |

SPI packets retain both streams even though a version 1 custom schema selects
one input stream. This avoids losing full-duplex timing and permits a later
schema version to correlate MOSI commands with MISO responses. Missing optional
SPI lanes are represented as absent streams, not fabricated zero bytes.

I2C address and ACK bits are transport attributes rather than payload bytes.
The byte stream contains data bytes only. Their individual ACK/NACK status must
also remain available by byte index so a message can identify which byte was
not acknowledged.

UART is naturally continuous. Its decoder emits timed bytes and gaps; the
custom framer groups them into packets. Transport errors remain attached to
their byte ranges and are propagated to the decoded result even when the
schema can otherwise parse the frame.

Incremental decoders retain only the minimum state required across capture
chunks. A transport boundary or byte may begin in one logic batch and finish
in the next. Cancellation and end-of-input flush that state as incomplete
rather than silently dropping it.

## YAML protocol files

The recommended filename is `<name>.wavecraft-protocol.yaml`. The root `schema`
field is required and controls compatibility:

The complete copyable example is
[`examples/motor-controller.wavecraft-protocol.yaml`](examples/motor-controller.wavecraft-protocol.yaml).
Its matching UART capture is
[`examples/motor-controller-uart.dsv`](examples/motor-controller-uart.dsv).
Regenerate the recording deterministically from the workspace root with:

```sh
cargo run -p wavecraft-recording --example generate_motor_controller_dsv
```

To try it in the GUI, open the `.dsv`, add `1:UART`, map `RX/TX` to `D0`,
save the analyzer, and use **Load YAML** on its card to select the YAML file.
The decoded-data panel shows three messages with speeds of 1200, 2000, and
750 rpm.

The [`BNO08x SH-2 I2C example`](examples/bno08x-sh2-i2c.wavecraft-protocol.yaml)
decodes the SHTP header, Product ID and feature-control messages, plus common
accelerometer, gyroscope, magnetometer, gravity, and rotation-vector reports.
Add an I2C analyzer, map SCL and SDA, save it, then load the example YAML from
the analyzer card. The current schema version decodes the first SH-2 sensor
report in each I2C transaction and displays sensor values as raw fixed-point
integers annotated with their Q point. It does not yet reassemble SHTP
continuations or iterate over multiple reports in one cargo.

```yaml
schema: wavecraft.protocol/v1

protocol:
  id: example.motor-controller
  name: Motor Controller
  version: 1
  description: Example controller protocol over UART

input:
  transport: uart
  stream: data

framing:
  strategy: length_field
  sync: [0xAA, 0x55]
  length:
    offset: 2
    width: 1
    endian: little
    adjustment: 4
  max_frame_bytes: 256
  resync: next_sync

messages:
  - id: set_speed
    name: Set speed
    match:
      - offset: 3
        value: 0x10
    fields:
      - name: sync
        type: bytes
        size: 2
        expect: [0xAA, 0x55]
        hidden: true
      - name: payload_length
        type: u8
      - name: opcode
        type: u8
        enum:
          0x10: set_speed
          0x11: get_status
      - name: motor
        type: u8
      - name: speed_rpm
        type: u16le
        display:
          unit: rpm
      - name: flags
        type: u8
        bits:
          enabled: 0
          reverse: 1
          brake: 2
      - name: checksum
        type: u8
    validate:
      - checksum:
          algorithm: crc8_smbus
          from: 2
          through_field: flags
          expected_field: checksum
    summary: "Motor {motor}: {speed_rpm}"
```

Unknown root or structural fields are errors in version 1. This catches typos
instead of silently changing decoder behavior. A future additive schema version
may explicitly mark selected maps as extensible.

### Input selection

`input.transport` is one of `uart`, `spi`, or `i2c`. `input.stream` must name a
stream supplied by that transport. The GUI connects the custom analyzer to a
specific transport-analyzer instance; a schema never names physical logic
channels.

RF modulation is intentionally not represented as a pretend UART/SPI/I2C
transport. The planned FSK/GFSK pipeline first converts IQ into timed bits and
framed packet bytes. A future protocol-schema version accepts that generic
packet-byte capability as input while preserving version 1 behavior. See
[RF/SDR capture and signal-decoding architecture](rf-sdr-architecture.md).

Multiple custom analyzers may consume the same transport instance. Removing a
transport analyzer with dependants requires confirmation and identifies the
custom analyzers that would become unavailable.

### Framing

Version 1 supports these bounded strategies:

- `transport_transaction` uses an SPI chip-select transaction or an I2C
  START-to-STOP transaction;
- `gap` ends a UART frame after a configured minimum idle duration;
- `delimiter` ends a frame at a configured byte sequence;
- `fixed_length` consumes an exact number of bytes; and
- `length_field` reads a bounded integer at a fixed offset and computes total
  frame bytes as `decoded_length + adjustment`.

Every strategy requires or derives `max_frame_bytes`. The engine rejects a
schema whose maximum exceeds the application limit. At runtime an overlong,
unterminated, or impossible frame emits a diagnostic, releases buffered data,
and follows the declared resynchronization rule. The parser distinguishes an
incomplete final frame from an invalid complete frame.

For `length_field`, `sync` is optional. When present, the framer searches for
that sequence before reading the length. `resync: next_sync` discards input up
to the next candidate sequence after an invalid length or failed parse. The
schema compiler validates that the length bytes fit within the prefix needed
to determine a frame.

### Message selection and fields

Messages are evaluated in document order. A `match` clause contains bounded
checks against fixed byte offsets and transport attributes. At most one message
may match. Schema validation rejects identical match clauses, and runtime
ambiguity produces a diagnostic rather than silently selecting a message.
One message may be declared as the default for otherwise unmatched frames.

Version 1 field types are:

- `u8`, `s8`, `u16le`, `u16be`, `s16le`, `s16be`, `u32le`, `u32be`, `s32le`,
  and `s32be`;
- fixed-size `bytes` and UTF-8 or ASCII `string`;
- bounded arrays whose count references an earlier integer field;
- enums over integer fields; and
- named bit fields over integer fields.

Fields are read sequentially. A field may be conditional on an equality check
against an earlier scalar or a transport attribute. Version 1 does not expose
a general-purpose expression language. Size, count, and conditional references
must point backwards, be type-compatible, and be resolved by schema validation.

Each decoded field carries its source byte range and derived sample range.
Containers derive their range from their children. Calculated display values
without source bytes inherit the range of their declared inputs.

Validation initially supports expected constants, integer ranges, valid enum
membership, and an allowlist of unambiguous checksum variants such as
`crc8_smbus`. A validation failure
marks the relevant field or message as a warning or error but does not modify
the raw bytes. Checksum ranges are resolved by field name or fixed offset and
must remain inside the current frame.

Summary templates may interpolate decoded scalar and enum-display values. They
cannot call functions or access data outside the current message. Missing
template fields are schema errors.

## Generic decoded result

Transport-specific enums in the current GUI are replaced at the crate boundary
by a generic tree:

```rust
pub struct DecodedMessage {
    pub analyzer_id: AnalyzerInstanceId,
    pub protocol_id: String,
    pub message_id: String,
    pub range: SampleRange,
    pub summary: String,
    pub severity: Severity,
    pub fields: Vec<DecodedField>,
    pub diagnostics: Vec<DecodeDiagnostic>,
}

pub struct DecodedField {
    pub name: String,
    pub value: DecodedValue,
    pub display: Option<String>,
    pub range: Option<SampleRange>,
    pub children: Vec<DecodedField>,
}
```

`DecodedValue` contains only portable scalar, byte, string, enum, and null
variants so it can be represented consistently in JSON/NDJSON. Diagnostics
include stable machine codes plus human-readable context. Presentation color,
expanded-tree state, and localized labels are GUI concerns and do not belong in
decoder results.

## Schema loading and execution

Loading has three distinct phases:

1. Parse YAML into a versioned data-transfer structure.
2. Validate IDs, references, framing limits, field layout, templates, matches,
   and checksum ranges without capture data.
3. Compile the validated structure into an immutable execution plan.

The execution plan resolves names and offsets ahead of time. Runtime parsing
does not repeatedly traverse YAML values or perform string-based field lookup.
Plans are cached by the SHA-256 of the normalized UTF-8 schema bytes plus
decoder-engine version. A cache entry is an optimization only; the project
snapshot remains the authoritative schema input.

Schema errors include the source path and, where supported by the YAML loader,
line and column. Reloading an edited schema builds a new plan first. The active
analyzer keeps its previous valid plan if the new file is invalid.

## Resource and trust boundaries

A custom schema is data, not trusted code. The loader and runtime enforce:

- maximum schema byte size, nesting, collection length, and string length;
- maximum compiled messages, fields, and template substitutions;
- maximum frame size, array count, retained incomplete bytes, and emitted
  messages per input batch;
- checked integer arithmetic for offsets, lengths, and sample positions;
- an allowlist of field types, conditions, and checksum implementations;
- deterministic execution with no filesystem, network, environment, process,
  clock, or random access; and
- cancellation checks while decoding large recordings.

YAML aliases must not allow limits to be bypassed through expansion. Invalid
device input returns diagnostics and resynchronizes; production code must not
panic on it.

## Project files

Raw `.dsv` recordings contain samples and capture metadata only. Analyzer
configuration, custom schema snapshots, annotations, and view state are saved
separately in `capture.wavecraft-project`, whose content is versioned YAML:

```yaml
schema: wavecraft.project/v1

recording:
  path: capture.dsv
  id: 019a4d3e-7c42-7f30-a43c-000000000001

analyzers:
  - id: uart0
    type: uart
    channels:
      data: 0
    settings:
      baud_rate: 115200
      data_bits: 8
      parity: none
      stop_bits: 1

  - id: motor
    type: custom_yaml
    input: uart0
    schema_snapshot:
      source_path: protocols/motor.wavecraft-protocol.yaml
      sha256: 85f0c3...
      content: |
        schema: wavecraft.protocol/v1
        protocol:
          id: example.motor-controller
          name: Motor Controller
          version: 1
        # Remaining snapshot omitted from this example.
```

The recording path is stored relative to the project when possible. The schema
snapshot, not `source_path`, is authoritative when reopening a project.
`source_path` supports an explicit **Reload from source** action. WaveCraft
compares its hash with the snapshot and does not silently change an existing
analysis when the external file changes.

Saving a project never modifies the referenced `.dsv`. One recording may have
multiple project files with different interpretations.

### Recording identity

Every newly created native `.dsv` receives an immutable generated identifier
in the existing extensible session metadata map:

```text
wavecraft.recording_id = <generated UUID>
```

The project copies that value into `recording.id`. Readers do not synthesize an
ID for legacy recordings, do not hash them, and do not modify them. A project
for a legacy recording stores only its path and relies on the filename and the
user's selection.

When reopening or relinking:

| Project reference | Selected recording | Behavior |
| --- | --- | --- |
| ID present | same ID | open normally |
| ID present | different ID | warn and require explicit **Use anyway** |
| ID present | recording has no ID | warn and require explicit confirmation |
| no ID | recording has no ID | use path/name and show a legacy-ID warning |

The warning explains that WaveCraft cannot confirm this is the original raw
recording, so decoded messages, annotations, and time ranges may refer to the
wrong signal. It must not claim that the raw file itself is corrupt.

The recording information UI and a CLI inspection command display the ID and
offer a copy action. Users do not choose or edit an ID when recording; it is
generated automatically. Copies of the same immutable `.dsv` intentionally
retain the same ID, while a newly captured file receives a new ID even when it
has the same filename.

## GUI workflow

The analyzer stack is explicit:

```text
Logic channel 0
  `-- UART 115200 8-N-1                 [uart0]
        |-- Motor Controller            [motor]
        `-- Diagnostic Console          [diagnostics]

Logic channels 1-4
  `-- SPI mode 0                        [spi0]
        `-- Custom flash commands       [flash]
```

Each configured analyzer card provides **Load YAML**, which selects and
validates a YAML file before attaching it to that UART, SPI, or I2C transport.
An incompatible transport is rejected without changing the analyzer. **Clear**
removes only the attached YAML layer, while **×** removes the entire analyzer.
The settings dialog displays the source path and decoder chain.

Before committing, the dialog previews bounded results from the current raw
capture or recording. It shows schema errors separately from data diagnostics.
The decoded-data panel renders generic message summaries and expandable fields;
selecting a message or field focuses its exact sample range.

The project UI exposes recording name, path, format, and recording ID. A legacy
recording shows a persistent but non-blocking identity warning. A mismatched ID
is a blocking relink decision, not a transient toast.

## Persistence and compatibility

Protocol and project formats have independent version identifiers. Readers
reject an unsupported major version with a clear error and preserve unknown
files unchanged. Minor additive changes require an explicit compatibility rule;
they are not inferred from YAML permissiveness.

Project writes use a temporary file followed by an atomic replace where the
platform permits it. A failed save must leave the previous project readable.
The source YAML and snapshot are stored as UTF-8 with normalized line endings
for hashing, while diagnostics retain the original source location where
possible.

Raw decoded results are not required to be cached in the project. They are
deterministically regenerated from the recording, analyzer settings, schema
snapshot, and decoder engine. A future result cache must be disposable and
keyed by all of those inputs.

## Test strategy

Transport decoder tests cover bytes and boundaries that span capture chunks,
cancellation, end-of-input, malformed symbols, and absolute sample ordering.

Continuous-capture tests feed the same raw stream with different batch sizes,
advance the display ring past old samples, and require identical still-retained
events without restarting decoder state. They also cover packets spanning
capture batches, explicit discontinuity resets, final incomplete-frame flush,
bounded decoded-event eviction, and equality between live Loop decoding and a
complete `.dsv` offline pass.

YAML tests cover:

- valid examples for UART, SPI, and I2C;
- schema-version and unknown-field rejection;
- invalid references, overlapping matches, and template errors;
- every primitive type, endian mode, enum, bit field, and conditional;
- fixed, delimiter, gap, transport-transaction, and length-field framing;
- truncated and overlong frames, resynchronization, and arithmetic overflow;
- bounds against malicious lengths, arrays, nesting, and YAML aliases;
- deterministic output across arbitrary capture-chunk boundaries; and
- exact field-to-sample-range mapping.

Project tests cover relative paths, snapshot authority, explicit schema reload,
atomic save, matching and mismatching recording IDs, and legacy recordings
without an ID. Integration tests decode deterministic simulated capture both
live and after a `.dsv` round trip and require identical results.

## Implementation sequence

1. Add generated recording IDs to new native `.dsv` metadata and expose them
   through recording inspection. Do not modify or fingerprint legacy files.
2. Introduce `wavecraft-decode` generic ranges, transport packets, decoded
   messages, and incremental decoder traits.
3. Move UART, SPI, and I2C semantics out of the GUI while retaining current UI
   behavior through adapters.
4. Move decoder execution from `ProtocolDecodeCache` to persistent analyzer
   runtimes that consume live and recorded `CaptureFrame` streams, including
   Loop-mode boundary, reset, flush, and bounded-history behavior. Implemented;
   `ProtocolDecodeCache` remains only as the post-capture compatibility path.
5. Add YAML DTOs, strict validation, immutable execution plans, and bounded
   framing and field parsing.
6. Replace GUI-specific decoded kinds with generic message and field rendering.
7. Add versioned project loading/saving, recording-ID checks, and embedded YAML
   schema snapshots.
8. Add CLI validation and offline decode commands using the same services.
9. Add request/response correlation, multi-stream SPI parsing, or Kaitai Struct
   only after version 1 behavior and resource limits are proven.
