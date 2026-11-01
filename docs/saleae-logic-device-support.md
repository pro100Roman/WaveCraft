# Saleae Logic Device Support Assessment

This document assesses adding Saleae Logic hardware support to WaveCraft. It
covers Saleae logic analyzers only. Saleae Logic MSO is explicitly out of scope.

## Summary

Supporting current Saleae Logic devices through Saleae's public Logic 2
Automation API is feasible, but it would require Logic 2 to be installed and
running. Direct, standalone USB capture is not currently implementable from an
official public Saleae driver or SDK: Saleae's public repositories do not
contain the firmware, USB acquisition protocol, or host-side capture driver for
current devices.

The recommended first integration is therefore an automation-backed adapter.
Native USB support should remain conditional on Saleae providing protocol and
firmware access with suitable redistribution terms, or on a separately
approved reverse-engineering effort.

## Public Saleae repositories

A review of Saleae's public GitHub organization found several repositories
whose names suggest device support, but none implements direct communication
with current Saleae Logic hardware:

| Repository | Purpose | Value to WaveCraft device support |
| --- | --- | --- |
| [AnalyzerSDK](https://github.com/saleae/AnalyzerSDK) | Builds protocol analyzers that consume samples already captured by Logic | Useful as an analyzer API reference, not as a hardware driver |
| [usb-analyzer](https://github.com/saleae/usb-analyzer) | Decodes USB traffic observed on logic probe channels | No Saleae device-control implementation |
| [logic2-automation](https://github.com/saleae/logic2-automation) | Publishes the Logic 2 gRPC protocol and Python client | Viable official integration point through a running Logic 2 process |
| [SaleaeSocketApi](https://github.com/saleae/SaleaeSocketApi) | Automates older Saleae software through a socket interface | Legacy automation reference only |
| [gitbook-docs](https://github.com/saleae/gitbook-docs/blob/master/product/logic-software/driver-install.md) | Documents operating-system USB driver installation | Describes device binding, not capture protocol or firmware source |

The `usb-analyzer` repository is easy to misinterpret. Its USB control-transfer
code parses transactions captured from a USB bus; it does not issue control
transfers to a Saleae instrument.

Saleae's install documentation also refers to device "drivers." These are the
operating-system files that bind the USB device to Logic. The acquisition
implementation and the firmware uploaded by Logic when a device connects are
not published in those repositories.

## Available integration paths

### Logic 2 Automation API

The supported [Logic 2 Automation API](https://github.com/saleae/logic2-automation)
can discover devices, configure channels and sample rates, start and stop a
capture, save `.sal` files, and export digital samples. This makes an
automation-backed WaveCraft adapter practical without understanding Saleae's
private USB protocol.

The adapter would:

1. connect to the local Logic 2 gRPC server;
2. discover and select a physical Saleae Logic device;
3. translate a supported WaveCraft logic configuration into an Automation API
   capture configuration;
4. start the capture and wait for completion;
5. export digital data or save a `.sal` capture; and
6. convert the result into WaveCraft `LogicBatch` values.

This is not equivalent to a native WaveCraft driver. Logic 2 remains the device
owner, must be installed and running, and the public API uses file-oriented
export rather than continuous real-time raw-sample streaming. An adapter may
therefore emit samples after a finite capture completes instead of delivering
live USB batches. The existing `.sal` importer can reduce the conversion work
when the adapter saves a capture file.

### Native USB backend

A native implementation would fit WaveCraft's architecture cleanly as another
`CaptureSource`, independent of the UI. However, it needs information and
assets that Saleae does not currently publish for the current Logic family:

- USB device identifiers and per-model capabilities;
- initialization and command/control-transfer definitions;
- acquisition endpoint and packet framing;
- trigger, threshold, clock, and channel configuration formats;
- firmware images plus permission to redistribute them; and
- representative devices and opt-in hardware tests for each supported model.

Saleae's [legacy Device SDK documentation](https://www.saleae.com/support/extensions-api/automation-api/device-sdk)
limits that SDK to Original Logic and Logic16 and states that it is unsupported.
It does not provide a path to current Logic devices.

Unofficial projects demonstrate that limited direct access is technically
possible for some generations. For example,
[saleae-logic-libusb](https://github.com/keesj/saleae-logic-libusb) targets the
Original Logic, and libsigrok's
[fx2lafw driver](https://github.com/sigrokproject/libsigrok/blob/master/src/hardware/fx2lafw/api.c)
contains legacy support. These are useful research references, but their model
coverage, firmware requirements, maintenance state, and licensing must be
evaluated separately. They do not establish support for the current Logic
family.

## Recommendation

Proceed in this order:

1. Prototype an opt-in Logic 2 Automation API adapter for finite digital
   captures. Keep it outside `wavecraft-usb`, because it communicates with a
   local application service rather than a USB device.
2. Test conversion with simulated Automation API responses and checked-in,
   redistributable sample exports so the default test suite needs neither
   Logic 2 nor hardware.
3. Validate supported configuration, capture size, cancellation, export
   ordering, disconnect errors, and model reporting with physical Logic
   devices in ignored hardware tests.
4. Ask Saleae whether a supported native SDK, USB protocol specification, and
   redistributable firmware package are available privately.
5. Consider a native backend only after its protocol source, firmware rights,
   target model matrix, and maintenance burden are explicit.

The automation-backed path can provide useful Saleae Logic interoperability
now. A claim of native support for current Saleae Logic devices should wait
until WaveCraft can own the complete capture lifecycle without relying on
unpublished interfaces or assets.
