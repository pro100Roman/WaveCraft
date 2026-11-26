# Serial / UART workspace

Select **Serial / UART**, choose a discovered port (or type its path / COM name),
set the baud rate, data bits, parity, stop bits and flow control, and press
**Start**. **Stop** releases the port; **Clear** resets the displayed history and
partial decoder state. Port and decoding changes apply on the next Start or Open raw;
color edits apply immediately.
The workspace can receive alongside the other instruments.

The graph and raw-data pane share the available height. Drag their divider to
resize them; double-click it to restore the default split. On the graph, scroll
to zoom the time range, drag to inspect earlier points, and use **Follow** or
**Fit** to return to incoming data. Colors can be changed in setup or the graph
legend and are saved with the other serial settings.

With **no text fields** or **no binary schema**, Start still reads the port and
shows only raw data in the lower pane. A schema is never required for receiving.
Text renders printable ASCII, CR/LF records and escaped nonprintable bytes.
Binary renders byte offsets, hexadecimal bytes and printable ASCII. Copy copies
the displayed representation; Save raw writes the retained bytes unchanged.

History is bounded to the latest 64 KiB of raw bytes and 10,000 points per field.
Save raw exports that retained window, not an unlimited recording. Open raw
accepts up to 8 MiB and processes it using the current decoding settings.
Live graph timestamps are host receive times; multiple records in one read
share that timestamp. Imported raw bytes have no timestamps, so their time axis
is estimated from the configured baud rate and UART frame size.

## Text fields

Add up to ten named fields and choose each line's color. For example, fields
`temperature` and `rpm` match:

```text
temperature=23.5 rpm=1200
temperature: -1.25e2, rpm: 950
{"temperature":24.25,"rpm":1000}
```

Names are case-sensitive and matched at identifier boundaries, so `temp` does
not match `temperature`. Records end at CR or LF, including CRLF, and can cross
arbitrary serial read boundaries. Missing or nonfinite values produce no point
for that field. Records longer than 64 KiB are discarded until the next record
separator and counted as rejected.

## Binary schema

Load [serial-schema.json](examples/serial-schema.json), or use **Save example
JSON…** in setup. A frame is:

```text
START WORD | FIELD 1 | FIELD 2 | ... | CRC | STOP WORD
```

`start_word_bytes` and `stop_word_bytes` specify lengths from 1 to 32 bytes.
Enter their actual byte sequences in setup as hex, for example `AA 55` and
`0D 0A`. The number of hex bytes must match the schema. Words are literal wire
byte sequences and are not reversed by the byte-order setting.

Fields appear consecutively in JSON order without implicit padding:

| JSON property | Meaning |
| --- | --- |
| `byte_order` | `little` (default) or `big` for all payload integers and floats |
| `name` | Unique field name |
| `size_bytes` | Integer storage width, 1–8 bytes; float storage width, 4 or 8 |
| `kind` | `unsigned` (default), `signed` (two's complement), or `float` (IEEE-754) |
| `bits` | Optional named bit fields within an integer storage field |
| `bit_offset` | Zero-based offset from the least-significant bit of the decoded integer |
| `bit_width` | Number of bits, from 1 to the remaining storage width |
| `signed` | Optional two's-complement interpretation of a bit field; default `false` |

For a storage field containing `bits`, each named bit field gets its own graph
line and color. The parent storage field consumes its stated byte width once.
Other fields produce one graph line each. Up to 64 storage fields and 128 graph
lines are supported. Values use the graph's `f64` representation, so integers
larger than 2^53 can lose precision; retained raw bytes remain exact. Unknown JSON
properties, duplicate names, invalid sizes and out-of-range bit fields are
rejected before receiving starts.

## CRC and frame recovery

CRC-8, CRC-16 and CRC-32 are supported. Choose a preset or enter a polynomial,
seed / init and final XOR in hexadecimal. Enter the **normal polynomial without
the implicit highest term**, including for reflected algorithms. For example,
MODBUS uses `8005`, rather than the reflected representation `A001`.

Input/output bit reflection and CRC wire byte order are independent controls.
By default CRC covers payload bytes only. **Include start word in CRC** adds
the start word to that coverage. The CRC and stop word are excluded. The sample
schema works with start `AA 55`, stop `0D 0A`, CRC-16, polynomial `1021`, seed
`FFFF`, final XOR `0000`, no reflection, and little-endian CRC bytes.

The decoder verifies the fixed-size frame's stop word and CRC before plotting.
Incomplete frames survive read boundaries. Bad frames are counted as rejected;
the decoder scans for the next valid start word, including overlapping start
candidates. All received bytes remain visible in raw history, including noise
and rejected frames.

Transport implements `CaptureSource` in `wavecraft-serial` and emits typed
`CaptureBatch::Serial` byte batches through the shared `CaptureContext`.
Protocol decoders and CRC have no dependency on the GUI. Hardware-free tests
exercise two sources, ordering, cancellation, read failures, CRC reference
vectors, fragmentation, resynchronization and bounded history. Physical UART
adapters and device firmware still require hardware validation.
