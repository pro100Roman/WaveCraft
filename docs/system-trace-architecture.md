# System and RTOS trace architecture

Status: research proposal. No implementation exists yet.

## Conclusion

Adding a SystemView-like trace analyzer to WaveCraft is feasible. It should be
implemented as a new **System Trace** instrument family rather than being
forced into the existing sample-oriented `CaptureBatch` model.

The recommended design is:

- a separate System Trace view with independent trace sessions;
- compatibility with firmware instrumented for the SEGGER SystemView target
  protocol;
- RTT transported over SWD through J-Link, ST-Link V2, V2.1, and V3, and
  Black Magic Probe (BMP);
- a provider-neutral debug-probe layer;
- a native event model and recording format; and
- no inclusion in WaveCraft's Combined view initially.

The main feasibility risks are sustained RTT throughput on ST-Link V2 and BMP
under bursty, heavily instrumented workloads, and whether a particular BMP
target permits non-halting memory access. These need focused hardware
benchmarks, but they are not architectural blockers.

## What SystemView does

SystemView is not a passive RTOS detector. Target firmware must be instrumented
to emit events such as:

- task creation, ready, running, blocking, suspension, and termination;
- ISR entry, exit, and switch-to-scheduler;
- software timer execution;
- RTOS and middleware API entry and exit;
- user markers and measured regions;
- resource names and synchronization-object activity;
- heap allocation and free events;
- log, warning, and error messages; and
- numeric samples for time-synchronized data plots.

The target library assigns high-resolution target timestamps and writes compact
packets into an RTT ring buffer. The host application reads those packets and
reconstructs task and interrupt state, CPU usage, statistics, and timelines.
SEGGER describes typical firmware cost as less than 2 KB of code, around 600
bytes of RAM, and under 1% CPU at 10,000 events per second on a 200 MHz
Cortex-M4. See the [SEGGER SystemView overview][segger-systemview].

The event stream is documented well enough to implement a compatible decoder:

- standard events 0 through 23 have fixed, known structures;
- events 24 through 31 and IDs above 31 carry lengths;
- IDs 32 through 1023 describe OS and module events;
- IDs 1024 through 2047 are user events;
- values use compact 7-bit variable-length encoding;
- every event contains a target timestamp delta; and
- overflow packets explicitly report lost events.

The packet rules and examples are documented in the
[official SystemView target source][sysview-source]. The standard and extended
event IDs are defined in [SEGGER_SYSVIEW.h][sysview-header].

The host also controls recording through the RTT down-channel. The current
target implementation recognizes commands for start, stop, system time, task
list, system description, module descriptions, and heartbeat. The command
values are defined in [SEGGER_SYSVIEW_Int.h][sysview-internal-header].

## Why multiple debug probes are possible

RTT is fundamentally a set of ring buffers in target RAM. J-Link is not required
to interpret SystemView events; it is one way to read and update those buffers
while the target continues running. ST-Link and Black Magic Probe can perform
the same target memory access through SWD.

```text
Instrumented firmware
        |
        v
SystemView event packets
        |
        v
RTT buffers in target RAM
        |
        +-- J-Link / SWD
        +-- ST-Link V2 / SWD
        +-- ST-Link V2.1 / SWD
        +-- ST-Link V3 / SWD
        `-- Black Magic Probe / SWD
                |
                v
WaveCraft decoder -> event model -> timeline, statistics, and UI
```

The strongest implementation basis is `probe-rs`, which already provides:

- J-Link, ST-Link, and Black Magic Probe discovery and connection;
- target memory access while the target is running;
- RTT control-block scanning or exact-address attachment;
- RTT up-channel reads and down-channel writes; and
- ELF-assisted discovery of `_SEGGER_RTT`.

Its public RTT API supports automatic attachment, exact addresses, restricted
memory ranges, and channel read and write operations. See the
[probe-rs RTT API][probe-rs-rtt].

The probe support documentation lists ST-Link V2 firmware 2.26 or newer and
ST-Link V3 firmware 3.2 or newer. V2.1 devices are recognized as members of the
V2 family, but must be explicitly included in WaveCraft's hardware test matrix.
See [probe-rs probe support][probe-rs-probes].

The current `probe-rs` implementation also has a native Black Magic backend. It
discovers the BMP debug CDC interface, supports its remote protocols, and
provides the target memory operations required by the generic `probe-rs` RTT
implementation. The project explicitly lists Black Magic Probe among its
supported probes. See the [probe-rs project description][probe-rs-bmp] and
[Black Magic backend source][probe-rs-bmp-source].

ST documents the following maximum interface clocks:

- ST-Link V2 and V2.1: SWD up to 4 MHz;
- ST-Link V3: SWD up to 24 MHz;
- ST-Link V2 SWV up to 2 MHz; and
- ST-Link V3 SWV up to 16 MHz, depending on model and configuration.

SystemView documentation describes ordinary trace loads around 5 to 15 KB/s,
so continuous ST-Link RTT is likely practical. High event-rate bursts can still
fill the target buffer, especially with V2. Actual sustained and peak rates must
be measured on each supported probe generation rather than inferred only from
the SWD clock.

## Black Magic Probe assessment

Black Magic Probe can support System Trace through two related transport paths.

### Generic probe-rs RTT path

This is the recommended baseline implementation. WaveCraft connects through the
`probe-rs` Black Magic backend, reads and writes target RAM, and uses the same
host-side RTT implementation as J-Link and ST-Link. It does not require BMP's
firmware-side RTT forwarding feature, and it keeps channel discovery, raw byte
capture, and SystemView start and stop commands identical across providers.

This path still needs a hardware benchmark. The BMP remote protocol adds a
serial link and command framing between WaveCraft and the probe, so its
throughput and burst latency cannot be assumed to match J-Link or ST-Link.

### BMP firmware-accelerated RTT path

Black Magic Debug firmware can optionally poll RTT inside the probe and expose
the selected RTT channels through its second USB CDC serial interface. Its
monitor commands support enabling RTT, restricting the RAM scan range,
selecting channels, inspecting the control block, and changing the adaptive
poll interval. The documented measured target-to-host rate is approximately
50,000 characters per second on several probe hardware builds. See the
[Black Magic RTT documentation][bmp-rtt].

This is a useful future optimized backend, with several constraints:

- the probe firmware must have RTT support enabled;
- WaveCraft must configure the correct SystemView up and down channels;
- auxiliary target UART output must not share the same CDC stream because BMP
  documents that UART and RTT output are interleaved;
- WaveCraft must treat the CDC stream as binary and must not apply terminal
  line-ending, echo, or text conversions; and
- host-to-target writes must be verified byte-for-byte because SystemView uses
  binary commands rather than terminal lines.

The generic `probe-rs` path should therefore establish initial BMP support.
Firmware-accelerated RTT should be added only after an end-to-end binary-stream
test proves that it preserves every SystemView byte and improves burst
tolerance.

### Non-halting access requirement

Some target architectures do not allow BMP to read memory while the core is
running. Black Magic can work around this by briefly halting the core during RTT
polling, but doing so perturbs the schedule being measured and is not acceptable
as transparent real-time tracing. Black Magic Debug 2.1 added or restored
non-halting RTT access for a set of targets, including several STM32, GD32,
AT32, and Nordic families. See the
[Black Magic Debug 2.1 release notes][bmp-2-1-release].

WaveCraft must report the resulting capability explicitly:

- **continuous, non-intrusive** when the target supports memory access while
  running;
- **intrusive** when BMP must briefly halt the target, disabled by default; or
- **single-shot/post-mortem only** when continuous access is unsuitable.

## Transport choice

### RTT over SWD

This should be the primary transport. It works with J-Link, the requested
ST-Link generations, and BMP, needs no additional target pins, preserves
compatibility with existing
SystemView-instrumented firmware, and provides ring-buffer semantics that allow
the target to detect and report overflow.

### SWO and ITM

SWO should be a possible future transport, not the first implementation.
ST-Link supports SWO, but standard SystemView firmware does not normally emit
its event stream through ITM. Supporting it would require another target-side
transport or instrumentation path. ST also warns that SWV bytes can be lost due
to probe buffer limitations. See the [ST SWV documentation][st-swv].

### UART

SystemView already supports a UART recorder. ST-Link V2.1 and V3 instruments
often provide a virtual COM port, while a standalone V2 generally requires a
separate USB-to-UART adapter. UART is a useful later backend but is not required
to meet the initial probe-support goal.

### IP

SystemView also supports an IP recorder. This is useful for large or remote
targets but is independent of the immediate debug-probe requirement.

## Fit with WaveCraft

### Keep trace separate from sampled captures

WaveCraft currently models logic, scope, and IQ payloads as sampled arrays in
`wavecraft-core`. System trace differs fundamentally:

- events are irregularly timed;
- tasks and interrupts form state transitions;
- contexts appear and disappear dynamically;
- nested ISR state must be reconstructed;
- metadata can arrive after related events; and
- overflow means part of the timeline is explicitly unknown.

Adding a `Trace` variant to `CaptureBatch` would spread trace-specific behavior
through sample counts, waveform viewports, recording, and Combined-capture
logic. Instead, WaveCraft should introduce a sibling subsystem with types such
as:

- `TraceInstrumentDescriptor`;
- `TraceSource`;
- `TraceSession`;
- `TraceEvent`;
- `TraceMetadata`; and
- `TraceRecording`.

An `InstrumentKind::SystemTrace` variant may be useful for unified discovery,
but it should not imply that a trace probe implements `CaptureSource`.

### Add a true top-level instrument view

The current GUI assumes selectable views are waveforms and maps each one to a
`CaptureTarget`. The clean extension is either to generalize `WaveformTab` into
an instrument or workspace view, or to add a wrapper such as:

```text
WorkspaceView
|- Waveform(WaveformTab)
`- SystemTrace
```

System Trace needs its own start and stop lifecycle instead of inheriting
Single, Repetitive, Loop, and Run semantics. It should have its own session
tabs, settings, recordings, and completed-session state.

Trace acquisition may run concurrently with logic, scope, or radio acquisition
when the instruments are independent. It should not initially participate in
Combined capture because target-cycle timestamps and WaveCraft's host monotonic
timestamps are not automatically synchronized.

Future cross-instrument correlation should require an explicit mechanism, such
as a trace marker paired with a captured GPIO edge, rather than using USB
arrival time as if it were exact.

### Discover separate trace instruments

Trace discovery should list debug probes separately from DSLogic, DSCope, and
SDR devices. Each entry should expose:

- provider: J-Link, ST-Link, or Black Magic Probe;
- probe model and generation;
- USB serial number;
- firmware version;
- available wire protocols;
- selected target MCU and core;
- connection state; and
- selected RTT control-block address and channel.

Probe connection must use per-instrument ownership. Connecting a trace probe
must not reserve unrelated logic, scope, or radio instruments.

## Recommended components

A suitable crate split would be:

- `wavecraft-trace`: provider-independent event types, context-state
  reconstruction, statistics, indexing, and queries;
- `wavecraft-systemview`: incremental SystemView packet decoding, command
  encoding, OS and module description support, and raw-stream compatibility;
- `wavecraft-debug-probe`: discovery, target attach, memory access, and RTT
  transport traits; and
- a `probe-rs` adapter providing J-Link, ST-Link, and Black Magic Probe access;
  and
- an optional later BMP firmware-accelerated RTT adapter using the probe's
  auxiliary CDC interface.

The GUI would then add modules for trace acquisition, probe settings, the trace
timeline, events, CPU load, context statistics, logging, data plots, and heap
analysis.

The normalized event model should retain at least:

- core ID;
- monotonic event sequence;
- target timestamp ticks;
- derived timestamp in nanoseconds;
- event kind;
- active context or task;
- task, ISR, timer, marker, resource, or heap identifiers;
- typed parameters;
- raw packet offset; and
- data-loss and discontinuity markers.

The decoder must be incremental because a USB or RTT read can divide a packet
at any byte. It must safely handle malformed variable-length integers,
truncated packets, timestamp wrapping, stream restarts, overflow events,
unknown event IDs, and bounded field lengths.

## Proposed System Trace view

### Setup panel

- Probe and serial-number selection.
- Target MCU and core.
- SWD speed.
- Attach without reset by default.
- ELF file selection.
- RTT control-block mode: ELF symbol, exact address, range, or automatic scan.
- RTT channel selection.
- Trace buffer usage and overflow status.

### Main timeline

- One lane per ISR, scheduler, timer, and task.
- Running, ready, blocked, and suspended intervals.
- Nested interrupt visualization.
- Zoom, pan, selection, markers, and duration measurement.

### Analysis panels

- Virtualized event table.
- Selected-event details.
- CPU-load graph.
- Context statistics.
- Logs and warnings.
- User data plots.
- Heap activity and outstanding allocations.

The first usable release does not need full SystemView parity. A task and ISR
timeline, event list, names, CPU load, logs, overflow visibility, and recording
would already form a valuable instrument.

## ELF and description support

ELF support materially improves usability:

- resolve `_SEGGER_RTT` without scanning all RAM;
- resolve target resources and symbols;
- decode Print-ELF messages from the `.sv-data` section;
- read target or core information where it is available; and
- preserve firmware build identity in recordings.

SystemView OS and middleware API events depend on description schemas that map
event IDs to names and typed parameters. WaveCraft should parse compatible
description files and embedded module descriptions instead of hard-coding
FreeRTOS calls.

Without a description file, WaveCraft can still show task switches,
interrupts, standard events, and raw custom-event parameters.

## Recording design

Trace should initially use a separate, versioned recording format such as
`.wctrace`. It should preserve:

- exact raw RTT bytes;
- transport chunk boundaries and discontinuities;
- probe, target, firmware, core, and ELF identity;
- system description and timing frequencies;
- normalized decoded events;
- task, resource, and module metadata;
- completion or failure state; and
- a time index for efficient offline navigation.

Keeping raw bytes allows a newer decoder to reinterpret an old recording.
Keeping normalized events avoids reparsing an entire large capture every time
it is opened.

Interoperability includes:

- import of raw `.SVDat` streams and desktop recordings with the standard
  text metadata preamble;
- export of raw SystemView-compatible streams.

Later interoperability can include CSV export for events, CPU usage, contexts,
and logs.

SEGGER documents that a raw single-shot RTT buffer can be saved with an
`.SVDat` extension and opened by SystemView. Its richer desktop recording
container is not a documented interchange format, so WaveCraft only recognizes
the metadata preamble observed in SEGGER recordings and then decodes the
standard target event stream that follows it. See the [SystemView recording
documentation][systemview-manual].

WaveCraft's current `.dsv` format has fixed instrument IDs for logic, scope,
and radio. A separate trace format avoids prematurely extending a sample-based
container before the trace model is stable.

## Risks and constraints

### ST-Link V2 bandwidth

Typical trace loads should fit, but peak interrupt and API traffic may overflow
the target buffer. WaveCraft must show loss prominently and recommend increasing
the RTT buffer, increasing SWD speed, or disabling noisy event groups.

### Black Magic Probe throughput and intrusion

The documented BMP firmware RTT rate of roughly 50 KB/s is above SystemView's
typical 5 to 15 KB/s load, but does not guarantee lossless handling of short
bursts. The generic probe-rs route may perform differently from BMP's
firmware-accelerated path. Both must be benchmarked with binary SystemView data,
large bursts, up/down traffic, and overflow accounting.

Continuous trace must also be rejected or clearly marked intrusive when the
probe has to halt the target to poll RTT.

### Debugger coexistence

Direct `probe-rs` access normally owns the USB probe. ST provides an ST-LINK
Server shared mode, but the server does not manage client priorities and warns
that one client can disturb another. See the
[ST-LINK Server limitations][stlink-server].

Initial support should therefore use exclusive access. Shared ST-LINK Server
support can be a separate transport backend later.

Black Magic Probe similarly exposes one debug-control CDC interface. WaveCraft
should assume exclusive ownership of that interface. Its auxiliary CDC
interface is independently usable for accelerated RTT only when the firmware
has been configured for that purpose.

### J-Link driver experience

`probe-rs` supports J-Link but documents lower performance than SEGGER's
proprietary stack. On Windows it also requires a generic WinUSB driver instead
of the normal SEGGER driver. A future optional backend that uses the official
J-Link library may provide better performance and coexistence, subject to its
licensing and redistribution terms.

### Rust version compatibility

WaveCraft currently declares Rust 1.79. Current `probe-rs` 0.32 declares Rust
1.95 in its [workspace manifest][probe-rs-manifest]. Implementation must choose
one of the following deliberately:

- raise WaveCraft's minimum supported Rust version;
- validate and pin an older `probe-rs` release; or
- isolate the current probe implementation in a separately built helper
  process.

This is an early dependency decision, not something to discover after the UI
has been built.

### Low-power and protected targets

RTT needs debug access to RAM while the MCU runs. Sleep modes, debug-disabled
low-power states, read-out protection, TrustZone, or debug authentication can
prevent continuous recording. WaveCraft should diagnose these states and
provide guidance. It must not silently reset the MCU or modify security and
low-power configuration.

### Large and continuous recordings

Trace streams can run indefinitely. The implementation needs:

- a bounded live display cache;
- direct streaming to disk;
- virtualized event lists;
- time- and context-based indexes; and
- bounded parser fields and message sizes.

The recorder must subscribe directly to incoming trace chunks rather than read
back from the UI's bounded cache.

## Licensing and naming

The SystemView desktop application has SEGGER commercial and friendly-license
terms. The current SystemView target sources and RTT sources use a permissive
one-condition redistribution license compatible with WaveCraft's
GPL-3.0-or-later license. See the [SystemView target license][sysview-license]
and [RTT license][rtt-license].

WaveCraft should call the feature **System Trace** or **RTOS Trace**, describe
it as compatible with the SEGGER SystemView target protocol, retain required
copyright notices when redistributing target sources, and avoid implying
SEGGER endorsement.

Black Magic Debug is published under GPL-3.0, BSD-3-Clause, and MIT licenses,
and `probe-rs` is MIT or Apache-2.0. Connecting to BMP through `probe-rs` adds no
license incompatibility with WaveCraft. If WaveCraft later distributes a BMP
firmware image rather than merely supporting installed firmware, that image and
its corresponding source and notices must be handled as a distinct distributed
component.

## Validation strategy

### Decoder tests

- Golden standard and extended event packets.
- Split every representative packet at every possible byte boundary.
- Malformed and overlong variable-length integers.
- Truncated strings and payloads.
- Timestamp delta wrapping and stream restart.
- Explicit overflow and unknown-event handling.
- Standard, module, OS, and user event ID ranges.
- Fuzzing with arbitrary target-provided bytes.

### State reconstruction tests

- Task create, ready, execute, block, and terminate transitions.
- Nested interrupts and interrupt-to-scheduler transitions.
- Timer nesting and context restoration.
- Trace start and stop boundaries.
- Gaps after overflow without invented activity.
- Multiple cores with independent streams and clocks.

### Transport tests

- Fake RTT memory with independently moving read and write offsets.
- Partial reads, wraparound, full buffers, cancellation, and disconnect.
- Exact address, ELF address, range scan, and full-RAM scan.
- Start, stop, task-list, and description commands.
- Target reset or RTT control-block replacement during acquisition.

### Opt-in hardware tests

At minimum, test official examples of:

- J-Link;
- ST-Link V2;
- ST-Link V2.1; and
- ST-Link V3;
- native Black Magic Probe hardware with current Black Magic Debug firmware;
  and
- BMP firmware-accelerated RTT, if that optimization is implemented.

For each probe, measure sustained throughput, burst tolerance, overflow rate,
stop latency, reconnect behavior, and the effect of common target sleep modes.
BMP tests must additionally verify binary transparency, down-channel commands,
CDC interface selection, non-halting access, and behavior when accelerated RTT
is absent from the probe firmware. Default workspace tests must continue to run
without debug hardware.

## Recommended implementation sequence

1. Decode raw SystemView streams and reconstruct task and ISR timelines
   offline.
2. Validate RTT reads and start/stop commands with J-Link, each requested
   ST-Link generation, and BMP through the generic probe-rs path.
3. Add ELF-based RTT discovery and System Trace instrument discovery.
4. Add the separate GUI view with timeline, events, logs, and overflow status.
5. Add streaming native recording and offline reopening.
6. Add CPU and context statistics, data plots, markers, heap analysis, and
   custom descriptions.
7. Evaluate and, if justified, add BMP firmware-accelerated RTT.
8. Add multicore, post-mortem, UART, IP, shared-debugger backends, and explicit
   cross-instrument synchronization.

The hardware benchmark in step 2 is the critical go/no-go gate for the quality
of continuous ST-Link V2 and BMP support. The protocol and architecture do not
contain a J-Link-specific dependency that prevents first-class ST-Link or BMP
support. BMP continuous-trace support must additionally require non-halting
memory access on the selected target.

[segger-systemview]: https://www.segger.com/products/development-tools/systemview/
[sysview-source]: https://raw.githubusercontent.com/SEGGERMicro/SystemView/main/SYSVIEW/SEGGER_SYSVIEW.c
[sysview-header]: https://raw.githubusercontent.com/SEGGERMicro/SystemView/main/SYSVIEW/SEGGER_SYSVIEW.h
[sysview-internal-header]: https://raw.githubusercontent.com/SEGGERMicro/SystemView/main/SYSVIEW/SEGGER_SYSVIEW_Int.h
[probe-rs-rtt]: https://docs.rs/probe-rs/latest/probe_rs/rtt/index.html
[probe-rs-probes]: https://probe.rs/docs/getting-started/probe-setup/
[probe-rs-bmp]: https://github.com/probe-rs/probe-rs
[probe-rs-bmp-source]: https://raw.githubusercontent.com/probe-rs/probe-rs/master/probe-rs/src/probe/blackmagic/mod.rs
[bmp-rtt]: https://black-magic.org/docs_v1/usage/rtt/
[bmp-2-1-release]: https://black-magic.org/blog/release-v2-1-0/
[st-swv]: https://dev.st.com/stm32cube-docs/prog/2.23.0/en/docs/markup/CubeProg_UserManual/Serial_wire_viewer_SWV.html
[systemview-manual]: https://doc.segger.com/UM08027_SystemView.html
[stlink-server]: https://www.st.com/resource/en/release_note/dm00402226-st-link-server-stmicroelectronics.pdf
[probe-rs-manifest]: https://github.com/probe-rs/probe-rs/blob/master/Cargo.toml
[sysview-license]: https://raw.githubusercontent.com/SEGGERMicro/SystemView/main/LICENSE.md
[rtt-license]: https://raw.githubusercontent.com/SEGGERMicro/RTT/main/LICENSE.md
