# CMSIS-DAP System Trace transport notes

This document records the work required for live SEGGER SystemView capture
through the ESP32JTAG CMSIS-DAP probe over USB and TCP. The host workarounds
are deliberately scoped to ESP32JTAG; J-Link and unrelated CMSIS-DAP probes
keep their existing transfer behavior.

## USB failure

The failing setup used the USB probe identified as `ESP32JTAG CMSIS-DAP` and
an nRF52832 target. A capture initially ran for slightly more than three
seconds, reported more than 32,000 target-dropped events, stopped updating the
timeline, and froze the target. A later run at a requested 1 MHz SWD clock had
the same failure.

J-Link captured the same target firmware correctly. The target SystemView RTT
up-buffer uses `SEGGER_RTT_MODE_NO_BLOCK_SKIP`, so a full trace buffer drops
events instead of blocking the target. Together, these observations isolate
the freeze to the ESP32JTAG CMSIS-DAP/SWD path rather than the target trace
configuration or WaveCraft's decoder.

## Confirmed firmware clock-control bug

The ESP32JTAG build selects
`components/CherryDAP/projects/esp32s3/main/DAP_config.h`. Its platform header
defined `DAP_CPU_CLOCK` as zero. CherryDAP's `Set_Clock_Delay` consequently
calculated a maximum SWJ clock of zero and selected the unthrottled fast GPIO
path for every nonzero `DAP_SWJ_Clock` request.

As a result, changing WaveCraft from 6 MHz to 1 MHz did not change the physical
SWD timing. This was a real firmware defect, but it was not the primary cause
of the capture failure: after flashing the correction, sustained capture still
froze the target with requested clocks from 1 MHz through 10 MHz.

Firmware commit
[`062f5906`](https://github.com/pro100Roman/esp32jtag_firmware/commit/062f59067224f1f4254b9ad90bcdf787b61ea3f4)
defines `DAP_CPU_CLOCK` from `CONFIG_ESP_DEFAULT_CPU_FREQ_MHZ`. With the current
ESP32-S3 configuration this is 240 MHz. CherryDAP can now calculate a delay for
the requested SWD frequency instead of always selecting its fast path.

The firmware was rebuilt successfully with ESP-IDF 5.5.6. The generated image
is `build/esp32jtag_firmware.bin`; the complete flash image is the generated
`build/esp32jtag_*_full.bin` file. Keep the fix because requested clock control
is required for useful diagnostics, but do not treat it as resolving the
target freeze.

## CMSIS-DAP host behavior

The ESP32JTAG probe advertises eight CMSIS-DAP packet buffers. An initial host
workaround forced the packet count to one because the failure looked like a
pipelined-reply loss. Hardware testing showed that serialization reduced RTT
throughput and did not prevent the target freeze. Both serialized and
pipelined runs still used CherryDAP's `DAP_TransferBlock` command for sustained
memory reads, so serialization did not isolate that firmware path.

Probe-rs commit
[`b99e3f7b`](https://github.com/pro100Roman/probe-rs/commit/b99e3f7bdfe9bbdb94d8fe44b84863acbb1b5e57)
reverts that workaround. Its descendant used by WaveCraft therefore uses the
packet depth reported by the USB probe. Any future packet-queue workaround
must be supported by a captured request/reply ordering failure rather than
inferred from a target freeze.

Probe-rs commit
[`ec01f319`](https://github.com/pro100Roman/probe-rs/commit/ec01f319fab9e597bb417fa5df7a33d307b7b458)
adds a product-specific isolation for `ESP32JTAG CMSIS-DAP`. Long repeated
accesses are split into ordinary multi-access `DAP_Transfer` packets instead of
using `DAP_TransferBlock`. Other CMSIS-DAP probes retain block transfers and
normal pipelining. This deliberately trades some RTT throughput for target
liveness and establishes whether CherryDAP's block-transfer implementation is
the failing layer.

Hardware testing after that change showed that avoiding `DAP_TransferBlock`
alone was insufficient: sustained ordinary `DAP_Transfer` traffic could still
leave the target unresponsive. The relevant difference from J-Link is not the
SystemView decoder or RTT ring-buffer mode. Probe-rs configured CMSIS-DAP with
`idle_cycles = 0`, so every long RTT memory read became a continuous series of
SWD accesses. At the observed roughly 10.6 KiB/s, a single 16 KiB logical RTT
read can occupy the CMSIS-DAP/SWD path for more than a second. Sleeping after
that whole read does not provide useful backpressure inside the burst.

Probe-rs commit
[`e3ec8255`](https://github.com/pro100Roman/probe-rs/commit/e3ec82551c192be71b3be6c1387c7e147ba243d4)
tested two ESP32JTAG compatibility changes together:

- `DAP_TransferBlock` remains disabled, so repeated accesses travel as bounded
  ordinary `DAP_Transfer` packets.
- `DAP_TransferConfigure` requests eight idle SWD clock cycles after every
  transfer. This creates fine-grained target-bus recovery windows inside a
  large RTT read instead of delaying only after the read has completed.

Hardware validation disproved that combination as a complete fix. The target
still became unresponsive, and roughly 82,000 target events were dropped in
about six seconds. Because SystemView channel 1 is an 8 KiB
`SEGGER_RTT_MODE_NO_BLOCK_SKIP` buffer, overflow cannot block the target, but it
does show that the serialized non-block path is far below the event rate.

The checked target configuration generates about 245,000 events and 1.4 MiB of
trace in roughly eight seconds when captured successfully with J-Link. A
10.6 KiB/s serialized CMSIS-DAP path cannot preserve that stream. Further
serialization or sleeps therefore make both the target-drop count and apparent
stall worse rather than addressing the transport bottleneck.

Probe-rs commit
[`01b5b6d4`](https://github.com/pro100Roman/probe-rs/commit/01b5b6d4f38e14d5984b17228cf36eb86e4dbc54)
keeps the eight per-transfer idle cycles but restores normal
`DAP_TransferBlock` requests and the probe-reported packet pipeline. This is the
next hardware-test configuration: it combines fine-grained target recovery
windows with the throughput-oriented block pipeline. WaveCraft's TCP proxy now
also preserves ESP32JTAG's reported packet count instead of forcing it to one.

The rule is selected only for the exact USB product `ESP32JTAG CMSIS-DAP` and
for WaveCraft's CMSIS-DAP TCP selector, whose transport has no USB product
descriptor. J-Link uses a different backend and is unaffected. Other USB
CMSIS-DAP products retain zero extra idle cycles and normal block transfers.

## Host-side backpressure

The target freeze was independent of GUI load, but the live path still needs
bounded work when the target produces events faster than USB RTT can drain it:

- The helper streams as many as four consecutive 16 KiB RTT drains in one
  request, keeping the total request capacity at 64 KiB.
- The GUI consumes at most 4,096 trace updates per frame and then yields to
  painting and input processing.
- Updates collected in one frame are applied as a batch, so timeline pruning
  runs once per batch rather than once per evicted event.
- Displayed event history is bounded at 20,000 events by default. Reconstructed
  intervals older than the live window are discarded while current state is
  preserved.
- A display-gap timestamp advances the visible time bound even when queued
  display events are intentionally skipped.
- Raw `.SVDat` recording remains independent of the bounded display model and
  preserves the complete byte stream received by WaveCraft.

Target loss and display loss are separate measurements. The SystemView
target-drop count means the target RTT producer could not preserve events. The
WaveCraft display-skip count means the GUI intentionally skipped queued live
updates to remain interactive. Correct SWD timing and a stable CMSIS-DAP
command path are separate requirements; neither guarantees that a 64-byte
full-speed USB CMSIS-DAP link can carry every generated event.

## Validation procedure

After flashing corrected ESP32JTAG firmware and building WaveCraft with the
pinned probe-rs revision:

1. Disconnect other debugger software and power-cycle or reset the target.
2. Start with a requested 1 MHz SWD clock. Confirm the WaveCraft helper uses
   probe-rs `01b5b6d4` or a descendant so both the block pipeline and eight idle
   cycles are active.
3. Run SystemView capture well beyond the previous failure interval. Record
   firmware revision, target MCU, requested and measured SWD clocks, RTT
   up-buffer size, capture duration, received bytes, target-drop count, and
   display-skip count.
4. Stop and disconnect. Confirm that the target is still running and a new
   session can attach without resetting either the probe or target.
5. Repeat at higher requested clocks. Treat target liveness and error-free SWD
   access as pass criteria; throughput alone is not sufficient. If the target
   still freezes, capture its halted/running state and fault registers before
   reset, then instrument CherryDAP's ordinary `DAP_Transfer` path.

Software verification for probe-rs `01b5b6d4` includes 69 focused CMSIS-DAP
tests, all 22 WaveCraft probe-adapter tests, the complete WaveCraft workspace
suite, Clippy with warnings denied, formatting, and diff checks. Sustained USB
and TCP captures remain the decisive hardware tests.

## Network CMSIS-DAP today

WaveCraft accepts network selectors in the form
`wavecraft:cmsis-dap-network:<host>:<port>`. The adapter connects to the remote
CMSIS-DAP TCP server through a local, framing-aware proxy so probe-rs can use
its CMSIS-DAP TCP backend.

The proxy currently:

- validates the `DAP\0` frame header, request or response type, and payload
  length;
- preserves the `DAP_Info` packet count and all other request and response
  payloads;
- enables `TCP_NODELAY`; and
- uses an abortive close so a stalled ESP32JTAG/lwIP session does not retain
  the single remote client slot after timeout or helper termination.

The SWD clock fix applies to the command engine shared by USB and network
CMSIS-DAP, but it does not validate the Wi-Fi/TCP transport. CMSIS-DAP TCP has
no USB product descriptor, so the probe-rs fork explicitly routes WaveCraft's
TCP selector through the same ESP32JTAG compatibility rule. Block transfers,
the reported packet pipeline, and eight idle cycles are now consistent between
USB and TCP.

## Network follow-up plan

Use corrected USB firmware as the behavioral baseline and investigate network
CMSIS-DAP in this order:

1. Run the same target image and SystemView event load through USB and network,
   recording sustained throughput and command latency.
2. Capture CMSIS-DAP request and reply sequence numbers, frame sizes, RTT
   occupancy, timeout location, and the last completed command when a stall
   occurs.
3. Exercise start, sustained capture, stop, disconnect, helper termination,
   network loss, and immediate reconnect. Confirm that both the local proxy and
   ESP32JTAG server release their sockets.
4. If a stall remains with `01b5b6d4`, determine whether it originates in the
   firmware server task, Wi-Fi/lwIP transport, shared command engine, WaveCraft
   proxy, or probe-rs timeout handling before changing batching or retry policy.
5. Tune logical RTT read size and helper batching separately from CMSIS-DAP
   packet pipelining. A large serialized memory read can improve throughput but
   delay stop or disconnect commands.
6. Add opt-in hardware tests for sustained capture, overflow accounting,
   cancellation, target liveness, and reconnect. Keep the default workspace
   suite independent of debug hardware.

Success means capture is not bounded by a transport stall, target and display
loss remain separately measurable, stop and disconnect complete in bounded
time, the target remains alive, and a new session can attach without a reset.
