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
/// Adaptive timing follows GNU Radio's GFSK receive ordering: quadrature
/// discriminator, carrier-offset removal, Mueller-and-Muller clock recovery,
/// then a zero slicer. Fixed timing retains the optional GFSK receive filter.
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
    gaussian_filter: Vec<f64>,
    gaussian_history: VecDeque<DiscriminatorSample>,
    carrier_acquisition: VecDeque<DiscriminatorSample>,
    timing_recovery: Option<MuellerMullerClockRecovery>,
    tone_tracker: ToneTracker,
}

#[derive(Clone, Debug)]
struct DiscriminatorSample {
    likelihood: f64,
    effective_source_index: u64,
    source_range: Range<u64>,
}

#[derive(Clone, Debug, Default)]
struct ToneTracker {
    acquisition: VecDeque<f64>,
    candidate_lower: Option<f64>,
    candidate_upper: Option<f64>,
    stable_candidate_samples: usize,
    lower: Option<f64>,
    upper: Option<f64>,
}

impl ToneTracker {
    fn reset(&mut self) {
        self.acquisition.clear();
        self.candidate_lower = None;
        self.candidate_upper = None;
        self.stable_candidate_samples = 0;
        self.lower = None;
        self.upper = None;
    }

    fn normalize(&mut self, value: f64, acquisition_samples: usize) -> Option<f64> {
        if self.lower.is_none() || self.upper.is_none() {
            self.acquisition.push_back(value);
            while self.acquisition.len() > acquisition_samples {
                self.acquisition.pop_front();
            }
            if self.acquisition.len() < acquisition_samples {
                return None;
            }
            let mut sorted = self.acquisition.iter().copied().collect::<Vec<_>>();
            sorted.sort_by(f64::total_cmp);
            // GFSK spends many discriminator samples on shaped transitions.
            // Use the outer deciles so the tone centres are not pulled toward
            // the midpoint by those transition samples.
            let cluster_samples = (sorted.len() / 10).max(4).min(sorted.len() / 2);
            let lower = sorted[..cluster_samples].iter().sum::<f64>() / cluster_samples as f64;
            let upper = sorted[sorted.len() - cluster_samples..].iter().sum::<f64>()
                / cluster_samples as f64;
            let separation = upper - lower;
            // Random phase in a quiet channel commonly spans most of the
            // discriminator's Nyquist range. Do not mistake that wide spread
            // for the two configured FSK tones.
            if !(MIN_TONE_SEPARATION..=MAX_TONE_SEPARATION).contains(&separation) {
                self.candidate_lower = None;
                self.candidate_upper = None;
                self.stable_candidate_samples = 0;
                return None;
            }
            let stable_tolerance = separation * 0.1;
            // A burst can start partway through the retained FFT window. Wait
            // for the candidate centers to settle as noise leaves the rolling
            // window before committing to carrier correction.
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
            if self.stable_candidate_samples < (acquisition_samples / 2).max(4) {
                return None;
            }
            self.lower = Some(lower);
            self.upper = Some(upper);
            self.acquisition.clear();
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
            gaussian_filter: Vec::new(),
            gaussian_history: VecDeque::new(),
            carrier_acquisition: VecDeque::new(),
            timing_recovery,
            tone_tracker: ToneTracker::default(),
        })
    }

    pub fn config(&self) -> &FskConfig {
        &self.config
    }

    pub fn timing_locked(&self) -> bool {
        match &self.timing_recovery {
            Some(timing_recovery) => timing_recovery.is_locked(),
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

    pub fn timing_lock_quality(&self) -> f32 {
        self.timing_recovery
            .as_ref()
            .map_or(1.0, MuellerMullerClockRecovery::lock_quality)
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
        self.gaussian_filter.clear();
        self.gaussian_history.clear();
        self.carrier_acquisition.clear();
        self.tone_tracker.reset();
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
        if matches!(self.config.modulation, FskModulation::Gfsk { .. })
            && self.config.timing == FskTiming::Fixed
            && self.gaussian_filter.is_empty()
        {
            self.gaussian_filter = design_gaussian_matched_filter(
                batch.sample_rate_hz,
                self.config.symbol_rate_hz,
                self.config.modulation,
            )?;
            self.gaussian_history = VecDeque::with_capacity(self.gaussian_filter.len());
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
            let discriminator = DiscriminatorSample {
                likelihood: frequency_hz / self.config.deviation_hz as f64,
                effective_source_index: effective_index,
                source_range: previous.source_range.start..sample.source_range.end,
            };
            for mut discriminator in self.filter_discriminator(discriminator) {
                if matches!(self.config.timing, FskTiming::Adaptive { .. }) {
                    let acquisition_samples = carrier_acquisition_sample_count(
                        batch.sample_rate_hz,
                        self.config.symbol_rate_hz,
                    );
                    if !self.tone_tracker.is_acquired() {
                        self.carrier_acquisition.push_back(discriminator);
                        while self.carrier_acquisition.len() > acquisition_samples.saturating_mul(2)
                        {
                            self.carrier_acquisition.pop_front();
                        }
                        let Some(latest) = self.carrier_acquisition.back() else {
                            continue;
                        };
                        if self
                            .tone_tracker
                            .normalize(latest.likelihood, acquisition_samples)
                            .is_none()
                        {
                            continue;
                        }
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
                            timing_samples.push(TimedScalarSample {
                                value: acquired.likelihood as f32,
                                source_range: acquired.source_range,
                            });
                        }
                        continue;
                    }
                    discriminator.likelihood = self
                        .tone_tracker
                        .normalize(discriminator.likelihood, acquisition_samples)
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
            bits.extend(
                timing_recovery
                    .push(&timing_samples, batch.sample_rate_hz, false)?
                    .into_iter()
                    .map(|symbol| TimedSoftBit {
                        likelihood: symbol.value.clamp(-1.0, 1.0),
                        source_range: symbol.source_range,
                    }),
            );
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
            let likelihood = (self.likelihood_sum / self.likelihood_count as f64).clamp(-1.0, 1.0);
            if let Some(source_range) = self.symbol_range.take() {
                bits.push(TimedSoftBit {
                    likelihood: likelihood as f32,
                    source_range,
                });
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

    fn filter_discriminator(&mut self, sample: DiscriminatorSample) -> Vec<DiscriminatorSample> {
        if matches!(self.config.modulation, FskModulation::Fsk)
            || matches!(self.config.timing, FskTiming::Adaptive { .. })
        {
            return vec![sample];
        }

        self.gaussian_history.push_back(sample);
        if self.gaussian_history.len() < self.gaussian_filter.len() {
            return Vec::new();
        }
        let midpoint = self.gaussian_filter.len() / 2;
        let history_start = self
            .gaussian_history
            .front()
            .map(|sample| sample.source_range.start)
            .unwrap_or_default();
        let outputs = (midpoint..=midpoint)
            .map(|output_index| {
                let likelihood = self
                    .gaussian_filter
                    .iter()
                    .enumerate()
                    .map(|(tap_index, coefficient)| {
                        let history_index = (output_index as isize + tap_index as isize
                            - midpoint as isize)
                            .clamp(0, self.gaussian_history.len() as isize - 1)
                            as usize;
                        self.gaussian_history[history_index].likelihood * coefficient
                    })
                    .sum();
                let range_end_index =
                    (output_index + midpoint).min(self.gaussian_history.len().saturating_sub(1));
                DiscriminatorSample {
                    likelihood,
                    effective_source_index: self.gaussian_history[output_index]
                        .effective_source_index,
                    source_range: history_start
                        ..self.gaussian_history[range_end_index].source_range.end,
                }
            })
            .collect();
        self.gaussian_history.pop_front();
        outputs
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
        if matches!(self.config.modulation, FskModulation::Gfsk { .. })
            && self.config.timing == FskTiming::Fixed
        {
            gaussian_filter_tap_count(batch.sample_rate_hz, self.config.symbol_rate_hz)?;
        }
        Ok(())
    }
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
        const FREQUENCIES_HZ: [f64; 8] = [
            -3_900.0, -3_000.0, -2_500.0, -3_600.0, 1_000.0, -2_800.0, -3_400.0, 500.0,
        ];
        let mut phase: f64 = 0.0;
        (0..sample_count)
            .map(|sample_index| {
                let index = first_sample_index + sample_index as u64;
                let sample = TimedComplexSample {
                    i: phase.cos() as f32,
                    q: phase.sin() as f32,
                    source_range: index..index + 1,
                };
                phase += TAU * FREQUENCIES_HZ[sample_index % FREQUENCIES_HZ.len()] / SAMPLE_RATE_HZ;
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
        // The residual offset is larger than the configured deviation, so a
        // zero slicer without carrier/DC correction sees both tones as one.
        let samples = gfsk_samples_with_carrier_offset(&expected, 0, 0.5, 1_200.0);
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
        let samples = fsk_samples_with_carrier_offset(&expected, 0, 1_200.0);
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
        let mut samples = noise_samples(64, 0);
        samples.extend(gfsk_samples(&expected, samples.len() as u64, 0.5));
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
            "preamble after noise was not retained in {decisions:?}"
        );
    }
}
