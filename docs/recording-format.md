# WaveCraft recording format

WaveCraft's native `.dsv` format is a lossless, little-endian stream of bounded
records. Format version 2 preserves logic, scope, and raw IQ capture events and
can be written without retaining the complete capture in memory. Readers
continue to accept version 1 logic/scope recordings unchanged.

## File header

| Field | Size | Value |
| --- | ---: | --- |
| Magic | 8 bytes | legacy `DSVIEW`, `0x1a`, newline |
| Format version | `u16` | `2` (`1` remains readable) |
| Flags | `u16` | `0` |

The header is immediately followed by one session-metadata record.

## Record framing

Every record has a 16-byte header followed by its payload:

| Field | Size | Description |
| --- | ---: | --- |
| Kind | `u8` | Session, frame, source failure, source stopped, or footer |
| Flags/reserved | 3 bytes | Zero in versions 1 and 2 |
| Payload length | `u64` | Bounded to 256 MiB |
| Payload CRC-32 | `u32` | CRC of payload bytes |

Strings are UTF-8 with a `u32` byte length. Counts and sample values use their
explicit fixed-width little-endian representation. Logic data stores one `u32`
sample word per sampling instant. Scope data stores IEEE-754 `f32` values in
channel-major order. IQ data stores signed complex `i8` pairs together with the
sample rate, center frequency, absolute first-sample index, and discontinuity
flag. Frame timestamps contain separate `u64` seconds and `u32` nanoseconds on
the capture coordinator's shared monotonic timeline.

## Session and completion

Session metadata contains every `DeviceDescriptor` and a sorted map of
schema-extensible string attributes. Frame records preserve the device ID,
sequence number, timestamp, sample rate, channel layout, and all sample values.
Source failures and clean source stops are also recorded.

A valid file ends with a footer containing the completion status and total
event count. Status is `Complete`, `Cancelled`, or `Failed`. A missing or
truncated footer makes the recording incomplete even when earlier records can
be read. Readers verify every record checksum, reject impossible lengths and
counts before allocation, and verify the footer's event count.

The native file is the source of truth. Future CSV, VCD, or decoder exports may
represent only compatible subsets, but must not modify the original recording.

## RF/IQ payload

Format version 1 has frame payloads only for logic and scope samples. Version 2
adds the IQ payload while retaining read compatibility with version 1 and never
rewriting legacy recordings on open. IQ batches preserve their signed complex
bytes, sample rate, center frequency, absolute source-sample position, and
discontinuities. Spectrum, waterfall, demodulated bits, and decoded packets
remain reproducible derived data rather than authoritative recording content.
The complete design is in
[RF/SDR capture and signal-decoding architecture](rf-sdr-architecture.md).

The **Record** action can attach to an existing Logic Analyzer Loop or active
Oscilloscope/RF Run acquisition and stream native `.dsv` frames until the
operator stops. Starting Record from an idle RF view also starts Run mode.
Because Run has no predetermined duration, RF file size grows at approximately
two bytes per IQ sample plus record framing.

WaveCraft also offers a decoded-data recording that can start or attach during
Run in either RAW or PACKETS mode. This is a separate newline-delimited JSON
(`.jsonl`) stream, not a native `.dsv` capture. Its first record describes the
RF session and analyzer channels. RAW records contain completed messages;
PACKETS records contain accepted framed packets with confidence. Both include
the channel, source-sample range, preamble bytes, and decoded data bytes. The
final record reports completion status and record count. It does not contain
IQ, spectrum, waterfall, or enough information to re-run demodulation.

## Saleae Logic 2 interoperability

The recording crate can import and export the ZIP-based `.sal` container used
by Saleae Logic 2. It reads and writes `<SALEAE>` binary version 3 digital RLE
channels, analog `i16` channels and their rendering pyramids, schema-version-22
metadata, and the empty trigger store. Conversion preserves sample rates,
channel indices and names, packed digital values, and analog voltages (subject
to the format's 12-bit signed quantization). It does not translate triggers,
protocol analyzers, measurements, or view state.

`.sal` is an internal, explicitly unstable format. Readers reject unknown
binary versions rather than guessing at their layout. The native `.dsv` file
remains the lossless source of truth.
