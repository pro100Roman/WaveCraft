# DSView recording format

DSView's native `.dsv` format is a lossless, little-endian stream of bounded
records. Format version 1 preserves capture events exactly and can be written
without retaining the complete capture in memory.

## File header

| Field | Size | Value |
| --- | ---: | --- |
| Magic | 8 bytes | `DSVIEW`, `0x1a`, newline |
| Format version | `u16` | `1` |
| Flags | `u16` | `0` |

The header is immediately followed by one session-metadata record.

## Record framing

Every record has a 16-byte header followed by its payload:

| Field | Size | Description |
| --- | ---: | --- |
| Kind | `u8` | Session, frame, source failure, source stopped, or footer |
| Flags/reserved | 3 bytes | Zero in version 1 |
| Payload length | `u64` | Bounded to 256 MiB |
| Payload CRC-32 | `u32` | CRC of payload bytes |

Strings are UTF-8 with a `u32` byte length. Counts and sample values use their
explicit fixed-width little-endian representation. Logic data stores one `u32`
sample word per sampling instant. Scope data stores IEEE-754 `f32` values in
channel-major order. Frame timestamps contain separate `u64` seconds and `u32`
nanoseconds on the capture coordinator's shared monotonic timeline.

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
