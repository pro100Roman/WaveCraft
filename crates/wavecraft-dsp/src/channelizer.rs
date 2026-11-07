use crate::DspError;
use std::f64::consts::{PI, TAU};
use std::ops::Range;
use wavecraft_core::IqBatch;

const DC_TRACKING_GAIN: f64 = 1.0 / 16_384.0;
const MIXER_RENORMALIZE_INTERVAL: u32 = 4_096;

/// Longest channel filter the channelizer builds or accepts.
pub const MAX_FILTER_TAP_COUNT: usize = 4_095;
const MIN_AUTOMATIC_TAP_COUNT: usize = 15;
/// Stopband attenuation the Kaiser window is shaped for.
const STOPBAND_ATTENUATION_DB: f64 = 60.0;
/// Width of the automatic design's transition band, centred on the channel
/// edge, as a fraction of the channel bandwidth.
const TRANSITION_BANDWIDTH_FRACTION: f64 = 0.25;
/// Most fractional-delay filters kept for one channel. Ratios whose reduced
/// denominator is at most this are resampled exactly; anything else snaps the
/// output instant to the nearest 1/64 of an input sample.
const MAX_FRACTIONAL_PHASES: u64 = 64;
/// Independent accumulators in the FIR dot product. Floating-point addition is
/// not associative, so the compiler only vectorizes the reduction when the
/// lanes are explicit.
const DOT_LANES: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelizerConfig {
    /// Signed channel offset from the input batch center frequency.
    pub frequency_offset_hz: i64,
    /// Full complex channel bandwidth (the -6 dB width of the channel filter).
    pub bandwidth_hz: u64,
    /// Rate after bounded rational downsampling. Upsampling is not supported.
    pub output_sample_rate_hz: u64,
    /// Odd Kaiser-windowed-sinc FIR length, or 0 to size the filter from the
    /// input sample rate and the channel bandwidth (see
    /// [`automatic_filter_tap_count`]).
    pub filter_tap_count: usize,
}

impl ChannelizerConfig {
    pub fn validate(&self) -> Result<(), DspError> {
        if self.bandwidth_hz == 0 {
            return Err(DspError::new("radio channel bandwidth must be nonzero"));
        }
        if self.output_sample_rate_hz == 0 {
            return Err(DspError::new(
                "radio channel output sample rate must be nonzero",
            ));
        }
        if self.bandwidth_hz > self.output_sample_rate_hz {
            return Err(DspError::new(
                "radio channel bandwidth must not exceed its output sample rate",
            ));
        }
        if self.filter_tap_count != 0
            && (self.filter_tap_count < 3
                || self.filter_tap_count % 2 == 0
                || self.filter_tap_count > MAX_FILTER_TAP_COUNT)
        {
            return Err(DspError::new(
                "radio channel filter must have an odd tap count between 3 and 4095, or 0 for automatic",
            ));
        }
        Ok(())
    }

    /// FIR length used for an input stream at `input_sample_rate_hz`: the
    /// configured length, or the automatic design when it is 0.
    pub fn resolved_filter_tap_count(&self, input_sample_rate_hz: u64) -> usize {
        if self.filter_tap_count == 0 {
            automatic_filter_tap_count(input_sample_rate_hz, self.bandwidth_hz)
        } else {
            self.filter_tap_count
        }
    }
}

/// Odd FIR length whose Kaiser-windowed-sinc low pass has a transition band
/// of a quarter of `bandwidth_hz` (centred on the channel edge) and 60 dB of
/// stopband attenuation at `input_sample_rate_hz`.
///
/// The length grows in proportion to the input rate: a fixed 257-tap filter
/// that cuts a 200 kHz channel cleanly out of a 2 MS/s capture leaves an
/// adjacent channel only about 29 dB down at 20 MS/s. The result is clamped to
/// [`MAX_FILTER_TAP_COUNT`], so very wide captures of narrow channels get a
/// wider transition band instead of a longer filter.
pub fn automatic_filter_tap_count(input_sample_rate_hz: u64, bandwidth_hz: u64) -> usize {
    let transition_hz = (bandwidth_hz as f64 * TRANSITION_BANDWIDTH_FRACTION).max(1.0);
    let normalized_width = TAU * transition_hz / input_sample_rate_hz.max(1) as f64;
    // Kaiser's estimate: N = (A - 7.95) / (2.285 * delta_omega) + 1.
    let estimate = (STOPBAND_ATTENUATION_DB - 7.95) / (2.285 * normalized_width) + 1.0;
    let taps = if estimate.is_finite() {
        estimate.ceil().min(MAX_FILTER_TAP_COUNT as f64) as usize
    } else {
        MAX_FILTER_TAP_COUNT
    };
    (taps | 1).clamp(MIN_AUTOMATIC_TAP_COUNT, MAX_FILTER_TAP_COUNT)
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedComplexSample {
    pub i: f32,
    pub q: f32,
    /// Half-open source IQ range that contributed to this filtered sample.
    pub source_range: Range<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelizedBatch {
    pub source_sample_rate_hz: u64,
    pub sample_rate_hz: u64,
    /// Full complex bandwidth of the channel filter that produced the samples.
    pub bandwidth_hz: u64,
    pub center_frequency_hz: u64,
    /// First input sample index after the latest gap, retune, or rate change.
    pub segment_start_sample_index: u64,
    /// FIR latency in input-sample units.
    pub filter_delay_input_samples: u64,
    pub discontinuity: bool,
    pub samples: Vec<TimedComplexSample>,
}

impl ChannelizedBatch {
    /// The stream description without the samples or the gap flag.
    pub(crate) fn clone_header(&self) -> ChannelizedBatch {
        ChannelizedBatch {
            discontinuity: false,
            samples: Vec::new(),
            ..*self
        }
    }
}

/// Wideband IQ with integer conversion and DC tracking applied once before
/// fan-out to one or more channelizers.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedIqBatch {
    pub sample_rate_hz: u64,
    pub center_frequency_hz: u64,
    pub first_sample_index: u64,
    pub discontinuity: bool,
    samples: Vec<(f64, f64)>,
}

impl PreparedIqBatch {
    /// Number of IQ samples in the batch.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Joins the trailing run of `batches` that follow each other without a
    /// gap, retune or rate change into one batch, or `None` if there are none.
    ///
    /// A channelizer started on the joined batch sees the earlier samples as
    /// the beginning of its stream, which is how a burst detected late keeps
    /// its first symbols.
    pub fn join(batches: &[&PreparedIqBatch]) -> Option<PreparedIqBatch> {
        let last = batches.last()?;
        let mut first = batches.len() - 1;
        while first > 0 {
            let (previous, next) = (batches[first - 1], batches[first]);
            let contiguous = !next.discontinuity
                && previous.sample_rate_hz == next.sample_rate_hz
                && previous.center_frequency_hz == next.center_frequency_hz
                && previous
                    .first_sample_index
                    .saturating_add(previous.samples.len() as u64)
                    == next.first_sample_index;
            if !contiguous {
                break;
            }
            first -= 1;
        }
        let run = &batches[first..];
        let mut samples = Vec::with_capacity(run.iter().map(|batch| batch.samples.len()).sum());
        for batch in run {
            samples.extend_from_slice(&batch.samples);
        }
        Some(PreparedIqBatch {
            sample_rate_hz: last.sample_rate_hz,
            center_frequency_hz: last.center_frequency_hz,
            first_sample_index: run[0].first_sample_index,
            discontinuity: run[0].discontinuity,
            samples,
        })
    }
}

/// Stateful preprocessing shared by a bank of channelizers.
#[derive(Default)]
pub struct WidebandIqPreprocessor {
    expected_next_index: Option<u64>,
    sample_rate_hz: Option<u64>,
    center_frequency_hz: Option<u64>,
    dc_i: f64,
    dc_q: f64,
    dc_initialized: bool,
    scratch_samples: Vec<(f64, f64)>,
}

impl WidebandIqPreprocessor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        let scratch_samples = std::mem::take(&mut self.scratch_samples);
        *self = Self {
            scratch_samples,
            ..Self::default()
        };
    }

    pub fn push(&mut self, batch: &IqBatch) -> Result<PreparedIqBatch, DspError> {
        if batch.sample_rate_hz == 0 {
            return Err(DspError::new("IQ sample rate must be nonzero"));
        }
        let discontinuity = batch.discontinuity
            || self.expected_next_index != Some(batch.first_sample_index)
            || self.sample_rate_hz != Some(batch.sample_rate_hz)
            || self.center_frequency_hz != Some(batch.center_frequency_hz);
        if discontinuity {
            self.dc_i = 0.0;
            self.dc_q = 0.0;
            self.dc_initialized = false;
        }

        let mut samples = std::mem::take(&mut self.scratch_samples);
        samples.clear();
        samples.reserve(batch.samples.len().saturating_sub(samples.capacity()));
        samples.extend(batch.samples.iter().map(|sample| {
            let raw = (f64::from(sample.i) / 128.0, f64::from(sample.q) / 128.0);
            if !self.dc_initialized {
                self.dc_initialized = true;
            } else {
                self.dc_i += DC_TRACKING_GAIN * (raw.0 - self.dc_i);
                self.dc_q += DC_TRACKING_GAIN * (raw.1 - self.dc_q);
            }
            (raw.0 - self.dc_i, raw.1 - self.dc_q)
        }));

        self.expected_next_index = Some(
            batch
                .first_sample_index
                .saturating_add(batch.samples.len() as u64),
        );
        self.sample_rate_hz = Some(batch.sample_rate_hz);
        self.center_frequency_hz = Some(batch.center_frequency_hz);
        Ok(PreparedIqBatch {
            sample_rate_hz: batch.sample_rate_hz,
            center_frequency_hz: batch.center_frequency_hz,
            first_sample_index: batch.first_sample_index,
            discontinuity,
            samples,
        })
    }

    /// Returns a prepared batch's allocation for reuse by the next input.
    pub fn recycle(&mut self, mut batch: PreparedIqBatch) {
        batch.samples.clear();
        self.scratch_samples = batch.samples;
    }
}

/// Low-pass filter evaluated at a set of fractional output instants.
///
/// Row `p` holds the Kaiser-windowed sinc centred `p / phase_count` of an
/// input sample after the middle tap, so an output can be produced at any
/// rational point of the input timeline instead of being snapped to the
/// nearest input sample. With a rational ratio `P/Q` the fractional offsets are
/// multiples of `1/Q`, which the bank reproduces exactly while `Q` stays within
/// [`MAX_FRACTIONAL_PHASES`].
struct FilterBank {
    tap_count: usize,
    phase_count: usize,
    /// Output-rate units per phase when the bank is exact.
    exact_phase_step: Option<u64>,
    output_rate_hz: u64,
    taps: Vec<f32>,
}

impl FilterBank {
    fn design(
        input_rate_hz: u64,
        output_rate_hz: u64,
        bandwidth_hz: u64,
        tap_count: usize,
    ) -> Self {
        let divisor = gcd(input_rate_hz, output_rate_hz);
        let denominator = output_rate_hz / divisor;
        let exact = denominator <= MAX_FRACTIONAL_PHASES;
        let phase_count = denominator.min(MAX_FRACTIONAL_PHASES) as usize;
        let cutoff = bandwidth_hz as f64 / (2.0 * input_rate_hz as f64);
        let mut taps = Vec::with_capacity(phase_count * tap_count);
        for phase in 0..phase_count {
            let delay = phase as f64 / phase_count as f64;
            taps.extend(design_low_pass_phase(cutoff, tap_count, delay));
        }
        Self {
            tap_count,
            phase_count,
            exact_phase_step: exact.then_some(divisor),
            output_rate_hz,
            taps,
        }
    }

    /// Filter row for an output instant `remainder / output_rate` of an input
    /// sample after its integer position, plus the number of whole input
    /// samples the window moves when rounding reaches the next sample.
    fn locate(&self, remainder: u64) -> (usize, u64) {
        if let Some(step) = self.exact_phase_step {
            return ((remainder / step) as usize, 0);
        }
        let phases = self.phase_count as u64;
        let row = (remainder * phases + self.output_rate_hz / 2) / self.output_rate_hz;
        if row == phases {
            (0, 1)
        } else {
            (row as usize, 0)
        }
    }

    fn row(&self, row: usize) -> &[f32] {
        &self.taps[row * self.tap_count..(row + 1) * self.tap_count]
    }
}

/// Incremental frequency translator, low-pass filter, and rate reducer.
///
/// The input is mixed to baseband into a flat buffer, and every output is one
/// dot product over a contiguous window of it, so the work per output is
/// `tap_count` multiply-adds regardless of how the input was batched.
pub struct Channelizer {
    config: ChannelizerConfig,
    preprocessor: WidebandIqPreprocessor,
    bank: Option<FilterBank>,
    bank_input_rate_hz: Option<u64>,
    mixed_i: Vec<f32>,
    mixed_q: Vec<f32>,
    /// Source index of `mixed_i[0]`.
    buffer_start_index: u64,
    /// Source index of the input sample the next output is centred on, before
    /// its fractional offset.
    next_centre_index: u64,
    /// Fractional offset of the next output, in output-rate units.
    next_fraction: u64,
    expected_next_index: Option<u64>,
    input_sample_rate_hz: Option<u64>,
    input_center_frequency_hz: Option<u64>,
    segment_start_sample_index: Option<u64>,
    mixer_i: f64,
    mixer_q: f64,
    mixer_step_i: f64,
    mixer_step_q: f64,
    mixer_samples_since_normalize: u32,
    pending_discontinuity: bool,
}

impl Channelizer {
    pub fn new(config: ChannelizerConfig) -> Result<Self, DspError> {
        config.validate()?;
        Ok(Self {
            config,
            preprocessor: WidebandIqPreprocessor::new(),
            bank: None,
            bank_input_rate_hz: None,
            mixed_i: Vec::new(),
            mixed_q: Vec::new(),
            buffer_start_index: 0,
            next_centre_index: 0,
            next_fraction: 0,
            expected_next_index: None,
            input_sample_rate_hz: None,
            input_center_frequency_hz: None,
            segment_start_sample_index: None,
            mixer_i: 1.0,
            mixer_q: 0.0,
            mixer_step_i: 1.0,
            mixer_step_q: 0.0,
            mixer_samples_since_normalize: 0,
            pending_discontinuity: false,
        })
    }

    pub fn config(&self) -> &ChannelizerConfig {
        &self.config
    }

    pub fn reset(&mut self) {
        self.preprocessor.reset();
        self.bank = None;
        self.bank_input_rate_hz = None;
        self.mixed_i.clear();
        self.mixed_q.clear();
        self.buffer_start_index = 0;
        self.next_centre_index = 0;
        self.next_fraction = 0;
        self.expected_next_index = None;
        self.input_sample_rate_hz = None;
        self.input_center_frequency_hz = None;
        self.segment_start_sample_index = None;
        self.mixer_i = 1.0;
        self.mixer_q = 0.0;
        self.mixer_step_i = 1.0;
        self.mixer_step_q = 0.0;
        self.mixer_samples_since_normalize = 0;
        self.pending_discontinuity = true;
    }

    pub fn push(&mut self, batch: &IqBatch) -> Result<ChannelizedBatch, DspError> {
        let prepared = self.preprocessor.push(batch)?;
        let output = self.push_prepared(&prepared);
        self.preprocessor.recycle(prepared);
        output
    }

    pub fn push_prepared(&mut self, batch: &PreparedIqBatch) -> Result<ChannelizedBatch, DspError> {
        self.validate_batch(batch)?;
        let reset = batch.discontinuity
            || self.expected_next_index != Some(batch.first_sample_index)
            || self.input_sample_rate_hz != Some(batch.sample_rate_hz)
            || self.input_center_frequency_hz != Some(batch.center_frequency_hz);
        if reset {
            self.reset_for_batch(batch);
        }

        let selected_frequency =
            selected_frequency(batch.center_frequency_hz, self.config.frequency_offset_hz)?;
        self.mix(&batch.samples);
        let (output, filter_delay_input_samples) = self.filter(batch.sample_rate_hz);

        self.expected_next_index = Some(
            batch
                .first_sample_index
                .saturating_add(batch.samples.len() as u64),
        );
        let discontinuity = self.pending_discontinuity;
        if !output.is_empty() {
            self.pending_discontinuity = false;
        }
        Ok(ChannelizedBatch {
            source_sample_rate_hz: batch.sample_rate_hz,
            sample_rate_hz: self.config.output_sample_rate_hz,
            bandwidth_hz: self.config.bandwidth_hz,
            center_frequency_hz: selected_frequency,
            segment_start_sample_index: self
                .segment_start_sample_index
                .unwrap_or(batch.first_sample_index),
            filter_delay_input_samples,
            discontinuity,
            samples: output,
        })
    }

    fn validate_batch(&self, batch: &PreparedIqBatch) -> Result<(), DspError> {
        if self.config.output_sample_rate_hz > batch.sample_rate_hz {
            return Err(DspError::new(
                "radio channel output rate must not exceed the IQ sample rate",
            ));
        }
        let occupied_edge = i128::from(self.config.frequency_offset_hz.unsigned_abs())
            + i128::from(self.config.bandwidth_hz.div_ceil(2));
        if occupied_edge.saturating_mul(2) > i128::from(batch.sample_rate_hz) {
            return Err(DspError::new(
                "radio channel extends outside the input Nyquist range",
            ));
        }
        selected_frequency(batch.center_frequency_hz, self.config.frequency_offset_hz)?;
        Ok(())
    }

    fn reset_for_batch(&mut self, batch: &PreparedIqBatch) {
        if self.bank_input_rate_hz != Some(batch.sample_rate_hz) {
            self.bank = Some(FilterBank::design(
                batch.sample_rate_hz,
                self.config.output_sample_rate_hz,
                self.config.bandwidth_hz,
                self.config.resolved_filter_tap_count(batch.sample_rate_hz),
            ));
            self.bank_input_rate_hz = Some(batch.sample_rate_hz);
        }
        let half = self.bank.as_ref().map_or(0, |bank| bank.tap_count / 2) as u64;
        self.mixed_i.clear();
        self.mixed_q.clear();
        self.buffer_start_index = batch.first_sample_index;
        // The first output is centred on the middle of the first full window.
        self.next_centre_index = batch.first_sample_index.saturating_add(half);
        self.next_fraction = 0;
        self.expected_next_index = None;
        self.input_sample_rate_hz = Some(batch.sample_rate_hz);
        self.input_center_frequency_hz = Some(batch.center_frequency_hz);
        self.segment_start_sample_index = Some(batch.first_sample_index);
        let mixer_step =
            -TAU * self.config.frequency_offset_hz as f64 / batch.sample_rate_hz as f64;
        self.mixer_step_i = mixer_step.cos();
        self.mixer_step_q = mixer_step.sin();
        let reduced_index = batch.first_sample_index % batch.sample_rate_hz;
        let mixer_phase = wrap_phase(mixer_step * reduced_index as f64);
        self.mixer_i = mixer_phase.cos();
        self.mixer_q = mixer_phase.sin();
        self.mixer_samples_since_normalize = 0;
        self.pending_discontinuity = true;
    }

    /// Translates `samples` to baseband and appends them to the filter buffer.
    fn mix(&mut self, samples: &[(f64, f64)]) {
        let old_length = self.mixed_i.len();
        self.mixed_i.resize(old_length + samples.len(), 0.0);
        self.mixed_q.resize(old_length + samples.len(), 0.0);
        let mut destination_i = &mut self.mixed_i[old_length..];
        let mut destination_q = &mut self.mixed_q[old_length..];
        let (mut mixer_i, mut mixer_q) = (self.mixer_i, self.mixer_q);
        let (step_i, step_q) = (self.mixer_step_i, self.mixer_step_q);
        let mut remaining = samples;
        while !remaining.is_empty() {
            let until_normalize =
                (MIXER_RENORMALIZE_INTERVAL - self.mixer_samples_since_normalize) as usize;
            let block = remaining.len().min(until_normalize);
            let (input, rest) = remaining.split_at(block);
            let (block_i, rest_i) = destination_i.split_at_mut(block);
            let (block_q, rest_q) = destination_q.split_at_mut(block);
            for ((&(x, y), out_i), out_q) in
                input.iter().zip(block_i.iter_mut()).zip(block_q.iter_mut())
            {
                *out_i = (x * mixer_i - y * mixer_q) as f32;
                *out_q = (x * mixer_q + y * mixer_i) as f32;
                let next_i = mixer_i * step_i - mixer_q * step_q;
                let next_q = mixer_i * step_q + mixer_q * step_i;
                mixer_i = next_i;
                mixer_q = next_q;
            }
            self.mixer_samples_since_normalize += block as u32;
            if self.mixer_samples_since_normalize == MIXER_RENORMALIZE_INTERVAL {
                let magnitude = mixer_i.hypot(mixer_q);
                if magnitude > f64::EPSILON {
                    mixer_i /= magnitude;
                    mixer_q /= magnitude;
                }
                self.mixer_samples_since_normalize = 0;
            }
            remaining = rest;
            destination_i = rest_i;
            destination_q = rest_q;
        }
        self.mixer_i = mixer_i;
        self.mixer_q = mixer_q;
    }

    /// Produces every output whose window is fully buffered, then drops the
    /// samples no later window needs.
    fn filter(&mut self, input_rate_hz: u64) -> (Vec<TimedComplexSample>, u64) {
        let Some(bank) = self.bank.as_ref() else {
            return (Vec::new(), 0);
        };
        let tap_count = bank.tap_count;
        let half = (tap_count / 2) as u64;
        let output_rate_hz = self.config.output_sample_rate_hz;
        let whole_step = input_rate_hz / output_rate_hz;
        let fraction_step = input_rate_hz % output_rate_hz;
        let buffer_end = self.buffer_start_index + self.mixed_i.len() as u64;

        let available = buffer_end.saturating_sub(self.next_centre_index.saturating_sub(half));
        let mut output =
            Vec::with_capacity((available / whole_step.max(1)).saturating_add(1) as usize);
        loop {
            let (row, carry) = bank.locate(self.next_fraction);
            let start = self.next_centre_index - half + carry;
            let end = start + tap_count as u64;
            if end > buffer_end {
                break;
            }
            let offset = (start - self.buffer_start_index) as usize;
            let (i, q) = dot_pair(
                &self.mixed_i[offset..offset + tap_count],
                &self.mixed_q[offset..offset + tap_count],
                bank.row(row),
            );
            output.push(TimedComplexSample {
                i,
                q,
                source_range: start..end,
            });

            self.next_centre_index += whole_step;
            self.next_fraction += fraction_step;
            if self.next_fraction >= output_rate_hz {
                self.next_fraction -= output_rate_hz;
                self.next_centre_index += 1;
            }
        }

        let keep_from = self.next_centre_index - half;
        let consumed = keep_from
            .saturating_sub(self.buffer_start_index)
            .min(self.mixed_i.len() as u64) as usize;
        self.mixed_i.drain(..consumed);
        self.mixed_q.drain(..consumed);
        self.buffer_start_index += consumed as u64;
        (output, half)
    }
}

/// Inner products of two sample windows with one set of taps.
fn dot_pair(window_i: &[f32], window_q: &[f32], taps: &[f32]) -> (f32, f32) {
    let mut accumulator_i = [0.0_f32; DOT_LANES];
    let mut accumulator_q = [0.0_f32; DOT_LANES];
    let mut chunks_i = window_i.chunks_exact(DOT_LANES);
    let mut chunks_q = window_q.chunks_exact(DOT_LANES);
    let mut chunks_taps = taps.chunks_exact(DOT_LANES);
    for ((chunk_i, chunk_q), chunk_taps) in chunks_i
        .by_ref()
        .zip(chunks_q.by_ref())
        .zip(chunks_taps.by_ref())
    {
        for lane in 0..DOT_LANES {
            accumulator_i[lane] += chunk_i[lane] * chunk_taps[lane];
            accumulator_q[lane] += chunk_q[lane] * chunk_taps[lane];
        }
    }
    let mut sum_i = accumulator_i.iter().sum::<f32>();
    let mut sum_q = accumulator_q.iter().sum::<f32>();
    for ((&i, &q), &tap) in chunks_i
        .remainder()
        .iter()
        .zip(chunks_q.remainder())
        .zip(chunks_taps.remainder())
    {
        sum_i += i * tap;
        sum_q += q * tap;
    }
    (sum_i, sum_q)
}

fn selected_frequency(center_frequency_hz: u64, offset_hz: i64) -> Result<u64, DspError> {
    let frequency = i128::from(center_frequency_hz) + i128::from(offset_hz);
    u64::try_from(frequency)
        .map_err(|_| DspError::new("radio channel center frequency is outside the valid range"))
}

fn wrap_phase(phase: f64) -> f64 {
    (phase + PI).rem_euclid(TAU) - PI
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left.max(1)
}

/// Zeroth-order modified Bessel function of the first kind.
fn bessel_i0(x: f64) -> f64 {
    let quarter_square = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..80 {
        term *= quarter_square / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Kaiser-windowed sinc, sampled so its centre sits `delay` input samples
/// after the middle tap, normalized to unit gain at DC.
///
/// `cutoff` is the -6 dB frequency in cycles per input sample.
fn design_low_pass_phase(cutoff: f64, tap_count: usize, delay: f64) -> Vec<f32> {
    let beta = 0.1102 * (STOPBAND_ATTENUATION_DB - 8.7);
    let window_norm = bessel_i0(beta);
    let middle = (tap_count / 2) as f64;
    // One sample wider than the filter so a delayed window still reaches zero
    // inside the last tap.
    let half_width = middle + 1.0;
    let mut taps = (0..tap_count)
        .map(|index| {
            let position = index as f64 - middle - delay;
            let argument = 2.0 * cutoff * position;
            let sinc = if argument.abs() < 1e-12 {
                1.0
            } else {
                (PI * argument).sin() / (PI * argument)
            };
            let ratio = position / half_width;
            let window = bessel_i0(beta * (1.0 - ratio * ratio).max(0.0).sqrt()) / window_norm;
            2.0 * cutoff * sinc * window
        })
        .collect::<Vec<_>>();
    let sum = taps.iter().sum::<f64>();
    for tap in &mut taps {
        *tap /= sum;
    }
    taps.into_iter().map(|tap| tap as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;
    use wavecraft_core::Iq8;

    #[test]
    fn joins_the_trailing_contiguous_prepared_batches_into_one() {
        let mut preprocessor = WidebandIqPreprocessor::new();
        let first = preprocessor.push(&tone_batch(0, 4, true)).unwrap();
        let second = preprocessor.push(&tone_batch(4, 4, false)).unwrap();
        let after_gap = preprocessor.push(&tone_batch(100, 4, false)).unwrap();
        let last = preprocessor.push(&tone_batch(104, 4, false)).unwrap();
        assert!(!second.discontinuity);
        assert!(after_gap.discontinuity);

        let joined = PreparedIqBatch::join(&[&first, &second]).unwrap();
        assert_eq!(joined.len(), 8);
        assert_eq!(joined.first_sample_index, 0);
        assert!(joined.discontinuity);

        // Batches before a gap do not belong to the same stream.
        let joined = PreparedIqBatch::join(&[&first, &second, &after_gap, &last]).unwrap();
        assert_eq!(joined.len(), 8);
        assert_eq!(joined.first_sample_index, 100);
        assert!(joined.discontinuity);

        assert_eq!(PreparedIqBatch::join(&[&last]).unwrap().len(), 4);
        assert!(PreparedIqBatch::join(&[]).is_none());
    }

    fn tone_batch(first_sample_index: u64, count: usize, discontinuity: bool) -> IqBatch {
        tone_batch_at(first_sample_index, count, discontinuity, 1_000.0)
    }

    fn tone_batch_at(
        first_sample_index: u64,
        count: usize,
        discontinuity: bool,
        frequency_hz: f64,
    ) -> IqBatch {
        IqBatch {
            sample_rate_hz: 8_000,
            center_frequency_hz: 100_000_000,
            first_sample_index,
            discontinuity,
            samples: (0..count)
                .map(|offset| {
                    let index = first_sample_index + offset as u64;
                    let phase = TAU * frequency_hz * index as f64 / 8_000.0;
                    Iq8 {
                        i: (100.0 * phase.cos()).round() as i8,
                        q: (100.0 * phase.sin()).round() as i8,
                    }
                })
                .collect(),
        }
    }

    fn config() -> ChannelizerConfig {
        ChannelizerConfig {
            frequency_offset_hz: 1_000,
            bandwidth_hz: 1_000,
            output_sample_rate_hz: 2_000,
            filter_tap_count: 9,
        }
    }

    #[test]
    fn channelization_is_independent_of_input_batch_boundaries() {
        let mut whole = Channelizer::new(config()).unwrap();
        let expected = whole.push(&tone_batch(0, 64, false)).unwrap();

        let mut chunked = Channelizer::new(config()).unwrap();
        let first = chunked.push(&tone_batch(0, 13, false)).unwrap();
        let second = chunked.push(&tone_batch(13, 17, false)).unwrap();
        let third = chunked.push(&tone_batch(30, 34, false)).unwrap();
        let actual = first
            .samples
            .into_iter()
            .chain(second.samples)
            .chain(third.samples)
            .collect::<Vec<_>>();

        assert_eq!(actual, expected.samples);
        assert_eq!(expected.center_frequency_hz, 100_001_000);
        assert_eq!(expected.filter_delay_input_samples, 4);
        assert!(expected.discontinuity);
        assert!(actual
            .iter()
            .all(|sample| sample.source_range.end - sample.source_range.start == 9));
        assert!(actual
            .iter()
            .skip(2)
            .all(|sample| { sample.i > 0.70 && sample.q.abs() < 0.03 }));
    }

    #[test]
    fn discontinuity_discards_filter_history_and_restarts_source_mapping() {
        let mut channelizer = Channelizer::new(config()).unwrap();
        let first = channelizer.push(&tone_batch(0, 12, false)).unwrap();
        assert!(!first.samples.is_empty());

        let reset = channelizer.push(&tone_batch(100, 16, true)).unwrap();
        assert!(reset.discontinuity);
        assert!(reset
            .samples
            .iter()
            .all(|sample| sample.source_range.start >= 100));
    }

    #[test]
    fn rejects_a_channel_outside_the_input_nyquist_range() {
        let mut invalid = config();
        invalid.frequency_offset_hz = 3_750;
        let mut channelizer = Channelizer::new(invalid).unwrap();
        assert!(channelizer.push(&tone_batch(0, 16, false)).is_err());
    }

    #[test]
    fn channel_filter_rejects_a_tone_outside_the_selected_bandwidth() {
        let mut filter_config = config();
        filter_config.filter_tap_count = 65;
        let mut desired_channel = Channelizer::new(filter_config.clone()).unwrap();
        let desired = desired_channel
            .push(&tone_batch_at(0, 512, false, 1_000.0))
            .unwrap();
        let mut rejected_channel = Channelizer::new(filter_config).unwrap();
        let rejected = rejected_channel
            .push(&tone_batch_at(0, 512, false, -2_500.0))
            .unwrap();
        let mean_magnitude = |batch: &ChannelizedBatch| {
            batch
                .samples
                .iter()
                .map(|sample| f64::from(sample.i).hypot(f64::from(sample.q)))
                .sum::<f64>()
                / batch.samples.len() as f64
        };

        assert!(mean_magnitude(&desired) > 0.70);
        assert!(mean_magnitude(&rejected) < mean_magnitude(&desired) * 0.02);
    }

    /// Phase advance per output sample of a channelized tone.
    fn phase_steps(batch: &ChannelizedBatch) -> Vec<f64> {
        batch
            .samples
            .windows(2)
            .map(|pair| {
                let (previous, next) = (&pair[0], &pair[1]);
                let real = f64::from(next.i * previous.i + next.q * previous.q);
                let imaginary = f64::from(next.q * previous.i - next.i * previous.q);
                imaginary.atan2(real)
            })
            .collect()
    }

    fn assert_constant_phase_steps(output_rate_hz: u64, tolerance_rad: f64) {
        let mut channelizer = Channelizer::new(ChannelizerConfig {
            frequency_offset_hz: 1_000,
            bandwidth_hz: 1_000,
            output_sample_rate_hz: output_rate_hz,
            filter_tap_count: 65,
        })
        .unwrap();
        // 1.3 kHz is 300 Hz above the channel centre.
        let output = channelizer
            .push(&tone_batch_at(0, 4_000, false, 1_300.0))
            .unwrap();
        let expected = TAU * 300.0 / output_rate_hz as f64;
        let steps = phase_steps(&output);
        assert!(steps.len() > 100);
        for (index, step) in steps.iter().enumerate().skip(4) {
            assert!(
                (step - expected).abs() < tolerance_rad,
                "output {index}: phase step {step} differs from {expected}"
            );
        }
    }

    #[test]
    fn non_integer_decimation_keeps_the_output_instants_uniform() {
        // 8000 / 2500 = 3.2 input samples per output. Snapping each output to
        // the nearest input sample would move it by up to 0.1 rad of tone phase
        // at 300 Hz; the fractional-delay bank is exact because the ratio has
        // a small denominator.
        assert_constant_phase_steps(2_500, 0.02);
    }

    #[test]
    fn ratios_beyond_the_exact_phase_count_stay_within_a_fraction_of_a_sample() {
        // 8000 / 2999 reduces to a denominator far above 64 phases, so each
        // output is placed at the nearest 1/64 of an input sample.
        assert_constant_phase_steps(2_999, 0.02);
    }

    #[test]
    fn automatic_filter_length_tracks_the_input_rate_and_stays_odd() {
        let slow = automatic_filter_tap_count(2_000_000, 200_000);
        let fast = automatic_filter_tap_count(20_000_000, 200_000);
        assert_eq!(slow % 2, 1);
        assert_eq!(fast % 2, 1);
        assert!((140..=160).contains(&slow), "{slow}");
        // Ten times the rate needs ten times the taps for the same transition.
        assert!((1_400..=1_500).contains(&fast), "{fast}");
        assert_eq!(
            automatic_filter_tap_count(1_000_000_000, 1_000),
            MAX_FILTER_TAP_COUNT
        );
        assert!(automatic_filter_tap_count(8_000, 4_000) >= 15);
    }

    #[test]
    fn automatic_filter_keeps_adjacent_channel_rejection_at_any_input_rate() {
        let rejection_db = |input_rate_hz: u64, tone_offset_hz: f64| {
            let config = ChannelizerConfig {
                frequency_offset_hz: 0,
                bandwidth_hz: 2_000,
                output_sample_rate_hz: 5_000,
                filter_tap_count: 0,
            };
            let power = |frequency_hz: f64, config: &ChannelizerConfig| {
                let mut channelizer = Channelizer::new(config.clone()).unwrap();
                let samples = (0..40_000)
                    .map(|index| {
                        let phase = TAU * frequency_hz * index as f64 / input_rate_hz as f64;
                        Iq8 {
                            i: (100.0 * phase.cos()).round() as i8,
                            q: (100.0 * phase.sin()).round() as i8,
                        }
                    })
                    .collect();
                let output = channelizer
                    .push(&IqBatch {
                        sample_rate_hz: input_rate_hz,
                        center_frequency_hz: 100_000_000,
                        first_sample_index: 0,
                        discontinuity: true,
                        samples,
                    })
                    .unwrap();
                let skip = output.samples.len() / 2;
                output.samples[skip..]
                    .iter()
                    .map(|sample| f64::from(sample.i).powi(2) + f64::from(sample.q).powi(2))
                    .sum::<f64>()
                    / (output.samples.len() - skip) as f64
            };
            10.0 * (power(tone_offset_hz, &config) / power(500.0, &config)).log10()
        };
        // A tone 1.5x the bandwidth from the centre must be well down whether
        // the capture runs at 40 kS/s or 400 kS/s; a fixed tap count would
        // lose that selectivity at the higher rate.
        for input_rate_hz in [40_000, 400_000] {
            let rejection = rejection_db(input_rate_hz, 3_000.0);
            assert!(rejection < -50.0, "{input_rate_hz} S/s: {rejection:.1} dB");
        }
    }

    #[test]
    fn shared_preprocessing_matches_independent_channelizers() {
        let input = tone_batch_at(0, 128, true, 1_000.0);
        let mut second_config = config();
        second_config.frequency_offset_hz = -1_000;

        let expected_first = Channelizer::new(config()).unwrap().push(&input).unwrap();
        let expected_second = Channelizer::new(second_config.clone())
            .unwrap()
            .push(&input)
            .unwrap();

        let prepared = WidebandIqPreprocessor::new().push(&input).unwrap();
        let actual_first = Channelizer::new(config())
            .unwrap()
            .push_prepared(&prepared)
            .unwrap();
        let actual_second = Channelizer::new(second_config)
            .unwrap()
            .push_prepared(&prepared)
            .unwrap();

        assert_eq!(actual_first, expected_first);
        assert_eq!(actual_second, expected_second);
    }
}
