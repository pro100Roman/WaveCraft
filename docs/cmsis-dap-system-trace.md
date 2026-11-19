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
is `build_board_esp32jtag/esp32jtag_firmware.bin`; the complete flash image is
the generated `build_board_esp32jtag/esp32jtag_*_full.bin` file. Keep the fix
because requested clock control is required for useful diagnostics, but do not
treat it as resolving the target freeze.

## Confirmed firmware GPIO bottleneck

A headless WaveCraft helper benchmark read 8 KiB RAM blocks through the same
USB CMSIS-DAP path used by RTT. It sustained 100.1 KiB/s at a requested 1 MHz,
99.8 KiB/s at 4 MHz, and 99.8 KiB/s at 6 MHz. Identical throughput at all three
settings shows that the requested clock is no longer the limiting delay. The
ESP32-S3 CherryDAP port called the full ESP-IDF `gpio_set_level()` and
`gpio_get_level()` APIs for every SWCLK and SWDIO edge, imposing an effective
roughly 1 MHz ceiling on the bit-banged transport.

Firmware commit
[`6950e01`](https://github.com/pro100Roman/esp32jtag_firmware/commit/6950e01)
replaces those per-edge driver calls with the ESP32-S3 inline low-level GPIO
accessors. Direction changes still use the ESP-IDF driver because they occur
only at SWD turnarounds.

After flashing `6950e01`, repeated 8 KiB RAM reads sustained about 239 KiB/s
at requested clocks of 1, 4, and 6 MHz, up from about 100 KiB/s before the
change. An eight-second SystemView run received 1,176,155 bytes (143.6 KiB/s),
decoded 202,492 events, and reported 6,831 target-dropped events. The final
`DHCSR=0x01010001` showed that the core remained running rather than halted or
locked up. This is a 2.4x raw-memory improvement and a 3.75x trace-throughput
improvement, but it is still below the event rate captured by J-Link.

The remaining hot path switched SWDIO direction and the external
`SWDIO_RDnWR` buffer through full ESP-IDF driver calls once per SWD transfer.
Firmware commit
[`50960f5`](https://github.com/pro100Roman/esp32jtag_firmware/commit/50960f5)
uses low-level output-enable and output-disable register access for those
turnarounds as well. Its initial implementation omitted the corresponding
ESP32-S3 input-enable operation, leaving SWDIO unable to receive target replies
and causing attach to fail with `No target responded to the debug port`.
Firmware commit
[`f4e6b82`](https://github.com/pro100Roman/esp32jtag_firmware/commit/f4e6b82)
restores the complete input/output direction semantics with low-level GPIO
accessors. It builds cleanly with ESP-IDF 5.5.6 and is the next hardware-test
revision; the throughput numbers above do not include either direction-change
revision. A subsequent hardware run with `f4e6b82` attached successfully and
reported no target-dropped events or target freeze.

These GPIO optimizations are confined to the CherryDAP CMSIS-DAP port. The
Black Magic Probe GDB server retains its separate ESP32 GPIO implementation,
while logic-analyzer and XVC routing remains controlled by the FPGA
`set_cfga()` configuration applied at startup. Changing a port between LA,
SWD/JTAG, and XVC still requires the existing save-and-restart flow. GDB and
CMSIS-DAP share the physical target debug pins when SWD/JTAG routing is active,
so only one debugger connection should drive the target at a time.

Firmware commit
[`cbdbc22`](https://github.com/pro100Roman/esp32jtag_firmware/commit/cbdbc22)
also shows the short firmware commit on the LCD. The display keeps XVC and GDB
on one row and places `DAP:4441` on the next row aligned with XVC. Follow-up
commit
[`56d60b7`](https://github.com/pro100Roman/esp32jtag_firmware/commit/56d60b7)
anchors the firmware hash at the bottom-right corner of the display.

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

An eight-second headless run of this revision received 313,624 bytes
(38.3 KiB/s). The decoder recovered 54,026 events and reported 35,720 target
drops. This confirms that restoring the block pipeline improved the earlier
10.6 KiB/s serialized result, but the pre-`6950e01` GPIO implementation still
could not carry the target's event rate.

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

The connected nRF52832 used during the November 18 hardware check exposed a
separate application failure after reset. `DHCSR=0x01030003` and `DFSR=0x2`
showed a debugger-visible breakpoint rather than an SWD transport stall. Core
registers placed the PC at `app_error_fault_handler`; its saved SDK assert data
identified `sensor.c:339`, where `APP_ERROR_CHECK(dps310_reset(...))` stops the
debug build. Subsequent captures contain no SystemView data until the target
and DPS310 startup failure are recovered. Do not attribute that deterministic
target `BKPT` to CMSIS-DAP polling, and do not use such a run to validate trace
throughput.

Software verification for probe-rs `01b5b6d4` includes 69 focused CMSIS-DAP
tests, all 22 WaveCraft probe-adapter tests, the complete WaveCraft workspace
suite, Clippy with warnings denied, formatting, and diff checks. Sustained USB
and TCP captures remain the decisive hardware tests.

## Network CMSIS-DAP implementation and measurements

WaveCraft accepts network selectors in the form
`wavecraft:cmsis-dap-network:<host>:<port>`. The adapter connects to the remote
CMSIS-DAP TCP server through a local proxy so probe-rs can use its existing TCP
backend without treating a network endpoint as a USB device.

Four independent batching limits caused the original roughly 15-24 KiB/s
throughput:

- ESP32JTAG exposed USB-sized 64-byte packets and an eight-packet queue over
  TCP, even though the TCP framing can carry larger commands.
- Probe-rs pipelined only adjacent block commands. TAR setup transfers divided
  each RTT memory window into a separate network round trip.
- WaveCraft's framing-aware proxy parsed a coalesced probe-rs write and emitted
  one TCP write per DAP frame. That destroyed the batching before it reached
  the ESP32JTAG receive loop.
- The firmware's 5,760-byte lwIP send buffer could not hold a complete response
  batch.

The ESP32JTAG TCP server now reports a transport-specific 1,024-byte packet
capacity and 32-packet queue, allocates matching request and response batches,
and uses a 32 KiB TCP send buffer. The USB descriptor and CherryDAP USB queue
remain 64 bytes and eight packets. GDB port 4242, XVC port 2542, the logic
analyzer routing, and their application protocols are unchanged.

The probe-rs fork now lowers a complete consecutive SWD transfer segment into
ordered `DAP_Transfer` and `DAP_TransferBlock` commands. Up to 32 mixed commands
remain in flight across TAR auto-increment boundaries, so eight 1 KiB RTT
windows can share one network round trip. Probe-rs deliberately caps the
logical TCP packet size at 576 bytes even when the firmware advertises 1,024.
At 576 bytes each 1 KiB window needs two block commands plus its TAR setup, so
eight windows need 24 commands and fit in the firmware queue. Larger packets
reduced utilization of the ESP32 TCP send window; smaller packets needed more
than one queue drain.

WaveCraft's local CMSIS-DAP proxy is now a transparent, 32 KiB buffered
bidirectional byte-stream relay. It no longer splits a coalesced TCP write at
DAP frame boundaries. Both sockets use `TCP_NODELAY`, and the remote side uses
an abortive close so a terminated helper does not retain ESP32JTAG's single
client slot.

The connected nRF52832 produced these repeated 8 KiB RAM-read results at a
requested 6 MHz SWD clock:

| Path | Sustained throughput |
| --- | ---: |
| Original network path | about 15-24 KiB/s |
| Direct probe-rs TCP after firmware and pipeline changes | about 192 KiB/s |
| WaveCraft network selector before the transparent proxy fix | 24.1 KiB/s |
| WaveCraft network selector after the proxy fix | 203-219 KiB/s |
| USB CMSIS-DAP regression check | 275.8 KiB/s |

Three immediate network connect/attach/read/close cycles completed at 203.3,
218.8, and 210.8 KiB/s without a timeout or stale previous connection. This
also verifies that the close path releases the ESP32JTAG client slot.

The final full-rate SystemView drop count still requires a clean target run.
The available target currently stops deterministically in its separate
`sensor.c:339` DPS310 startup assert after a debugger reset
(`DHCSR=0x01030003`), producing no trace bytes. A run in that state is not a
network CMSIS-DAP failure and must not be used to judge event loss. Fully
power-cycle the target and sensor, start capture without another debugger
reset, then record received bytes, target drops, display skips, and final core
state.

### Non-halting trace attach

A clean power-cycle exposed a second attach-time failure which was independent
of RTT polling. `probe-rs::Session::new()` unconditionally called
`clear_all_hw_breakpoints()`. That operation uses `halted_access()`, briefly
halts every running core, clears FPB comparators, and resumes the cores. The
pause is short over a local probe but long enough over ESP32JTAG Wi-Fi to break
Nordic SoftDevice radio timing.

The resulting target state was deterministic: `DHCSR=0x01030003` showed a
halted Cortex-M, `DFSR=0x00000003` reported a breakpoint, and the PC was the
`BKPT` in `app_error_fault_handler`. The saved handler arguments were
`id=1`, `pc=0x1100a`, and `info=0`, identifying
`NRF_FAULT_ID_SD_ASSERT` inside the binary SoftDevice rather than an RTT or SWD
transport lockup.

The probe-rs fork now provides an opt-in attach which preserves existing
hardware breakpoints and skips breakpoint cleanup both when the session opens
and when it is dropped. WaveCraft selects it only for CMSIS-DAP. Existing
probe-rs attach behavior remains the default, and the separate J-Link network
backend is unchanged.

A five-second network capture with this change kept
`DHCSR=0x01010001` and `DFSR=0` before RTT attach, after trace start, and during
every periodic check. The target no longer stopped at connect or during trace.

### Remaining network throughput limit

With the attach failure removed, the old eight-idle-cycle TCP workaround could
be measured independently. Removing those cycles only for CMSIS-DAP TCP raised
the three-second trace rate from about 78.1 KiB/s to 104.5 KiB/s, and the
target remained running with `DFSR=0` during that short test. A longer GUI run
disproved the change: normal trace traffic ended after roughly 3.26 seconds,
the application became unresponsive while the Cortex-M still reported running,
and 55,079 target-dropped events accumulated. The no-idle optimization was
therefore reverted. ESP32JTAG keeps eight idle cycles over both USB and TCP;
J-Link is unaffected.

The short no-idle run reported 26,907 dropped events while WaveCraft received
326,330 bytes and decoded 56,145 events. The producer emits
roughly 175 KiB/s, while the 8 KiB SystemView ring exposed only about 4.6 KiB
per RTT drain. Every drain must read the producer and consumer pointers before
the payload, so Wi-Fi round-trip latency prevents the host from reaching the
raw-memory rate even though block RAM reads exceed 200 KiB/s.

An experimental full-ring snapshot with a deferred consumer-pointer update was
also measured and reverted. It reduced throughput to 89.9 KiB/s and increased
reported loss to 35,926 events because it transferred the complete ring on
every poll and kept the target-visible buffer occupied longer.

The practical next target-side change is to increase the SystemView RTT up
buffer from 8 KiB to at least 16 KiB, preferably 32 KiB when RAM permits. A
larger ring both absorbs Wi-Fi jitter and amortizes each dependent pointer
round trip over more payload. WaveCraft now uses the network CMSIS-DAP channel
descriptor to request as much as the ring's usable capacity, up to 64 KiB, and
automatically reduces the helper batch count so one request never exceeds its
64 KiB IPC limit. USB and J-Link keep their existing 16 KiB maximum read size.

All new packet-size, queue-depth, and mixed-command pipeline behavior is
selected only for CMSIS-DAP TCP. USB CMSIS-DAP retains its existing transfer
limits. J-Link uses a different backend and is unaffected. GDB and XVC retain
their ports and handlers; the only shared network setting is the larger lwIP
send queue, which changes buffering capacity rather than protocol behavior.

