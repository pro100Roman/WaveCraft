# CMSIS-DAP System Trace transport notes

This document records the transport work needed to keep live SEGGER SystemView
capture responsive through CMSIS-DAP. It separates the validated USB changes
from the network CMSIS-DAP work that still needs hardware investigation.

## USB failure that was observed

The failing setup used the USB probe identified as `ESP32JTAG CMSIS-DAP`. A
capture ran for slightly more than three seconds, reported more than 32,000
dropped target events, stopped updating the timeline, and left the target
unresponsive.

Investigation found two independent failure risks:

1. The probe firmware advertises eight CMSIS-DAP packet buffers. `probe-rs`
   therefore pipelined `DAP_TransferBlock` requests during large RTT memory
   reads. This ESP32JTAG firmware did not reliably preserve the replies during
   sustained pipelined traffic.
2. Once events reached the application, the live timeline reconstruction could
   retain more historical intervals than the bounded event list displayed.
   Rendering and per-frame ingestion work consequently grew during a busy
   capture.

Target loss and display loss are different conditions. The SystemView target
drop count means the target RTT producer could not preserve events. The
WaveCraft display-skip count means the GUI intentionally skipped queued live
events to remain interactive. A transport fix can prevent a probe or target
stall without guaranteeing that the link has enough bandwidth for every event.

## USB driver fix

The WaveCraft `probe-rs` fork has an ESP32JTAG compatibility rule in commit
[`1809a9d5`](https://github.com/pro100Roman/probe-rs/commit/1809a9d547243e91151b8d85679b9f23ec60dbe0).
WaveCraft pins the helper dependency to that revision. When the product name is
exactly `ESP32JTAG CMSIS-DAP`, `probe-rs` overrides the reported packet count to
one.

This serialization happens inside the CMSIS-DAP driver. A logical 16 KiB RTT
read can still be issued, but its 64-byte `DAP_TransferBlock` packets are sent
and acknowledged in order. WaveCraft does not split the logical read into tiny
RTT reads, because every RTT read also accesses control-block pointers and
would substantially reduce throughput.

The compatibility rule is deliberately limited to the exact product name. It
does not slow unrelated CMSIS-DAP probes. If ESP32JTAG firmware later provides
reliable pipelined replies, validate that firmware under sustained RTT load
before relaxing or removing the host quirk.

## Host-side backpressure changes

The USB driver fix addresses the transport stall. The following changes keep
the rest of the live path bounded:

- The helper streams as many as four consecutive 16 KiB RTT drains in one
  request. This removes helper-process round trips while keeping the total
  request capacity at 64 KiB. Each completed drain is returned immediately.
- The GUI consumes at most 4,096 trace updates per frame, then yields to paint
  and input processing.
- Updates collected in one frame are applied as a batch. Timeline pruning runs
  once after the batch instead of once for every evicted event.
- The displayed event history remains bounded at 20,000 events by default.
  Reconstructed execution and marker intervals older than that live window are
  discarded while current task, interrupt, marker, and aggregate state remain
  valid.
- A display-gap timestamp advances the visible time bound even though the
  skipped events themselves are not retained.
- Raw `.SVDat` recording is independent of the bounded display model and keeps
  the complete byte stream that reached WaveCraft.

## USB validation

The change was verified with:

- the focused `probe-rs` product-quirk test;
- all tests in the separately built `wavecraft-probe-rs` adapter;
- the complete WaveCraft workspace test suite;
- workspace and adapter Clippy with warnings denied;
- formatting and diff checks; and
- discovery of the connected `0d28:0204` probe with the expected
  `ESP32JTAG CMSIS-DAP` product string.

A sustained hardware capture remains the final throughput test. Record the
probe firmware version, target MCU, SWD clock, RTT up-buffer size, enabled
SystemView event groups, capture duration, received bytes, target drop count,
and display-skip count. The target must continue running after stop and
disconnect.

## Network CMSIS-DAP today

WaveCraft accepts network selectors in the form
`wavecraft:cmsis-dap-network:<host>:<port>`. The adapter connects to the remote
CMSIS-DAP TCP server through a local, framing-aware proxy so that `probe-rs` can
use its CMSIS-DAP TCP backend.

The proxy currently:

- validates the `DAP\0` frame header, request or response type, and payload
  length;
- recognizes the `DAP_Info` packet-count query and changes its response to one
  in-flight packet;
- preserves all other request and response payloads;
- enables `TCP_NODELAY`; and
- uses an abortive close so a stalled ESP32JTAG/lwIP session does not keep the
  single remote client slot occupied after timeout or helper termination.

These safeguards make network behavior conservative, but they do not establish
that sustained SystemView capture is reliable or fast enough over Wi-Fi.

## Network follow-up plan

Do not increase the network packet pipeline until ordered, lossless replies are
proven under sustained load. Use the serialized USB path as the behavioral
baseline and investigate the network path in this order:

1. Run the same target firmware and SystemView load through USB and network
   CMSIS-DAP, recording both sustained throughput and command latency.
2. Capture CMSIS-DAP request and reply sequence numbers, frame sizes, RTT
   occupancy, timeout location, and the last completed command when a stall
   occurs.
3. Exercise start, sustained capture, stop, disconnect, helper termination,
   network loss, and immediate reconnect. Confirm that both the local proxy and
   ESP32JTAG server release their sockets.
4. Determine whether a stall originates in the firmware CMSIS-DAP server task,
   Wi-Fi/lwIP transport, shared SWD command engine, WaveCraft proxy, or
   `probe-rs` timeout handling before changing batching or retry policy.
5. Tune logical RTT read size and helper batching separately from CMSIS-DAP
   packet pipelining. A large serialized memory read may maximize throughput but
   can delay stop or disconnect commands.
6. Add opt-in hardware tests for sustained capture, burst overflow accounting,
   cancellation, target liveness, and reconnect. Keep the default workspace
   suite independent of network and debug hardware.

Success means that capture duration is not bounded by a transport stall, target
and display loss remain separately measurable, stop and disconnect complete in
bounded time, and a new session can attach without power-cycling the probe or
target.
