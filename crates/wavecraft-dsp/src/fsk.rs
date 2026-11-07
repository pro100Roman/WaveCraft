use crate::{
    ChannelizedBatch, DemodulatedBatch, Demodulator, DemodulatorDescriptor, DemodulatorFactory,
    DemodulatorOutputKind, DemodulatorRegistry, DemodulatorTelemetry, DspError,
    MuellerMullerClockRecovery, TimedComplexSample, TimedScalarSample, TimedSoftSymbol,
    TimedSoftSymbolBatch, TimingRecoveryConfig,
};
use std::any::Any;
use std::collections::VecDeque;
use std::f64::consts::PI;
use std::f64::consts::TAU;
use std::ops::Range;

const GFSK_FILTER_SPAN_SYMBOLS: u64 = 4;
const MAX_GFSK_FILTER_TAPS: usize = 4_095;
const CARRIER_ACQUISITION_SYMBOLS: u64 = 4;
const MIN_CARRIER_ACQUISITION_SAMPLES: usize = 32;
const MAX_CARRIER_ACQUISITION_SAMPLES: usize = 4_096;
const MIN_TONE_SEPARATION: f64 = 0.25;
const MAX_TONE_SEPARATION: f64 = 4.0;
const TONE_TRACKING_GAIN: f64 = 0.002;
/// Raw (unfiltered) discriminator spread above this fraction of its full range
/// is treated as noise. Noise-only windows spread over most of the range;
/// signal windows are much narrower unless the SNR is very low.
const NOISE_SPREAD_FRACTION: f64 = 0.4;
/// How far the tone centres of consecutive acquisition windows may differ,
/// relative to their separation, while still counting as the same candidate.
/// The centres are outer-decile means of a few dozen samples, so at moderate
/// SNR they jitter by more than 10%: with that limit 13% of bursts on a
/// 100 kbit/s link at 12 dB Eb/N0 were acquired late and 3% lost the whole
/// preamble. At 20% none were, and the noise-lead-in cases are unchanged.
const TONE_STABILITY_FRACTION: f64 = 0.2;

pub const BINARY_FSK_DEMODULATOR_ID: &str = "fsk2";
pub const BINARY_GFSK_DEMODULATOR_ID: &str = "gfsk2";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FskPolarity {
    Normal,
    Inverted,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FskModulation {
    Fsk,
    Gfsk { bt: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FskTiming {
    Fixed,
    Adaptive {
        loop_bandwidth: f64,
        max_clock_error_ppm: u32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FskConfig {
    pub symbol_rate_hz: u64,
    /// Expected positive or negative tone displacement from channel center.
    pub deviation_hz: u64,
    pub modulation: FskModulation,
    pub polarity: FskPolarity,
    pub timing: FskTiming,
}

impl FskConfig {
    pub fn validate(&self) -> Result<(), DspError> {
        if self.symbol_rate_hz == 0 {
            return Err(DspError::new("FSK symbol rate must be nonzero"));
        }
        if self.deviation_hz == 0 {
            return Err(DspError::new("FSK deviation must be nonzero"));
        }
        if let FskModulation::Gfsk { bt } = self.modulation {
            if !bt.is_finite() || bt <= 0.0 {
                return Err(DspError::new(
                    "GFSK BT must be finite and greater than zero",
                ));
            }
        }
        if let FskTiming::Adaptive {
            loop_bandwidth,
            max_clock_error_ppm,
        } = self.timing
        {
            TimingRecoveryConfig {
                symbol_rate_hz: self.symbol_rate_hz,
                loop_bandwidth,
                max_clock_error_ppm,
            }
            .validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedSoftBit {
    /// Negative values represent zero, positive values represent one, and the
    /// magnitude up to one expresses decision confidence.
    pub likelihood: f32,
    pub source_range: Range<u64>,
}

impl From<TimedSoftBit> for TimedSoftSymbol {
    fn from(bit: TimedSoftBit) -> Self {
        let one_likelihood = (bit.likelihood.clamp(-1.0, 1.0) + 1.0) * 0.5;
        Self {
            likelihoods: vec![1.0 - one_likelihood, one_likelihood],
            source_range: bit.source_range,
        }
    }
}

/// Incremental binary-FSK discriminator with fixed or adaptive symbol timing.
///
/// A receive filter matched to the transmitted frequency pulse follows the
/// quadrature discriminator: Gaussian for GFSK in both timing modes, and a
/// one-symbol average for FSK with adaptive timing (fixed timing already
/// integrates each symbol). Adaptive timing then removes the carrier offset,
/// recovers the symbol clock with Mueller-and-Muller, and applies a zero
/// slicer.
///
/// GNU Radio's `gfsk_demod` omits the post-discriminator filter and relies on
/// the channel filter alone. At the moderate SNR of real captures the
/// discriminator output is dominated by wideband noise, and the
/// decision-directed clock loop then slips symbols. Filtering first removes
/// that noise before the loop sees it.
pub struct FskDemodulator {
    config: FskConfig,
    previous_sample: Option<TimedComplexSample>,
    source_sample_rate_hz: Option<u64>,
    sample_rate_hz: Option<u64>,
    center_frequency_hz: Option<u64>,
    segment_start_sample_index: Option<u64>,
    current_symbol_number: Option<u128>,
    current_symbol_complete: bool,
    likelihood_sum: f64,
    likelihood_count: u64,
    symbol_range: Option<Range<u64>>,
    receive_filter: Vec<f64>,
    receive_history: VecDeque<DiscriminatorSample>,
    /// Filter input values of `receive_history`, contiguous so the filter is
    /// one slice dot product. The window is `receive_likelihood[receive_start..]`.
    receive_likelihood: Vec<f64>,
    receive_start: usize,
    carrier_acquisition: VecDeque<DiscriminatorSample>,
    timing_recovery: Option<MuellerMullerClockRecovery>,
    tone_tracker: ToneTracker,
    decisions: DecisionStatistics,
}

/// Magnitude the decision variable has at a nominal tone, which is where tone
/// normalization puts it. Seeds the statistics before any symbol was seen.
const DECISION_PRIOR_MEAN: f64 = 0.9;
/// Spread assumed before any symbol was seen: a moderately noisy link.
const DECISION_PRIOR_SD: f64 = 0.25;
/// The prior counts as this many symbols, so the first few decisions move the
/// estimate quickly and later ones refine it.
const DECISION_PRIOR_WEIGHT: f64 = 8.0;
/// Forgetting factor once the estimate has settled: about 64 symbols.
const DECISION_FORGETTING: f64 = 1.0 / 64.0;
/// Decision magnitudes are limited before they enter the statistics, so one
/// click from the discriminator cannot dominate the spread.
const DECISION_MAX_MAGNITUDE: f64 = 2.0;
/// Lower bound on the spread; the noise-free link still shows about 0.075 of
/// intersymbol interference.
const DECISION_MIN_SD: f64 = 0.05;
/// Symbols needed before the quality figures are reported.
const DECISION_MIN_SYMBOLS: u64 = 16;
/// Eye opening below which the decisions are treated as noise.
const EYE_OPEN_THRESHOLD: f64 = 0.1;

/// Running statistics of the decision magnitudes `|v|` at the symbol centres.
///
/// A symbol's confidence is the probability that noise of the measured spread
/// would not have carried a sample of the opposite symbol across the slicer
/// as far as this one lies from it, `1 - 2 Q(|v| / sd)`. Normalizing by the
/// burst's own spread makes the same value mean the same thing on a clean
/// link and a noisy one; the raw magnitude, clipped to one, ranked errors
/// across bursts barely better than chance on a 100 kbit/s link.
#[derive(Clone, Debug)]
struct DecisionStatistics {
    count: u64,
    mean: f64,
    variance: f64,
}

impl Default for DecisionStatistics {
    fn default() -> Self {
        Self {
            count: 0,
            mean: DECISION_PRIOR_MEAN,
            variance: DECISION_PRIOR_SD * DECISION_PRIOR_SD,
        }
    }
}

impl DecisionStatistics {
    fn sd(&self) -> f64 {
        self.variance.sqrt().max(DECISION_MIN_SD)
    }

    /// Signed confidence in the decision `value`, in `[-1, 1]`, from the
    /// statistics of the symbols before it.
    fn soft_decision(&self, value: f64) -> f64 {
        let confidence = erf(value.abs() / (std::f64::consts::SQRT_2 * self.sd()));
        if value >= 0.0 {
            confidence
        } else {
            -confidence
        }
    }

    fn observe(&mut self, value: f64) {
        let magnitude = value.abs().min(DECISION_MAX_MAGNITUDE);
        let weight = (1.0 / (self.count as f64 + DECISION_PRIOR_WEIGHT)).max(DECISION_FORGETTING);
        let delta = magnitude - self.mean;
        self.mean += weight * delta;
        self.variance = (1.0 - weight) * (self.variance + weight * delta * delta);
        self.count = self.count.saturating_add(1);
    }

    /// Margin between the slicer and the decisions, two spreads below their
    /// mean, as a fraction of the nominal tone distance. Zero until
    /// [`DECISION_MIN_SYMBOLS`] symbols were seen.
    fn eye_opening(&self) -> f64 {
        if self.count < DECISION_MIN_SYMBOLS {
            return 0.0;
        }
        (self.mean - 2.0 * self.sd()).clamp(0.0, 1.0)
    }

    fn is_open(&self) -> bool {
        self.eye_opening() >= EYE_OPEN_THRESHOLD
    }

    /// Mean squared over variance of the decision magnitudes, in dB.
    fn snr_db(&self) -> Option<f64> {
        (self.count >= DECISION_MIN_SYMBOLS)
            .then(|| 10.0 * (self.mean * self.mean / (self.sd() * self.sd())).log10())
    }
}

/// Error function, Abramowitz and Stegun 7.1.26 (absolute error below 1.5e-7).
fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let polynomial = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let magnitude = 1.0 - polynomial * (-x * x).exp();
    if x >= 0.0 {
        magnitude
    } else {
        -magnitude
    }
}

#[derive(Clone, Debug)]
struct DiscriminatorSample {
    likelihood: f64,
    /// Discriminator value before any receive filtering.
    raw: f64,
    effective_source_index: u64,
    source_range: Range<u64>,
}

#[derive(Clone, Debug, Default)]
struct ToneTracker {
    acquisition: VecDeque<f64>,
    acquisition_raw: VecDeque<f64>,
    candidate_lower: Option<f64>,
    candidate_upper: Option<f64>,
    stable_candidate_samples: usize,
    lower: Option<f64>,
    upper: Option<f64>,
}

/// Means of the lowest and highest tenth of `window` (at least four samples).
fn outer_decile_means(window: &VecDeque<f64>) -> (f64, f64) {
    let mut sorted = window.iter().copied().collect::<Vec<_>>();
    sorted.sort_by(f64::total_cmp);
    let cluster_samples = (sorted.len() / 10).max(4).min(sorted.len() / 2);
    let lower = sorted[..cluster_samples].iter().sum::<f64>() / cluster_samples as f64;
    let upper =
        sorted[sorted.len() - cluster_samples..].iter().sum::<f64>() / cluster_samples as f64;
    (lower, upper)
}

impl ToneTracker {
    fn reset(&mut self) {
        self.acquisition.clear();
        self.acquisition_raw.clear();
        self.candidate_lower = None;
        self.candidate_upper = None;
        self.stable_candidate_samples = 0;
        self.lower = None;
        self.upper = None;
    }

    /// Normalizes `value` by the tone centres. While the centres are unknown,
    /// `value` and `raw` fill the acquisition window; `raw_spread_limit` is the
    /// widest unfiltered tone separation still considered a signal.
    fn normalize(
        &mut self,
        value: f64,
        raw: f64,
        acquisition_samples: usize,
        raw_spread_limit: f64,
    ) -> Option<f64> {
        if self.lower.is_none() || self.upper.is_none() {
            self.acquisition.push_back(value);
            self.acquisition_raw.push_back(raw);
            while self.acquisition.len() > acquisition_samples {
                self.acquisition.pop_front();
                self.acquisition_raw.pop_front();
            }
            if self.acquisition.len() < acquisition_samples {
                return None;
            }
            // GFSK spends many discriminator samples on shaped transitions.
            // Use the outer deciles so the tone centres are not pulled toward
            // the midpoint by those transition samples.
            let (lower, upper) = outer_decile_means(&self.acquisition);
            let separation = upper - lower;
            // Random phase in a quiet channel commonly spans most of the
            // discriminator's Nyquist range. Do not mistake that wide spread
            // for the two configured FSK tones. The receive filter narrows
            // that spread until it resembles two tones, so judge the
            // unfiltered samples too.
            let (raw_lower, raw_upper) = outer_decile_means(&self.acquisition_raw);
            if !(MIN_TONE_SEPARATION..=MAX_TONE_SEPARATION).contains(&separation)
                || raw_upper - raw_lower > raw_spread_limit
            {
                self.candidate_lower = None;
                self.candidate_upper = None;
                self.stable_candidate_samples = 0;
                return None;
            }
            let stable_tolerance = separation * TONE_STABILITY_FRACTION;
            // A burst can start partway through the retained window. A window
            // that straddles the burst edge mixes noise into the tone centres,
            // so commit only after the candidate has stayed put for a whole
            // window length: the window then lies entirely inside one regime.
            let stable = self.candidate_lower.zip(self.candidate_upper).is_some_and(
                |(candidate_lower, candidate_upper)| {
                    (lower - candidate_lower).abs() <= stable_tolerance
                        && (upper - candidate_upper).abs() <= stable_tolerance
                },
            );
            if stable {
                self.stable_candidate_samples = self.stable_candidate_samples.saturating_add(1);
            } else {
                self.candidate_lower = Some(lower);
                self.candidate_upper = Some(upper);
                self.stable_candidate_samples = 1;
            }
            if self.stable_candidate_samples < acquisition_samples {
                return None;
            }
            self.lower = Some(lower);
            self.upper = Some(upper);
            self.acquisition.clear();
            self.acquisition_raw.clear();
            self.candidate_lower = None;
            self.candidate_upper = None;
            self.stable_candidate_samples = 0;
        }

        let mut lower = self.lower?;
        let mut upper = self.upper?;
        let separation = upper - lower;
        let lower_distance = (value - lower).abs();
        let upper_distance = (value - upper).abs();
        if lower_distance.min(upper_distance) <= separation * 0.35 {
            if lower_distance <= upper_distance {
                lower += TONE_TRACKING_GAIN * (value - lower);
            } else {
                upper += TONE_TRACKING_GAIN * (value - upper);
            }
            self.lower = Some(lower);
            self.upper = Some(upper);
        }
        self.normalize_acquired(value)
    }

    fn is_acquired(&self) -> bool {
        self.lower.is_some() && self.upper.is_some()
    }

    fn normalize_acquired(&self, value: f64) -> Option<f64> {
        let lower = self.lower?;
        let upper = self.upper?;
        let midpoint = (lower + upper) * 0.5;
        let half_separation = ((upper - lower) * 0.5).max(MIN_TONE_SEPARATION * 0.5);
        Some((value - midpoint) / half_separation)
    }

    fn offset(&self) -> Option<f64> {
        Some((self.lower? + self.upper?) * 0.5)
    }

    fn separation(&self) -> Option<f64> {
        Some(self.upper? - self.lower?)
    }
}

impl FskDemodulator {
    pub fn new(config: FskConfig) -> Result<Self, DspError> {
        config.validate()?;
        let timing_recovery = match config.timing {
            FskTiming::Fixed => None,
            FskTiming::Adaptive {
                loop_bandwidth,
                max_clock_error_ppm,
            } => Some(MuellerMullerClockRecovery::new(TimingRecoveryConfig {
                symbol_rate_hz: config.symbol_rate_hz,
                loop_bandwidth,
                max_clock_error_ppm,
            })?),
        };
        Ok(Self {
            config,
            previous_sample: None,
            source_sample_rate_hz: None,
            sample_rate_hz: None,
            center_frequency_hz: None,
            segment_start_sample_index: None,
            current_symbol_number: None,
            current_symbol_complete: false,
            likelihood_sum: 0.0,
            likelihood_count: 0,
            symbol_range: None,
            receive_filter: Vec::new(),
            receive_history: VecDeque::new(),
            receive_likelihood: Vec::new(),
            receive_start: 0,
            carrier_acquisition: VecDeque::new(),
            timing_recovery,
            tone_tracker: ToneTracker::default(),
            decisions: DecisionStatistics::default(),
        })
    }

    pub fn config(&self) -> &FskConfig {
        &self.config
    }

    /// Whether the symbol clock is recovered and its decisions are clear of
    /// the slicer: the clock loop has converged on enough symbols and the eye
    /// at the decision instants is open (see [`Self::timing_lock_quality`]).
    /// Fixed timing has no clock to recover and always reports locked.
    pub fn timing_locked(&self) -> bool {
        match &self.timing_recovery {
            Some(timing_recovery) => timing_recovery.is_locked() && self.decisions.is_open(),
            None => true,
        }
    }

    pub fn estimated_symbol_rate_hz(&self) -> Option<f64> {
        self.timing_recovery
            .as_ref()
            .and_then(MuellerMullerClockRecovery::estimated_symbol_rate_hz)
    }

    /// Estimated residual carrier offset relative to the configured channel center.
    pub fn estimated_frequency_offset_hz(&self) -> Option<f64> {
        self.tone_tracker
            .offset()
            .map(|offset| offset * self.config.deviation_hz as f64)
    }

    /// Estimated distance between the two FSK tones.
    pub fn estimated_tone_separation_hz(&self) -> Option<f64> {
        self.tone_tracker
            .separation()
            .map(|separation| separation * self.config.deviation_hz as f64)
    }

    /// Eye opening at the decision instants from zero (closed, or too few
    /// symbols) to one (every decision at a nominal tone): the mean decision
    /// magnitude less two standard deviations. A mistimed sampler sees the
    /// transitions instead of the symbol centres and closes the eye, and so
    /// does noise. Always one with fixed timing.
    pub fn timing_lock_quality(&self) -> f32 {
        match &self.timing_recovery {
            Some(_) => self.decisions.eye_opening() as f32,
            None => 1.0,
        }
    }

    /// Ratio of the squared mean decision magnitude to its variance, in dB.
    /// Intersymbol interference caps it near 22 dB on a noise-free GFSK link.
    pub fn decision_snr_db(&self) -> Option<f32> {
        self.decisions.snr_db().map(|snr| snr as f32)
    }

    pub fn reset(&mut self) {
        self.previous_sample = None;
        self.source_sample_rate_hz = None;
        self.sample_rate_hz = None;
        self.center_frequency_hz = None;
        self.segment_start_sample_index = None;
        self.current_symbol_number = None;
        self.current_symbol_complete = false;
        self.likelihood_sum = 0.0;
        self.likelihood_count = 0;
        self.symbol_range = None;
        self.receive_filter.clear();
        self.receive_history.clear();
        self.receive_likelihood.clear();
        self.receive_start = 0;
        self.carrier_acquisition.clear();
        self.tone_tracker.reset();
        self.decisions = DecisionStatistics::default();
        if let Some(timing_recovery) = &mut self.timing_recovery {
            timing_recovery.reset();
        }
    }

    pub fn push(&mut self, batch: &ChannelizedBatch) -> Result<Vec<TimedSoftBit>, DspError> {
        self.validate_batch(batch)?;
        if batch.discontinuity
            || self
                .source_sample_rate_hz
                .is_some_and(|rate| rate != batch.source_sample_rate_hz)
            || self
                .sample_rate_hz
                .is_some_and(|rate| rate != batch.sample_rate_hz)
            || self
                .center_frequency_hz
                .is_some_and(|frequency| frequency != batch.center_frequency_hz)
            || self
                .segment_start_sample_index
                .is_some_and(|start| start != batch.segment_start_sample_index)
        {
            self.reset();
        }
        self.source_sample_rate_hz = Some(batch.source_sample_rate_hz);
        self.sample_rate_hz = Some(batch.sample_rate_hz);
        self.center_frequency_hz = Some(batch.center_frequency_hz);
        self.segment_start_sample_index = Some(batch.segment_start_sample_index);
        if self.receive_filter.is_empty() {
            self.receive_filter = design_receive_filter(
                batch.sample_rate_hz,
                self.config.symbol_rate_hz,
                self.config.modulation,
                self.config.timing,
            )?;
            self.receive_history = VecDeque::with_capacity(self.receive_filter.len());
            self.receive_likelihood = Vec::with_capacity(2 * self.receive_filter.len());
            self.receive_start = 0;
        }

        let mut bits = Vec::new();
        let mut timing_samples = Vec::new();
        for sample in &batch.samples {
            validate_sample(sample)?;
            let effective_index = effective_source_index(sample, batch)?;
            let Some(previous) = self.previous_sample.replace(sample.clone()) else {
                if self.config.timing == FskTiming::Fixed {
                    let (number, remainder) = symbol_position(
                        effective_index,
                        batch.segment_start_sample_index,
                        batch.source_sample_rate_hz,
                        self.config.symbol_rate_hz,
                    )?;
                    self.current_symbol_number = Some(number);
                    self.current_symbol_complete = remainder == 0;
                }
                continue;
            };
            if sample.source_range.start < previous.source_range.start {
                self.reset();
                return Err(DspError::new(
                    "channelized samples are not ordered by source range",
                ));
            }

            let real = f64::from(sample.i) * f64::from(previous.i)
                + f64::from(sample.q) * f64::from(previous.q);
            let imaginary = f64::from(sample.q) * f64::from(previous.i)
                - f64::from(sample.i) * f64::from(previous.q);
            let frequency_hz = imaginary.atan2(real) * batch.sample_rate_hz as f64 / TAU;
            let normalized_frequency = frequency_hz / self.config.deviation_hz as f64;
            let discriminator = DiscriminatorSample {
                likelihood: normalized_frequency,
                raw: normalized_frequency,
                effective_source_index: effective_index,
                source_range: previous.source_range.start..sample.source_range.end,
            };
            if let Some(mut discriminator) = self.filter_discriminator(discriminator) {
                if matches!(self.config.timing, FskTiming::Adaptive { .. }) {
                    let acquisition_samples = carrier_acquisition_sample_count(
                        batch.sample_rate_hz,
                        self.config.symbol_rate_hz,
                    );
                    // Pure noise fills the discriminator's whole range, which
                    // spans `sample_rate / deviation` deviations.
                    let raw_spread_limit = MAX_TONE_SEPARATION.max(
                        NOISE_SPREAD_FRACTION * batch.sample_rate_hz as f64
                            / self.config.deviation_hz as f64,
                    );
                    if !self.tone_tracker.is_acquired() {
                        self.carrier_acquisition.push_back(discriminator);
                        // Acquisition needs one window to fill and one more
                        // to prove stable, so keep enough history to replay
                        // the start of the burst once the tones are known.
                        while self.carrier_acquisition.len() > acquisition_samples.saturating_mul(3)
                        {
                            self.carrier_acquisition.pop_front();
                        }
                        let Some(latest) = self.carrier_acquisition.back() else {
                            continue;
                        };
                        if self
                            .tone_tracker
                            .normalize(
                                latest.likelihood,
                                latest.raw,
                                acquisition_samples,
                                raw_spread_limit,
                            )
                            .is_none()
                        {
                            continue;
                        }
                        let mut acquired_samples =
                            Vec::with_capacity(self.carrier_acquisition.len());
                        for mut acquired in self.carrier_acquisition.drain(..) {
                            acquired.likelihood = self
                                .tone_tracker
                                .normalize_acquired(acquired.likelihood)
                                .ok_or_else(|| {
                                    DspError::new("FSK tone acquisition lost its tone centres")
                                })?;
                            if self.config.polarity == FskPolarity::Inverted {
                                acquired.likelihood = -acquired.likelihood;
                            }
                            acquired_samples.push(acquired);
                        }
                        let samples_per_symbol =
                            batch.sample_rate_hz as f64 / self.config.symbol_rate_hz as f64;
                        let values = acquired_samples
                            .iter()
                            .map(|sample| sample.likelihood)
                            .collect::<Vec<_>>();
                        // Only the last two windows are known to hold the
                        // signal; anything older may be the noise the burst
                        // began in. Start the clock loop on the symbol grid
                        // found in that tail and read the older samples
                        // backwards along the same grid.
                        let tail_start = values.len().saturating_sub(acquisition_samples * 2);
                        let skip =
                            tail_start + eye_centre_skip(&values[tail_start..], samples_per_symbol);
                        for (value, source_range) in earlier_symbol_centres(
                            &acquired_samples,
                            skip as f64 + 0.5,
                            samples_per_symbol,
                        ) {
                            bits.push(TimedSoftBit {
                                likelihood: self.decisions.soft_decision(value) as f32,
                                source_range,
                            });
                        }
                        timing_samples.extend(acquired_samples.into_iter().skip(skip).map(
                            |acquired| TimedScalarSample {
                                value: acquired.likelihood as f32,
                                source_range: acquired.source_range,
                            },
                        ));
                        continue;
                    }
                    discriminator.likelihood = self
                        .tone_tracker
                        .normalize(
                            discriminator.likelihood,
                            discriminator.raw,
                            acquisition_samples,
                            raw_spread_limit,
                        )
                        .ok_or_else(|| DspError::new("FSK tone tracking lost its tone centres"))?;
                }
                if self.config.polarity == FskPolarity::Inverted {
                    discriminator.likelihood = -discriminator.likelihood;
                }
                match self.config.timing {
                    FskTiming::Fixed => self.push_fixed_sample(discriminator, batch, &mut bits)?,
                    FskTiming::Adaptive { .. } => timing_samples.push(TimedScalarSample {
                        value: discriminator.likelihood as f32,
                        source_range: discriminator.source_range,
                    }),
                }
            }
        }
        if let Some(timing_recovery) = &mut self.timing_recovery {
            for symbol in timing_recovery.push(&timing_samples, batch.sample_rate_hz, false)? {
                let value = f64::from(symbol.value);
                bits.push(TimedSoftBit {
                    likelihood: self.decisions.soft_decision(value) as f32,
                    source_range: symbol.source_range,
                });
                self.decisions.observe(value);
            }
        }
        Ok(bits)
    }

    fn push_fixed_sample(
        &mut self,
        discriminator: DiscriminatorSample,
        batch: &ChannelizedBatch,
        bits: &mut Vec<TimedSoftBit>,
    ) -> Result<(), DspError> {
        let (symbol_number, symbol_remainder) = symbol_position(
            discriminator.effective_source_index,
            batch.segment_start_sample_index,
            batch.source_sample_rate_hz,
            self.config.symbol_rate_hz,
        )?;
        let mut current_symbol_number = self.current_symbol_number.unwrap_or(symbol_number);
        if self.likelihood_count == 0 && symbol_number > current_symbol_number + 1 {
            current_symbol_number = symbol_number;
            self.current_symbol_number = Some(symbol_number);
            self.current_symbol_complete = symbol_remainder == 0;
        }
        self.likelihood_sum += discriminator.likelihood;
        self.likelihood_count += 1;
        extend_range(&mut self.symbol_range, discriminator.source_range);
        if symbol_number <= current_symbol_number {
            return Ok(());
        }
        if symbol_number != current_symbol_number + 1 {
            self.reset();
            return Err(DspError::new(
                "channelized sample spacing skipped an FSK symbol boundary",
            ));
        }
        if self.current_symbol_complete && self.likelihood_count != 0 {
            let value = self.likelihood_sum / self.likelihood_count as f64;
            if let Some(source_range) = self.symbol_range.take() {
                bits.push(TimedSoftBit {
                    likelihood: self.decisions.soft_decision(value) as f32,
                    source_range,
                });
                self.decisions.observe(value);
            }
        } else {
            self.symbol_range = None;
        }
        self.likelihood_sum = 0.0;
        self.likelihood_count = 0;
        self.current_symbol_number = Some(symbol_number);
        self.current_symbol_complete = symbol_number == current_symbol_number + 1;
        Ok(())
    }

    /// Runs one discriminator sample through the receive filter and returns
    /// the output centred half a filter span earlier, once enough samples have
    /// arrived.
    fn filter_discriminator(&mut self, sample: DiscriminatorSample) -> Option<DiscriminatorSample> {
        if self.receive_filter.is_empty() {
            return Some(sample);
        }

        if self.receive_history.is_empty()
            && matches!(self.config.timing, FskTiming::Adaptive { .. })
        {
            // The filter output is centred, so its first output would
            // otherwise consume half a filter span of the burst. Adaptive
            // decoding must retain the start of the preamble for framing, so
            // extend the first sample backwards instead of dropping it.
            for _ in 0..self.receive_filter.len() / 2 {
                self.receive_likelihood.push(sample.likelihood);
                self.receive_history.push_back(sample.clone());
            }
        }
        self.receive_likelihood.push(sample.likelihood);
        self.receive_history.push_back(sample);
        let filter_length = self.receive_filter.len();
        if self.receive_history.len() < filter_length {
            return None;
        }

        let midpoint = filter_length / 2;
        let window = &self.receive_likelihood[self.receive_start..];
        let likelihood = dot_product(&window[..filter_length], &self.receive_filter);
        let last = self.receive_history.len() - 1;
        let output = DiscriminatorSample {
            likelihood,
            raw: self.receive_history[midpoint].raw,
            effective_source_index: self.receive_history[midpoint].effective_source_index,
            source_range: self.receive_history[0].source_range.start
                ..self.receive_history[(midpoint + midpoint).min(last)]
                    .source_range
                    .end,
        };

        self.receive_history.pop_front();
        self.receive_start += 1;
        if self.receive_start >= filter_length {
            self.receive_likelihood.drain(..self.receive_start);
            self.receive_start = 0;
        }
        Some(output)
    }

    fn validate_batch(&self, batch: &ChannelizedBatch) -> Result<(), DspError> {
        if batch.sample_rate_hz == 0 {
            return Err(DspError::new("channelized sample rate must be nonzero"));
        }
        if batch.source_sample_rate_hz == 0 {
            return Err(DspError::new("source IQ sample rate must be nonzero"));
        }
        if self.config.symbol_rate_hz.saturating_mul(2) > batch.sample_rate_hz {
            return Err(DspError::new(
                "FSK decoding requires at least two samples per symbol",
            ));
        }
        if self.config.deviation_hz.saturating_mul(2) >= batch.sample_rate_hz {
            return Err(DspError::new(
                "FSK deviation must remain below the channel Nyquist frequency",
            ));
        }
        receive_filter_tap_count(
            batch.sample_rate_hz,
            self.config.symbol_rate_hz,
            self.config.modulation,
            self.config.timing,
        )?;
        Ok(())
    }
}

/// Frequency error each side of a channel allows for: about 17 ppm at 868 MHz,
/// which covers a typical transmitter crystal plus the SDR's own.
const CARRIER_TOLERANCE_HZ: u64 = 15_000;
const CHANNEL_BANDWIDTH_STEP_HZ: u64 = 5_000;

/// Bandwidth holding nearly all the power of a binary FSK/GFSK signal
/// (Carson's rule): `2 * (deviation + symbol_rate / 2)`, the highest
/// modulating frequency of an alternating bit pattern being half the bit rate.
pub fn fsk_carson_bandwidth_hz(symbol_rate_hz: u64, deviation_hz: u64) -> u64 {
    deviation_hz
        .saturating_mul(2)
        .saturating_add(symbol_rate_hz)
}

/// Channel bandwidth for [`fsk_carson_bandwidth_hz`] plus
/// [`CARRIER_TOLERANCE_HZ`] on each side, rounded up to 5 kHz.
///
/// Noise power grows with the bandwidth, so a channel much wider than the
/// signal costs sensitivity. On the 19.2 kbit/s, 35 kHz deviation GFSK link at
/// 14 dB Eb/N0 a 120 kHz channel decodes with about a seventh of the bit
/// errors of a 200 kHz one, and still tolerates a 20 kHz carrier error; with
/// no margin at all (90 kHz) a 20 kHz error loses the signal.
pub fn recommended_fsk_channel_bandwidth_hz(symbol_rate_hz: u64, deviation_hz: u64) -> u64 {
    fsk_carson_bandwidth_hz(symbol_rate_hz, deviation_hz)
        .saturating_add(2 * CARRIER_TOLERANCE_HZ)
        .div_ceil(CHANNEL_BANDWIDTH_STEP_HZ)
        .saturating_mul(CHANNEL_BANDWIDTH_STEP_HZ)
}

/// Inner product with independent accumulators, so the compiler can vectorize
/// the reduction (floating-point addition is not associative).
fn dot_product(values: &[f64], coefficients: &[f64]) -> f64 {
    const LANES: usize = 4;
    let mut accumulator = [0.0; LANES];
    let mut value_chunks = values.chunks_exact(LANES);
    let mut coefficient_chunks = coefficients.chunks_exact(LANES);
    for (value, coefficient) in value_chunks.by_ref().zip(coefficient_chunks.by_ref()) {
        for lane in 0..LANES {
            accumulator[lane] += value[lane] * coefficient[lane];
        }
    }
    let mut sum = accumulator.iter().sum::<f64>();
    for (value, coefficient) in value_chunks
        .remainder()
        .iter()
        .zip(coefficient_chunks.remainder())
    {
        sum += value * coefficient;
    }
    sum
}

/// Number of leading samples to drop so symbol decisions start at the eye
/// centre.
///
/// Mueller-and-Muller produces no timing error for an ideal alternating
/// preamble, so its starting phase would persist through the preamble. A phase
/// near a symbol boundary then yields near-zero soft values and, as the period
/// estimate creeps, a duplicated symbol. Choosing the phase whose symbol
/// centres have the largest mean magnitude starts the loop where it belongs.
fn eye_centre_skip(values: &[f64], samples_per_symbol: f64) -> usize {
    let mut best_skip = 0;
    let mut best_score = f64::NEG_INFINITY;
    for skip in 0..samples_per_symbol.ceil().max(1.0) as usize {
        let mut position = skip as f64 + 0.5;
        let mut sum = 0.0;
        let mut count = 0_usize;
        while position + 1.0 < values.len() as f64 {
            let index = position.floor() as usize;
            let fraction = position - index as f64;
            let value = values[index] + (values[index + 1] - values[index]) * fraction;
            sum += value.abs();
            count += 1;
            position += samples_per_symbol;
        }
        if count == 0 {
            continue;
        }
        let score = sum / count as f64;
        if score > best_score {
            best_score = score;
            best_skip = skip;
        }
    }
    best_skip
}

/// Soft values at the symbol centres before `first_centre`, oldest first,
/// read from the grid of `period` samples running backwards from it.
///
/// A loop that only starts at the first centre cannot have been tracking
/// earlier symbols, and the few it spans are too few for the nominal period to
/// drift by a noticeable part of a symbol.
fn earlier_symbol_centres(
    samples: &[DiscriminatorSample],
    first_centre: f64,
    period: f64,
) -> Vec<(f64, Range<u64>)> {
    let mut centres = Vec::new();
    let mut position = first_centre - period;
    while position >= 0.0 {
        let index = position.floor() as usize;
        if let [earlier, later, ..] = &samples[index.min(samples.len())..] {
            let fraction = position - index as f64;
            centres.push((
                earlier.likelihood + (later.likelihood - earlier.likelihood) * fraction,
                earlier.source_range.start..later.source_range.end,
            ));
        }
        position -= period;
    }
    centres.reverse();
    centres
}

fn carrier_acquisition_sample_count(sample_rate_hz: u64, symbol_rate_hz: u64) -> usize {
    usize::try_from(
        sample_rate_hz
            .saturating_mul(CARRIER_ACQUISITION_SYMBOLS)
            .div_ceil(symbol_rate_hz),
    )
    .unwrap_or(MAX_CARRIER_ACQUISITION_SAMPLES)
    .clamp(
        MIN_CARRIER_ACQUISITION_SAMPLES,
        MAX_CARRIER_ACQUISITION_SAMPLES,
    )
}

impl Demodulator for FskDemodulator {
    fn reset(&mut self) {
        FskDemodulator::reset(self);
    }

    fn push(&mut self, input: &ChannelizedBatch) -> Result<DemodulatedBatch, DspError> {
        let symbols = FskDemodulator::push(self, input)?
            .into_iter()
            .map(TimedSoftSymbol::from)
            .collect();
        Ok(DemodulatedBatch::SoftSymbols(TimedSoftSymbolBatch {
            alphabet_size: 2,
            discontinuity: input.discontinuity,
            symbols,
        }))
    }

    fn telemetry(&self) -> DemodulatorTelemetry {
        DemodulatorTelemetry {
            timing_locked: self.timing_locked(),
            timing_lock_quality: self.timing_lock_quality(),
            decision_snr_db: self.decision_snr_db(),
            estimated_symbol_rate_hz: self.estimated_symbol_rate_hz(),
            estimated_frequency_offset_hz: self.estimated_frequency_offset_hz(),
            estimated_tone_separation_hz: self.estimated_tone_separation_hz(),
        }
    }
}

#[derive(Clone, Copy)]
enum BinaryFskKind {
    Fsk,
    Gfsk,
}

struct BinaryFskFactory {
    descriptor: DemodulatorDescriptor,
    kind: BinaryFskKind,
}

impl BinaryFskFactory {
    fn new(id: &str, display_name: &str, kind: BinaryFskKind) -> Self {
        Self {
            descriptor: DemodulatorDescriptor {
                id: id.to_owned(),
                display_name: display_name.to_owned(),
                output: DemodulatorOutputKind::SoftSymbols { alphabet_size: 2 },
            },
            kind,
        }
    }
}

impl DemodulatorFactory for BinaryFskFactory {
    fn descriptor(&self) -> &DemodulatorDescriptor {
        &self.descriptor
    }

    fn create(&self, config: &dyn Any) -> Result<Box<dyn Demodulator>, DspError> {
        let config = config.downcast_ref::<FskConfig>().ok_or_else(|| {
            DspError::new(format!(
                "demodulator '{}' requires FskConfig",
                self.descriptor.id
            ))
        })?;
        let modulation_matches = matches!(
            (self.kind, config.modulation),
            (BinaryFskKind::Fsk, FskModulation::Fsk)
                | (BinaryFskKind::Gfsk, FskModulation::Gfsk { .. })
        );
        if !modulation_matches {
            return Err(DspError::new(format!(
                "demodulator '{}' does not match the configured FSK modulation",
                self.descriptor.id
            )));
        }
        Ok(Box::new(FskDemodulator::new(config.clone())?))
    }
}

pub(crate) fn register_builtin_fsk_demodulators(
    registry: &mut DemodulatorRegistry,
) -> Result<(), DspError> {
    registry.register(BinaryFskFactory::new(
        BINARY_FSK_DEMODULATOR_ID,
        "Binary FSK",
        BinaryFskKind::Fsk,
    ))?;
    registry.register(BinaryFskFactory::new(
        BINARY_GFSK_DEMODULATOR_ID,
        "Binary GFSK",
        BinaryFskKind::Gfsk,
    ))?;
    Ok(())
}

/// Length of the receive filter, or zero when the configuration needs none.
fn receive_filter_tap_count(
    sample_rate_hz: u64,
    symbol_rate_hz: u64,
    modulation: FskModulation,
    timing: FskTiming,
) -> Result<usize, DspError> {
    match (modulation, timing) {
        (FskModulation::Gfsk { .. }, _) => {
            gaussian_filter_tap_count(sample_rate_hz, symbol_rate_hz)
        }
        (FskModulation::Fsk, FskTiming::Adaptive { .. }) => {
            boxcar_filter_tap_count(sample_rate_hz, symbol_rate_hz)
        }
        // Fixed timing integrates each symbol, which is already matched.
        (FskModulation::Fsk, FskTiming::Fixed) => Ok(0),
    }
}

/// Filter applied to the discriminator before clock recovery and slicing,
/// matched to the transmitted frequency pulse: Gaussian for GFSK and one
/// symbol of averaging for rectangular FSK. Empty when none is needed.
fn design_receive_filter(
    sample_rate_hz: u64,
    symbol_rate_hz: u64,
    modulation: FskModulation,
    timing: FskTiming,
) -> Result<Vec<f64>, DspError> {
    match (modulation, timing) {
        (FskModulation::Gfsk { .. }, _) => {
            design_gaussian_matched_filter(sample_rate_hz, symbol_rate_hz, modulation)
        }
        (FskModulation::Fsk, FskTiming::Adaptive { .. }) => {
            design_boxcar_matched_filter(sample_rate_hz, symbol_rate_hz)
        }
        (FskModulation::Fsk, FskTiming::Fixed) => Ok(Vec::new()),
    }
}

/// Odd tap count covering one symbol, so the filter output is centred.
fn boxcar_filter_tap_count(sample_rate_hz: u64, symbol_rate_hz: u64) -> Result<usize, DspError> {
    let half_symbol = sample_rate_hz as f64 / symbol_rate_hz as f64 / 2.0;
    let tap_count = 2 * (half_symbol - 0.5).ceil().max(0.0) as usize + 1;
    if tap_count > MAX_GFSK_FILTER_TAPS {
        return Err(DspError::new(
            "FSK receive filter exceeds 4095 taps; reduce the channel sample rate",
        ));
    }
    Ok(tap_count)
}

/// Moving average over one symbol. Edge taps carry the fraction of their
/// sample that lies inside the symbol, so a non-integer samples-per-symbol
/// ratio still averages exactly one symbol.
fn design_boxcar_matched_filter(
    sample_rate_hz: u64,
    symbol_rate_hz: u64,
) -> Result<Vec<f64>, DspError> {
    let tap_count = boxcar_filter_tap_count(sample_rate_hz, symbol_rate_hz)?;
    let half_symbol = sample_rate_hz as f64 / symbol_rate_hz as f64 / 2.0;
    let midpoint = (tap_count / 2) as isize;
    let mut taps = (0..tap_count)
        .map(|index| {
            let center = (index as isize - midpoint) as f64;
            ((center + 0.5).min(half_symbol) - (center - 0.5).max(-half_symbol)).clamp(0.0, 1.0)
        })
        .collect::<Vec<_>>();
    let sum = taps.iter().sum::<f64>();
    for tap in &mut taps {
        *tap /= sum;
    }
    Ok(taps)
}

fn gaussian_filter_tap_count(sample_rate_hz: u64, symbol_rate_hz: u64) -> Result<usize, DspError> {
    let span_samples = u128::from(sample_rate_hz)
        .saturating_mul(u128::from(GFSK_FILTER_SPAN_SYMBOLS))
        .div_ceil(u128::from(symbol_rate_hz));
    let mut tap_count = usize::try_from(span_samples)
        .map_err(|_| DspError::new("GFSK matched filter is too large"))?
        .saturating_add(1);
    if tap_count % 2 == 0 {
        tap_count = tap_count.saturating_add(1);
    }
    if tap_count > MAX_GFSK_FILTER_TAPS {
        return Err(DspError::new(
            "GFSK matched filter exceeds 4095 taps; reduce the channel sample rate",
        ));
    }
    Ok(tap_count)
}

fn design_gaussian_matched_filter(
    sample_rate_hz: u64,
    symbol_rate_hz: u64,
    modulation: FskModulation,
) -> Result<Vec<f64>, DspError> {
    let FskModulation::Gfsk { bt } = modulation else {
        return Ok(Vec::new());
    };
    let tap_count = gaussian_filter_tap_count(sample_rate_hz, symbol_rate_hz)?;
    let samples_per_symbol = sample_rate_hz as f64 / symbol_rate_hz as f64;
    let midpoint = (tap_count / 2) as isize;
    let mut taps = (0..tap_count)
        .map(|index| {
            let time_symbols = (index as isize - midpoint) as f64 / samples_per_symbol;
            (-2.0 * (PI * bt * time_symbols).powi(2) / 2.0_f64.ln()).exp()
        })
        .collect::<Vec<_>>();
    let sum = taps.iter().sum::<f64>();
    for tap in &mut taps {
        *tap /= sum;
    }
    Ok(taps)
}

fn effective_source_index(
    sample: &TimedComplexSample,
    batch: &ChannelizedBatch,
) -> Result<u64, DspError> {
    let index = sample
        .source_range
        .start
        .saturating_add(batch.filter_delay_input_samples);
    if index >= sample.source_range.end {
        return Err(DspError::new(
            "channelized filter delay falls outside the contributing source range",
        ));
    }
    Ok(index)
}

fn symbol_position(
    source_index: u64,
    segment_start: u64,
    source_sample_rate_hz: u64,
    symbol_rate_hz: u64,
) -> Result<(u128, u128), DspError> {
    let offset = source_index
        .checked_sub(segment_start)
        .ok_or_else(|| DspError::new("channelized sample precedes its tuning segment"))?;
    let position = u128::from(offset) * u128::from(symbol_rate_hz);
    let denominator = u128::from(source_sample_rate_hz);
    Ok((position / denominator, position % denominator))
}

fn validate_sample(sample: &TimedComplexSample) -> Result<(), DspError> {
    if !sample.i.is_finite() || !sample.q.is_finite() {
        return Err(DspError::new(
            "channelized sample contains a non-finite value",
        ));
    }
    if sample.source_range.is_empty() {
        return Err(DspError::new(
            "channelized sample has an empty source range",
        ));
    }
    Ok(())
}

fn extend_range(current: &mut Option<Range<u64>>, next: Range<u64>) {
    match current {
        Some(range) => {
            range.start = range.start.min(next.start);
            range.end = range.end.max(next.end);
        }
        None => *current = Some(next),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fsk_samples(bits: &[bool], first_sample_index: u64) -> Vec<TimedComplexSample> {
        fsk_samples_with_carrier_offset(bits, first_sample_index, 0.0)
    }

    fn fsk_samples_with_carrier_offset(
        bits: &[bool],
        first_sample_index: u64,
        carrier_offset_hz: f64,
    ) -> Vec<TimedComplexSample> {
        const SAMPLE_RATE_HZ: f64 = 8_000.0;
        const DEVIATION_HZ: f64 = 1_000.0;
        const SAMPLES_PER_SYMBOL: usize = 8;
        let mut phase: f64 = 0.0;
        let mut samples = Vec::new();
        for bit in bits {
            let frequency = carrier_offset_hz + if *bit { DEVIATION_HZ } else { -DEVIATION_HZ };
            for _ in 0..SAMPLES_PER_SYMBOL {
                let index = first_sample_index + samples.len() as u64;
                samples.push(TimedComplexSample {
                    i: phase.cos() as f32,
                    q: phase.sin() as f32,
                    source_range: index..index + 1,
                });
                phase += TAU * frequency / SAMPLE_RATE_HZ;
            }
        }
        let index = first_sample_index + samples.len() as u64;
        samples.push(TimedComplexSample {
            i: phase.cos() as f32,
            q: phase.sin() as f32,
            source_range: index..index + 1,
        });
        samples
    }

    fn fsk_samples_with_clock_error(
        bits: &[bool],
        first_sample_index: u64,
        actual_symbol_rate_hz: f64,
        symbol_phase_samples: f64,
    ) -> Vec<TimedComplexSample> {
        const SAMPLE_RATE_HZ: f64 = 8_000.0;
        const DEVIATION_HZ: f64 = 1_000.0;
        let symbol_period = SAMPLE_RATE_HZ / actual_symbol_rate_hz;
        let mut carrier_phase: f64 = 0.0;
        let mut samples = Vec::new();
        loop {
            let sample_index = samples.len();
            let symbol_index =
                ((sample_index as f64 + symbol_phase_samples) / symbol_period).floor() as usize;
            let Some(bit) = bits.get(symbol_index) else {
                break;
            };
            let source_index = first_sample_index + sample_index as u64;
            samples.push(TimedComplexSample {
                i: carrier_phase.cos() as f32,
                q: carrier_phase.sin() as f32,
                source_range: source_index..source_index + 1,
            });
            let frequency = if *bit { DEVIATION_HZ } else { -DEVIATION_HZ };
            carrier_phase += TAU * frequency / SAMPLE_RATE_HZ;
        }
        let source_index = first_sample_index + samples.len() as u64;
        samples.push(TimedComplexSample {
            i: carrier_phase.cos() as f32,
            q: carrier_phase.sin() as f32,
            source_range: source_index..source_index + 1,
        });
        samples
    }

    fn gfsk_samples(bits: &[bool], first_sample_index: u64, bt: f64) -> Vec<TimedComplexSample> {
        gfsk_samples_with_carrier_offset(bits, first_sample_index, bt, 0.0)
    }

    fn gfsk_samples_with_carrier_offset(
        bits: &[bool],
        first_sample_index: u64,
        bt: f64,
        carrier_offset_hz: f64,
    ) -> Vec<TimedComplexSample> {
        const SAMPLE_RATE_HZ: u64 = 8_000;
        const SYMBOL_RATE_HZ: u64 = 1_000;
        const DEVIATION_HZ: f64 = 1_000.0;
        const SAMPLES_PER_SYMBOL: usize = (SAMPLE_RATE_HZ / SYMBOL_RATE_HZ) as usize;
        let levels = bits
            .iter()
            .flat_map(|bit| {
                std::iter::repeat(if *bit { 1.0 } else { -1.0 }).take(SAMPLES_PER_SYMBOL)
            })
            .collect::<Vec<_>>();
        let taps = design_gaussian_matched_filter(
            SAMPLE_RATE_HZ,
            SYMBOL_RATE_HZ,
            FskModulation::Gfsk { bt },
        )
        .unwrap();
        let midpoint = (taps.len() / 2) as isize;
        let shaped = (0..levels.len())
            .map(|sample_index| {
                taps.iter()
                    .enumerate()
                    .map(|(tap_index, tap)| {
                        let level_index = (sample_index as isize + tap_index as isize - midpoint)
                            .clamp(0, levels.len() as isize - 1)
                            as usize;
                        levels[level_index] * tap
                    })
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();

        let mut phase: f64 = 0.0;
        let mut samples = Vec::with_capacity(shaped.len() + 1);
        for sample_index in 0..=shaped.len() {
            let index = first_sample_index + sample_index as u64;
            samples.push(TimedComplexSample {
                i: phase.cos() as f32,
                q: phase.sin() as f32,
                source_range: index..index + 1,
            });
            if let Some(level) = shaped.get(sample_index) {
                phase += TAU * (carrier_offset_hz + DEVIATION_HZ * level) / SAMPLE_RATE_HZ as f64;
            }
        }
        samples
    }

    fn noise_samples(sample_count: usize, first_sample_index: u64) -> Vec<TimedComplexSample> {
        const SAMPLE_RATE_HZ: f64 = 8_000.0;
        // A quiet channel looks like constant-envelope noise whose
        // instantaneous frequency is zero-mean and spans nearly the whole
        // discriminator range. A deterministic generator keeps the test
        // reproducible.
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut phase: f64 = 0.0;
        (0..sample_count)
            .map(|sample_index| {
                let index = first_sample_index + sample_index as u64;
                let sample = TimedComplexSample {
                    i: phase.cos() as f32,
                    q: phase.sin() as f32,
                    source_range: index..index + 1,
                };
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let uniform = (state >> 11) as f64 / (1_u64 << 53) as f64 * 2.0 - 1.0;
                phase += TAU * 3_900.0 * uniform / SAMPLE_RATE_HZ;
                sample
            })
            .collect()
    }

    fn batch(
        samples: Vec<TimedComplexSample>,
        discontinuity: bool,
        segment_start_sample_index: u64,
    ) -> ChannelizedBatch {
        ChannelizedBatch {
            source_sample_rate_hz: 8_000,
            sample_rate_hz: 8_000,
            center_frequency_hz: 100_000_000,
            segment_start_sample_index,
            filter_delay_input_samples: 0,
            discontinuity,
            samples,
        }
    }

    fn demodulator() -> FskDemodulator {
        FskDemodulator::new(FskConfig {
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            modulation: FskModulation::Fsk,
            polarity: FskPolarity::Normal,
            timing: FskTiming::Fixed,
        })
        .unwrap()
    }

    #[test]
    fn demodulates_binary_fsk_into_timed_soft_bits() {
        let expected = [true, false, true, true, false];
        let mut demodulator = demodulator();
        let bits = demodulator
            .push(&batch(fsk_samples(&expected, 0), true, 0))
            .unwrap();

        assert_eq!(bits.len(), expected.len());
        assert_eq!(
            bits.iter()
                .map(|bit| bit.likelihood.is_sign_positive())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(bits.iter().all(|bit| bit.likelihood.abs() > 0.99));
        assert_eq!(bits[0].source_range, 0..9);
    }

    #[test]
    fn demodulation_is_independent_of_channelized_batch_boundaries() {
        let expected = [true, false, false, true];
        let samples = fsk_samples(&expected, 0);
        let mut whole = demodulator();
        let whole_bits = whole.push(&batch(samples.clone(), true, 0)).unwrap();

        let mut chunked = demodulator();
        let first = chunked
            .push(&batch(samples[..7].to_vec(), true, 0))
            .unwrap();
        let second = chunked
            .push(&batch(samples[7..19].to_vec(), false, 0))
            .unwrap();
        let third = chunked
            .push(&batch(samples[19..].to_vec(), false, 0))
            .unwrap();
        let chunked_bits = first
            .into_iter()
            .chain(second)
            .chain(third)
            .collect::<Vec<_>>();

        assert_eq!(chunked_bits, whole_bits);
    }

    #[test]
    fn discontinuity_drops_a_partial_symbol() {
        let samples = fsk_samples(&[true], 0);
        let mut demodulator = demodulator();
        assert!(demodulator
            .push(&batch(samples[..4].to_vec(), true, 0))
            .unwrap()
            .is_empty());

        let reset_samples = fsk_samples(&[false], 100);
        let bits = demodulator.push(&batch(reset_samples, true, 100)).unwrap();
        assert_eq!(bits.len(), 1);
        assert!(bits[0].likelihood.is_sign_negative());
        assert!(bits[0].source_range.start >= 100);
    }

    #[test]
    fn inverted_polarity_reverses_the_hard_decision() {
        let mut demodulator = FskDemodulator::new(FskConfig {
            polarity: FskPolarity::Inverted,
            ..demodulator().config().clone()
        })
        .unwrap();
        let bits = demodulator
            .push(&batch(fsk_samples(&[true], 0), true, 0))
            .unwrap();
        assert!(bits[0].likelihood.is_sign_negative());
    }

    #[test]
    fn gfsk_matched_filter_decodes_shaped_symbols_across_batches() {
        let expected = [
            true, true, true, false, true, false, false, true, true, false, true, false, false,
            false,
        ];
        let samples = gfsk_samples(&expected, 0, 0.5);
        let make_demodulator = || {
            FskDemodulator::new(FskConfig {
                symbol_rate_hz: 1_000,
                deviation_hz: 1_000,
                modulation: FskModulation::Gfsk { bt: 0.5 },
                polarity: FskPolarity::Normal,
                timing: FskTiming::Fixed,
            })
            .unwrap()
        };

        let mut whole = make_demodulator();
        let whole_bits = whole.push(&batch(samples.clone(), true, 0)).unwrap();

        let mut chunked = make_demodulator();
        let chunked_bits = [0..13, 13..41, 41..76, 76..samples.len()]
            .into_iter()
            .enumerate()
            .flat_map(|(index, range)| {
                chunked
                    .push(&batch(samples[range].to_vec(), index == 0, 0))
                    .unwrap()
            })
            .collect::<Vec<_>>();

        assert_eq!(chunked_bits, whole_bits);
        assert_eq!(
            whole_bits
                .iter()
                .map(|bit| bit.likelihood.is_sign_positive())
                .collect::<Vec<_>>(),
            expected[3..expected.len() - 2]
        );
        assert!(whole_bits.iter().all(|bit| bit.likelihood.abs() > 0.35));
    }

    #[test]
    fn rejects_invalid_gfsk_bt() {
        for bt in [0.0, -0.5, f64::NAN] {
            assert!(FskDemodulator::new(FskConfig {
                symbol_rate_hz: 1_000,
                deviation_hz: 1_000,
                modulation: FskModulation::Gfsk { bt },
                polarity: FskPolarity::Normal,
                timing: FskTiming::Fixed,
            })
            .is_err());
        }
    }

    #[test]
    fn generic_demodulator_contract_exposes_binary_soft_symbols() {
        let expected = [true, false, true];
        let mut demodulator: Box<dyn Demodulator> = Box::new(demodulator());
        let output = demodulator
            .push(&batch(fsk_samples(&expected, 0), true, 0))
            .unwrap();
        let DemodulatedBatch::SoftSymbols(symbols) = output else {
            panic!("binary FSK must emit soft symbols");
        };

        assert!(symbols.validate().is_ok());
        assert_eq!(symbols.alphabet_size, 2);
        assert!(symbols.discontinuity);
        assert_eq!(
            symbols
                .symbols
                .iter()
                .map(TimedSoftSymbol::decision)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(0), Some(1)]
        );
    }

    #[test]
    fn adaptive_timing_tracks_clock_error_across_batches() {
        let expected = [
            true, false, true, false, true, false, true, false, true, true, false, true, false,
            false, true, true, false, false, false, true, false, true, true, true, false, true,
            false, false, true, false, true, false, false, true, true, false, true, true, false,
            false,
        ];
        let actual_symbol_rate_hz = 1_004.0;
        let samples = fsk_samples_with_clock_error(&expected, 0, actual_symbol_rate_hz, 2.7);
        let make_demodulator = || {
            FskDemodulator::new(FskConfig {
                symbol_rate_hz: 1_000,
                deviation_hz: 1_000,
                modulation: FskModulation::Fsk,
                polarity: FskPolarity::Normal,
                timing: FskTiming::Adaptive {
                    loop_bandwidth: 0.2,
                    max_clock_error_ppm: 10_000,
                },
            })
            .unwrap()
        };

        let mut whole = make_demodulator();
        let whole_bits = whole.push(&batch(samples.clone(), true, 0)).unwrap();

        let mut chunked = make_demodulator();
        let chunked_bits = [0..17, 17..89, 89..211, 211..samples.len()]
            .into_iter()
            .enumerate()
            .flat_map(|(index, range)| {
                chunked
                    .push(&batch(samples[range].to_vec(), index == 0, 0))
                    .unwrap()
            })
            .collect::<Vec<_>>();

        assert_eq!(chunked_bits, whole_bits);
        assert!(whole.timing_locked());
        assert!(chunked.timing_locked());
        let estimated_symbol_rate_hz = whole.estimated_symbol_rate_hz().unwrap();
        assert!(
            (estimated_symbol_rate_hz - actual_symbol_rate_hz).abs() < 5.0,
            "estimated {estimated_symbol_rate_hz} Hz"
        );
        let decisions = whole_bits
            .iter()
            .map(|bit| bit.likelihood.is_sign_positive())
            .collect::<Vec<_>>();
        // M&M begins producing decisions immediately and converges without
        // consuming a separate carrier-acquisition prefix.
        assert!(decisions.len() >= expected.len() - 10);
        assert!(expected
            .windows(decisions.len())
            .any(|window| window == decisions));
    }

    #[test]
    fn adaptive_timing_follows_the_gnu_radio_gfsk_chain() {
        let expected = (0..48).map(|index| index % 2 == 0).collect::<Vec<_>>();
        let samples = gfsk_samples(&expected, 0, 0.5);
        let mut demodulator = FskDemodulator::new(FskConfig {
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            modulation: FskModulation::Gfsk { bt: 0.5 },
            polarity: FskPolarity::Normal,
            timing: FskTiming::Adaptive {
                loop_bandwidth: 0.2,
                max_clock_error_ppm: 10_000,
            },
        })
        .unwrap();

        let bits = demodulator.push(&batch(samples, true, 0)).unwrap();
        let decisions = bits
            .iter()
            .map(|bit| bit.likelihood.is_sign_positive())
            .collect::<Vec<_>>();

        assert!(
            demodulator.timing_locked(),
            "timing quality {}, carrier {:?}/{:?} Hz, recovered {} decisions: {decisions:?}",
            demodulator.timing_lock_quality(),
            demodulator.estimated_frequency_offset_hz(),
            demodulator.estimated_tone_separation_hz(),
            decisions.len()
        );
        assert!(decisions.len() > 35);
        assert!(expected
            .windows(decisions.len())
            .any(|window| window == decisions));
    }

    #[test]
    fn adaptive_gfsk_preserves_a_ten_byte_preamble_for_framing() {
        let preamble = (0..80).map(|index| index % 2 == 0).collect::<Vec<_>>();
        let payload = [
            false, false, true, false, true, true, false, true, true, false, false, true, false,
            true, true, false,
        ];
        let expected = preamble.iter().copied().chain(payload).collect::<Vec<_>>();
        let samples = gfsk_samples(&expected, 0, 0.5);
        let mut demodulator = FskDemodulator::new(FskConfig {
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            modulation: FskModulation::Gfsk { bt: 0.5 },
            polarity: FskPolarity::Normal,
            timing: FskTiming::Adaptive {
                loop_bandwidth: 0.2,
                max_clock_error_ppm: 10_000,
            },
        })
        .unwrap();

        let decisions = demodulator
            .push(&batch(samples, true, 0))
            .unwrap()
            .into_iter()
            .map(|bit| bit.likelihood.is_sign_positive())
            .collect::<Vec<_>>();

        assert!(
            decisions
                .windows(preamble.len())
                .any(|window| window == preamble),
            "ten-byte preamble was not retained in {decisions:?}"
        );
    }

    #[test]
    fn adaptive_gfsk_removes_carrier_offset_before_timing_and_slicing() {
        let preamble = (0..80).map(|index| index % 2 == 0).collect::<Vec<_>>();
        let payload = [
            true, true, false, false, true, false, false, false, true, false, true, true, false,
            true, false, false,
        ];
        let expected = preamble.iter().copied().chain(payload).collect::<Vec<_>>();
        // The centred Gaussian receive filter spans two symbols on each side,
        // so the last two symbols are decided only once later samples arrive.
        // A real burst is followed by a quiet channel; model that with two
        // trailing symbols.
        let transmitted = expected
            .iter()
            .copied()
            .chain([false, true])
            .collect::<Vec<_>>();
        // The residual offset is larger than the configured deviation, so a
        // zero slicer without carrier/DC correction sees both tones as one.
        let samples = gfsk_samples_with_carrier_offset(&transmitted, 0, 0.5, 1_200.0);
        let mut demodulator = FskDemodulator::new(FskConfig {
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            modulation: FskModulation::Gfsk { bt: 0.5 },
            polarity: FskPolarity::Normal,
            timing: FskTiming::Adaptive {
                loop_bandwidth: 0.2,
                max_clock_error_ppm: 10_000,
            },
        })
        .unwrap();

        let decisions = demodulator
            .push(&batch(samples, true, 0))
            .unwrap()
            .into_iter()
            .map(|bit| bit.likelihood.is_sign_positive())
            .collect::<Vec<_>>();

        assert!(
            decisions
                .windows(expected.len())
                .any(|window| window == expected),
            "carrier-corrected packet was not retained in {decisions:?}"
        );
        assert!((demodulator.estimated_frequency_offset_hz().unwrap() - 1_200.0).abs() < 100.0);
    }

    #[test]
    fn adaptive_fsk_removes_carrier_offset_before_timing_and_slicing() {
        let expected = (0..80).map(|index| index % 2 == 0).collect::<Vec<_>>();
        // The centred one-symbol average needs half a symbol of look-ahead, so
        // the last symbol is decided only once later samples arrive. A real
        // burst is followed by a quiet channel; model that with two trailing
        // symbols.
        let transmitted = expected
            .iter()
            .copied()
            .chain([true, false])
            .collect::<Vec<_>>();
        let samples = fsk_samples_with_carrier_offset(&transmitted, 0, 1_200.0);
        let mut demodulator = FskDemodulator::new(FskConfig {
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            modulation: FskModulation::Fsk,
            polarity: FskPolarity::Normal,
            timing: FskTiming::Adaptive {
                loop_bandwidth: 0.2,
                max_clock_error_ppm: 10_000,
            },
        })
        .unwrap();

        let decisions = demodulator
            .push(&batch(samples, true, 0))
            .unwrap()
            .into_iter()
            .map(|bit| bit.likelihood.is_sign_positive())
            .collect::<Vec<_>>();

        assert!(
            decisions
                .windows(expected.len())
                .any(|window| window == expected),
            "carrier-corrected preamble was not retained in {decisions:?}"
        );
    }

    #[test]
    fn adaptive_gfsk_does_not_acquire_noise_before_the_preamble() {
        let expected = (0..80).map(|index| index % 2 == 0).collect::<Vec<_>>();
        // Two trailing symbols stand in for the quiet channel after a burst;
        // the centred receive filter needs them to decide the last symbols.
        let transmitted = expected
            .iter()
            .copied()
            .chain([true, false])
            .collect::<Vec<_>>();
        let mut samples = noise_samples(64, 0);
        samples.extend(gfsk_samples(&transmitted, samples.len() as u64, 0.5));
        // An ideal alternating preamble gives Mueller-and-Muller no timing
        // error, so nothing corrects the clock bias that the few noise
        // symbols at the start of the replayed window introduce. The GUI's
        // default loop bandwidth keeps that bias small enough for 80 symbols.
        let mut demodulator = FskDemodulator::new(FskConfig {
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            modulation: FskModulation::Gfsk { bt: 0.5 },
            polarity: FskPolarity::Normal,
            timing: FskTiming::Adaptive {
                loop_bandwidth: 0.1,
                max_clock_error_ppm: 10_000,
            },
        })
        .unwrap();

        let decisions = demodulator
            .push(&batch(samples, true, 0))
            .unwrap()
            .into_iter()
            .map(|bit| bit.likelihood.is_sign_positive())
            .collect::<Vec<_>>();

        assert!(
            decisions
                .windows(expected.len())
                .any(|window| window == expected),
            "preamble after noise was not retained in {decisions:?}"
        );
    }

    /// Decision magnitudes `1 + noise` with a deterministic uniform noise of
    /// the given standard deviation, alternating in sign.
    fn noisy_decisions(count: usize, sd: f64) -> Vec<f64> {
        (0..count)
            .map(|index| {
                let uniform = (index as f64 * 0.754_877_666_2).fract() - 0.5;
                let value = 1.0 + uniform * sd * 12.0_f64.sqrt();
                if index % 2 == 0 {
                    value
                } else {
                    -value
                }
            })
            .collect()
    }

    #[test]
    fn clean_decisions_are_fully_confident_with_an_open_eye() {
        let mut statistics = DecisionStatistics::default();
        for value in noisy_decisions(200, 0.0) {
            statistics.observe(value);
        }
        assert!(statistics.soft_decision(1.0) > 0.999);
        assert!(statistics.soft_decision(-0.4) < -0.999);
        assert!(statistics.eye_opening() > 0.85);
        assert!(statistics.is_open());
    }

    #[test]
    fn confidence_is_relative_to_the_noise_of_the_burst() {
        let mut quiet = DecisionStatistics::default();
        let mut noisy = DecisionStatistics::default();
        for (quiet_value, noisy_value) in noisy_decisions(300, 0.05)
            .into_iter()
            .zip(noisy_decisions(300, 0.3))
        {
            quiet.observe(quiet_value);
            noisy.observe(noisy_value);
        }
        // The same sample 0.2 from the slicer is a safe decision on the quiet
        // link and a doubtful one on the noisy link.
        assert!(
            quiet.soft_decision(0.2) > 0.99,
            "{}",
            quiet.soft_decision(0.2)
        );
        let doubtful = noisy.soft_decision(0.2);
        assert!((0.3..0.7).contains(&doubtful), "{doubtful}");
        assert_eq!(noisy.soft_decision(-0.2), -doubtful);
        assert!(quiet.eye_opening() > noisy.eye_opening() + 0.3);
        assert!(quiet.snr_db().unwrap() > noisy.snr_db().unwrap() + 8.0);
    }

    #[test]
    fn decisions_hugging_the_slicer_close_the_eye() {
        let mut statistics = DecisionStatistics::default();
        for index in 0..100 {
            statistics.observe(if index % 2 == 0 { 0.08 } else { -0.05 });
        }
        assert!(!statistics.is_open());
        assert!(statistics.eye_opening() < EYE_OPEN_THRESHOLD);
        // Too few symbols to say anything.
        assert_eq!(DecisionStatistics::default().eye_opening(), 0.0);
        assert!(DecisionStatistics::default().snr_db().is_none());
    }

    #[test]
    fn a_clean_adaptive_burst_reports_an_open_eye_and_confident_bits() {
        let transmitted = (0..120)
            .map(|index| index % 3 != 0)
            .chain([false, true, false])
            .collect::<Vec<_>>();
        let mut demodulator = FskDemodulator::new(FskConfig {
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            modulation: FskModulation::Gfsk { bt: 0.5 },
            polarity: FskPolarity::Normal,
            timing: FskTiming::Adaptive {
                loop_bandwidth: 0.1,
                max_clock_error_ppm: 10_000,
            },
        })
        .unwrap();
        assert!(!demodulator.timing_locked());
        assert_eq!(demodulator.timing_lock_quality(), 0.0);
        assert!(demodulator.decision_snr_db().is_none());

        let bits = demodulator
            .push(&batch(gfsk_samples(&transmitted, 0, 0.5), true, 0))
            .unwrap();

        assert!(demodulator.timing_locked());
        assert!(demodulator.timing_lock_quality() > 0.5);
        assert!(demodulator.decision_snr_db().unwrap() > 15.0);
        let confident = bits
            .iter()
            .skip(20)
            .filter(|bit| bit.likelihood.abs() > 0.99)
            .count();
        assert!(
            confident + 5 >= bits.len() - 20,
            "{confident} of {}",
            bits.len()
        );

        demodulator.reset();
        assert!(!demodulator.timing_locked());
        assert!(demodulator.decision_snr_db().is_none());
    }

    #[test]
    fn noise_alone_is_never_reported_as_locked() {
        let mut demodulator = FskDemodulator::new(FskConfig {
            symbol_rate_hz: 1_000,
            deviation_hz: 1_000,
            modulation: FskModulation::Gfsk { bt: 0.5 },
            polarity: FskPolarity::Normal,
            timing: FskTiming::Adaptive {
                loop_bandwidth: 0.1,
                max_clock_error_ppm: 10_000,
            },
        })
        .unwrap();
        let bits = demodulator
            .push(&batch(noise_samples(4_000, 0), true, 0))
            .unwrap();
        assert!(bits.is_empty());
        assert!(!demodulator.timing_locked());
        assert_eq!(demodulator.timing_lock_quality(), 0.0);
    }

    #[test]
    fn fixed_timing_has_no_clock_to_lose() {
        let mut demodulator = demodulator();
        assert!(demodulator.timing_locked());
        assert_eq!(demodulator.timing_lock_quality(), 1.0);
        let bits = [true, false, true, true, false, false, true, false];
        let decoded = demodulator
            .push(&batch(fsk_samples(&bits, 0), true, 0))
            .unwrap();
        assert!(decoded.iter().all(|bit| bit.likelihood.abs() > 0.99));
        assert!(demodulator.decision_snr_db().is_none());
    }

    #[test]
    fn carson_bandwidth_covers_the_deviation_and_half_the_bit_rate_each_side() {
        assert_eq!(fsk_carson_bandwidth_hz(19_200, 35_000), 89_200);
        assert_eq!(fsk_carson_bandwidth_hz(100_000, 50_000), 200_000);
        assert_eq!(fsk_carson_bandwidth_hz(u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn recommended_channel_adds_a_carrier_tolerance_in_whole_steps() {
        assert_eq!(
            recommended_fsk_channel_bandwidth_hz(19_200, 35_000),
            120_000
        );
        assert_eq!(
            recommended_fsk_channel_bandwidth_hz(100_000, 50_000),
            230_000
        );
        let narrow = recommended_fsk_channel_bandwidth_hz(1_200, 5_000);
        assert_eq!(narrow, 45_000);
        assert!(narrow >= fsk_carson_bandwidth_hz(1_200, 5_000) + 30_000);
    }
}
