# Serial / UART workspace

Select **Serial / UART**, choose a discovered port from the dropdown,
set the baud rate, data bits, parity, stop bits and flow control, and press
**Start**. **Stop** releases the port; **Clear** resets the displayed history and
partial decoder state. Port and decoding changes apply on the next Start or Open raw;
color edits apply immediately.
The workspace can receive alongside the other instruments. PORT and DATA FORMAT
can be collapsed independently. Baud rate uses an editable field with a dropdown,
like the System Trace target selector. Type to filter standard rates or enter a
custom value; use the arrow to show all standard rates through 3,000,000.

The graph and raw-data pane share the available height. Drag their divider to
resize them; double-click it to restore the default split. On the graph, scroll
to zoom the time range and drag to inspect earlier points. **Follow** tracks
incoming plotted values; **Fit** fits all retained plotted samples to the graph
width. Fit uses actual point timestamps, including sub-millisecond recordings,
and ignores trailing raw bytes with no matching values. Time-axis labels use
seconds, milliseconds, microseconds or nanoseconds as appropriate.

**PLOTTED FIELDS** in setup is the single place to choose field colors and line
styles. The style selector shows the actual stroke: solid, dashed, dotted,
dash-dot, dash-dot-dot, or a line with filled star or triangle markers in the
same color as the line. Markers stay evenly spaced on screen at different
sample densities. These choices apply to retained traces and are saved with
the other serial settings; binary channels have independent styles too. The
graph does not repeat the field list.

With **no text fields** or **no binary schema**, Start still reads the port and
shows only raw data in the lower pane. A schema is never required for receiving.
Text renders printable ASCII, CR/LF records and escaped nonprintable bytes.
Binary renders byte offsets, hexadecimal bytes and printable ASCII. Select text
and use Cmd+C / Ctrl+C to copy it; Save raw writes the retained bytes unchanged.

History is bounded to the latest 64 KiB of raw bytes and 10,000 points per field.
Save raw exports that retained window, not an unlimited recording. Open raw
accepts up to 8 MiB and processes it using the current decoding settings.
Live graph timestamps are host receive times; multiple records in one read
share that timestamp. Imported raw bytes have no timestamps, so their time axis
is estimated from the configured baud rate and UART frame size. Multiple
decoded records in one file-read block are distributed over that estimated
interval; live records continue to share their receive timestamp.

## Text fields

Add up to ten named fields and choose each line's color and stroke style. For example, fields
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

After loading JSON, **PLOTTED FIELDS** lists the decoded field names with an
independent color and line-style selector for each channel. Fields containing
`bits` show their parent JSON name above the child bit-field controls. Reloading
a schema keeps colors and styles for retained channel names.

For a storage field containing `bits`, each named bit field gets its own graph
line, color and style. The parent storage field consumes its stated byte width once.
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
