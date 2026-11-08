//! Sample-accurate detection of abrupt carrier level changes in a channel.
//!
//! A channel can hold several transmissions one after the other: a strong
//! burst, then a weak one from another transmitter a millisecond later. The
//! spectrum-window squelch cannot tell them apart or find where a burst
//! stops, but the level of the filtered channel shows both as a step. The
//! FSK/GFSK envelope is constant, so a step of several dB is a different
//! signal, not modulation.
//!
//! The envelope is not perfectly constant after the channel filter: a tone
//! away from the channel centre is attenuated, so a strong burst can swing by
//! four to five dB inside itself. A step that small is therefore only trusted
//! at the noise floor, where a carrier begins or ends; see
//! [`LevelStepSegmenter`].

use crate::{ChannelizedBatch, TimedComplexSample};
use std::collections::VecDeque;

/// Length of each of the two windows whose mean powers are compared, in
/// symbols. Long enough that the power of a noisy weak signal is steady,
/// short enough that two bursts a millisecond apart do not share a window.
const WINDOW_SYMBOLS: f64 = 3.0;
/// Smallest power ratio between the windows that counts as a step. The mean
/// power of a window of noise varies by about one dB, so this is several times
/// that; a signal only a few dB above the noise has no detectable edge.
const MIN_STEP_DB: f64 = 6.0;
/// How far below its peak the level ratio falls before the step is taken to be
/// at the peak. For an ideal step the ratio peaks exactly at the step.
const PEAK_FALL_DB: f64 = 1.5;
/// Smallest power ratio that counts as a step when one side of it is at the
/// noise floor. Swings inside a carrier stay well above the floor, so they
/// cannot reach this; a carrier a few dB over the noise can.
const FLOOR_STEP_DB: f64 = 4.0;
/// How far above the quietest window seen a window may be and still count as
/// at the noise floor. The quietest of many windows of noise lies a little
/// below their mean, which this allows for.
const FLOOR_MARGIN_DB: f64 = 4.5;
/// A window this far above the quietest one shows that the quietest is the
/// noise floor and not a weak carrier: only a carrier stands that far over it.
const FLOOR_TRUST_DB: f64 = 6.0;
/// How fast the remembered floor may rise, so that a change of gain is
/// followed without a burst's own level being taken for it.
const FLOOR_RISE_DB_PER_S: f64 = 2.0;
/// Added to window powers so silence compares as no change instead of 0/0.
const POWER_EPSILON: f64 = 1e-30;

fn db_to_power(db: f64) -> f64 {
    10f64.powf(db / 10.0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LevelStep {
    /// The channel became at least [`MIN_STEP_DB`] louder.
    Rise,
    /// The channel became at least [`MIN_STEP_DB`] quieter.
    Drop,
}

/// A run of channel samples with a steady level.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelSegment {
    /// The step this segment starts at, if it starts at one. `None` for the
    /// continuation of a segment begun by an earlier call.
    pub step: Option<LevelStep>,
    /// The samples. [`ChannelizedBatch::discontinuity`] is set when the
    /// segment follows a gap in the stream.
    pub batch: ChannelizedBatch,
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    /// Absolute index of the first sample after the step.
    position: u64,
    step_db: f64,
    /// Evaluations since the ratio last reached a new peak.
    since_peak: usize,
}

/// Splits a channel's samples where its power steps up or down.
///
/// The level before and after each sample is compared over windows of a few
/// symbols and the sharpest change is reported at its exact sample. Deciding
/// needs the window after the step, so samples are released with a delay of
/// one window; they are neither dropped nor changed.
///
/// A change of [`MIN_STEP_DB`] is a step anywhere. A change of
/// [`FLOOR_STEP_DB`] is one if the quieter side is at the noise floor, the
/// quietest level seen on the channel, once that is known to be noise:
/// a window [`FLOOR_TRUST_DB`] above it has been seen. This finds the edges of
/// weak carriers without taking the swings inside a strong one for steps.
/// The floor is a property of the channel, so it survives a gap in the stream.
pub struct LevelStepSegmenter {
    symbol_rate_hz: u64,
    sample_rate_hz: u64,
    window: usize,
    /// Samples not yet released, oldest first.
    held: VecDeque<TimedComplexSample>,
    /// Absolute index of `held[0]`.
    held_start: u64,
    /// Total samples taken in.
    pushed: u64,
    /// Powers of the latest samples since the last step, at most two windows.
    powers: VecDeque<f64>,
    /// Sum of the older window of `powers`.
    sum_before: f64,
    /// Sum of the newer window of `powers`.
    sum_after: f64,
    candidate: Option<Candidate>,
    /// Mean power of the quietest window seen on the channel.
    floor: Option<f64>,
    /// Whether a window far above `floor` has been seen, so that it is noise.
    floor_trusted: bool,
    /// Factor by which `floor` may rise per sample.
    floor_growth: f64,
    pending_step: Option<LevelStep>,
    pending_discontinuity: bool,
}

impl LevelStepSegmenter {
    pub fn new(symbol_rate_hz: u64) -> Self {
        Self {
            symbol_rate_hz: symbol_rate_hz.max(1),
            sample_rate_hz: 0,
            window: 0,
            held: VecDeque::new(),
            held_start: 0,
            pushed: 0,
            powers: VecDeque::new(),
            sum_before: 0.0,
            sum_after: 0.0,
            candidate: None,
            floor: None,
            floor_trusted: false,
            floor_growth: 1.0,
            pending_step: None,
            pending_discontinuity: false,
        }
    }

    /// Forgets the stream, including samples still held back, and what was
    /// learned about the channel's noise floor.
    pub fn reset(&mut self) {
        self.forget_stream();
        self.floor = None;
        self.floor_trusted = false;
    }

    /// Forgets the stream and keeps the noise floor.
    fn forget_stream(&mut self) {
        self.sample_rate_hz = 0;
        self.window = 0;
        self.held.clear();
        self.held_start = 0;
        self.pushed = 0;
        self.powers.clear();
        self.sum_before = 0.0;
        self.sum_after = 0.0;
        self.candidate = None;
        self.pending_step = None;
        self.pending_discontinuity = false;
    }

    /// Takes in a batch and returns the samples that are now certain to lie
    /// before the next step, split at every step found.
    pub fn push(&mut self, batch: &ChannelizedBatch) -> Vec<ChannelSegment> {
        if batch.discontinuity || batch.sample_rate_hz != self.sample_rate_hz {
            if batch.sample_rate_hz != self.sample_rate_hz {
                self.reset();
            } else {
                self.forget_stream();
            }
            self.sample_rate_hz = batch.sample_rate_hz;
            self.floor_growth =
                10f64.powf(FLOOR_RISE_DB_PER_S / 10.0 / batch.sample_rate_hz.max(1) as f64);
            self.window = ((WINDOW_SYMBOLS * batch.sample_rate_hz as f64
                / self.symbol_rate_hz as f64)
                .ceil() as usize)
                .max(2);
            self.pending_discontinuity = batch.discontinuity;
        }
        let mut segments = Vec::new();
        let mut current = Vec::new();
        for sample in &batch.samples {
            let power = f64::from(sample.i).powi(2) + f64::from(sample.q).powi(2);
            self.held.push_back(sample.clone());
            self.pushed += 1;
            if let Some((position, step)) = self.observe(power) {
                self.release_until(position, &mut current);
                self.close_segment(batch, &mut current, &mut segments);
                self.pending_step = Some(step);
                self.restart_statistics_at(position);
            }
            let limit = self.candidate.map_or(
                self.pushed.saturating_sub(self.window as u64),
                |candidate| candidate.position,
            );
            self.release_until(limit, &mut current);
        }
        self.close_segment(batch, &mut current, &mut segments);
        segments
    }

    fn release_until(&mut self, limit: u64, out: &mut Vec<TimedComplexSample>) {
        while self.held_start < limit {
            let Some(sample) = self.held.pop_front() else {
                break;
            };
            out.push(sample);
            self.held_start += 1;
        }
    }

    fn close_segment(
        &mut self,
        batch: &ChannelizedBatch,
        samples: &mut Vec<TimedComplexSample>,
        segments: &mut Vec<ChannelSegment>,
    ) {
        if samples.is_empty() {
            return;
        }
        segments.push(ChannelSegment {
            step: self.pending_step.take(),
            batch: ChannelizedBatch {
                discontinuity: std::mem::take(&mut self.pending_discontinuity),
                samples: std::mem::take(samples),
                ..batch.clone_header()
            },
        });
    }

    /// Takes in one sample's power. Returns the absolute index of the first
    /// sample after a step, once the step is certain to have peaked.
    fn observe(&mut self, power: f64) -> Option<(u64, LevelStep)> {
        let window = self.window;
        self.powers.push_back(power);
        self.sum_after += power;
        let length = self.powers.len();
        if length > window {
            let moved = self.powers[length - 1 - window];
            self.sum_after -= moved;
            self.sum_before += moved;
        }
        if length > 2 * window {
            if let Some(gone) = self.powers.pop_front() {
                self.sum_before -= gone;
            }
        }
        // Sliding sums drift; start them afresh now and then.
        if self.pushed % (16 * window as u64) == 0 {
            self.recompute_sums();
        }
        if self.powers.len() < 2 * window {
            return None;
        }

        let at_floor = self.track_floor();
        let position = self.pushed - window as u64;
        let step_db =
            10.0 * ((self.sum_after + POWER_EPSILON) / (self.sum_before + POWER_EPSILON)).log10();
        match &mut self.candidate {
            None => {
                let step = if at_floor { FLOOR_STEP_DB } else { MIN_STEP_DB };
                if step_db.abs() >= step {
                    self.candidate = Some(Candidate {
                        position,
                        step_db,
                        since_peak: 0,
                    });
                }
                None
            }
            Some(candidate) => {
                if step_db.signum() == candidate.step_db.signum()
                    && step_db.abs() > candidate.step_db.abs()
                {
                    *candidate = Candidate {
                        position,
                        step_db,
                        since_peak: 0,
                    };
                    return None;
                }
                candidate.since_peak += 1;
                if step_db.abs() >= candidate.step_db.abs() - PEAK_FALL_DB
                    && candidate.since_peak < window
                {
                    return None;
                }
                let step = if candidate.step_db > 0.0 {
                    LevelStep::Rise
                } else {
                    LevelStep::Drop
                };
                let position = candidate.position;
                self.candidate = None;
                Some((position, step))
            }
        }
    }

    /// Updates the noise floor from the two windows and tells whether the
    /// quieter of them is at a floor that is known to be noise.
    fn track_floor(&mut self) -> bool {
        let window = self.window as f64;
        let (before, after) = (self.sum_before / window, self.sum_after / window);
        let quietest = before.min(after);
        let floor = match self.floor {
            Some(floor) => (floor * self.floor_growth).min(quietest),
            None => quietest,
        };
        self.floor = Some(floor);
        if before.max(after) >= floor * db_to_power(FLOOR_TRUST_DB) + POWER_EPSILON {
            self.floor_trusted = true;
        }
        self.floor_trusted && quietest <= floor * db_to_power(FLOOR_MARGIN_DB) + POWER_EPSILON
    }

    /// Makes the samples from `position` on the whole history, so the level
    /// before the step cannot count against the level after it.
    fn restart_statistics_at(&mut self, position: u64) {
        let keep = (self.pushed - position) as usize;
        let excess = self.powers.len().saturating_sub(keep);
        self.powers.drain(..excess);
        self.candidate = None;
        self.recompute_sums();
    }

    fn recompute_sums(&mut self) {
        let length = self.powers.len();
        let split = length.saturating_sub(self.window);
        self.sum_before = self.powers.iter().take(split).sum();
        self.sum_after = self.powers.iter().skip(split).sum();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SYMBOL_RATE_HZ: u64 = 19_200;
    const SAMPLE_RATE_HZ: u64 = 512_000;
    /// Source samples per channel sample.
    const SPACING: u64 = 4;

    /// Deterministic noise with a uniform distribution.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) - 0.5
        }

        /// Roughly Gaussian, with unit variance per component.
        fn gaussian(&mut self) -> f64 {
            (0..12).map(|_| self.next()).sum::<f64>()
        }
    }

    /// A carrier of constant amplitude at a tone, plus complex noise.
    fn signal(
        start: usize,
        count: usize,
        amplitude: f64,
        noise: f64,
        random: &mut Noise,
    ) -> Vec<TimedComplexSample> {
        (start..start + count)
            .map(|index| {
                let phase = TAU * 0.07 * index as f64;
                TimedComplexSample {
                    i: (amplitude * phase.cos() + noise * random.gaussian()) as f32,
                    q: (amplitude * phase.sin() + noise * random.gaussian()) as f32,
                    source_range: index as u64 * SPACING..index as u64 * SPACING + SPACING,
                }
            })
            .collect()
    }

    fn batch(samples: Vec<TimedComplexSample>, discontinuity: bool) -> ChannelizedBatch {
        ChannelizedBatch {
            source_sample_rate_hz: SAMPLE_RATE_HZ * SPACING,
            sample_rate_hz: SAMPLE_RATE_HZ,
            bandwidth_hz: 120_000,
            center_frequency_hz: 868_000_000,
            segment_start_sample_index: 0,
            filter_delay_input_samples: 0,
            discontinuity,
            samples,
        }
    }

    /// First sample index of every segment that starts at a step.
    fn steps(segments: &[ChannelSegment]) -> Vec<(LevelStep, u64)> {
        segments
            .iter()
            .filter_map(|segment| {
                Some((
                    segment.step?,
                    segment.batch.samples.first()?.source_range.start / SPACING,
                ))
            })
            .collect()
    }

    fn run(samples: Vec<TimedComplexSample>, chunk: usize) -> Vec<ChannelSegment> {
        let mut segmenter = LevelStepSegmenter::new(SYMBOL_RATE_HZ);
        samples
            .chunks(chunk)
            .flat_map(|chunk| segmenter.push(&batch(chunk.to_vec(), false)))
            .collect()
    }

    #[test]
    fn finds_the_sample_where_the_carrier_ends() {
        let mut random = Noise(1);
        let mut samples = signal(0, 1_000, 1.0, 0.0, &mut random);
        samples.extend(signal(1_000, 1_000, 0.0, 0.03, &mut random));

        let segments = run(samples, 211);

        assert_eq!(steps(&segments), vec![(LevelStep::Drop, 1_000)]);
        // Nothing is lost or reordered; the last window stays held back.
        let released: Vec<_> = segments
            .iter()
            .flat_map(|segment| segment.batch.samples.iter())
            .map(|sample| sample.source_range.start / SPACING)
            .collect();
        assert!(released.windows(2).all(|pair| pair[1] == pair[0] + 1));
        assert_eq!(released.first(), Some(&0));
    }

    #[test]
    fn finds_the_sample_where_a_louder_burst_begins() {
        let mut random = Noise(2);
        let mut samples = signal(0, 800, 0.1, 0.0, &mut random);
        samples.extend(signal(800, 800, 1.0, 0.0, &mut random));

        let segments = run(samples, 97);

        assert_eq!(steps(&segments), vec![(LevelStep::Rise, 800)]);
    }

    #[test]
    fn splits_a_strong_burst_from_a_weak_one_after_a_short_gap() {
        // Strong burst, a gap of noise, then a burst 16 dB weaker.
        let mut random = Noise(3);
        let mut samples = signal(0, 1_200, 1.0, 0.0, &mut random);
        samples.extend(signal(1_200, 700, 0.0, 0.02, &mut random));
        samples.extend(signal(1_900, 1_500, 0.25, 0.02, &mut random));
        samples.extend(signal(3_400, 600, 0.0, 0.02, &mut random));

        let segments = run(samples, 512);

        let found = steps(&segments);
        let kinds: Vec<_> = found.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            kinds,
            vec![LevelStep::Drop, LevelStep::Rise, LevelStep::Drop]
        );
        assert_eq!(found[0].1, 1_200);
        assert!(found[1].1.abs_diff(1_900) <= 2, "{found:?}");
        assert!(found[2].1.abs_diff(3_400) <= 2, "{found:?}");
    }

    #[test]
    fn the_result_does_not_depend_on_the_batch_size() {
        let mut random = Noise(4);
        let mut samples = signal(0, 900, 1.0, 0.0, &mut random);
        samples.extend(signal(900, 900, 0.0, 0.03, &mut random));
        samples.extend(signal(1_800, 900, 0.3, 0.03, &mut random));

        let reference = steps(&run(samples.clone(), samples.len()));
        for chunk in [1, 7, 64, 333] {
            assert_eq!(steps(&run(samples.clone(), chunk)), reference, "{chunk}");
        }
        assert_eq!(reference.len(), 2);
    }

    #[test]
    fn a_change_below_the_step_is_not_a_step() {
        let mut random = Noise(5);
        let mut samples = signal(0, 1_000, 1.0, 0.0, &mut random);
        // 4 dB quieter.
        samples.extend(signal(1_000, 1_000, 0.63, 0.0, &mut random));

        assert!(steps(&run(samples, 128)).is_empty());
    }

    #[test]
    fn noise_alone_has_no_steps() {
        let mut random = Noise(6);
        let samples = signal(0, 200_000, 0.0, 0.05, &mut random);

        assert!(steps(&run(samples, 1_024)).is_empty());
    }

    #[test]
    fn a_weak_burst_in_noise_is_not_torn_apart() {
        // A carrier a few dB over the noise has no edge that stands out; its
        // edges may or may not be found, but fluctuations inside it must not
        // look like steps.
        let mut random = Noise(7);
        let noise = 0.05;
        let carrier = noise * 10f64.powf(5.0 / 20.0) * 1.2;
        let mut samples = signal(0, 5_000, 0.0, noise, &mut random);
        samples.extend(signal(5_000, 20_000, carrier, noise, &mut random));
        samples.extend(signal(25_000, 5_000, 0.0, noise, &mut random));

        let inside = steps(&run(samples, 1_024))
            .into_iter()
            .filter(|(_, index)| (5_300..24_700).contains(index))
            .collect::<Vec<_>>();

        assert!(inside.is_empty(), "{inside:?}");
    }

    const NOISE: f64 = 0.05;

    /// Amplitude of a carrier that makes the channel `db` louder than noise.
    fn carrier_over_noise(db: f64) -> f64 {
        NOISE * (2.0 * (10f64.powf(db / 10.0) - 1.0)).sqrt()
    }

    /// Noise, a strong burst that shows the noise is noise, and noise again.
    fn learn_the_floor(random: &mut Noise) -> Vec<TimedComplexSample> {
        let mut samples = signal(0, 3_000, 0.0, NOISE, random);
        samples.extend(signal(
            3_000,
            2_000,
            carrier_over_noise(15.0),
            NOISE,
            random,
        ));
        samples.extend(signal(5_000, 3_000, 0.0, NOISE, random));
        samples
    }

    #[test]
    fn the_edges_of_a_burst_a_few_db_over_the_noise_are_found_at_the_floor() {
        let mut random = Noise(9);
        let mut samples = learn_the_floor(&mut random);
        samples.extend(signal(
            8_000,
            6_000,
            carrier_over_noise(5.0),
            NOISE,
            &mut random,
        ));
        samples.extend(signal(14_000, 3_000, 0.0, NOISE, &mut random));

        let found = steps(&run(samples, 700));

        let weak = found
            .into_iter()
            .filter(|(_, index)| *index > 6_000)
            .collect::<Vec<_>>();
        assert_eq!(weak.len(), 2, "{weak:?}");
        assert_eq!(weak[0].0, LevelStep::Rise);
        assert!(weak[0].1.abs_diff(8_000) <= 40, "{weak:?}");
        assert_eq!(weak[1].0, LevelStep::Drop);
        assert!(weak[1].1.abs_diff(14_000) <= 40, "{weak:?}");
    }

    #[test]
    fn swings_inside_a_strong_carrier_are_not_steps() {
        // A tone away from the channel centre is attenuated: the level of a
        // burst alternates by 5 dB although the carrier is constant.
        let mut random = Noise(10);
        let mut samples = learn_the_floor(&mut random);
        let mut at = 8_000;
        for level in [14.0, 19.0, 14.0, 19.0, 14.0, 19.0, 14.0] {
            samples.extend(signal(
                at,
                700,
                carrier_over_noise(level),
                NOISE,
                &mut random,
            ));
            at += 700;
        }
        samples.extend(signal(at, 3_000, 0.0, NOISE, &mut random));

        let found = steps(&run(samples, 512));

        let inside = found
            .into_iter()
            .filter(|(_, index)| (8_300..8_000 + 7 * 700 - 300).contains(index))
            .collect::<Vec<_>>();
        assert!(inside.is_empty(), "{inside:?}");
    }

    #[test]
    fn the_noise_floor_survives_a_gap_in_the_stream() {
        let mut random = Noise(11);
        let mut segmenter = LevelStepSegmenter::new(SYMBOL_RATE_HZ);
        segmenter.push(&batch(learn_the_floor(&mut random), false));

        // The channel is opened again later with a weak burst in it.
        let mut later = signal(100_000, 3_000, 0.0, NOISE, &mut random);
        later.extend(signal(
            103_000,
            6_000,
            carrier_over_noise(5.0),
            NOISE,
            &mut random,
        ));
        later.extend(signal(109_000, 3_000, 0.0, NOISE, &mut random));
        let found = steps(&segmenter.push(&batch(later, true)));

        let kinds = found.iter().map(|(kind, _)| *kind).collect::<Vec<_>>();
        assert_eq!(kinds, vec![LevelStep::Rise, LevelStep::Drop], "{found:?}");
    }

    #[test]
    fn a_quiet_level_that_is_not_known_to_be_noise_is_not_a_floor() {
        // A stream that starts inside a strong carrier never shows the noise.
        // Its lower swing level is the quietest level seen, but it is a
        // carrier, so a 5 dB swing is not a step.
        let mut random = Noise(12);
        let mut samples = Vec::new();
        for (block, level) in [14.0, 19.0, 14.0, 19.0, 14.0, 19.0, 14.0, 19.0]
            .into_iter()
            .enumerate()
        {
            samples.extend(signal(
                block * 700,
                700,
                carrier_over_noise(level),
                NOISE,
                &mut random,
            ));
        }

        assert!(steps(&run(samples, 700)).is_empty());
    }

    #[test]
    fn a_discontinuity_discards_held_samples_and_flags_the_next_segment() {
        let mut random = Noise(8);
        let mut segmenter = LevelStepSegmenter::new(SYMBOL_RATE_HZ);
        let first = segmenter.push(&batch(signal(0, 600, 1.0, 0.0, &mut random), false));
        assert!(first.iter().all(|segment| !segment.batch.discontinuity));

        let resumed = segmenter.push(&batch(signal(10_000, 600, 1.0, 0.0, &mut random), true));

        let first_segment = resumed.first().expect("samples after the gap are released");
        assert!(first_segment.batch.discontinuity);
        assert_eq!(
            first_segment.batch.samples[0].source_range.start / SPACING,
            10_000
        );
        assert!(resumed
            .iter()
            .skip(1)
            .all(|segment| !segment.batch.discontinuity));
    }
}
