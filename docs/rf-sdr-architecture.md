# RF/SDR capture and signal-decoding architecture

Status: in progress. The core IQ model, bounded capture path, recording v2,
spectrum DSP/display store, simulator, SDR facade, and receive-only HackRF One
adapter are implemented. Channelization and the binary FSK/GFSK demodulator are
also implemented, including bounded streaming GFSK matched filtering with
configurable BT and adaptive symbol timing with bounded clock-error tracking.
A stable-ID demodulator registry and modulation-tagged analyzer profiles expose
typed FSK/GFSK factories without making them the top-level architecture. The
GUI persists those profiles and runs the selected analyzer for live and replayed
IQ into a bounded modulation-neutral output store. An expandable Signal
Analyzer panel edits the tagged channel, modulation, and timing settings;
the RF view renders bounded timed soft-symbol decisions or analog output with
drop, discontinuity, and analyzer-error diagnostics.
For binary FSK, the panel also edits a one-byte hexadecimal preamble value and
an independent preamble length in bytes. After acquisition, the RF view shows
the bounded raw hexadecimal stream including the matched preamble bytes.

WaveCraft treats HackRF as the first backend for a generic radio-receiver
instrument, not as a GUI-specific feature. The backend produces ordered raw IQ
samples through the same capture coordination used by logic analyzers and
oscilloscopes. Device-independent DSP turns IQ into spectrum rows or decoded
signal events, and the GUI renders those derived results without owning device
access or decoding policy.

The first implementation is receive-only. Transmission, duplex scheduling,
regulatory policy, and waveform generation are outside this scope.

## Goals and initial scope

The first RF version must:

- discover and select an attached HackRF by a stable connection identity;
- configure center frequency, sample rate, analog bandwidth, and receive gain;
- stream signed 8-bit complex IQ without unbounded memory growth;
- show a live spectrum and waterfall in a generic **RF Spectrum** view;
- record raw IQ in native `.dsv` recordings and replay it through the same DSP;
- detect discontinuities instead of silently joining samples across overruns;
- support deterministic built-in 2-FSK and GFSK decoding as the first
  modulation family behind a modulation-neutral analyzer contract; and
- hand framed packet bytes to the existing structured decoder model and, in a
  later schema revision, to custom YAML protocols.

The first version does not include automatic modulation classification,
arbitrary GNU Radio graphs, transmission, multi-device coherent reception, or
sample-accurate synchronization with DSLogic or DSCope hardware.

## Layered data flow

```text
HackRF callback / transfer pool
             |
             v
      raw I/Q capture worker
             |
             +----------------------> native .dsv recorder
             |
             v
      bounded I/Q distribution
             |
             +----> spectrum DSP ----> spectrum + waterfall view
             |
             `----> RF channelizer
                         |
                         v
                 selected demodulator
                         |
             +-----------+-----------+
             |                       |
             v                       v
      timed soft symbols      timed analog samples
             |
             v
       symbol mapping
             |
             v
   bit transforms + packet framer
             |
             v
      timed packet bytes
             |
             v
  structured/YAML protocol parser
```

Raw IQ is the source of truth. Spectrum rows, waterfall pixels, discriminator
samples, bits, packets, and decoded fields are derived data. Live and offline
processing must use the same deterministic stages and configuration.

## Crate boundaries

`wavecraft-core` owns only device-independent capture contracts:

- `InstrumentKind::RadioReceiver`;
- `CaptureBatch::Iq(IqBatch)`;
- absolute source-sample positions and discontinuity metadata; and
- capture coordination, cancellation, and error propagation.

`wavecraft-hackrf` owns HackRF discovery, device claiming, configuration, the
native or pure-Rust hardware binding, transfer callbacks, and conversion into
`IqBatch`. Neither GUI types nor FFT and modulation logic belong in this crate.
The exact binding is an internal backend decision; applications depend on the
WaveCraft capture interface, not directly on the HackRF API.

The same interface is implemented independently by `wavecraft-limesdr` for the
LimeSDR-USB Type-A board through Lime Suite NG, and by `wavecraft-antsdr` for
the AntSDR E200 through libiio/IIOD over Gigabit Ethernet. Provider identity,
physical model, and connection backend are separate values: for example,
AntSDR E200 is a model, libiio is its initial provider adapter, and network is
its connection backend. Backend libraries never leak into core, DSP, recording,
or GUI types.

`wavecraft-dsp` owns reusable, hardware-independent processing:

- complex normalization and optional DC/IQ correction;
- frequency translation, FIR filtering, decimation, and rational resampling;
- FFT windows, power spectra, averaging, and peak hold;
- reusable carrier detection, AGC, squelch, AFC, carrier recovery, and symbol
  timing;
- pluggable digital and analog demodulators, with FSK/GFSK as the first digital
  implementation; and
- bitstream transformations and radio packet framing.

The existing scope FFT can later use the same window and spectrum primitives,
but its migration is not a prerequisite for the first RF slice.

`wavecraft-recording` serializes raw IQ batches and RF capture metadata.
`wavecraft-gui`, the CLI, and automation consume the core, DSP, recording, and
backend APIs without duplicating device or decoder behavior.

## Core IQ model

HackRF supplies interleaved signed 8-bit I and Q values. Keeping that native
representation avoids expanding a high-rate stream to `f32` before a consumer
actually needs normalized samples.

```rust
pub struct Iq8 {
    pub i: i8,
    pub q: i8,
}

pub struct IqBatch {
    pub sample_rate_hz: u64,
    pub center_frequency_hz: u64,
    pub first_sample_index: u64,
    pub discontinuity: bool,
    pub samples: Vec<Iq8>,
}
```

`first_sample_index` is an absolute index within one continuous tuning segment.
The expected next value is the previous index plus its sample count. A jump or
`discontinuity` prevents filters, FFT overlap, demodulators, and packet framers
from joining unrelated samples. Retuning or changing sample rate starts a new
segment and resets stateful DSP.

`CaptureFrame.captured_at` denotes the best available coordinator time for the
first sample in the batch. For HackRF this is host-derived timing, not a
hardware timestamp. It is suitable for display and approximate correlation,
but WaveCraft must not claim phase or sample alignment with another device.

Center frequency belongs to every IQ batch rather than only mutable UI state.
This keeps recordings reproducible and prevents a replay from interpreting old
samples using a later tuning value. Gain, filter, amplifier, antenna, and other
capture settings are saved in session metadata and project settings; settings
that can change during acquisition require a timestamped configuration event.

## Bounded streaming and backpressure

The current coordinator uses an unbounded event channel. That is acceptable
for the existing finite devices but unsafe for sustained IQ rates: a slow UI,
decoder, or disk can otherwise consume all available memory. RF integration
therefore requires bounded queues before enabling a physical HackRF source.

The hardware callback performs only bounded work:

1. obtain a buffer from a fixed pool;
2. copy or transfer ownership of the received bytes;
3. enqueue the buffer without waiting on DSP or the GUI; and
4. return promptly so the USB receive loop remains serviced.

Recording, live DSP, and display are separate consumers with different loss
policies:

- the recorder is lossless while the configured disk path sustains the stream;
- a recorder overrun or device overrun marks a discontinuity and reports a
  capture error instead of inventing contiguous samples;
- spectrum and waterfall consumers may drop intermediate display frames and
  keep the newest bounded state; and
- a live decoder may reset at a reported gap, while offline decoding can be
  rerun from a complete recording.

Blocking the USB callback until the GUI catches up is not valid backpressure.
Queue capacity, dropped display-frame counts, hardware overruns, and decoder
resets must be observable in status and automation output.

## Spectrum and waterfall DSP

The RF display path processes IQ outside the render loop. A worker converts
`Iq8` to normalized complex values, removes optional DC offset, applies the
selected window, performs an FFT, rotates bins into negative-to-positive
frequency order, and converts magnitude to dBFS. It emits a bounded model:

```rust
pub struct SpectrumFrame {
    pub source_range: SampleRange,
    pub center_frequency_hz: u64,
    pub bin_width_hz: f64,
    pub power_dbfs: Vec<f32>,
}
```

The spectrum view retains the newest frame plus optional averaging and peak
hold. The waterfall retains a bounded ring of spectrum rows or an already
color-mapped texture with enough metadata to reconstruct frequency and time at
the pointer. It never retains an unbounded IQ history.

One generic **RF Spectrum** view contains:

- spectrum power in dBFS against absolute frequency;
- a waterfall sharing the same frequency viewport;
- optional compact time-domain I and Q traces;
- center-frequency, span, FFT-size, window, averaging, peak-hold, and dynamic-
  range controls;
- frequency, delta-frequency, and channel-power markers; and
- visible overrun, dropped-row, and decoder-state indicators.

The capture settings panel identifies the selected backend as HackRF and owns
hardware controls such as center frequency, sample rate, bandwidth, LNA/VGA
gain, and RF amplifier. Device controls do not become rendering options.
Retuning clears the waterfall or starts a visibly separate segment because its
rows no longer share one frequency axis.

The initial RF view remains separate from the current Combined view. A future
combined RF/logic/scope view may align panes on coordinator time, but it must
retain the same warning that independent sample clocks are not phase-locked.

## Signal-decoder stack

Radio decoding is an analyzer stack analogous to the existing
logic-to-UART-to-custom-protocol stack:

```text
HackRF IQ
  `-- RF channel: +125 kHz, 250 kHz bandwidth
        `-- GFSK: 100 ksym/s, 50 kHz deviation, BT 0.5
              `-- packet framer: preamble/sync/length/CRC/whitening
                    `-- custom YAML: application fields
```

Each layer declares its required input and typed output. A layer never reaches
back into a hardware driver or reads pixels from the RF view. The analyzer
configuration, including every DSP parameter, is snapshot into the project so
offline results remain reproducible.

### Modulation-neutral analyzer contracts

FSK/GFSK is the first implemented demodulator family, not the architecture's
top-level abstraction. Channel selection, rate conversion, AGC, squelch, AFC,
carrier recovery, and symbol timing are reusable DSP blocks. A selected
demodulator consumes a channelized stream and produces one of two typed output
families:

- a digital demodulator emits timed soft symbols with a symbol alphabet,
  per-symbol likelihoods or confidence, source-sample ranges, and lock
  diagnostics; or
- an analog demodulator emits a bounded real-valued sample stream with its
  sample rate, source-sample mapping, and lock/squelch diagnostics.

Binary FSK and GFSK map their two-symbol soft output into timed bits. Later
OOK/ASK, M-FSK, BPSK/QPSK, and QAM implementations use the same soft-symbol
contract with different alphabets and carrier/timing requirements. AM, FM, and
SSB use the analog-stream contract and do not pass through a pretend bit or
packet layer. Chirp and spread-spectrum schemes may provide a specialized
first-party demodulator while still preserving the same channelized input,
source ranges, bounded state, reset behavior, and diagnostics.

Application-facing configuration uses a tagged modulation profile rather than
placing FSK fields in shared settings. Each profile owns only parameters that
apply to that modulation. The GUI renders the selected profile's controls
dynamically, and recordings/projects persist the tagged profile and component
versions. Adding a modulation must not change the raw IQ, channelizer,
recording, or RF-view contracts.

The implementation boundary is conceptually:

```rust
pub enum DemodulatedBatch {
    SoftSymbols(TimedSoftSymbolBatch),
    Analog(TimedAnalogBatch),
}

pub trait Demodulator {
    fn reset(&mut self);
    fn push(&mut self, input: &ChannelizedBatch)
        -> Result<DemodulatedBatch, DspError>;
}
```

Concrete blocks may use narrower internal types for efficiency. The public
contract must report discontinuities and must never silently coerce analog or
multi-symbol modulation into binary bits.

An in-process registry selects factories by stable IDs such as `fsk2` and
`gfsk2`. Each descriptor declares its output family and alphabet size, while
the factory accepts only its modulation-specific typed configuration. Duplicate
IDs and mismatched configuration types are rejected. Additional digital or
analog factories can register without changing the channelized input or
demodulated output contracts.

### Reusable signal profiles

The implemented in-memory profile builds the channelizer and selected
demodulator as one streaming analyzer. Its tagged FSK and GFSK variants keep
GFSK-only BT out of the FSK profile. The GUI will build this stack from typed
settings. A later reusable `wavecraft.signal/v1` YAML format may describe a
bounded composition of first-party blocks, but it does not contain expressions
or executable DSP code.
It is separate from `wavecraft.protocol`, because RF acquisition and
demodulation parameters answer a different question from the meaning of packet
fields.

An illustrative GFSK profile is:

```yaml
schema: wavecraft.signal/v1

channel:
  offset_hz: 125000
  bandwidth_hz: 250000

demodulation:
  kind: gfsk2
  symbol_rate: 100000
  deviation_hz: 50000
  gaussian_bt: 0.5
  polarity: normal

framing:
  preamble: "10101010"
  minimum_preamble_bits: 32
  sync_word: 0xD391
  sync_bits: 16
  bit_order: lsb_first
  max_packet_bytes: 255
```

This is a configuration snapshot, not a promise that automatic detection has
inferred these values. A project embeds the normalized profile content, its
hash, and component versions using the same source/snapshot rules as custom
protocol YAML. Importing or reloading a signal profile rebuilds the analyzer
plan transactionally and keeps the previous valid plan on error.

Built-in FSK/GFSK blocks are the reference implementation for the first
version. Later in-process modulation blocks and an out-of-process
signal-decoder plugin use the same typed soft-symbol, analog-stream, or packet
output contracts. By default such a plugin receives a
host-channelized, rate-limited IQ stream rather than the entire wideband device
stream; requesting full-rate IQ is an explicit capability with stricter queue,
memory, and timeout limits.

### Channel selection and rate conversion

A decoder should not receive the complete wideband stream when it needs one
narrow signal. The RF channelizer:

1. mixes the configured frequency offset to complex baseband with an NCO;
2. applies a bounded low-pass FIR channel filter;
3. decimates or rationally resamples to a rate appropriate for the symbol
   rate, normally retaining several samples per symbol; and
4. preserves a rational mapping from every output sample to the original IQ
   sample range.

The mapping is essential: carrier regions, bits, packets, and decoded fields
must highlight the correct time range in live and recorded views even after
filter delay and resampling. Filter group delay is accounted for explicitly,
not hidden by shifting results until they merely look plausible.

### FSK and GFSK demodulation

The first demodulator supports binary 2-FSK. After channel filtering it uses a
quadrature frequency discriminator based on the phase difference between
adjacent complex samples:

```text
d[n] = arg(x[n] * conjugate(x[n-1]))
```

The sign and magnitude of `d[n]` represent instantaneous frequency relative
to the selected channel center. A bounded AFC loop may estimate residual
carrier offset from a known preamble or long-term mark/space symmetry, but it
must expose its estimate and reset on a discontinuity.

GFSK uses the same frequency discriminator. Its symbol transitions have been
Gaussian-shaped by the transmitter, so the receive path adds a configurable
Gaussian matched filter before timing recovery and slicing. The profile stores
the expected Gaussian BT product; using the wrong BT remains a diagnostic or
user configuration issue rather than an implicit guess.

Initial demodulator configuration includes:

- channel offset and channel-filter bandwidth;
- symbol rate;
- mark/space deviation or tone separation;
- FSK or GFSK mode and GFSK BT;
- normal or inverted polarity;
- fixed or adaptive decision threshold;
- timing-recovery loop bandwidth; and
- carrier/squelch threshold with minimum burst duration.

The first version requires a configured symbol rate and approximate deviation.
Automatic baud, deviation, and modulation detection can be added later as an
explicit estimation tool; it must not silently change a saved decoder profile.
Four-level FSK is also later work because it requires multi-threshold symbol
decisions and a symbol-to-bit mapping rather than the binary slicer.

### Symbol timing and soft symbols

The discriminator stream passes through matched filtering and clock recovery.
The implemented scalar transition-aided loop interpolates symbol centers and
tracks bounded clock error for binary FSK/GFSK and can be reused by OOK/ASK.
Fixed source-time sampling remains available for deterministic captures. Later
PSK/QAM demodulators can provide a different timing-error detector, such as a
Gardner loop, behind the same timed-symbol contract.

Digital demodulators emit soft symbols rather than immediately packing bytes.
The generic contract retains likelihoods for every member of the configured
alphabet. Binary modulation can expose its compact compatibility view as:

```rust
pub struct TimedSoftBit {
    pub likelihood: f32, // negative = 0, positive = 1
    pub range: SampleRange,
}
```

For this binary view, the sign is the hard decision and magnitude is
confidence. `range` refers to
the contributing samples in the original IQ source domain. Low confidence,
loss of carrier, loop unlock, and input discontinuity are explicit events so a
framer can reject or mark a packet instead of returning convincing bad data.

### Bit transforms and packet framing

Physical radio protocols often transform bits before application bytes exist.
A separate bounded bit pipeline may perform, in a declared order:

- polarity inversion and NRZ/NRZI or Manchester decoding;
- dewhitening with a configured polynomial, width, seed, and bit order;
- preamble and sync-word detection with an explicit Hamming-distance limit;
- fixed, delimiter, or length-field packet framing;
- LSB-first or MSB-first byte packing;
- optional bit stuffing removal; and
- radio CRC validation over an explicitly defined range.

The first implemented transform stage accepts timed hard decisions with
confidence, applies optional polarity inversion, and decodes streaming NRZ,
NRZI, or Manchester line coding. NRZI state survives arbitrary input chunks;
Manchester retains at most one chip and reports invalid or incomplete pairs.
All transform state resets explicitly at a discontinuity.

The implemented radio framer searches for a configured sync word with an
explicit Hamming-distance limit, packs timed bits MSB-first or LSB-first, and
emits fixed-size or bounded length-field packets. Sync search, length fields,
packet storage, and partial state all have explicit limits. Gaps flush partial
packets with diagnostics, and accepted sync errors remain attached to the
packet rather than being hidden.

Radio CRC validation supports explicitly configured 8-, 16-, 24-, or 32-bit
polynomials, initial values, reflection, final XOR, byte order, covered range,
and transmitted field location. A mismatch is attached to the retained packet
with its source range. An optional discard policy is returned separately so
failed bytes remain inspectable instead of being erased by validation.

A single streaming packet pipeline now composes line decoding, sync search,
byte packing, framing, and optional CRC validation. It produces timed packets
and stable diagnostics identically across arbitrary input chunks. This is the
UI/CLI boundary; callers do not coordinate the individual state machines.

These operations are not application-field parsing. Their output is a
`RadioPacket` containing timed bytes, source range, RSSI/power estimate,
frequency-error estimate, confidence, and diagnostics. CRC failure does not
silently discard a packet unless the analyzer is configured to do so; keeping
the failed packet is useful for RF debugging.

Before packet boundaries are configured, a bounded raw-byte mode can acquire
byte alignment from a repeated one-byte preamble. Its configuration contains
the preamble byte, repetition count, and bit order. Output includes the
preamble bytes and then every following byte until a discontinuity or reset;
it intentionally does not infer packet boundaries. The initial GUI exposes
this mode for binary FSK so users can inspect an unknown protocol before
choosing length, delimiter, sync, whitening, or CRC settings.

Packet framing is bounded by maximum preamble search, packet bits, retained
partial bits, and emitted packets per input chunk. A carrier end or IQ gap
flushes incomplete state with a diagnostic.

### Connection to custom YAML

The current `wavecraft.protocol/v1` schema accepts UART, SPI, or I2C timed
bytes. It must not pretend that an RF packet is one of those transports. A
future compatible schema revision generalizes the input capability to timed
packet bytes while preserving all v1 files. The project connects a concrete
packet-framer analyzer instance to the schema; the schema still does not name
physical devices, center frequencies, or DSP parameters.

Conceptually:

```yaml
schema: wavecraft.protocol/v2

input:
  kind: packet_bytes
  stream: data
```

The authoritative schema snapshot, hash, source path, and decoder-engine
versions follow the same reproducibility rules as UART/SPI/I2C custom
protocols. The project also snapshots the RF channelizer, demodulator, and
packet-framer configuration because changing any of them can change every
decoded byte.

## Decoder execution and caching

Live decoders consume ordered IQ independently of the GUI display ring. Offline
decoders consume raw IQ frames from `.dsv`; they never decode waterfall pixels
or FFT display bins. Both modes use the same state machine and produce the same
source ranges and diagnostics for the same input and configuration.

Derived results may be cached, but the cache is disposable and keyed by at
least:

- recording ID and exact IQ segment;
- IQ sample format, sample rate, and center frequency;
- channelizer, filter, resampler, and demodulator versions and settings;
- bit-transform and packet-framer versions and settings; and
- custom protocol schema hash and decoder version, when present.

Changing a key invalidates the affected downstream stages. For example,
changing only the YAML schema may reuse framed packet bytes, while changing
symbol rate invalidates demodulation, bits, packets, and fields.

## Native recording changes

Native recording version 1 has payload tags only for logic and scope frames.
RF support requires a versioned IQ payload containing:

- sample format (`ci8` initially);
- sample rate and center frequency;
- first absolute source-sample index;
- tuning-segment/discontinuity information; and
- the complete interleaved I/Q byte sequence.

The native format should move to version 2. The new reader continues to accept
version 1 unchanged; the writer emits version 2 once IQ support lands. Existing
v1 recordings are never rewritten merely because a newer application opens
them. Spectrum, waterfall, bits, and decoded packets are not required recording
records because they are reproducible derived data.

IQ size is made explicit before capture. The UI estimates bytes per second,
expected file size, and available duration from the selected sample rate. Long
or unlimited capture requires an explicit recording destination and surfaces
disk throughput or free-space failure instead of buffering indefinitely.

## Project and GUI state

RF view state and analyzer configuration belong in the project file, not the
raw recording. A project stores:

- selected recording ID and IQ source device;
- spectrum/waterfall viewport and display settings;
- RF markers and annotations;
- channelizer instances and their source frequency ranges;
- demodulator and packet-framer settings;
- YAML schema snapshots attached to framed packet streams; and
- exact component versions required to reproduce derived results.

The analyzer panel displays the stack and lets users edit, disable, duplicate,
or remove one layer without silently mutating downstream configuration. If a
source layer disappears, dependent analyzers remain visible as unavailable and
identify the missing input.

## Error and diagnostic model

RF capture and decoding report stable machine-readable diagnostics for:

- USB/device overrun and host buffer exhaustion;
- discontinuous sample indices or a retune boundary;
- invalid hardware settings;
- carrier lost, squelch closed, or AFC limit reached;
- symbol timing unlocked or low-confidence bits;
- preamble/sync not found, invalid length, or incomplete packet;
- dewhitening/line-code failure and CRC mismatch; and
- unsupported decoder configuration or schema version.

Device input and malformed recordings never cause a panic. Diagnostics retain
the source sample range when one exists and identify which analyzer layer
emitted them.

## Test strategy

`wavecraft-sim` gains a deterministic IQ source capable of generating:

- one or more complex tones at exact frequency offsets;
- configurable noise, DC offset, frequency error, and amplitude;
- 2-FSK and GFSK bursts with known bits, preamble, sync, whitening, and CRC;
- arbitrary capture-chunk boundaries; and
- explicit gaps, retunes, and overrun/discontinuity events.

Tests cover:

- IQ batch ordering, cancellation, bounded queues, and overflow propagation;
- `.dsv` v2 round trips plus unchanged v1 reading;
- FFT bin frequency, dBFS scaling, windowing, averaging, and waterfall bounds;
- channel-filter rejection and exact source-range mapping after resampling;
- FSK/GFSK decode across arbitrary batch splits and small clock/frequency
  offsets;
- polarity, line coding, whitening, bit order, framing, and CRC variants;
- live/offline result identity; and
- decoder reset at gaps and retune boundaries.

Hardware-dependent HackRF tests are opt-in. The default workspace suite uses
only deterministic simulation and recorded fixtures.

## Implementation sequence

1. Add the core `RadioReceiver`/`IqBatch` contracts and deterministic IQ
   simulator, without a physical backend.
2. Replace the RF path's unbounded fan-out with bounded queues and explicit
   discontinuity/overflow behavior.
3. Add `.dsv` version 2 IQ records and v1/v2 compatibility tests.
4. Implement reusable spectrum DSP and the generic RF Spectrum view against
   simulated and recorded IQ.
5. Add `wavecraft-hackrf` discovery, configuration, cancellation, and receive
   streaming with opt-in hardware tests.
6. Add channelization, modulation-neutral timed-symbol contracts, reusable
   synchronization, and built-in 2-FSK/GFSK decoding.
7. Wire the pluggable-demodulator registry and tagged profiles into the GUI and
   recording path before expanding to additional modulation families.
8. Add bit transforms, packet framing, CRC diagnostics, and packet inspection
   in the GUI/CLI.
9. Generalize the custom-protocol input contract in a new schema version and
   attach YAML parsing above framed RF packet bytes.

This order validates high-rate memory behavior, recording compatibility, and
DSP determinism before hardware availability or application-level packet
schemas can hide lower-layer defects.
