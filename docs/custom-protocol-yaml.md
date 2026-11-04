# Custom protocol YAML reference

This is the authoring reference for the implemented
`wavecraft.protocol/v1` format. A custom protocol file tells WaveCraft how to
frame and interpret bytes that have already been decoded by a UART, SPI, or I2C
analyzer. It does not decode voltage levels or configure the transport itself.

Use the recommended filename suffix `.wavecraft-protocol.yaml`. The complete
examples in this repository are:

- [Motor Controller over UART](examples/motor-controller.wavecraft-protocol.yaml)
- [BNO08x SH-2 over I2C](examples/bno08x-sh2-i2c.wavecraft-protocol.yaml)

The [custom protocol architecture](custom-protocols.md) explains how this
format fits into capture and decoding. This page is authoritative for fields
accepted by the current loader.

## Quick start

Start with this shape:

```yaml
schema: wavecraft.protocol/v1

protocol:
  id: example.thermometer
  name: Example Thermometer
  version: 1
  description: Fixed five-byte temperature reports

input:
  transport: uart
  stream: data

framing:
  strategy: fixed_length
  bytes: 5

messages:
  - id: temperature
    name: Temperature
    match:
      - offset: 0
        value: 0xA1
    fields:
      - name: opcode
        type: u8
        expect: 0xA1
        hidden: true
      - name: sensor_id
        type: u8
      - name: temperature
        type: s16le
        display:
          fixed_point_q: 4
          unit: C
      - name: status
        type: u8
        bits:
          sensor_fault: 0
          battery_low: 1
    summary: "Sensor {sensor_id}: {temperature}"
```

In the GUI, add and save a UART, SPI, or I2C analyzer first. Select **Load
YAML** on that analyzer's card and choose the file. WaveCraft validates the
whole schema before attaching it. The YAML transport must match the selected
analyzer. **Clear** detaches the YAML layer without removing the transport
analyzer.

There is not yet a dedicated CLI schema-validation command.

## Format rules

The root is a mapping with exactly these fields:

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `schema` | string | yes | Must be exactly `wavecraft.protocol/v1`. |
| `protocol` | mapping | yes | Human and machine-readable protocol metadata. |
| `input` | mapping | yes | Transport and byte stream consumed by the parser. |
| `framing` | mapping | yes | How transport bytes become frames. |
| `messages` | sequence | yes | One or more message definitions. |

Unknown fields are errors at every level. Field names and enum strings are
case-sensitive.

### Scalar syntax

Integer values may be YAML integers or strings in these forms:

```yaml
decimal: 42
hexadecimal: 0x2A
binary: 0b0010_1010
quoted: "0x2A"
negative: -10
```

Underscores are allowed in numeric strings. Negative values are accepted only
where the field's range permits them. Byte values must be in `0..=255`.
Booleans must be YAML booleans, `true` or `false`, rather than quoted strings.

An identifier used by `protocol.id`, message `id`, field `name`, or bit name:

- must contain 1 to 128 bytes;
- may contain only ASCII letters, digits, `.`, `_`, and `-`; and
- must be unique within its applicable list or mapping.

Display names and descriptions may contain ordinary UTF-8 text.

## `protocol`

```yaml
protocol:
  id: vendor.device.protocol
  name: Device Protocol
  version: 3
  description: Optional description for people
```

| Field | Type | Required | Constraints and behavior |
| --- | --- | --- | --- |
| `id` | identifier string | yes | Stable protocol identifier, at most 128 bytes. It is copied to decoded messages. |
| `name` | non-empty string | yes | Human-readable name, at most 4,096 bytes. |
| `version` | unsigned integer | yes | Protocol author's version. It does not select the WaveCraft schema format. |
| `description` | string | no | Documentation text, at most 4,096 bytes. |

Increment `protocol.version` when the on-wire interpretation changes. The
loader records it as metadata; it does not apply migration or compatibility
logic to this value. `schema`, not `protocol.version`, selects the file format.

## `input`

```yaml
input:
  transport: spi
  stream: mosi
```

| `transport` | Allowed `stream` values |
| --- | --- |
| `uart` or `usart` | `data`, `rx`, `tx` |
| `spi` | `mosi`, `miso` |
| `i2c` | `data` |

Both fields are required strings. A schema consumes exactly one stream. In
particular, version 1 cannot correlate MOSI with MISO. I2C address, direction,
ACK/NACK, START, and STOP are handled by the transport analyzer and are not
inserted into the `data` byte stream.

The current GUI UART analyzer maps one physical `RX/TX` data channel. Its
`data`, `rx`, and `tx` stream names label that same selected byte stream; they
do not add direction detection or two-channel UART correlation.

The GUI attaches the file to an existing analyzer, so the schema does not name
logic channels, baud rate, SPI mode, I2C address, or other transport settings.

## `framing`

`framing.strategy` selects one of six strategies. Offsets and byte counts are
zero-based and refer to bytes in the selected input stream.

| Strategy | Intended input | Boundary source |
| --- | --- | --- |
| `transport_transaction` | SPI or I2C | One transport transaction |
| `segmented_transport_transaction` | SPI or I2C | One length-delimited packet across one or more transactions |
| `gap` | UART/USART | An idle interval |
| `delimiter` | Any supported byte stream | A byte sequence |
| `fixed_length` | Any supported byte stream | An exact byte count |
| `length_field` | Any supported byte stream | A length value inside the frame |

`max_frame_bytes`, where present, must be in `1..=1,048,576`. It also bounds
message match offsets and the sum of field widths. The application rejects a
schema that exceeds this limit.

### Transport transaction

```yaml
framing:
  strategy: transport_transaction
  max_frame_bytes: 4096
```

| Field | Type | Required | Default |
| --- | --- | --- | --- |
| `strategy` | `transport_transaction` | yes | none |
| `max_frame_bytes` | positive integer | no | `1,048,576` |

This strategy is valid only for SPI and I2C. Each transaction supplied by the
transport decoder is considered one frame. The transport decoder, rather than
the YAML framer, determines the boundary. The current runtime uses
`max_frame_bytes` to validate schema offsets and layouts but does not truncate
an oversized complete transport transaction.

### Segmented transport transaction

This strategy reassembles a packet whose declared total length spans
consecutive SPI or I2C transactions. It is intended for protocols such as SHTP
that repeat a header in each continuation transaction.

```yaml
framing:
  strategy: segmented_transport_transaction
  header_bytes: 4
  length:
    offset: 0
    width: 2
    endian: little
    mask: 0x7FFF
  continuation:
    offset: 1
    mask: 0x80
    value: 0x80
  channel_offset: 2
  sequence_offset: 3
  max_frame_bytes: 32767
```

| Field | Type | Required | Constraints |
| --- | --- | --- | --- |
| `strategy` | `segmented_transport_transaction` | yes | SPI or I2C only. |
| `header_bytes` | positive integer | yes | Must not exceed `max_frame_bytes`. |
| `length` | mapping | yes | Declared total length, including the first header. |
| `continuation` | mapping | yes | Identifies continuation transactions. |
| `channel_offset` | non-negative integer | yes | One byte inside the header. |
| `sequence_offset` | non-negative integer | yes | One byte inside the header. |
| `records` | mapping | no | Optionally splits selected channels into records. |
| `max_frame_bytes` | positive integer | no | Defaults to `1,048,576`. |

`length` has these fields:

| Field | Type | Required | Default and constraints |
| --- | --- | --- | --- |
| `offset` | non-negative integer | yes | `offset + width` must fit in `header_bytes`. |
| `width` | integer | yes | Exactly `1`, `2`, or `4` bytes. |
| `endian` | `little` or `big` | no | `little`. |
| `mask` | unsigned integer | no | All bits in `width`; must be non-zero and fit the width. |

`continuation` has these fields:

| Field | Type | Required | Default and constraints |
| --- | --- | --- | --- |
| `offset` | non-negative integer | yes | Must point inside the header. |
| `value` | byte | yes | May use only bits selected by `mask`. |
| `mask` | byte | no | `0xFF`; must be non-zero. |

A transaction is a continuation when
`header[offset] & mask == value`. The first transaction supplies the retained
header. For each continuation, the runtime verifies the same channel, the next
wrapping 8-bit sequence number, and a declared length equal to the remaining
payload plus that transaction's repeated header. Repeated continuation headers
are discarded. A mismatch abandons reassembly and emits a diagnostic.

#### Optional record splitting

Some packet payloads contain a prefix followed by several fixed-size records.
`records` can turn every recognized record into a separate candidate message:

```yaml
  records:
    channels: [3, 4]
    prefix:
      value: 0xFA
      mask: 0xFE
      bytes: 5
    lengths:
      0x01: 10
      0x05: 14
```

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `channels` | non-empty sequence of unique bytes | yes | Apply record splitting only when the reassembled header's channel byte has one of these values. |
| `prefix` | mapping | yes | Recognizes and sizes the context prefix retained for following records. |
| `lengths` | non-empty byte-to-positive-integer mapping | yes | Maps the first byte of a record to its total record size, including that identifier byte. |

`prefix.value` and optional `prefix.mask` (`0xFF` by default) follow the same
masked-byte rules as `continuation`; `prefix.bytes` is the total prefix size,
including its identifying first byte. A record identifier must not match the
prefix mask. Header, prefix, and the largest declared record must fit within
`max_frame_bytes`.

Parsing begins immediately after `header_bytes`. A matching prefix becomes the
current context; a later prefix replaces it. Every record produces a synthetic
frame containing the original header, the most recent prefix, and that record.
A record before a prefix, an unknown identifier, or an incomplete prefix or
record stops processing the remainder and emits a warning. Record splitting is
specialized fixed-layout behavior, not a general nested-record language.

### Gap

```yaml
framing:
  strategy: gap
  minimum_us: 1000
  max_frame_bytes: 256
```

| Field | Type | Required | Constraints |
| --- | --- | --- | --- |
| `strategy` | `gap` | yes | UART/USART only. |
| `minimum_us` | positive integer | yes | Minimum idle time in whole microseconds. |
| `max_frame_bytes` | positive integer | yes | Maximum buffered frame size. |

The gap is measured between the end sample of one decoded UART byte and the
start sample of the next. The sample interval is converted to microseconds
using integer division. A gap greater than or equal to `minimum_us` closes the
current non-empty frame. Exceeding `max_frame_bytes` clears the buffered frame
and emits an error.

### Delimiter

```yaml
framing:
  strategy: delimiter
  delimiter: [0x0D, 0x0A]
  include_delimiter: false
  max_frame_bytes: 1024
```

| Field | Type | Required | Default and constraints |
| --- | --- | --- | --- |
| `strategy` | `delimiter` | yes | none |
| `delimiter` | non-empty sequence of bytes | yes | Its length must not exceed `max_frame_bytes`. |
| `include_delimiter` | boolean | no | `false`. |
| `max_frame_bytes` | positive integer | yes | Maximum bytes buffered while searching. |

When `include_delimiter` is false, delimiter bytes are consumed but omitted
from the frame. An empty frame before a delimiter is not emitted in that mode.
When true, the delimiter is the final bytes of the frame. If the limit is
exceeded without a delimiter, the runtime emits an error and retains only a
suffix that could begin the delimiter. The current implementation checks this
limit only while no complete delimiter is buffered; a delimiter already found
beyond the limit still closes and emits that frame.

### Fixed length

```yaml
framing:
  strategy: fixed_length
  bytes: 12
```

| Field | Type | Required | Constraints |
| --- | --- | --- | --- |
| `strategy` | `fixed_length` | yes | none |
| `bytes` | positive integer | yes | At most `1,048,576`. |

The byte stream is divided into consecutive frames of exactly `bytes` bytes.
Remaining bytes at end of input are reported as an incomplete frame.

### Length field

```yaml
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
```

| Field | Type | Required | Default and constraints |
| --- | --- | --- | --- |
| `strategy` | `length_field` | yes | none |
| `sync` | sequence of bytes | no | Empty/no synchronization sequence. |
| `length` | mapping | yes | Described below. |
| `max_frame_bytes` | positive integer | yes | Bounds the calculated total. |
| `resync` | `drop_frame` or `next_sync` | no | `drop_frame`; `next_sync` requires non-empty `sync`. |

`length` has these fields:

| Field | Type | Required | Default and constraints |
| --- | --- | --- | --- |
| `offset` | non-negative integer | yes | Byte offset of the encoded length. |
| `width` | integer | yes | Exactly `1`, `2`, or `4` bytes. |
| `endian` | `little` or `big` | no | `little`. |
| `adjustment` | signed integer | no | `0`. |

The total frame size is `decoded_length + adjustment`. The adjustment is not
implicitly the header size. The result must be at least `offset + width` and at
most `max_frame_bytes`.

With `sync`, bytes before the first matching sequence are discarded with a
diagnostic, and framing begins at the sync byte. On an invalid calculated
length, `drop_frame` clears all currently buffered bytes. `next_sync` discards
one byte and searches again, allowing recovery at a later sync candidate.
Without `sync`, the next buffered byte is always treated as the beginning of a
frame.

## `messages`

`messages` is a non-empty sequence with at most 256 entries. Messages are
tested against each complete frame. Each entry accepts:

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `id` | identifier string | yes | Unique stable message identifier. |
| `name` | non-empty string | yes | Human-readable fallback summary. |
| `match` | non-empty sequence of byte matches | conditional | Required unless `default: true`. |
| `default` | boolean | no | Defaults to `false`; only one default is allowed. |
| `fields` | non-empty sequence | yes | Fixed sequential field layout. |
| `validate` | sequence | no | Checksum validations. |
| `summary` | string | no | Text with `{field_name}` placeholders. |

A normal message uses one or more matches:

```yaml
match:
  - offset: 0
    value: 0xA1
  - offset: 1
    value: 0x20
    mask: 0xF0
```

Each item has required non-negative `offset` and byte `value`, plus optional
byte `mask` (default `0xFF`). It matches when
`frame[offset] & mask == value & mask`. Offsets must be unique within one
message and less than `max_frame_bytes`.

All conditions in a message must match. The runtime emits an ambiguity error
if more than one message matches; document order is not a priority mechanism.
The loader rejects exactly identical match lists, while other overlapping
conditions may still be ambiguous at runtime.

A fallback message uses `default: true` and must omit `match`. Only one default
is allowed. If no normal message matches, WaveCraft uses the default. With no
default, an unmatched frame produces no decoded message.

### Field layout

`fields` contains 1 to 1,024 fields. Fields are decoded sequentially from byte
offset zero; there is no per-field offset. Use a `bytes` field for reserved or
padding bytes. The sum of widths must fit `max_frame_bytes`.

If a frame is shorter than the field layout, decoding fails. Bytes after the
last field are currently ignored. Match offsets can inspect those trailing
bytes, but summaries and validations cannot name them.

Every field accepts this common shape:

```yaml
- name: field_id
  type: u16le
  mask: 0x0FFF
  expect: 100
  enum:
    100: ready
  hidden: false
  display:
    unit: counts
    fixed_point_q: 2
```

Not every option applies to every type.

| Field | Type | Required | Applies to |
| --- | --- | --- | --- |
| `name` | identifier string | yes | all fields; unique within the message |
| `type` | type name | yes | all fields |
| `size` | positive integer | for `bytes`/`string` | `bytes`, `string` only |
| `encoding` | `utf8` or `ascii` | no | `string` only; default `utf8` |
| `mask` | integer | no | unsigned integer fields only |
| `expect` | value of the field type | no | all fields |
| `enum` | integer-to-non-empty-string mapping | no | integer fields only |
| `bits` | name-to-bit-index mapping | no | integer fields only |
| `hidden` | boolean | no | all fields; default `false` |
| `display` | mapping | no | all fields, with restrictions below |

`enum` and `bits` are mutually exclusive on the same field.

### Field types

| Type | Width | Range or behavior |
| --- | --- | --- |
| `u8` | 1 | `0..=255` |
| `s8` | 1 | `-128..=127` |
| `u16le` | 2 | Unsigned 16-bit, little-endian |
| `u16be` | 2 | Unsigned 16-bit, big-endian |
| `s16le` | 2 | Signed two's-complement 16-bit, little-endian |
| `s16be` | 2 | Signed two's-complement 16-bit, big-endian |
| `u32le` | 4 | Unsigned 32-bit, little-endian |
| `u32be` | 4 | Unsigned 32-bit, big-endian |
| `s32le` | 4 | Signed two's-complement 32-bit, little-endian |
| `s32be` | 4 | Signed two's-complement 32-bit, big-endian |
| `bytes` | `size` | Raw fixed-size byte sequence |
| `string` | `size` | Fixed-size UTF-8 or ASCII byte sequence |

`size` must be in `1..=1,048,576`. Strings are not NUL-terminated or trimmed.
Invalid UTF-8 is displayed lossily and produces an error diagnostic. A
non-ASCII byte is displayed as `?` and produces an error diagnostic.

### Masks, constants, enums, and bits

`mask` is allowed only on unsigned integers and must fit the integer type. The
runtime applies it before `expect`, enum lookup, display formatting, and
summary substitution. For example:

```yaml
- name: packet_length
  type: u16le
  mask: 0x7FFF
  bits:
    continuation: 15
```

Named `bits` are derived from the original unmasked integer, so this example
can expose bit 15 while the scalar value contains only the lower 15 bits. Bit
indices are zero-based, must fit the type width, and may not be repeated.

`expect` must fit the field type. For `bytes`, it is a byte sequence exactly
`size` bytes long. For `string`, its UTF-8 byte length must equal `size`. A
mismatch does not discard the message; it adds an error diagnostic.

An `enum` maps in-range integer values to display labels. An unlisted runtime
value is retained and adds a warning. Enum labels replace other display text,
including a unit or fixed-point rendering.

`hidden: true` is a presentation hint. The decoded field is retained and can
still be used in a summary or checksum.

### Display options

```yaml
display:
  fixed_point_q: 8
  unit: m/s^2
```

| Field | Type | Required | Behavior |
| --- | --- | --- | --- |
| `fixed_point_q` | integer `0..=31` | no | Integer fields only; displays `raw / 2^Q`. |
| `unit` | string | no | Appended to an integer's display text. |

Fixed-point output uses at most six fractional decimal places and removes
trailing zeros. It changes only display text; the decoded value remains the
integer. For non-integer fields the current loader accepts `unit`, but the
runtime uses the unit itself as their display text. Use `unit` on integers to
avoid replacing useful byte or string output.

### Summary templates

```yaml
summary: "Sensor {sensor_id}: {temperature}"
```

Without `summary`, the message `name` is used. A placeholder must exactly name
a field in the same message. It expands to the field's display text when one
exists (enum label, fixed-point value, or unit-qualified value), otherwise to
its raw representation. Bytes render as uppercase hexadecimal separated by
spaces.

Templates have no format specifiers, expressions, functions, conditional text,
or brace-escaping syntax. Every `{` must form one valid placeholder, and a
literal unmatched `}` is an error. The complete template is limited to 4,096
bytes.

### Checksum validation

`validate` is a sequence. Version 1 supports only CRC-8/SMBus:

```yaml
validate:
  - checksum:
      algorithm: crc8_smbus
      from: 2
      through_field: flags
      expected_field: checksum
```

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `algorithm` | `crc8_smbus` | yes | CRC-8 polynomial `0x07`, initial value `0x00`, no reflection, final XOR `0x00`. |
| `from` | non-negative integer | yes | Starting byte offset in the decoded field layout, inclusive. |
| `through_field` | field name | yes | Include bytes through the end of this field. |
| `expected_field` | field name | yes | Later `u8` field containing the transmitted CRC. |

`from` must precede the end of `through_field`, and `through_field` must appear
before `expected_field`. A mismatch retains the decoded message and adds an
error diagnostic at the expected checksum byte. There are no configurable CRC
parameters and no sum, XOR, CRC-16, or CRC-32 algorithm in version 1.

## Capabilities

The implemented format can:

- frame a continuous byte stream by fixed size, delimiter, idle gap, or length
  field;
- use native SPI and I2C transaction boundaries;
- reassemble a supported length/channel/sequence segmented transaction format;
- split specialized prefix-plus-fixed-record payloads;
- select messages with masked fixed-offset byte comparisons and one fallback;
- decode fixed-layout signed and unsigned integers, byte strings, and text;
- expose integer enums and individual named bits;
- validate expected constants and CRC-8/SMBus;
- render fixed-point integer values with units and message summaries; and
- preserve source sample ranges and transport parity/framing diagnostics.

## Limitations

Version 1 deliberately has no executable code or general expression language.
In particular, it does not support:

- variable-length fields, arrays, conditional fields, nested structures,
  unions, per-field offsets, or calculated fields;
- 64-bit integers, floating-point wire values, BCD, arbitrary bit ranges, or
  configurable string termination;
- matching transport attributes such as I2C address, read/write direction,
  SPI chip select, ACK/NACK, or UART direction;
- consuming more than one byte stream or correlating SPI MOSI and MISO;
- stateful request/response correlation between decoded frames;
- user-defined checksum/CRC parameters, scripts, plugins, imports, includes,
  YAML anchors, or aliases;
- packet encoding/generation or modifying captured bytes; or
- reading files, environment variables, the network, the clock, or random
  state from a schema.

Message fields always begin at byte zero and are decoded consecutively. Model a
reserved region as a `bytes` field, optionally hidden. Use separate message
definitions when packet layouts differ. When a protocol needs a capability
above, it currently requires decoder code rather than additional YAML.

Analyzer/project persistence for attached YAML remains incomplete. Keep the
source file with the capture and be prepared to attach it again when reopening
a recording.

## Resource and YAML restrictions

The loader applies these bounds before decoding capture data:

| Resource | Limit |
| --- | --- |
| YAML source size | 1,048,576 bytes |
| Parsed YAML nodes | 16,384 |
| String/real scalar | 4,096 bytes |
| Messages | 256 |
| Fields per message | 1,024 |
| Frame or fixed field size | 1,048,576 bytes |
| Identifier | 128 bytes |

The file must contain exactly one YAML document. Mapping keys must be strings,
except numeric enum keys and record-length identifiers where the format
explicitly accepts them. YAML aliases/anchors and custom tags are rejected.
Duplicate semantic IDs, message IDs, field names, match offsets, bit indices,
record channels, and record identifiers are rejected. The loader reports a
schema path such as `messages[0].fields[2].type` with the error.

Because unknown keys are errors, a misspelling never silently changes parser
behavior. Files for a future major schema version will be rejected until that
version is implemented.

## Authoring checklist

1. Configure and verify the transport analyzer without custom YAML.
2. Choose the input stream and a framing strategy that matches the actual byte
   boundaries.
3. Write message matches against zero-based bytes in the framed data.
4. Lay out every field from byte zero in wire order, including headers and
   reserved bytes.
5. Add `expect`, enums, bits, checksums, and summaries only after basic framing
   and field widths decode correctly.
6. Load the file through the analyzer card and fix every schema error.
7. Test unknown message types, truncated frames, corrupt lengths, checksum
   failures, and resynchronization—not only valid traffic.

For a small complete schema and matching deterministic recording, use the
[Motor Controller example](examples/motor-controller.wavecraft-protocol.yaml).
For segmented transactions and record splitting, use the
[BNO08x example](examples/bno08x-sh2-i2c.wavecraft-protocol.yaml).
