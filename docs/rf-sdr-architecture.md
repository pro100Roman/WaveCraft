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
Analyzer modulation, timing, framing, and whitening settings are shared by a
separate list of up to 100 channel offsets. Each configured offset owns an
independent channelizer, demodulator, synchronization state, and packet/raw
decoder while consuming the same captured IQ stream. The analyzer bank converts
the wideband integer IQ stream and tracks DC once per input chunk, then fans the
prepared samples out to the active per-channel mixers and FIR filters. Its shared
spectrum FFT also gates inactive channels before channelization. Decoded rows
retain and display their originating channel offset.
For binary FSK/GFSK, the panel selects raw discovery or fixed packet decoding.
Both modes edit a repeated hexadecimal preamble and unwhitened sync word.
Packet mode can apply an optional post-sync additive whitening LFSR. The RF
view shows bounded raw-message or completed-packet history with source timing
retained.
New radio sessions default to a 2 MHz capture sample rate, 1.75 MHz HackRF
baseband bandwidth, and a GFSK analyzer profile with a 19.2 kbit/s binary bit
rate and 35 kHz frequency deviation. The analyzer remains opt-in.

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
  implementation.

`wavecraft-decode` owns modulation-independent bitstream transformations,
preamble/sync acquisition, and radio packet framing. `wavecraft-radio` composes
the DSP demodulator output with squelch and raw framing without introducing GUI
or hardware-driver dependencies.

The existing scope FFT can later use the same window and spectrum primitives,
but its migration is not a prerequisite for the first RF slice.

`wavecraft-recording` serializes raw IQ batches and RF capture metadata.
`wavecraft-gui`, the CLI, and automation consume the core, DSP, radio,
recording, and backend APIs without duplicating device or decoder behavior.

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

The implemented spectrum trace exposes the nearest FFT bin under the pointer
with a crosshair and hover readout containing absolute frequency and dBFS
power. This uses the same bin-center mapping as the rendered trace.
The waterfall places each newest spectrum row at the top and moves older
history downward.

The capture settings panel identifies the selected backend as HackRF and owns
hardware controls such as center frequency, sample rate, bandwidth, LNA/VGA
gain, RF amplifier, and signed reference-frequency correction. HackRF
correction is stored in thousandths of a ppm and inversely adjusts both the
programmed RF frequency and sample clock. Positive values compensate a fast
reference while the IQ metadata retains the requested physical center
frequency and sample rate. During continuous Run capture, correction changes
discard queued pre-change IQ, reprogram both clocks, and mark the next batch as
discontinuous so stateful DSP resets. Device controls do not become rendering
options.
Retuning clears the waterfall or starts a visibly separate segment because its
rows no longer share one frequency axis.

The RF view remains visually separate from the logic/scope panes, but a
Combined acquisition includes the selected radio in the same coordinator
session. Operators can switch to the RF Spectrum tab while DSLogic and DSCope
continue running. A future unified three-pane view may align all panes on
coordinator time, but it must retain the warning that independent sample clocks
are not phase-locked.

RF **Single** capture has an explicit duration stored in milliseconds and
displayed in seconds. Without a trigger, the application derives its exact
sample limit from the selected sample rate and duration. With **Wait for
trigger** enabled, the source remains active until the spectrum-power trigger
fires. The triggering FFT window and a configurable 0–1000 ms post-trigger
interval are committed to the display, analyzers, and recording before the view
freezes; the post-trigger value has 0.001 ms resolution and defaults to 2 ms.
Later IQ is discarded.
The live armed spectrum is a preview. **Run** remains continuous.

The spectrum exposes a persisted RF trigger-level threshold in dBFS as a red
`T` marker on the right edge, matching the oscilloscope trigger-level affordance.
Dragging moves the horizontal dotted threshold line; double-click restores
-50 dBFS. The first trigger implementation compares this threshold with the
strongest bin in each 2048-point Hann-windowed spectrum. It is a one-shot RF
Single trigger. After raw FSK/GFSK acquisition locks to its preamble, the same
threshold also acts as a burst squelch over only the configured analyzer channel
bandwidth. Falling below it closes and resets the raw message instead of decoding
the following noise as payload. Channel-integrated power, hysteresis, and
minimum-duration qualification remain future refinements. The RF trigger-level
field and spectrum `T` marker remain editable during continuous **Run**, so the
squelch boundary changes without restarting capture.

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
Dependent numeric editor ranges are enforced on user edits, not passively while
rendering, so a transiently unavailable processing rate cannot rewrite bit rate
or frequency deviation to their minimum values.

Ending a capture or a squelch-delimited burst may leave fewer than eight raw
bits after the last complete byte. This expected tail is discarded silently;
an incomplete byte caused by an input discontinuity remains a visible warning.

The FSK/GFSK raw view groups the synchronized byte stream into messages at
each repeated configured preamble. It retains up to 10,000 messages, renders
the newest completed message first, and distinguishes preamble bytes from data
bytes. Each row reports the decoded data length in bytes, excluding the
configured preamble. Preamble bytes are collapsed by default in each row and
can be expanded independently from its `PRE` control; the row tooltip always
retains the full preamble. An in-progress raw stream is reported as receiving
but is not inserted into the stable message list until the next preamble or
capture stop closes it. The list can be scrolled to older retained messages,
and hovering a row shows its full retained hexadecimal contents. A repeated
preamble may span arbitrary IQ and demodulator batches.

Raw acquisition also has a normalized minimum bit-confidence threshold and a
maximum uncertain-bit count. Confidence is the likelihood margin between the
winning and next-most-likely symbol, so zero is ambiguous and one is decisive.
A preamble whose uncertain-bit count exceeds the configured maximum cannot
acquire synchronization. Once synchronized, every low-confidence bit is counted
in its byte; a completed message above the limit is rejected instead of being
shown as valid RAW data. The view reports the cumulative rejected-message count.
Both settings are stored in recording metadata for reproducible offline decode.
Minimum bit confidence defaults to zero, which disables confidence filtering
until the operator explicitly enables it for a measured signal.

The RF view labels this modulation-neutral output pane **RAW DATA**. Horizontal
drag handles make the RF SPECTRUM, WATERFALL, and RAW DATA heights adjustable.
RAW DATA exposes both wheel scrolling and a draggable vertical scrollbar for
older retained messages. New messages remain pinned to the top while the list
is at its newest position. Once the user scrolls into retained history, incoming
messages preserve the visible rows until the user returns to the top. While
searching for a preamble, it shows a textual status instead of drawing every
soft symbol as a vertical stripe. Parameter names, panel titles, live status
text, and errors use selectable labels so they can be highlighted and copied
with the platform clipboard shortcut.

### Modulation-neutral analyzer contracts

FSK/GFSK is the first implemented demodulator family, not the architecture's
top-level abstraction. Channel selection, rate conversion, AGC, squelch, AFC,
carrier recovery, and symbol timing are reusable DSP blocks. A selected
demodulator consumes a channelized stream and produces one of three typed output
families:

- a digital demodulator emits timed soft symbols with a symbol alphabet,
  per-symbol likelihoods or confidence, source-sample ranges, and lock
  diagnostics;
- a large-alphabet demodulator emits timed sparse symbol decisions,
  frame-boundary events, source-sample ranges, and synchronization diagnostics;
  or
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

The Signal Analyzer modulation selector includes **None**, which disables the
analyzer and hides all analyzer parameters and the **Analyzer channels**
section. Selecting a concrete modulation reveals only its shared and
modulation-specific controls; for example, Gaussian BT is visible only for
GFSK. Legacy saved settings with the former analyzer-enable flag disabled load
as **None**.

The implementation boundary is conceptually:

```rust
pub enum DemodulatedBatch {
    SoftSymbols(TimedSoftSymbolBatch),
    SparseSymbols(TimedSparseSymbolBatch),
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
IDs and mismatched configuration types are rejected. Additional soft-symbol,
sparse-symbol, or analog factories can register without changing the channelized
input contract.

### LoRa and chirp spread-spectrum integration

LoRa follows the same crate layering as FSK/GFSK, but it does not pass through
the generic binary `TimedBitBatch` packet pipeline. A LoRa symbol has `2^SF`
possible values, or as many as 4,096 values at SF12, and LoRa applies
interleaving and forward-error correction before payload bytes exist. Raising
the dense soft-symbol likelihood limit and treating each LoRa symbol as a group
of independent bits would waste memory and discard information needed by the
PHY decoder.

The intended processing stack is:

```text
channelized IQ
  `-- LoRa chirp demodulator (wavecraft-dsp)
        `-- timed sparse LoRa symbols and frame boundaries
              `-- LoRa PHY decoder (wavecraft-decode)
                    `-- RadioPacket
                          `-- LoRaWAN or application decoder
```

`wavecraft-dsp` owns dechirping, FFT-based symbol recovery, preamble and chirp
synchronization, carrier and sampling-offset correction, symbol confidence,
and source-sample mapping. Its LoRa output carries the selected symbol and a
bounded confidence representation, with optional bounded alternate candidates;
it must not allocate a dense 4,096-entry likelihood vector for every SF12
symbol.

`wavecraft-decode` owns LoRa PHY operations after symbol recovery: Gray
decoding, diagonal deinterleaving, Hamming error correction, dewhitening,
explicit or implicit header processing, payload extraction, and PHY CRC
validation. It emits the common `RadioPacket` representation so recording,
display, export, and later application decoding can share the existing packet
path. LoRaWAN interpretation is a layer above the LoRa PHY decoder and does not
belong in the DSP demodulator.

`wavecraft-radio` composes those stages and owns their burst lifecycle. The
runtime dispatches `SoftSymbols` to the existing binary raw/packet decoders and
`SparseSymbols` to a LoRa PHY decoder instead of unconditionally converting all
digital output to `TimedBitBatch`. The data-decoder configuration therefore
gains a LoRa variant alongside the existing raw and generic packet variants.

The shared DSP contract provides bounded `SparseSymbols`, and
`SignalAnalyzer` delegates factory configuration to each tagged modulation
profile instead of assuming every profile becomes an `FskConfig`. The radio
composition boundary rejects sparse symbols explicitly until a compatible PHY
decoder is selected, rather than silently coercing them to binary bits.

Adding a concrete LoRa path still requires the following focused extensions:

- add `DemodulationProfile::LoRa` with spreading factor, bandwidth, and chirp
  synchronization settings owned by that profile;
- register a stable LoRa demodulator ID whose descriptor declares sparse-symbol
  output and whose factory consumes the LoRa-specific configuration;
- make `RadioAnalyzerRuntime` dispatch on the typed demodulator output and add a
  LoRa PHY decoder variant whose configuration owns coding rate, header mode,
  payload bounds, and CRC policy.

This extension does not change `IqBatch`, device or USB backends, wideband IQ
preprocessing, the channelizer, raw-IQ recording, source-range tracking, the
analyzer-bank model, or the generic `RadioPacket` concept. The existing
`RadioPacketPipeline` remains the modulation-independent path for protocols
that naturally produce binary timed decisions; it is not expanded with
LoRa-specific interleaving or FEC behavior.

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

The implementation mixes each batch into flat `f32` buffers and produces every
output sample as one dot product over a contiguous window. Resampling is
fractional: the filter is a Kaiser-windowed sinc evaluated at the exact output
instant, from a bank of rows that sit a fraction of an input sample after the
middle tap. When the reduced output-rate denominator is at most 64 (2 MS/s to
512 kS/s needs 32) every output instant is reproduced exactly; otherwise it
snaps to the nearest 1/64 of an input sample. Snapping each output to the
nearest input sample instead modulated the discriminator output by 26% of a
50 kHz tone at 2 MS/s to 512 kS/s; the bank leaves 0.08%.

The filter length is part of the channel design. `filter_tap_count == 0`
selects the automatic design: a 60 dB Kaiser filter whose transition band is a
quarter of the channel bandwidth, so the length grows in proportion to the
input rate (about 150 taps for a 200 kHz channel at 2 MS/s, 1,450 at 20 MS/s).
A fixed 257-tap filter cut an adjacent channel 150 kHz away by 69 dB at 2 MS/s
but by only 12 dB at 20 MS/s; the automatic design gives 62 to 64 dB at every
rate. An explicit odd length from 3 to 4,095 is still honoured.

Idle channels cost nothing: the radio runtime does not push IQ into a channel
until its squelch opens, so a bank of channels pays only for those with an
active burst. A shared first decimation stage would add cost and coupling for
no gain in that model.

The squelch decides per spectrum window (2,048 samples, about 20 symbols at
19.2 kbit/s), so a burst is noticed in the window in which its energy crosses
the threshold, and its first symbols can lie in the window before. The radio
bank therefore prepares every window, keeps the last two, and starts a channel
that has just opened on those windows joined to the current one
(`PreparedIqBatch::join`, contiguous windows only). The channel filter is also
full when the burst arrives. On a 154 s capture of a 19.2 kbit/s link with a
10 byte preamble, requiring all ten preamble bytes found 133 bursts without the
pre-roll and 166 with it, against 174 when requiring eight. More than two windows of
lead-in noise is harmless only because of the noise gate described under
*Symbol timing and soft symbols*.

### Transmissions that follow each other

The squelch cannot see where a burst ends or that a second one began. It holds
a channel open for three quiet windows (about 3 ms, seven bytes at
19.2 kbit/s) after the carrier, and everything in that time reached the
demodulator and the decoder: a message grew by about five bytes of noise, a
63 byte frame read as 70, and a weak transmission starting 1.3 to 1.6 ms after
a strong one (a repeater answering) was decoded as part of the first, with the
tone and clock settings of the strong one and a damaged second preamble and
sync word.

The filtered channel shows both events as steps in level: the envelope of FSK
and GFSK is constant, so a change of 6 dB or more is a different signal, not
modulation. `LevelStepSegmenter` (`wavecraft-dsp`) compares the mean power of
the three symbols before and after every channel sample and reports the sharpest
step at its exact sample. A step needs the window after it, so the samples are
released three symbols late, unchanged. The radio runtime hands each steady run
to the demodulator separately. At a step it

1. finishes the demodulator (`FskDemodulator::finish`), which continues the
   carrier at its last frequency for half a receive-filter span so that the
   symbols the filter still holds, two for GFSK, are not lost,
2. ends the decoder's message (`finish`, then `reset`), and
3. starts the next run with `discontinuity` set, so a transmission is acquired
   from its own first sample and not from what the noise before it left in the
   tone tracker and clock loop.

A drop is the end of the carrier, so the noise after it is not decoded as part
of the message (it reaches an idle demodulator, which does not acquire noise).
A rise starts a transmission. A signal less than 6 dB above the noise has no
edge that stands out, and is left to the squelch as before.

On a 61 s capture with two channels and 63 messages, the message end was a
median 4.9 bytes (at most 6.9) past the end of the carrier and now is 0.0 (at
most 0.4); transmissions merged into one message fell from 10 to 1. Frames now
have the lengths the transmitter sends: a long frame is 63 bytes after the sync
word in all seven cases (it was 67 to 71, and 101 where two followed each other),
and its retransmissions on the other channel agree on exactly those 63 bytes.
Over the two captures 6 messages were found that were hidden before, and none
was lost.

#### Byte alignment without a sync word

When the raw decoder has no sync word to align to (`sync_word_bytes` 0) it
starts a message at the first window of preamble bytes it finds. The alternating
preamble matches itself shifted by two bits, so the earliest matching window
may start two bits before the real preamble and every byte after it is then
shifted: the sync word `91 d3` shows up as `a4 74`, `a9 1d` or `47 4f` with the
same bits.

The two bits that slide the window are the first two symbols of a burst. They
come out of a receive filter that is not filled yet, and have a low confidence
(0.05 to 0.54 in the captures), while bits inside the burst have a confidence
close to 1. A preamble window therefore must start on confident bits: the first
`PREAMBLE_HEAD_BITS` (2) bits of the window need a confidence of at least
`PREAMBLE_HEAD_CONFIDENCE` (0.6). A window that starts on doubtful bits is
skipped and the search moves on by one bit. The rule applies only to the
preamble-only acquisition, a configured sync word is matched as before, and it
does not drop a message: the window starts two bits later and still holds the
whole gate.

On the two captures the `91 d3` sync word is now byte aligned in 152 of 170
messages (it was 121) and in 64 of 65 (it was 52); the message count is the same
and no message is lost. What is left are weak bursts without a detectable
rise (see above). Their first bits are junk with a confidence close to 1, so the
rule cannot tell them from a real preamble.

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

The channel bandwidth defaults to Auto in the GUI (stored as 0, like the filter
length): Carson's rule `2 * deviation + bit rate` plus 15 kHz on each side for
carrier error, rounded up to 5 kHz, and never more than the processing rate.
That is 120 kHz for 19.2 kbit/s with 35 kHz deviation. Noise power grows with
the bandwidth, so the 200 kHz channel that used to be the default cost
sensitivity: at 14 dB Eb/N0 the bit error rate is 2.8e-3 at 120 kHz and 1.9e-2
at 200 kHz, and the preamble is intact in 59 of 60 bursts against 51. The
margin sets what the narrower channel tolerates: a 120 kHz channel decodes with
a 20 kHz carrier error (8.5e-3) and loses the signal at 30 kHz, whereas 90 kHz,
Carson with no margin, already fails at 20 kHz. A transmitter plus SDR that are
further apart than that need Auto switched off and a wider channel.

The first version requires a configured symbol rate and approximate deviation.
Automatic baud, deviation, and modulation detection can be added later as an
explicit estimation tool; it must not silently change a saved decoder profile.
Four-level FSK is also later work because it requires multi-threshold symbol
decisions and a symbol-to-bit mapping rather than the binary slicer.

For the implemented binary 2-FSK and GFSK profiles, one symbol carries exactly
one raw bit. The GUI therefore presents this setting as **Bit rate** in bit/s,
while the modulation-neutral DSP profile retains the `symbol_rate_hz` name.
Those values are identical for the current binary modes but will differ for a
future M-FSK profile that maps multiple bits to each symbol.

### Symbol timing and soft symbols

The discriminator stream passes through matched filtering and clock recovery.
The implemented scalar transition-aided loop interpolates symbol centers and
tracks bounded clock error for binary FSK/GFSK and can be reused by OOK/ASK.
Fixed source-time sampling remains available for deterministic captures. Later
PSK/QAM demodulators can provide a different timing-error detector, such as a
Gardner loop, behind the same timed-symbol contract.

The matched filter applies in both timing modes: Gaussian with the configured
BT for GFSK, and a one-symbol average for FSK with adaptive timing (fixed
timing already integrates each symbol). GNU Radio's `gfsk_demod` omits it, but
at the SNR of real captures the raw discriminator is dominated by wideband
noise and a decision-directed clock loop then slips symbols. In simulation
with 12 dB Eb/N0 the filter lowers the adaptive bit error rate from about
1e-1 to about 4e-4, which equals decoding with ideal timing. The `radio_fsk_noise`
integration tests keep this guarantee.

Three details keep adaptive decoding robust against the noise that precedes a
burst:

- Tone acquisition judges the *unfiltered* discriminator as well as the
  filtered one. Filtering narrows the spread of pure noise until it resembles
  two tones, so a noise window is rejected when its raw spread exceeds a limit
  in deviations. The filtered samples still provide the tone centres. The limit
  is the smaller of `max(4, 0.4 * sample_rate / deviation)`, for noise that
  fills the whole sample rate, and `max(4, 1.3 * channel_bandwidth / deviation)`,
  because the channel filter keeps noise inside its passband: a 120 kHz channel
  around a 35 kHz deviation signal spreads noise over about 5.5 deviations, not
  the 14.6 of the 512 kS/s sample rate. With the sample-rate limit alone 31% of
  24-symbol noise windows on that link were accepted as tones and the tones
  tracked from them ruined the burst that followed; at 1.3 it is 1 of 600 and
  every burst at 8 dB Eb/N0 is still acquired. The demodulator learns the
  bandwidth from `ChannelizedBatch::bandwidth_hz`.
- The tone centres are committed only after they stay put for a whole
  acquisition window, so a window straddling the burst edge never supplies
  them. Consecutive windows count as the same candidate when their centres
  agree within 20% of the tone separation: they are outer-decile means of a few
  dozen samples and jitter by more than 10% at moderate SNR, and with a 10%
  limit 13% of the bursts on a 100 kbit/s link at 12 dB Eb/N0 were acquired
  late and 3% lost the whole preamble. Samples seen meanwhile are replayed.
- Mueller-and-Muller has no timing error on an ideal alternating preamble, so
  its starting phase would persist and could sit on a symbol boundary. The
  replay therefore starts at the phase with the largest mean eye opening.
  That phase is chosen from the last two acquisition windows, which are known
  to lie inside the burst, and the older samples (which may be the noise the
  burst began in, up to a whole 2,048-sample chunk) are read backwards along
  the same grid rather than fed to the loop. With a 32 to 128 symbol noise
  lead-in on the 100 kbit/s link the preamble stays intact in 80 of 80 bursts
  instead of 53 to 71.

The centred filter needs look-ahead: the last two symbols of a GFSK burst (half
a symbol for FSK) are decided only when later samples arrive. When the burst's
end is found (see *Transmissions that follow each other*), `finish` supplies
them by continuing the carrier; otherwise the quiet channel until the squelch
releases does.
The filter's start is extended backwards so the first preamble symbols are kept,
and the first symbol of a burst is still decided from the first sample when its
centre lies up to 0.3 of a symbol before it. The channel filter outputs its first
sample half a filter length into the stream, which for a 257-tap filter at
2 MS/s is 1.2 symbols, so a burst that starts a few dozen samples after the
analyzer does has its first symbol centred just before the first output.
Dropping that symbol also moved a byte-aligned preamble search to the next `1`
bit, so a 10 byte preamble read as nine `aa` bytes and `a8`.

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

Confidence is `1 - 2 Q(|v| / sd)`: the probability that noise of the burst's
own spread would not have carried a sample of the opposite symbol as far across
the slicer as this one lies from it. `v` is the tone-normalized decision value
at the symbol centre and `sd` the running standard deviation of `|v|` (about 64
symbols of memory, seeded by a moderately noisy prior and floored at 0.05). The
raw magnitude clipped to one meant a different thing on every link: at 12 dB
Eb/N0 a bit at 0.45 was wrong 5.5% of the time and at 9 dB 24% of the time.
With the normalization the error rate per confidence bin agrees between the two
(`[0.7, 0.8)`: 14.7% and 17.4%; `[0.2, 0.3)`: 37% and 41%). Noise is heavier
tailed than Gaussian, so the middle bins are somewhat overconfident (35% errors
where 20 to 25% is predicted at `[0.5, 0.6)`), and bits that are wrong with a
large margin, such as after a symbol slip, still look confident.

The demodulator also reports how open the eye is. `timing_lock_quality` is the
mean decision magnitude less two standard deviations, clamped to `[0, 1]` and
zero until 16 symbols were decided: about 0.7 on a clean GFSK link (intersymbol
interference keeps it below one), 0.4 at 12 dB, 0.1 to 0.2 at 9 dB and zero at
6 dB, tracking the measured error rate. The earlier figure, built from the loop
error alone, did not (0.49 for a burst at 0.8% errors and 0.81 for one at
2.8%). `timing_locked` requires the clock loop to have converged, 16 symbols,
and an eye opening of at least 0.1; the former `output_count >= 4` test passed
for pure noise. `decision_snr_db` is the squared mean over the variance of the
decision magnitudes; intersymbol interference caps it near 22 dB.

### Bit transforms and packet framing

Physical radio protocols often transform bits before application bytes exist.
A separate bounded bit pipeline performs protocol-declared transforms. The
default packet receive order is:

- polarity inversion and NRZ/NRZI or Manchester decoding;
- preamble and sync-word detection with an explicit Hamming-distance limit;
- dewhitening of the configured post-sync packet-bit range;
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
explicit Hamming-distance limit. Once sync is accepted, it resets an optional
additive-whitening LFSR and XORs its PN sequence with the configured packet-bit
range before interpreting a length field or packing bytes. The configuration
declares the LFSR width, tap mask, seed, shift direction, output inversion,
covered range, and whether excluded bits advance the register. Dewhitening
changes only bit values; confidence and source ranges remain attached. The
framer then packs timed bits MSB-first or LSB-first and emits fixed-size or
bounded length-field packets. Sync search, length fields, packet storage, and
partial state all have explicit limits. Gaps flush partial packets with
diagnostics, and accepted sync errors remain attached to the packet rather than
being hidden.

Radio CRC validation supports explicitly configured 8-, 16-, 24-, or 32-bit
polynomials, initial values, reflection, final XOR, byte order, covered range,
and transmitted field location. A mismatch is attached to the retained packet
with its source range. An optional discard policy is returned separately so
failed bytes remain inspectable instead of being erased by validation.

A single streaming packet pipeline now composes line decoding, sync search,
optional post-sync dewhitening, byte packing, framing, and optional CRC
validation. It produces timed packets and stable diagnostics identically across
arbitrary input chunks. This is the UI/CLI boundary; callers do not coordinate
the individual state machines.

In fixed-decoding mode the same view is labeled **PACKETS**. Until configurable
length-field framing is exposed, it collects post-sync bytes through the end of
the stream or an input discontinuity instead of assuming a constant packet
length. Acquisition requires the configured preamble immediately before sync,
and PACKETS rows retain that preamble separately from dewhitened data. A final
sub-byte tail is ignored rather than discarding complete bytes. Signal Analyzer
settings expose whitening enablement, LFSR width, tap mask, seed, shift
direction, output inversion, covered data-bit range, and excluded-bit
advancement. These settings are saved in capture profiles and recording
metadata so replay uses the same packet transform.
Whitening can use a **Custom** preset that exposes those individual fields or
the fixed **PN9** preset used by standard SX127x FSK packet whitening. PN9 uses
the x⁹ + x⁵ + 1 polynomial and resets to seed `0x1FF` after each sync word.
Named presets hide the manual LFSR fields while retaining custom values for
later reuse.
When a modulation is selected, the **Analyzer channels** section follows the
common **Signal analyzer** section and provides editable names plus add,
remove, and offset controls.
Capture profiles and recording metadata retain the bounded ordered channel
list and its names; legacy recordings without names receive numbered defaults,
and a legacy `channel_offset_hz` loads as a one-channel list. RAW DATA and
PACKETS rows show the configured channel name and an unprefixed byte count.

These operations are not application-field parsing. Their output is a
`RadioPacket` containing timed bytes, source range, RSSI/power estimate,
frequency-error estimate, confidence, and diagnostics. CRC failure does not
silently discard a packet unless the analyzer is configured to do so; keeping
the failed packet is useful for RF debugging.

Before packet boundaries are configured, a bounded raw-byte mode can acquire
byte alignment from a repeated one-byte preamble. Its configuration contains
the preamble byte, repetition count, and bit order. Output includes the
preamble bytes and then every following byte until the next configured
preamble, discontinuity, or reset. Repeated preambles provide provisional
message boundaries without assuming a length field, delimiter, sync word,
whitening, or CRC. The GUI exposes this mode so users can inspect an unknown
protocol before switching to the fixed-length packet framer and optional
dewhitening.

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

For long-running monitoring where retaining wideband IQ is unnecessary, the RF
GUI can instead stream completed RAW messages or accepted PACKETS to a
newline-delimited JSON file. This derived-data export is intentionally separate
from native `.dsv`: it records channel identity, source-sample ranges, preamble,
and decoded bytes, plus packet confidence when available, but cannot reproduce a
different demodulator configuration. Both native IQ recording and decoded-data
recording can attach to an active Run session and are finalized when the
operator stops acquisition.

Decoded-data JSON Lines files can be reopened from the existing Open action
when RF is the current view. The loader restores session/channel metadata and
streams the retained records into the bounded RAW DATA or PACKETS history; it
does not synthesize IQ, spectrum, or waterfall content.

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
