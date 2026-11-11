//! Logic-analyzer waveform rendering: signal lanes, channel markers, time
//! markers and timing measurements, and the protocol-decode lanes.

use crate::{
    align_waveform_chrome, analyzer_transport_name, channel_label, default_logic_channel_colors,
    egui, format_duration, format_frequency, half_transparent_plate, marker_text_color,
    paint_empty, paint_plot_background, paint_plot_grid, paint_waveform_status,
    paint_waveform_time_scale, protocol_stack, scope_marker_color, split_waveform_chrome,
    update_waveform_viewport, waveform_sample_x, Color32, Duration, FontId,
    LogicChannelSettingsDraft, LogicSampleBucket, LogicWaveformOptions, Pos2,
    ProtocolAnalyzerInstance, ProtocolAnalyzerSettings, ProtocolDecodeCache, ProtocolDecodeOutcome,
    ProtocolDecodeSnapshot, Range, Rect, RichText, SelectableHoverText, Sense, Stroke, Vec2,
    VecDeque, WaveformStore, WaveformViewport, WaveformViewportOptions, WaveformZoomAnchor, ACCENT,
    BACKGROUND, GRID, LOGIC_CHANNEL_COUNT, LOGIC_CHANNEL_NAME_MAX_CHARS,
    LOGIC_MARKER_EDGE_SNAP_DISTANCE, LOGIC_MARKER_LIMIT, PANEL, SAMPLE_COUNT, SAMPLE_RATE_HZ,
    TEXT_MUTED,
};

pub(crate) fn paint_logic_zero_reference(
    painter: &egui::Painter,
    plot: Rect,
    y: f32,
    channel_color: Color32,
) {
    const DOT_SPACING: f32 = 7.0;
    let mut x = plot.left();
    while x <= plot.right() {
        painter.circle_filled(Pos2::new(x, y), 0.7, channel_color.gamma_multiply(0.3));
        x += DOT_SPACING;
    }
}

pub(crate) const LOGIC_CHANNEL_MARKER_MIN_BODY_WIDTH: f32 = 16.0;

pub(crate) const LOGIC_CHANNEL_MARKER_TIP_WIDTH: f32 = 6.0;

pub(crate) const LOGIC_CHANNEL_MARKER_CHARACTER_WIDTH: f32 = 6.0;

pub(crate) const LOGIC_CHANNEL_MARKER_HORIZONTAL_PADDING: f32 = 8.0;

pub(crate) const LOGIC_SIGNAL_DEFAULT_STEPS_PER_LANE: usize = 4;

#[derive(Clone, Copy, Debug)]
pub(crate) struct LogicSignalLayout {
    pub(crate) base_height: f32,
    pub(crate) levels: [(f32, f32); LOGIC_CHANNEL_COUNT],
}

pub(crate) fn logic_signal_layout(
    plot_top: f32,
    plot_height: f32,
    maximum_heights: &[u8; LOGIC_CHANNEL_COUNT],
) -> LogicSignalLayout {
    let requested_height_steps = maximum_heights
        .iter()
        .map(|height| usize::from((*height).clamp(1, 5)))
        .sum::<usize>();
    let minimum_steps = LOGIC_CHANNEL_COUNT * LOGIC_SIGNAL_DEFAULT_STEPS_PER_LANE;
    let required_steps = requested_height_steps + LOGIC_CHANNEL_COUNT + 1;
    let total_steps = required_steps
        .max(minimum_steps)
        .div_ceil(LOGIC_CHANNEL_COUNT)
        * LOGIC_CHANNEL_COUNT;
    let lane_steps = total_steps / LOGIC_CHANNEL_COUNT;
    let base_height = plot_height / total_steps as f32;
    let mut step_levels = [(0_i32, 0_i32); LOGIC_CHANNEL_COUNT];

    for channel in 0..LOGIC_CHANNEL_COUNT {
        let maximum_height = i32::from(maximum_heights[channel].clamp(1, 5));
        let nominal_low = (channel * lane_steps + lane_steps - 1) as i32;
        let mut high = nominal_low - maximum_height;
        let mut low = nominal_low;
        let minimum_high = if channel == 0 {
            1
        } else {
            step_levels[channel - 1].1 + 1
        };
        if high < minimum_high {
            let movement = minimum_high - high;
            high += movement;
            low += movement;
        }
        step_levels[channel] = (high, low);
    }

    for channel in (0..LOGIC_CHANNEL_COUNT).rev() {
        let maximum_low = if channel + 1 == LOGIC_CHANNEL_COUNT {
            total_steps as i32 - 1
        } else {
            step_levels[channel + 1].0 - 1
        };
        let (mut high, mut low) = step_levels[channel];
        if low > maximum_low {
            let movement = low - maximum_low;
            high -= movement;
            low -= movement;
        }
        step_levels[channel] = (high, low);
    }

    debug_assert!(step_levels[0].0 >= 1);
    debug_assert!(step_levels[LOGIC_CHANNEL_COUNT - 1].1 < total_steps as i32);
    for channel in 1..LOGIC_CHANNEL_COUNT {
        debug_assert!(step_levels[channel - 1].1 < step_levels[channel].0);
    }

    LogicSignalLayout {
        base_height,
        levels: step_levels.map(|(high, low)| {
            (
                plot_top + high as f32 * base_height,
                plot_top + low as f32 * base_height,
            )
        }),
    }
}

pub(crate) fn logic_channel_marker_text(channel: usize, channel_name: &str) -> String {
    let channel_name = channel_name.trim();
    if channel_name.is_empty() {
        channel.to_string()
    } else {
        channel_name
            .chars()
            .take(LOGIC_CHANNEL_NAME_MAX_CHARS)
            .collect()
    }
}

pub(crate) fn logic_channel_marker_body_width(marker_text: &str) -> f32 {
    (marker_text.chars().count() as f32 * LOGIC_CHANNEL_MARKER_CHARACTER_WIDTH
        + LOGIC_CHANNEL_MARKER_HORIZONTAL_PADDING)
        .max(LOGIC_CHANNEL_MARKER_MIN_BODY_WIDTH)
}

pub(crate) fn logic_channel_marker_rect(
    plot: Rect,
    y: f32,
    lane_height: f32,
    marker_text: &str,
) -> Rect {
    let half_height = (lane_height * 0.22).clamp(3.5, 6.5);
    let body_width = logic_channel_marker_body_width(marker_text);
    Rect::from_min_max(
        Pos2::new(plot.left() + 1.0, y - half_height),
        Pos2::new(
            plot.left() + 1.0 + body_width + LOGIC_CHANNEL_MARKER_TIP_WIDTH,
            y + half_height,
        ),
    )
}

pub(crate) fn paint_logic_channel_marker(
    painter: &egui::Painter,
    plot: Rect,
    y: f32,
    lane_height: f32,
    marker_text: &str,
    color: Color32,
    opacity: u8,
) {
    let marker = logic_channel_marker_rect(plot, y, lane_height, marker_text);
    let body_width = logic_channel_marker_body_width(marker_text);
    let body_right = marker.left() + body_width;
    let marker_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), opacity);
    let text_color = marker_text_color(color);
    let text_color =
        Color32::from_rgba_unmultiplied(text_color.r(), text_color.g(), text_color.b(), opacity);
    let outline_color =
        Color32::from_rgba_unmultiplied(BACKGROUND.r(), BACKGROUND.g(), BACKGROUND.b(), opacity);
    painter
        .with_clip_rect(plot)
        .add(egui::Shape::convex_polygon(
            vec![
                marker.left_top(),
                Pos2::new(body_right, marker.top()),
                marker.right_center(),
                Pos2::new(body_right, marker.bottom()),
                marker.left_bottom(),
            ],
            marker_color,
            Stroke::new(1.0_f32, outline_color),
        ));
    painter.with_clip_rect(plot).text(
        Pos2::new(marker.left() + body_width * 0.5, marker.center().y),
        egui::Align2::CENTER_CENTER,
        marker_text,
        FontId::monospace((marker.height() * 0.65).clamp(7.0, 9.0)),
        text_color,
    );
}

pub(crate) const MAX_LOGIC_TRANSITIONS_PER_PIXEL: usize = 4;

#[derive(Clone, Copy, Debug)]
pub(crate) struct LogicTransitionBucket {
    pub(crate) pixel: i32,
    pub(crate) start_high: bool,
    pub(crate) end_high: bool,
    pub(crate) transition_count: usize,
    pub(crate) transition_x: [f32; MAX_LOGIC_TRANSITIONS_PER_PIXEL],
}

impl Default for LogicTransitionBucket {
    fn default() -> Self {
        Self {
            pixel: -1,
            start_high: false,
            end_high: false,
            transition_count: 0,
            transition_x: [0.0; MAX_LOGIC_TRANSITIONS_PER_PIXEL],
        }
    }
}

impl LogicTransitionBucket {
    pub(crate) fn begin(&mut self, pixel: i32, start_high: bool) {
        self.pixel = pixel;
        self.start_high = start_high;
        self.end_high = start_high;
        self.transition_count = 0;
    }

    pub(crate) fn record(&mut self, x: f32, end_high: bool) {
        if self.transition_count < MAX_LOGIC_TRANSITIONS_PER_PIXEL {
            self.transition_x[self.transition_count] = x;
        }
        self.transition_count += 1;
        self.end_high = end_high;
    }

    pub(crate) fn flush(&mut self, points: &mut Vec<Pos2>, plot: Rect, high: f32, low: f32) {
        if self.transition_count == 0 {
            return;
        }
        let y = |is_high| if is_high { high } else { low };
        if self.transition_count <= MAX_LOGIC_TRANSITIONS_PER_PIXEL {
            let mut is_high = self.start_high;
            for x in self
                .transition_x
                .iter()
                .copied()
                .take(self.transition_count)
            {
                points.push(Pos2::new(x, y(is_high)));
                is_high = !is_high;
                points.push(Pos2::new(x, y(is_high)));
            }
        } else {
            // Once several edges occupy the same screen pixel, their exact
            // spacing is no longer visible. Draw an activity stroke instead
            // of dropping the entire bucket and aliasing the signal.
            let x = (plot.left() + self.pixel as f32 + 0.5).clamp(plot.left(), plot.right());
            points.push(Pos2::new(x, y(self.start_high)));
            points.push(Pos2::new(x, y(!self.start_high)));
            if self.end_high == self.start_high {
                points.push(Pos2::new(x, y(self.end_high)));
            }
        }
        self.transition_count = 0;
        self.pixel = -1;
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LogicSampleView<'a> {
    pub(crate) samples: &'a VecDeque<LogicSampleBucket>,
    pub(crate) sample_count: usize,
    pub(crate) sample_stride: usize,
}

pub(crate) fn logic_waveform_points(
    sample_view: LogicSampleView<'_>,
    visible_range: Range<usize>,
    plot: Rect,
    viewport: WaveformViewport,
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
) -> [Vec<Pos2>; LOGIC_CHANNEL_COUNT] {
    let LogicSampleView {
        samples,
        sample_count,
        sample_stride,
    } = sample_view;
    let mut points: [Vec<Pos2>; LOGIC_CHANNEL_COUNT] = std::array::from_fn(|_| Vec::new());
    let Some(first_index) = visible_range.clone().next() else {
        return points;
    };
    let first_bucket_index = first_index / sample_stride;
    let end_bucket_index = ((visible_range.end - 1) / sample_stride + 1).min(samples.len());
    let Some(first_bucket) = samples.get(first_bucket_index) else {
        return points;
    };
    let mut previous = first_bucket.first;
    let first_x = if viewport.start >= 0.0 {
        plot.left()
    } else {
        waveform_sample_x(plot, viewport, first_index as f64, sample_count)
    };
    for channel in 0..LOGIC_CHANNEL_COUNT {
        if enabled_channels[channel] {
            let (high, low) = levels[channel];
            let y = if previous & (1_u32 << channel) != 0 {
                high
            } else {
                low
            };
            points[channel].push(Pos2::new(first_x, y));
        }
    }

    let enabled_mask = enabled_channels
        .iter()
        .enumerate()
        .fold(0_u32, |mask, (channel, enabled)| {
            mask | (u32::from(*enabled) << channel)
        });
    let mut buckets = [LogicTransitionBucket::default(); LOGIC_CHANNEL_COUNT];
    for (bucket_index, bucket) in samples
        .iter()
        .copied()
        .enumerate()
        .take(end_bucket_index)
        .skip(first_bucket_index)
    {
        let bucket_start = bucket_index.saturating_mul(sample_stride);

        let mut boundary_changes = (previous ^ bucket.first) & enabled_mask;
        if boundary_changes != 0 {
            let x = waveform_sample_x(plot, viewport, bucket_start as f64, sample_count)
                .clamp(plot.left(), plot.right());
            let pixel = (x - plot.left()).floor() as i32;
            while boundary_changes != 0 {
                let channel = boundary_changes.trailing_zeros() as usize;
                boundary_changes &= boundary_changes - 1;
                let transition_bucket = &mut buckets[channel];
                if transition_bucket.pixel != pixel {
                    let (high, low) = levels[channel];
                    transition_bucket.flush(&mut points[channel], plot, high, low);
                    transition_bucket.begin(pixel, previous & (1_u32 << channel) != 0);
                }
                transition_bucket.record(x, bucket.first & (1_u32 << channel) != 0);
            }
        }

        let mut activity = bucket.activity & enabled_mask;
        if activity != 0 {
            let activity_index = bucket_start
                .saturating_add(bucket.sample_count as usize / 2)
                .min(sample_count.saturating_sub(1));
            let x = waveform_sample_x(plot, viewport, activity_index as f64, sample_count)
                .clamp(plot.left(), plot.right());
            let pixel = (x - plot.left()).floor() as i32;
            while activity != 0 {
                let channel = activity.trailing_zeros() as usize;
                activity &= activity - 1;
                let mask = 1_u32 << channel;
                let starts_high = bucket.first & mask != 0;
                let ends_high = bucket.last & mask != 0;
                let transition_bucket = &mut buckets[channel];
                if transition_bucket.pixel != pixel {
                    let (high, low) = levels[channel];
                    transition_bucket.flush(&mut points[channel], plot, high, low);
                    transition_bucket.begin(pixel, starts_high);
                }
                if starts_high == ends_high {
                    transition_bucket.record(x, !starts_high);
                }
                transition_bucket.record(x, ends_high);
            }
        }
        previous = bucket.last;
    }

    let last_index = visible_range.end - 1;
    let last_x = if viewport.start + viewport.span <= 1.0 {
        plot.right()
    } else {
        waveform_sample_x(plot, viewport, last_index as f64, sample_count)
    };
    for channel in 0..LOGIC_CHANNEL_COUNT {
        if !enabled_channels[channel] {
            continue;
        }
        let (high, low) = levels[channel];
        buckets[channel].flush(&mut points[channel], plot, high, low);
        let y = if previous & (1_u32 << channel) != 0 {
            high
        } else {
            low
        };
        points[channel].push(Pos2::new(last_x, y));
    }
    points
}

pub(crate) fn logic_marker_color(index: usize) -> Color32 {
    scope_marker_color(index % 2, index / 2)
}

pub(crate) fn logic_sample_at_x(
    plot: Rect,
    viewport: WaveformViewport,
    sample_offset: u64,
    sample_count: usize,
    x: f32,
) -> Option<u64> {
    if sample_count == 0 {
        return None;
    }
    let screen_fraction = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    let domain_fraction = (f64::from(viewport.start)
        + f64::from(screen_fraction) * f64::from(viewport.span))
    .clamp(0.0, 1.0);
    let local_sample = (domain_fraction * sample_count.saturating_sub(1) as f64).round() as u64;
    Some(sample_offset.saturating_add(local_sample))
}

pub(crate) fn logic_marker_x(
    plot: Rect,
    viewport: WaveformViewport,
    sample_offset: u64,
    sample_count: usize,
    sample: u64,
) -> Option<f32> {
    let last_sample = sample_offset.saturating_add(sample_count.saturating_sub(1) as u64);
    if sample_count == 0 || sample < sample_offset || sample > last_sample {
        return None;
    }
    Some(waveform_sample_x(
        plot,
        viewport,
        sample.saturating_sub(sample_offset) as f64,
        sample_count,
    ))
}

pub(crate) fn logic_channel_at_y(
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
    pointer_y: f32,
) -> Option<usize> {
    levels
        .iter()
        .enumerate()
        .filter(|(channel, _)| enabled_channels[*channel])
        .min_by(|(_, left), (_, right)| {
            let left_distance = (pointer_y - (left.0 + left.1) * 0.5).abs();
            let right_distance = (pointer_y - (right.0 + right.1) * 0.5).abs();
            left_distance.total_cmp(&right_distance)
        })
        .map(|(channel, _)| channel)
}

pub(crate) fn nearest_logic_transition_sample(
    waveforms: &WaveformStore,
    channel: usize,
    target_sample: u64,
    search_radius: u64,
) -> Option<u64> {
    if channel >= LOGIC_CHANNEL_COUNT
        || waveforms.logic_samples.is_empty()
        || waveforms.logic_sample_stride == 0
    {
        return None;
    }
    let local_target = target_sample.saturating_sub(waveforms.logic_sample_offset);
    let stride = waveforms.logic_sample_stride as u64;
    let target_bucket = local_target / stride;
    let bucket_radius = search_radius.div_ceil(stride).saturating_add(2);
    let first_bucket = target_bucket
        .saturating_sub(bucket_radius)
        .min(waveforms.logic_samples.len() as u64) as usize;
    let end_bucket = target_bucket
        .saturating_add(bucket_radius)
        .saturating_add(1)
        .min(waveforms.logic_samples.len() as u64) as usize;
    let channel_mask = 1_u32 << channel;
    let mut closest = None;

    for bucket_index in first_bucket..end_bucket {
        let bucket = waveforms.logic_samples[bucket_index];
        let bucket_start = (bucket_index as u64).saturating_mul(stride);
        if bucket_index > 0 {
            let previous = waveforms.logic_samples[bucket_index - 1];
            if (previous.last ^ bucket.first) & channel_mask != 0 {
                let candidate = waveforms.logic_sample_offset.saturating_add(bucket_start);
                let distance = candidate.abs_diff(target_sample);
                if distance <= search_radius
                    && closest.map_or(true, |(_, closest_distance)| distance < closest_distance)
                {
                    closest = Some((candidate, distance));
                }
            }
        }
        if bucket.activity & channel_mask != 0 {
            let activity_sample = bucket_start
                .saturating_add(u64::from(bucket.sample_count) / 2)
                .min(waveforms.logic_sample_count.saturating_sub(1) as u64);
            let candidate = waveforms
                .logic_sample_offset
                .saturating_add(activity_sample);
            let distance = candidate.abs_diff(target_sample);
            if distance <= search_radius
                && closest.map_or(true, |(_, closest_distance)| distance < closest_distance)
            {
                closest = Some((candidate, distance));
            }
        }
    }
    closest.map(|(sample, _)| sample)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LogicSmartTimingMeasurement {
    pub(crate) channel: usize,
    pub(crate) edge_start_sample: u64,
    pub(crate) edge_end_sample: u64,
    pub(crate) cycle: Option<LogicTimingMeasurement>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LogicTransition {
    pub(crate) sample: u64,
    pub(crate) new_state_high: bool,
}

pub(crate) fn exact_logic_bucket_state(bucket: LogicSampleBucket, mask: u32) -> Option<bool> {
    (bucket.activity & mask == 0).then_some(bucket.first & mask != 0)
}

pub(crate) fn logic_transition_within_bucket(
    waveforms: &WaveformStore,
    mask: u32,
    bucket_index: usize,
) -> Option<LogicTransition> {
    let bucket = waveforms.logic_samples[bucket_index];
    if bucket.activity & mask == 0 || bucket.multiple_transitions & mask != 0 {
        return None;
    }
    let bucket_start = (bucket_index as u64).saturating_mul(waveforms.logic_sample_stride as u64);
    Some(LogicTransition {
        sample: waveforms
            .logic_sample_offset
            .saturating_add(bucket_start)
            .saturating_add(u64::from(bucket.sample_count) / 2),
        new_state_high: bucket.last & mask != 0,
    })
}

pub(crate) fn logic_transition_before_bucket(
    waveforms: &WaveformStore,
    mask: u32,
    before_bucket: usize,
) -> Option<LogicTransition> {
    for right_index in (1..=before_bucket).rev() {
        let left = waveforms.logic_samples[right_index - 1];
        let right = waveforms.logic_samples[right_index];
        if (left.last ^ right.first) & mask != 0 {
            return Some(LogicTransition {
                sample: waveforms.logic_sample_offset.saturating_add(
                    (right_index as u64).saturating_mul(waveforms.logic_sample_stride as u64),
                ),
                new_state_high: right.first & mask != 0,
            });
        }
        if left.activity & mask != 0 {
            return logic_transition_within_bucket(waveforms, mask, right_index - 1);
        }
    }
    None
}

pub(crate) fn logic_transition_after_bucket(
    waveforms: &WaveformStore,
    mask: u32,
    after_bucket: usize,
) -> Option<LogicTransition> {
    for right_index in after_bucket.saturating_add(1)..waveforms.logic_samples.len() {
        let left = waveforms.logic_samples[right_index - 1];
        let right = waveforms.logic_samples[right_index];
        if (left.last ^ right.first) & mask != 0 {
            return Some(LogicTransition {
                sample: waveforms.logic_sample_offset.saturating_add(
                    (right_index as u64).saturating_mul(waveforms.logic_sample_stride as u64),
                ),
                new_state_high: right.first & mask != 0,
            });
        }
        if right.activity & mask != 0 {
            return logic_transition_within_bucket(waveforms, mask, right_index);
        }
    }
    None
}

pub(crate) fn logic_smart_timing_measurement(
    waveforms: &WaveformStore,
    channel: usize,
    anchor_sample: u64,
) -> Option<LogicSmartTimingMeasurement> {
    if channel >= LOGIC_CHANNEL_COUNT
        || waveforms.logic_rate_hz == 0
        || waveforms.logic_sample_stride == 0
        || waveforms.logic_samples.is_empty()
    {
        return None;
    }
    let local_sample = anchor_sample.saturating_sub(waveforms.logic_sample_offset);
    let bucket_index = usize::try_from(local_sample / waveforms.logic_sample_stride as u64)
        .ok()?
        .min(waveforms.logic_samples.len().saturating_sub(1));
    let mask = 1_u32 << channel;
    let state_high = exact_logic_bucket_state(waveforms.logic_samples[bucket_index], mask)?;
    let previous = logic_transition_before_bucket(waveforms, mask, bucket_index)?;
    let next = logic_transition_after_bucket(waveforms, mask, bucket_index)?;
    if previous.new_state_high != state_high || next.new_state_high == state_high {
        return None;
    }

    let cycle = if state_high {
        logic_transition_after_bucket(
            waveforms,
            mask,
            usize::try_from(
                next.sample.saturating_sub(waveforms.logic_sample_offset)
                    / waveforms.logic_sample_stride as u64,
            )
            .ok()?,
        )
        .filter(|transition| transition.new_state_high)
        .map(|next_rising| LogicTimingMeasurement {
            channel,
            period_start_sample: previous.sample,
            period_end_sample: next_rising.sample,
            period_samples: next_rising.sample.saturating_sub(previous.sample),
            high_samples: next.sample.saturating_sub(previous.sample),
        })
    } else {
        let falling_bucket = usize::try_from(
            previous
                .sample
                .saturating_sub(waveforms.logic_sample_offset)
                / waveforms.logic_sample_stride as u64,
        )
        .ok()?;
        logic_transition_before_bucket(waveforms, mask, falling_bucket.saturating_sub(1))
            .filter(|transition| transition.new_state_high)
            .map(|previous_rising| LogicTimingMeasurement {
                channel,
                period_start_sample: previous_rising.sample,
                period_end_sample: next.sample,
                period_samples: next.sample.saturating_sub(previous_rising.sample),
                high_samples: previous.sample.saturating_sub(previous_rising.sample),
            })
    }
    .filter(|timing| {
        timing.period_samples > 0
            && timing.high_samples > 0
            && timing.high_samples < timing.period_samples
    });

    Some(LogicSmartTimingMeasurement {
        channel,
        edge_start_sample: previous.sample,
        edge_end_sample: next.sample,
        cycle,
    })
}

pub(crate) fn logic_marker_sample_at_pointer(
    plot: Rect,
    viewport: WaveformViewport,
    waveforms: &WaveformStore,
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
    pointer: Pos2,
) -> Option<u64> {
    let sample = logic_sample_at_x(
        plot,
        viewport,
        waveforms.logic_sample_offset,
        waveforms.logic_sample_count,
        pointer.x.clamp(plot.left(), plot.right()),
    )?;
    let visible_samples_per_pixel = viewport.span
        * waveforms.logic_sample_count.saturating_sub(1) as f32
        / plot.width().max(1.0);
    let search_radius = (visible_samples_per_pixel * LOGIC_MARKER_EDGE_SNAP_DISTANCE).ceil() as u64;
    let Some(channel) = logic_channel_at_y(levels, enabled_channels, pointer.y) else {
        return Some(sample);
    };
    let Some(snapped) = nearest_logic_transition_sample(waveforms, channel, sample, search_radius)
    else {
        return Some(sample);
    };
    let snapped_x = logic_marker_x(
        plot,
        viewport,
        waveforms.logic_sample_offset,
        waveforms.logic_sample_count,
        snapped,
    )?;
    if (snapped_x - pointer.x).abs() <= LOGIC_MARKER_EDGE_SNAP_DISTANCE {
        Some(snapped)
    } else {
        Some(sample)
    }
}

pub(crate) fn pan_logic_marker_viewport_at_edge(
    viewport: &mut WaveformViewport,
    plot: Rect,
    pointer_x: f32,
) -> bool {
    const EDGE_WIDTH: f32 = 32.0;
    const PAN_STEP: f32 = 0.02;

    let edge_width = EDGE_WIDTH.min(plot.width() * 0.25).max(1.0);
    let direction = if pointer_x < plot.left() + edge_width {
        -((plot.left() + edge_width - pointer_x) / edge_width).clamp(0.0, 1.0)
    } else if pointer_x > plot.right() - edge_width {
        ((pointer_x - (plot.right() - edge_width)) / edge_width).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if direction == 0.0 {
        return false;
    }

    let previous_start = viewport.start;
    viewport.start = viewport.clamp_start(viewport.start + direction * viewport.span * PAN_STEP);
    if (viewport.start - previous_start).abs() <= f32::EPSILON {
        return false;
    }
    viewport.anchor_locked = false;
    true
}

pub(crate) fn update_logic_marker_gesture(
    ui: &egui::Ui,
    plot: Rect,
    waveforms: &WaveformStore,
    viewport: &mut WaveformViewport,
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
    settings: &mut LogicMarkerSettings,
) {
    if !settings.adding
        || settings.markers.len() >= LOGIC_MARKER_LIMIT
        || waveforms.logic_sample_count < 2
        || waveforms.logic_rate_hz == 0
    {
        settings.draft = None;
        return;
    }

    let (pressed, down, released, pointer_position) = ui.input(|input| {
        (
            input.pointer.button_pressed(egui::PointerButton::Primary),
            input.pointer.button_down(egui::PointerButton::Primary),
            input.pointer.button_released(egui::PointerButton::Primary),
            input.pointer.interact_pos(),
        )
    });
    if pointer_position.is_some_and(|pointer| plot.contains(pointer)) || settings.draft.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if pressed {
        if let Some(pointer) = pointer_position.filter(|pointer| plot.contains(*pointer)) {
            if let Some(sample) = logic_marker_sample_at_pointer(
                plot,
                *viewport,
                waveforms,
                levels,
                enabled_channels,
                pointer,
            ) {
                settings.start(sample);
            }
        }
    }
    if settings.draft.is_some() && (down || released) {
        if let Some(pointer) = pointer_position {
            if down && pan_logic_marker_viewport_at_edge(viewport, plot, pointer.x) {
                ui.ctx().request_repaint();
            }
            if let Some(sample) = logic_marker_sample_at_pointer(
                plot,
                *viewport,
                waveforms,
                levels,
                enabled_channels,
                pointer,
            ) {
                settings.update(sample);
            }
        }
    }
    if released && settings.draft.is_some() {
        settings.finish(waveforms.logic_rate_hz);
    }
}

pub(crate) fn paint_logic_marker_span(
    painter: &egui::Painter,
    plot: Rect,
    first_x: f32,
    second_x: f32,
    label: &str,
    color: Color32,
) {
    let left = first_x.min(second_x).clamp(plot.left(), plot.right());
    let right = first_x.max(second_x).clamp(plot.left(), plot.right());
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24);
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(left, plot.top()), Pos2::new(right, plot.bottom())),
        0.0,
        fill,
    );
    for x in [left, right] {
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.5_f32, color),
        );
    }
    painter.line_segment(
        [
            Pos2::new(left, plot.top() + 3.0),
            Pos2::new(right, plot.top() + 3.0),
        ],
        Stroke::new(1.5_f32, color),
    );
    painter.text(
        Pos2::new((left + right) * 0.5, plot.top() + 6.0),
        egui::Align2::CENTER_TOP,
        label,
        FontId::monospace(10.0),
        color,
    );
}

pub(crate) fn paint_logic_markers(
    ui: &egui::Ui,
    plot: Rect,
    waveforms: &WaveformStore,
    viewport: WaveformViewport,
    settings: &mut LogicMarkerSettings,
) {
    let last_sample = waveforms
        .logic_sample_offset
        .saturating_add(waveforms.logic_sample_count.saturating_sub(1) as u64);
    for (index, marker) in settings.markers.iter().enumerate() {
        if marker.end_sample < waveforms.logic_sample_offset || marker.start_sample > last_sample {
            continue;
        }
        let start = marker
            .start_sample
            .clamp(waveforms.logic_sample_offset, last_sample);
        let end = marker
            .end_sample
            .clamp(waveforms.logic_sample_offset, last_sample);
        let (Some(start_x), Some(end_x)) = (
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                start,
            ),
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                end,
            ),
        ) else {
            continue;
        };
        let marker_label = if marker.name.trim().is_empty() {
            format!("M{}", index + 1)
        } else {
            marker.name.trim().to_owned()
        };
        paint_logic_marker_span(
            ui.painter(),
            plot,
            start_x,
            end_x,
            &format!("{marker_label}  {}", format_duration(marker.duration())),
            marker.color,
        );
    }

    if let Some(draft) = settings.draft {
        if let (Some(start_x), Some(end_x)) = (
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                draft.start_sample,
            ),
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                draft.end_sample,
            ),
        ) {
            let duration = LogicMarker::new(
                draft.start_sample,
                draft.end_sample,
                waveforms.logic_rate_hz,
                ACCENT,
            )
            .map(|marker| format_duration(marker.duration()))
            .unwrap_or_else(|| "0.0 ns".to_owned());
            paint_logic_marker_span(
                ui.painter(),
                plot,
                start_x,
                end_x,
                &format!("New  {duration}"),
                ACCENT,
            );
        }
    }
}

pub(crate) fn paint_logic_timing_hint(
    painter: &egui::Painter,
    plot: Rect,
    waveforms: &WaveformStore,
    viewport: WaveformViewport,
    measurement: LogicSmartTimingMeasurement,
    color: Color32,
) {
    if plot.width() < 120.0 || plot.height() < 80.0 {
        return;
    }
    let painter = painter.with_clip_rect(plot);
    let channel = measurement.channel;

    if let Some(timing) = measurement.cycle {
        if let (Some(start_x), Some(end_x)) = (
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                timing.period_start_sample,
            ),
            logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                timing.period_end_sample,
            ),
        ) {
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(start_x.min(end_x), plot.top()),
                    Pos2::new(start_x.max(end_x), plot.bottom()),
                ),
                0.0,
                color.gamma_multiply(0.08),
            );
        }
    }
    for sample in [measurement.edge_start_sample, measurement.edge_end_sample] {
        if let Some(x) = logic_marker_x(
            plot,
            viewport,
            waveforms.logic_sample_offset,
            waveforms.logic_sample_count,
            sample,
        ) {
            painter.line_segment(
                [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                Stroke::new(0.75_f32, color.gamma_multiply(0.3)),
            );
        }
    }
    if let Some(timing) = measurement.cycle {
        for sample in [timing.period_start_sample, timing.period_end_sample] {
            if let Some(x) = logic_marker_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                sample,
            ) {
                painter.line_segment(
                    [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                    Stroke::new(0.75_f32, color.gamma_multiply(0.5)),
                );
                painter.circle_filled(
                    Pos2::new(x, plot.top() + 3.0),
                    2.0,
                    color.gamma_multiply(0.5),
                );
            }
        }
    }

    let width = 205.0_f32.min(plot.width() - 16.0);
    let height = if measurement.cycle.is_some() {
        72.0
    } else {
        48.0
    };
    let hint = Rect::from_min_size(
        Pos2::new(plot.right() - width - 8.0, plot.top() + 26.0),
        Vec2::new(width, height),
    );
    painter.rect_filled(hint, 5.0, half_transparent_plate(PANEL));
    let border = Stroke::new(0.75_f32, color.gamma_multiply(0.65));
    painter.line_segment([hint.left_top(), hint.right_top()], border);
    painter.line_segment([hint.right_top(), hint.right_bottom()], border);
    painter.line_segment([hint.right_bottom(), hint.left_bottom()], border);
    painter.line_segment([hint.left_bottom(), hint.left_top()], border);

    let left = hint.left() + 8.0;
    painter.text(
        Pos2::new(left, hint.top() + 7.0),
        egui::Align2::LEFT_TOP,
        format!("D{channel}  SMART TIMING"),
        FontId::monospace(10.0),
        color,
    );
    painter.text(
        Pos2::new(left, hint.top() + 23.0),
        egui::Align2::LEFT_TOP,
        format!(
            "EDGE-EDGE  {}",
            format_duration(Duration::from_secs_f64(
                measurement
                    .edge_end_sample
                    .saturating_sub(measurement.edge_start_sample) as f64
                    / waveforms.logic_rate_hz as f64,
            ))
        ),
        FontId::monospace(10.0),
        Color32::WHITE,
    );
    if let Some(timing) = measurement.cycle {
        painter.text(
            Pos2::new(left, hint.top() + 39.0),
            egui::Align2::LEFT_TOP,
            format!(
                "RISE-RISE  {}",
                format_duration(timing.period(waveforms.logic_rate_hz))
            ),
            FontId::monospace(10.0),
            Color32::WHITE,
        );
        painter.text(
            Pos2::new(left, hint.top() + 55.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{}   DUTY {:.1}%",
                format_frequency(timing.frequency_hz(waveforms.logic_rate_hz)),
                timing.duty_cycle_percent()
            ),
            FontId::monospace(10.0),
            ACCENT,
        );
    }
}

pub(crate) const LOGIC_PROTOCOL_LANE_HEIGHT: f32 = 30.0;

pub(crate) const LOGIC_PROTOCOL_LABEL_HORIZONTAL_PADDING: f32 = 6.0;

pub(crate) const LOGIC_PROTOCOL_LABEL_FONT_SIZE: f32 = 9.0;

pub(crate) const LOGIC_PROTOCOL_VALUE_FONT_SIZE: f32 = 13.0;

pub(crate) fn protocol_data_channel(analyzer: &ProtocolAnalyzerInstance) -> usize {
    let base_id = protocol_stack(analyzer.protocol_id)
        .and_then(|stack| stack.first().map(|protocol| protocol.id));
    let preferred_mapping = match base_id {
        Some("0:i2c" | "1:i2c") => analyzer.channel_mappings.get(1),
        Some("0:spi" | "1:spi") => analyzer
            .channel_mappings
            .get(2)
            .filter(|mapping| mapping.is_some())
            .or_else(|| analyzer.channel_mappings.get(1)),
        _ => None,
    };
    preferred_mapping
        .copied()
        .flatten()
        .or_else(|| {
            analyzer
                .channel_mappings
                .iter()
                .rev()
                .flatten()
                .copied()
                .next()
        })
        .unwrap_or(0)
        .min(LOGIC_CHANNEL_COUNT - 1)
}

pub(crate) fn logic_protocol_lane_layout(
    plot: Rect,
    maximum_heights: &[u8; LOGIC_CHANNEL_COUNT],
    analyzers: &[ProtocolAnalyzerInstance],
) -> (LogicSignalLayout, Vec<Rect>) {
    if analyzers.is_empty() {
        return (
            logic_signal_layout(plot.top(), plot.height(), maximum_heights),
            Vec::new(),
        );
    }
    let available_height = (plot.height() - 80.0).max(0.0);
    let total_height = (analyzers.len() as f32 * LOGIC_PROTOCOL_LANE_HEIGHT)
        .min(available_height)
        .min(plot.height() * 0.4);
    if total_height <= 0.0 {
        return (
            logic_signal_layout(plot.top(), plot.height(), maximum_heights),
            Vec::new(),
        );
    }
    let lane_height = total_height / analyzers.len() as f32;
    let mut signal_layout =
        logic_signal_layout(plot.top(), plot.height() - total_height, maximum_heights);
    let base_levels = signal_layout.levels;
    let targets = analyzers
        .iter()
        .map(protocol_data_channel)
        .collect::<Vec<_>>();
    let mut insertion_order = targets.iter().copied().enumerate().collect::<Vec<_>>();
    insertion_order.sort_by_key(|(analyzer_index, target)| (*target, *analyzer_index));
    let mut lanes = vec![plot; analyzers.len()];
    for (insertion_index, (analyzer_index, target)) in insertion_order.iter().copied().enumerate() {
        let base_y = if target == 0 {
            plot.top()
        } else {
            (base_levels[target - 1].1 + base_levels[target].0) * 0.5
        };
        let top = base_y + insertion_index as f32 * lane_height;
        lanes[analyzer_index] = Rect::from_min_max(
            Pos2::new(plot.left(), top),
            Pos2::new(plot.right(), top + lane_height),
        );
    }
    for (channel, (high, low)) in signal_layout.levels.iter_mut().enumerate() {
        let preceding_lanes = targets.iter().filter(|target| **target <= channel).count();
        let offset = preceding_lanes as f32 * lane_height;
        *high += offset;
        *low += offset;
    }
    (signal_layout, lanes)
}

pub(crate) fn paint_logic_protocol_lanes(
    ui: &egui::Ui,
    lanes: &[Rect],
    waveforms: &WaveformStore,
    viewport: WaveformViewport,
    settings: &ProtocolAnalyzerSettings,
    cache: &mut ProtocolDecodeCache,
    streamed: Option<&ProtocolDecodeSnapshot>,
) {
    if lanes.is_empty() {
        return;
    }
    let outcomes = if let Some(streamed) =
        streamed.filter(|snapshot| snapshot.matches_analyzers(&settings.analyzers))
    {
        streamed.waveform_outcomes()
    } else {
        cache.waveform_outcomes(waveforms, &settings.analyzers, None)
    };
    for (lane_index, ((lane, analyzer), outcome)) in lanes
        .iter()
        .zip(&settings.analyzers)
        .zip(outcomes)
        .enumerate()
    {
        let color = logic_marker_color(lane_index);
        let painter = ui.painter().with_clip_rect(*lane);
        let label = analyzer_transport_name(analyzer);
        let label_galley = painter.layout_no_wrap(
            label.to_owned(),
            FontId::monospace(LOGIC_PROTOCOL_LABEL_FONT_SIZE),
            color,
        );
        let label_width = (label_galley.size().x + LOGIC_PROTOCOL_LABEL_HORIZONTAL_PADDING * 2.0)
            .min(lane.width());
        painter.rect_filled(*lane, 0.0, PANEL.gamma_multiply(0.72));
        painter.line_segment(
            [lane.left_top(), lane.right_top()],
            Stroke::new(1.0_f32, GRID),
        );

        match outcome {
            ProtocolDecodeOutcome::Events(events) => {
                for (event_index, event) in events.iter().enumerate() {
                    let (Some(start_x), Some(end_x)) = (
                        logic_marker_x(
                            *lane,
                            viewport,
                            waveforms.logic_sample_offset,
                            waveforms.logic_sample_count,
                            event.start_sample,
                        ),
                        logic_marker_x(
                            *lane,
                            viewport,
                            waveforms.logic_sample_offset,
                            waveforms.logic_sample_count,
                            event.end_sample,
                        ),
                    ) else {
                        continue;
                    };
                    if end_x < lane.left() || start_x > lane.right() {
                        continue;
                    }
                    let event_rect = Rect::from_min_max(
                        Pos2::new(start_x.max(lane.left()), lane.top() + 3.0),
                        Pos2::new(
                            end_x.max(start_x + 1.0).min(lane.right()),
                            lane.bottom() - 3.0,
                        ),
                    );
                    let event_color = if event.warning {
                        Color32::LIGHT_RED
                    } else {
                        color
                    };
                    painter.rect_filled(event_rect, 3.0, event_color.gamma_multiply(0.22));
                    painter.rect_stroke(event_rect, 3.0, Stroke::new(0.8_f32, event_color));
                    let value_galley = painter.layout_no_wrap(
                        event.text.clone(),
                        FontId::monospace(LOGIC_PROTOCOL_VALUE_FONT_SIZE),
                        Color32::WHITE,
                    );
                    if event_rect.width() >= value_galley.size().x + 8.0 {
                        let text_origin = event_rect.center() - value_galley.size() * 0.5;
                        painter.galley(text_origin, value_galley, Color32::WHITE);
                    }
                    ui.interact(
                        event_rect.expand(2.0),
                        ui.make_persistent_id((
                            "protocol_decoded_event",
                            analyzer.instance_id,
                            event_index,
                        )),
                        Sense::hover(),
                    )
                    .on_hover_selectable_text(format!(
                        "{}\nSamples {}–{}",
                        event.detail, event.start_sample, event.end_sample
                    ));
                }
                if events.is_empty() {
                    painter.text(
                        Pos2::new(lane.left() + label_width + 8.0, lane.center().y),
                        egui::Align2::LEFT_CENTER,
                        if waveforms.logic_sample_count == 0 {
                            "Waiting for logic samples"
                        } else {
                            "No decoded bytes"
                        },
                        FontId::monospace(9.0),
                        TEXT_MUTED,
                    );
                }
            }
            ProtocolDecodeOutcome::Unavailable(message) => {
                painter.text(
                    Pos2::new(lane.left() + label_width + 8.0, lane.center().y),
                    egui::Align2::LEFT_CENTER,
                    message,
                    FontId::monospace(9.0),
                    TEXT_MUTED,
                );
            }
        }

        let label_rect = Rect::from_min_max(
            lane.min,
            Pos2::new((lane.left() + label_width).min(lane.right()), lane.bottom()),
        );
        painter.line_segment(
            [label_rect.right_top(), label_rect.right_bottom()],
            Stroke::new(1.0_f32, color.gamma_multiply(0.65)),
        );
        let label_origin = Pos2::new(
            label_rect.left() + LOGIC_PROTOCOL_LABEL_HORIZONTAL_PADDING,
            label_rect.center().y - label_galley.size().y * 0.5,
        );
        painter.galley(label_origin, label_galley, color);
    }
}

/// The number of samples and the sample rate the logic plot is laid out for:
/// the capture's own, or a default until the first samples arrive.
fn logic_display_extent(waveforms: &WaveformStore) -> (usize, u64) {
    let sample_count = waveforms.logic_sample_count;
    let display_sample_count = if sample_count == 0 {
        SAMPLE_COUNT as usize
    } else {
        sample_count
    };
    let display_sample_rate_hz = if waveforms.logic_rate_hz == 0 {
        SAMPLE_RATE_HZ
    } else {
        waveforms.logic_rate_hz
    };
    (display_sample_count, display_sample_rate_hz)
}

pub(crate) fn paint_logic(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    options: LogicWaveformOptions<'_>,
    viewport: &mut WaveformViewport,
) {
    let LogicWaveformOptions {
        channel_colors,
        channel_marker_opacities,
        channel_names,
        channel_settings_drafts,
        smart_timing_enabled,
        marker_settings,
        maximum_heights,
        enabled_channels,
        protocol_analyzer,
        protocol_decode_cache,
        protocol_stream,
        time_axis,
    } = options;
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    let (status_area, ruler_area, plot) = split_waveform_chrome(rect);
    let (status_bar, time_ruler) = align_waveform_chrome(status_area, ruler_area, plot);
    paint_plot_grid(ui, plot);
    let sample_count = waveforms.logic_sample_count;
    let (display_sample_count, display_sample_rate_hz) = logic_display_extent(waveforms);
    let (signal_layout, protocol_lanes) =
        logic_protocol_lane_layout(plot, maximum_heights, &protocol_analyzer.analyzers);
    let _ = update_waveform_viewport(
        ui,
        plot,
        "logic_waveform_zoom",
        viewport,
        display_sample_count,
        display_sample_rate_hz,
        WaveformViewportOptions {
            reset_on_double_click: true,
            primary_pan_enabled: !marker_settings.adding,
            zoom_anchor: WaveformZoomAnchor::Pointer,
            timebase_basis: None,
            timebase_reconfigures_capture: false,
        },
    );
    update_logic_marker_gesture(
        ui,
        plot,
        waveforms,
        viewport,
        &signal_layout.levels,
        enabled_channels,
        marker_settings,
    );
    paint_logic_lane_separators(painter, plot, &signal_layout.levels);
    let visible_range = viewport.sample_range(sample_count);
    let visible_count = visible_range.len();
    let mut waveform_points = logic_waveform_points(
        LogicSampleView {
            samples: &waveforms.logic_samples,
            sample_count,
            sample_stride: waveforms.logic_sample_stride,
        },
        visible_range.clone(),
        plot,
        *viewport,
        &signal_layout.levels,
        enabled_channels,
    );
    paint_logic_channels(
        ui,
        &LogicChannelLanes {
            plot,
            levels: &signal_layout.levels,
            marker_lane_height: signal_layout.base_height
                * LOGIC_SIGNAL_DEFAULT_STEPS_PER_LANE as f32,
            has_samples: visible_count != 0,
        },
        &mut LogicChannelEdits {
            colors: channel_colors,
            marker_opacities: channel_marker_opacities,
            names: channel_names,
            settings_drafts: channel_settings_drafts,
            maximum_heights,
            enabled: enabled_channels,
        },
        &mut waveform_points,
    );
    paint_logic_protocol_lanes(
        ui,
        &protocol_lanes,
        waveforms,
        *viewport,
        protocol_analyzer,
        protocol_decode_cache,
        protocol_stream,
    );
    paint_logic_markers(ui, plot, waveforms, *viewport, marker_settings);
    let smart_timing = smart_timing_enabled
        .then(|| {
            hovered_logic_smart_timing(
                ui,
                plot,
                &protocol_lanes,
                &signal_layout.levels,
                enabled_channels,
                waveforms,
                *viewport,
            )
        })
        .flatten();
    if let Some(measurement) = smart_timing {
        paint_logic_timing_hint(
            painter,
            plot,
            waveforms,
            *viewport,
            measurement,
            channel_colors[measurement.channel],
        );
    }
    if visible_count == 0 {
        paint_empty(painter, plot, "Waiting for logic samples");
    }
    paint_waveform_time_scale(
        painter,
        time_ruler,
        display_sample_count,
        display_sample_rate_hz,
        waveforms.logic_sample_offset,
        *viewport,
        time_axis,
    );
    paint_waveform_status(
        painter,
        status_bar,
        display_sample_count,
        display_sample_rate_hz,
        *viewport,
        None,
    );
}

/// The thin line above each channel's lane (the first one is the plot's top).
fn paint_logic_lane_separators(
    painter: &egui::Painter,
    plot: Rect,
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
) {
    for channel in 0..LOGIC_CHANNEL_COUNT {
        let lane_top = if channel == 0 {
            plot.top()
        } else {
            (levels[channel - 1].1 + levels[channel].0) * 0.5
        };
        painter.line_segment(
            [
                Pos2::new(plot.left(), lane_top),
                Pos2::new(plot.right(), lane_top),
            ],
            Stroke::new(0.6_f32, GRID.gamma_multiply(0.45)),
        );
    }
}

/// The channel under the pointer and the edge or cycle around the sample
/// there, unless the pointer is over a protocol lane or outside the plot.
fn hovered_logic_smart_timing(
    ui: &egui::Ui,
    plot: Rect,
    protocol_lanes: &[Rect],
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
    waveforms: &WaveformStore,
    viewport: WaveformViewport,
) -> Option<LogicSmartTimingMeasurement> {
    ui.input(|input| input.pointer.hover_pos())
        .filter(|pointer| {
            plot.contains(*pointer) && !protocol_lanes.iter().any(|lane| lane.contains(*pointer))
        })
        .and_then(|pointer| {
            let channel = logic_channel_at_y(levels, enabled_channels, pointer.y)?;
            let sample = logic_sample_at_x(
                plot,
                viewport,
                waveforms.logic_sample_offset,
                waveforms.logic_sample_count,
                pointer.x,
            )?;
            logic_smart_timing_measurement(waveforms, channel, sample)
        })
}

/// Where the channels are drawn and whether there is anything to draw.
struct LogicChannelLanes<'a> {
    plot: Rect,
    levels: &'a [(f32, f32); LOGIC_CHANNEL_COUNT],
    marker_lane_height: f32,
    has_samples: bool,
}

/// The per-channel settings the user can change from a channel's marker.
struct LogicChannelEdits<'a> {
    colors: &'a mut [Color32; LOGIC_CHANNEL_COUNT],
    marker_opacities: &'a mut [u8; LOGIC_CHANNEL_COUNT],
    names: &'a mut [String; LOGIC_CHANNEL_COUNT],
    settings_drafts: &'a mut [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT],
    maximum_heights: &'a mut [u8; LOGIC_CHANNEL_COUNT],
    enabled: &'a mut [bool; LOGIC_CHANNEL_COUNT],
}

impl LogicChannelEdits<'_> {
    fn slot(&mut self, channel: usize) -> LogicChannelSlot<'_> {
        LogicChannelSlot {
            enabled: &mut self.enabled[channel],
            maximum_height: &mut self.maximum_heights[channel],
            name: &mut self.names[channel],
            color: &mut self.colors[channel],
            marker_opacity: &mut self.marker_opacities[channel],
            draft: &mut self.settings_drafts[channel],
        }
    }
}

/// One channel's settings and the draft its popup edits. The draft only
/// replaces the settings when the popup is saved.
struct LogicChannelSlot<'a> {
    enabled: &'a mut bool,
    maximum_height: &'a mut u8,
    name: &'a mut String,
    color: &'a mut Color32,
    marker_opacity: &'a mut u8,
    draft: &'a mut LogicChannelSettingsDraft,
}

impl LogicChannelSlot<'_> {
    /// Starts a new edit from the current settings.
    fn begin_edit(&mut self) {
        *self.draft = LogicChannelSettingsDraft {
            enabled: *self.enabled,
            maximum_height: *self.maximum_height,
            name: self.name.clone(),
            color: *self.color,
            marker_opacity: *self.marker_opacity,
        };
    }

    fn apply_draft(&mut self) {
        *self.enabled = self.draft.enabled;
        *self.maximum_height = self.draft.maximum_height;
        *self.name = self.draft.name.clone();
        *self.color = self.draft.color;
        *self.marker_opacity = self.draft.marker_opacity;
    }
}

fn paint_logic_channels(
    ui: &egui::Ui,
    lanes: &LogicChannelLanes<'_>,
    edits: &mut LogicChannelEdits<'_>,
    waveform_points: &mut [Vec<Pos2>; LOGIC_CHANNEL_COUNT],
) {
    for (channel, points) in waveform_points.iter_mut().enumerate() {
        if !edits.enabled[channel] {
            continue;
        }
        paint_logic_channel(ui, channel, lanes, &mut edits.slot(channel), points);
    }
}

/// One enabled channel: its zero line, the clickable marker with the settings
/// popup, the waveform, and the marker itself.
fn paint_logic_channel(
    ui: &egui::Ui,
    channel: usize,
    lanes: &LogicChannelLanes<'_>,
    slot: &mut LogicChannelSlot<'_>,
    points: &mut Vec<Pos2>,
) {
    let painter = ui.painter();
    let channel_label = channel_label("D", channel, slot.name);
    let marker_text = logic_channel_marker_text(channel, slot.name);
    let (_, low) = lanes.levels[channel];
    paint_logic_zero_reference(painter, lanes.plot, low, *slot.color);
    let marker_rect =
        logic_channel_marker_rect(lanes.plot, low, lanes.marker_lane_height, &marker_text);
    let marker_response = ui
        .interact(
            marker_rect.expand(3.0),
            ui.make_persistent_id(("logic_channel_marker", channel)),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_selectable_text(format!("Click to edit {channel_label} settings"));

    paint_logic_channel_settings_popup(ui, channel, &marker_response, slot);

    if lanes.has_samples {
        let points = std::mem::take(points);
        if points.len() > 1 {
            painter.add(egui::Shape::line(points, Stroke::new(1.2_f32, *slot.color)));
        }
    }
    paint_logic_channel_marker(
        painter,
        lanes.plot,
        low,
        lanes.marker_lane_height,
        &marker_text,
        *slot.color,
        *slot.marker_opacity,
    );
}

fn paint_logic_channel_settings_popup(
    ui: &egui::Ui,
    channel: usize,
    marker_response: &egui::Response,
    slot: &mut LogicChannelSlot<'_>,
) {
    let popup_id = ui.make_persistent_id(("logic_channel_settings_popup", channel));
    if marker_response.clicked() {
        slot.begin_edit();
        ui.memory_mut(|memory| memory.toggle_popup(popup_id));
    }
    egui::popup::popup_below_widget(
        ui,
        popup_id,
        marker_response,
        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
        |popup_ui| match paint_logic_channel_settings_form(popup_ui, channel, slot.draft) {
            LogicChannelSettingsAction::Save => {
                slot.apply_draft();
                popup_ui.memory_mut(egui::Memory::close_popup);
            }
            LogicChannelSettingsAction::Cancel => {
                popup_ui.memory_mut(egui::Memory::close_popup);
            }
            LogicChannelSettingsAction::Keep => {}
        },
    );
}

/// What the user decided in a channel's settings popup this frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogicChannelSettingsAction {
    Keep,
    Save,
    Cancel,
}

fn paint_logic_channel_settings_form(
    popup_ui: &mut egui::Ui,
    channel: usize,
    draft: &mut LogicChannelSettingsDraft,
) -> LogicChannelSettingsAction {
    let mut save_requested = false;
    let mut cancel_requested = false;
    popup_ui.set_min_width(280.0);
    popup_ui.label(RichText::new(format!("D{channel} settings")).strong());
    popup_ui.checkbox(&mut draft.enabled, "Enabled");
    popup_ui.horizontal(|ui| {
        ui.label("Max height");
        ui.add(
            egui::Slider::new(&mut draft.maximum_height, 1..=5)
                .integer()
                .suffix("×"),
        );
    });
    popup_ui.separator();
    popup_ui.horizontal(|ui| {
        ui.label("Name");
        let name_edit = ui.add(
            egui::TextEdit::singleline(&mut draft.name)
                .char_limit(LOGIC_CHANNEL_NAME_MAX_CHARS)
                .desired_width(190.0)
                .hint_text("Optional name"),
        );
        if name_edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            save_requested = true;
        }
    });
    popup_ui.separator();
    popup_ui.label("Color");
    egui::color_picker::color_picker_color32(
        popup_ui,
        &mut draft.color,
        egui::color_picker::Alpha::Opaque,
    );
    if popup_ui.button("Reset color").clicked() {
        draft.color = default_logic_channel_colors()[channel];
    }
    popup_ui.separator();
    popup_ui.horizontal(|ui| {
        ui.label("Opacity");
        let mut opacity_percent = f32::from(draft.marker_opacity) / 255.0 * 100.0;
        if ui
            .add(
                egui::Slider::new(&mut opacity_percent, 10.0..=100.0)
                    .fixed_decimals(0)
                    .suffix("%"),
            )
            .changed()
        {
            draft.marker_opacity = (opacity_percent / 100.0 * 255.0).round() as u8;
        }
    });
    popup_ui.separator();
    popup_ui.horizontal(|ui| {
        save_requested |= ui.button("Save").clicked();
        cancel_requested |= ui.button("Cancel").clicked();
    });
    if save_requested {
        LogicChannelSettingsAction::Save
    } else if cancel_requested {
        LogicChannelSettingsAction::Cancel
    } else {
        LogicChannelSettingsAction::Keep
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LogicMarker {
    pub(crate) name: String,
    pub(crate) color: Color32,
    pub(crate) start_sample: u64,
    pub(crate) end_sample: u64,
    pub(crate) sample_rate_hz: u64,
}

impl LogicMarker {
    pub(crate) fn new(
        first_sample: u64,
        second_sample: u64,
        sample_rate_hz: u64,
        color: Color32,
    ) -> Option<Self> {
        let (start_sample, end_sample) = if first_sample <= second_sample {
            (first_sample, second_sample)
        } else {
            (second_sample, first_sample)
        };
        (start_sample != end_sample && sample_rate_hz != 0).then_some(Self {
            name: String::new(),
            color,
            start_sample,
            end_sample,
            sample_rate_hz,
        })
    }

    pub(crate) fn duration(&self) -> Duration {
        Duration::from_secs_f64(
            self.end_sample.saturating_sub(self.start_sample) as f64 / self.sample_rate_hz as f64,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LogicTimingMeasurement {
    pub(crate) channel: usize,
    pub(crate) period_start_sample: u64,
    pub(crate) period_end_sample: u64,
    pub(crate) period_samples: u64,
    pub(crate) high_samples: u64,
}

impl LogicTimingMeasurement {
    pub(crate) fn period(self, sample_rate_hz: u64) -> Duration {
        Duration::from_secs_f64(self.period_samples as f64 / sample_rate_hz as f64)
    }

    pub(crate) fn frequency_hz(self, sample_rate_hz: u64) -> f64 {
        sample_rate_hz as f64 / self.period_samples as f64
    }

    pub(crate) fn duty_cycle_percent(self) -> f64 {
        self.high_samples as f64 / self.period_samples as f64 * 100.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LogicMarkerDraft {
    pub(crate) start_sample: u64,
    pub(crate) end_sample: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LogicMarkerSettings {
    pub(crate) markers: Vec<LogicMarker>,
    pub(crate) adding: bool,
    pub(crate) draft: Option<LogicMarkerDraft>,
}

impl LogicMarkerSettings {
    pub(crate) fn arm(&mut self) -> bool {
        if self.markers.len() >= LOGIC_MARKER_LIMIT {
            return false;
        }
        self.adding = true;
        self.draft = None;
        true
    }

    pub(crate) fn cancel(&mut self) {
        self.adding = false;
        self.draft = None;
    }

    pub(crate) fn start(&mut self, sample: u64) {
        if self.adding && self.markers.len() < LOGIC_MARKER_LIMIT {
            self.draft = Some(LogicMarkerDraft {
                start_sample: sample,
                end_sample: sample,
            });
        }
    }

    pub(crate) fn update(&mut self, sample: u64) {
        if let Some(draft) = &mut self.draft {
            draft.end_sample = sample;
        }
    }

    pub(crate) fn finish(&mut self, sample_rate_hz: u64) -> bool {
        let Some(draft) = self.draft.take() else {
            return false;
        };
        let Some(marker) = LogicMarker::new(
            draft.start_sample,
            draft.end_sample,
            sample_rate_hz,
            logic_marker_color(self.markers.len()),
        ) else {
            return false;
        };
        if self.markers.len() >= LOGIC_MARKER_LIMIT {
            self.cancel();
            return false;
        }
        self.markers.push(marker);
        self.adding = false;
        true
    }

    pub(crate) fn clear(&mut self) {
        self.markers.clear();
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    #[test]
    fn logic_markers_normalize_reverse_drags_and_stop_at_twenty() {
        let mut settings = LogicMarkerSettings::default();
        for index in 0..LOGIC_MARKER_LIMIT {
            assert!(settings.arm());
            settings.start((index * 10 + 9) as u64);
            settings.update((index * 10) as u64);
            assert!(settings.finish(1_000));
        }

        assert_eq!(settings.markers.len(), LOGIC_MARKER_LIMIT);
        assert_eq!(settings.markers[0].start_sample, 0);
        assert_eq!(settings.markers[0].end_sample, 9);
        assert_eq!(settings.markers[0].duration(), Duration::from_millis(9));
        assert!(settings.markers[0].name.is_empty());
        assert_eq!(settings.markers[0].color, logic_marker_color(0));
        settings.markers[0].name = "Clock pulse".to_owned();
        settings.markers[0].color = Color32::RED;
        assert_eq!(settings.markers[0].name, "Clock pulse");
        assert_eq!(settings.markers[0].color, Color32::RED);
        assert!(!settings.arm());
    }

    #[test]
    fn logic_channel_markers_stay_compact_and_inside_their_lanes() {
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 320.0));
        let lane_height = 20.0;
        let marker = logic_channel_marker_rect(plot, lane_height * 0.75, lane_height, "15");
        let named_marker =
            logic_channel_marker_rect(plot, lane_height * 0.75, lane_height, "CLOCKCLK");

        assert_eq!(marker.width(), 26.0);
        assert_eq!(named_marker.width(), 62.0);
        assert!(marker.height() <= 13.0);
        assert!(marker.top() >= plot.top());
        assert!(marker.bottom() <= plot.top() + lane_height);
    }

    #[test]
    fn logic_maximum_height_uses_the_selected_multiple() {
        let mut maximum_heights = [1; LOGIC_CHANNEL_COUNT];

        for multiple in 1..=5 {
            maximum_heights[8] = multiple;
            let layout = logic_signal_layout(0.0, 320.0, &maximum_heights);
            let (high, low) = layout.levels[8];
            assert!((low - high - layout.base_height * f32::from(multiple)).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn logic_maximum_height_reflows_channels_inside_the_plot() {
        let height_patterns = [
            [1; LOGIC_CHANNEL_COUNT],
            [5; LOGIC_CHANNEL_COUNT],
            [5, 5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 5, 5],
            [5, 1, 5, 1, 5, 1, 5, 1, 5, 1, 5, 1, 5, 1, 5, 1],
        ];

        for maximum_heights in height_patterns {
            let layout = logic_signal_layout(40.0, 320.0, &maximum_heights);
            assert!(layout.levels[0].0 >= 40.0 + layout.base_height);
            assert!(layout.levels[LOGIC_CHANNEL_COUNT - 1].1 <= 360.0 - layout.base_height);
            for channel in 1..LOGIC_CHANNEL_COUNT {
                assert!(
                    layout.levels[channel - 1].1 + layout.base_height
                        <= layout.levels[channel].0 + f32::EPSILON
                );
            }
        }
    }

    #[test]
    fn logic_reflow_moves_neighbors_in_base_height_steps() {
        let defaults = logic_signal_layout(0.0, 320.0, &[1; LOGIC_CHANNEL_COUNT]);
        let mut maximum_heights = [1; LOGIC_CHANNEL_COUNT];
        maximum_heights[0] = 5;
        let enlarged_first = logic_signal_layout(0.0, 320.0, &maximum_heights);
        assert!((defaults.base_height - enlarged_first.base_height).abs() < f32::EPSILON);
        assert!(
            (enlarged_first.levels[1].0 - defaults.levels[1].0 - defaults.base_height).abs()
                < f32::EPSILON
        );

        maximum_heights = [1; LOGIC_CHANNEL_COUNT];
        maximum_heights[LOGIC_CHANNEL_COUNT - 1] = 5;
        let enlarged_last = logic_signal_layout(0.0, 320.0, &maximum_heights);
        let movement = (defaults.levels[14].0 - enlarged_last.levels[14].0) / defaults.base_height;
        assert!((movement - movement.round()).abs() < f32::EPSILON);
        assert!(movement >= 1.0);
    }

    #[test]
    fn logic_channel_marker_uses_name_limited_to_eight_characters() {
        assert_eq!(logic_channel_marker_text(3, ""), "3");
        assert_eq!(logic_channel_marker_text(3, "  CLOCK  "), "CLOCK");
        assert_eq!(logic_channel_marker_text(3, "DATA-LONG-NAME"), "DATA-LON");
    }

    #[test]
    fn zero_length_logic_marker_drag_remains_armed() {
        let mut settings = LogicMarkerSettings::default();
        assert!(settings.arm());
        settings.start(42);
        settings.update(42);

        assert!(!settings.finish(1_000_000));
        assert!(settings.adding);
        assert!(settings.markers.is_empty());
        assert!(settings.draft.is_none());
    }

    #[test]
    fn the_display_extent_is_the_capture_or_a_default_before_the_first_samples() {
        let mut waveforms = WaveformStore::default();
        assert_eq!(
            logic_display_extent(&waveforms),
            (SAMPLE_COUNT as usize, SAMPLE_RATE_HZ)
        );

        waveforms.logic_sample_count = 250;
        assert_eq!(
            logic_display_extent(&waveforms),
            (250, SAMPLE_RATE_HZ),
            "the rate is still unknown"
        );

        waveforms.logic_rate_hz = 24_000_000;
        assert_eq!(logic_display_extent(&waveforms), (250, 24_000_000));
    }

    /// The settings of every logic channel, as the waveform view holds them.
    struct ChannelSettings {
        colors: [Color32; LOGIC_CHANNEL_COUNT],
        marker_opacities: [u8; LOGIC_CHANNEL_COUNT],
        names: [String; LOGIC_CHANNEL_COUNT],
        drafts: [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT],
        maximum_heights: [u8; LOGIC_CHANNEL_COUNT],
        enabled: [bool; LOGIC_CHANNEL_COUNT],
    }

    impl ChannelSettings {
        fn new() -> Self {
            Self {
                colors: default_logic_channel_colors(),
                marker_opacities: default_logic_channel_marker_opacities(),
                names: std::array::from_fn(|_| String::new()),
                drafts: default_logic_channel_settings_drafts(),
                maximum_heights: [1; LOGIC_CHANNEL_COUNT],
                enabled: [true; LOGIC_CHANNEL_COUNT],
            }
        }

        fn edits(&mut self) -> LogicChannelEdits<'_> {
            LogicChannelEdits {
                colors: &mut self.colors,
                marker_opacities: &mut self.marker_opacities,
                names: &mut self.names,
                settings_drafts: &mut self.drafts,
                maximum_heights: &mut self.maximum_heights,
                enabled: &mut self.enabled,
            }
        }
    }

    #[test]
    fn a_channel_edit_starts_from_the_settings_of_that_channel() {
        let mut settings = ChannelSettings::new();
        settings.enabled[5] = false;
        settings.maximum_heights[5] = 4;
        settings.names[5] = "CLK".to_owned();
        settings.colors[5] = Color32::RED;
        settings.marker_opacities[5] = 77;

        settings.edits().slot(5).begin_edit();

        assert_eq!(
            settings.drafts[5],
            LogicChannelSettingsDraft {
                enabled: false,
                maximum_height: 4,
                name: "CLK".to_owned(),
                color: Color32::RED,
                marker_opacity: 77,
            }
        );
        assert_eq!(
            settings.drafts[4],
            default_logic_channel_settings_drafts()[4],
            "the other drafts are left alone"
        );
    }

    #[test]
    fn saving_a_draft_changes_only_its_channel() {
        let mut settings = ChannelSettings::new();
        settings.drafts[2] = LogicChannelSettingsDraft {
            enabled: false,
            maximum_height: 3,
            name: "MOSI".to_owned(),
            color: Color32::GREEN,
            marker_opacity: 99,
        };
        let before = ChannelSettings::new();

        settings.edits().slot(2).apply_draft();

        assert!(!settings.enabled[2]);
        assert_eq!(settings.maximum_heights[2], 3);
        assert_eq!(settings.names[2], "MOSI");
        assert_eq!(settings.colors[2], Color32::GREEN);
        assert_eq!(settings.marker_opacities[2], 99);
        for channel in (0..LOGIC_CHANNEL_COUNT).filter(|channel| *channel != 2) {
            assert_eq!(settings.enabled[channel], before.enabled[channel]);
            assert_eq!(
                settings.maximum_heights[channel],
                before.maximum_heights[channel]
            );
            assert_eq!(settings.names[channel], before.names[channel]);
            assert_eq!(settings.colors[channel], before.colors[channel]);
            assert_eq!(
                settings.marker_opacities[channel],
                before.marker_opacities[channel]
            );
        }
    }

    /// A logic waveform painted frame after frame, with its channel markers
    /// and the settings popup they open.
    struct LogicView {
        context: egui::Context,
        waveforms: WaveformStore,
        settings: ChannelSettings,
        marker_settings: LogicMarkerSettings,
        protocol_analyzer: ProtocolAnalyzerSettings,
        protocol_decode_cache: ProtocolDecodeCache,
        viewport: WaveformViewport,
        smart_timing_enabled: bool,
        frames: u32,
    }

    struct PaintedLogic {
        texts: Vec<(String, Pos2)>,
        squares: Vec<Pos2>,
        shape_count: usize,
    }

    impl PaintedLogic {
        fn new(shapes: &[egui::epaint::ClippedShape]) -> Self {
            fn collect(shape: &egui::Shape, painted: &mut PaintedLogic) {
                if !matches!(shape, egui::Shape::Vec(_)) {
                    painted.shape_count += 1;
                }
                match shape {
                    egui::Shape::Vec(shapes) => {
                        shapes.iter().for_each(|shape| collect(shape, painted));
                    }
                    egui::Shape::Text(text) => painted.texts.push((
                        text.galley.text().to_owned(),
                        text.visual_bounding_rect().center(),
                    )),
                    egui::Shape::Rect(rect) => {
                        let size = rect.rect.size();
                        if (size.x - size.y).abs() < 1.0 && (10.0..24.0).contains(&size.x) {
                            painted.squares.push(rect.rect.center());
                        }
                    }
                    _ => {}
                }
            }
            let mut painted = Self {
                texts: Vec::new(),
                squares: Vec::new(),
                shape_count: 0,
            };
            for clipped in shapes {
                collect(&clipped.shape, &mut painted);
            }
            painted
        }

        fn has(&self, text: &str) -> bool {
            self.texts.iter().any(|(painted, _)| painted == text)
        }

        fn center_of(&self, text: &str) -> Pos2 {
            let matches = self
                .texts
                .iter()
                .filter(|(painted, _)| painted == text)
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1, "expected exactly one {text:?}");
            matches[0].1
        }

        /// The check box painted to the left of `label`.
        fn checkbox_before(&self, label: &str) -> Pos2 {
            let row = self.center_of(label);
            let matches = self
                .squares
                .iter()
                .filter(|square| (square.y - row.y).abs() < 6.0 && square.x < row.x)
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1, "expected one check box before {label:?}");
            *matches[0]
        }
    }

    impl LogicView {
        fn new() -> Self {
            let mut waveforms = WaveformStore {
                logic_rate_hz: 1_000_000,
                ..WaveformStore::default()
            };
            waveforms.configure_logic_capture(1_000);
            let samples = (0..1_000u32)
                .map(|index| index & 0xffff)
                .collect::<Vec<_>>();
            waveforms.ingest_logic_samples(&samples);
            let mut settings = ChannelSettings::new();
            settings.names[0] = "CLK".to_owned();
            settings.names[3] = "SCK".to_owned();
            let mut view = Self {
                context: egui::Context::default(),
                waveforms,
                settings,
                marker_settings: LogicMarkerSettings::default(),
                protocol_analyzer: ProtocolAnalyzerSettings::default(),
                protocol_decode_cache: ProtocolDecodeCache::default(),
                viewport: WaveformViewport {
                    start: 0.0,
                    span: 1.0,
                    anchor_locked: false,
                },
                smart_timing_enabled: false,
                frames: 0,
            };
            view.frame(Vec::new());
            view
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> PaintedLogic {
            self.frames += 1;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 700.0))),
                time: Some(f64::from(self.frames) / 60.0),
                events,
                ..egui::RawInput::default()
            };
            let rect = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(880.0, 640.0));
            let output = self.context.run(input, |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    paint_logic(
                        ui,
                        rect,
                        &self.waveforms,
                        LogicWaveformOptions {
                            channel_colors: &mut self.settings.colors,
                            channel_marker_opacities: &mut self.settings.marker_opacities,
                            channel_names: &mut self.settings.names,
                            channel_settings_drafts: &mut self.settings.drafts,
                            smart_timing_enabled: self.smart_timing_enabled,
                            marker_settings: &mut self.marker_settings,
                            maximum_heights: &mut self.settings.maximum_heights,
                            enabled_channels: &mut self.settings.enabled,
                            protocol_analyzer: &self.protocol_analyzer,
                            protocol_decode_cache: &mut self.protocol_decode_cache,
                            protocol_stream: None,
                            time_axis: None,
                        },
                        &mut self.viewport,
                    );
                });
            });
            PaintedLogic::new(&output.shapes)
        }

        fn click(&mut self, position: Pos2) {
            let button = |pressed| egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            self.frame(vec![egui::Event::PointerMoved(position), button(true)]);
            self.frame(vec![button(false)]);
            self.frame(Vec::new());
        }

        fn click_text(&mut self, text: &str) {
            let position = self.frame(Vec::new()).center_of(text);
            self.click(position);
        }

        fn key(&mut self, key: egui::Key) {
            self.frame(vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }]);
        }
    }

    #[test]
    fn saving_the_channel_popup_applies_what_was_changed_in_it() {
        let mut view = LogicView::new();
        view.settings.colors[3] = Color32::RED;

        view.click_text("SCK");
        assert!(view.frame(Vec::new()).has("D3 settings"));
        let checkbox = view.frame(Vec::new()).checkbox_before("Enabled");
        view.click(checkbox);
        view.click_text("Reset color");
        assert!(
            view.settings.enabled[3],
            "nothing is applied before the popup is saved"
        );
        assert_eq!(view.settings.colors[3], Color32::RED);
        view.click_text("Save");

        assert!(!view.settings.enabled[3]);
        assert_eq!(view.settings.colors[3], default_logic_channel_colors()[3]);
        assert!(
            !view.frame(Vec::new()).has("D3 settings"),
            "saving closes the popup"
        );
    }

    #[test]
    fn only_enabled_channels_get_a_marker() {
        let mut view = LogicView::new();
        view.settings.names[5] = "OFF".to_owned();
        assert!(view.frame(Vec::new()).has("OFF"));

        view.settings.enabled[5] = false;

        assert!(!view.frame(Vec::new()).has("OFF"));
        assert!(view.frame(Vec::new()).has("SCK"));
    }

    #[test]
    fn cancelling_the_channel_popup_keeps_the_settings() {
        let mut view = LogicView::new();

        view.click_text("SCK");
        let checkbox = view.frame(Vec::new()).checkbox_before("Enabled");
        view.click(checkbox);
        view.click_text("Cancel");

        assert!(view.settings.enabled[3]);
        assert!(!view.frame(Vec::new()).has("D3 settings"));
    }

    #[test]
    fn reopening_the_channel_popup_discards_an_earlier_unsaved_edit() {
        let mut view = LogicView::new();

        view.click_text("SCK");
        let checkbox = view.frame(Vec::new()).checkbox_before("Enabled");
        view.click(checkbox);
        view.click_text("Cancel");
        view.click_text("SCK");
        view.click_text("Save");

        assert!(view.settings.enabled[3]);
        assert!(
            !view.frame(Vec::new()).has("D3 settings"),
            "saving closes the popup"
        );
    }

    #[test]
    fn a_channel_name_is_saved_with_enter() {
        let mut view = LogicView::new();

        view.click_text("SCK");
        // The marker and the name field both show the name; the field is the
        // lower one.
        let painted = view.frame(Vec::new());
        let name_field = painted
            .texts
            .iter()
            .filter(|(text, _)| text == "SCK")
            .map(|(_, position)| *position)
            .max_by(|a, b| a.y.total_cmp(&b.y))
            .unwrap();
        view.click(name_field);
        view.frame(vec![egui::Event::Text("1".to_owned())]);
        view.key(egui::Key::Enter);
        view.frame(Vec::new());

        assert_eq!(
            view.settings.names[3], "1",
            "focusing the field selects the old name, so typing replaces it"
        );
        assert!(!view.frame(Vec::new()).has("D3 settings"));
    }

    #[test]
    fn the_timing_hint_follows_the_pointer_only_when_smart_timing_is_on() {
        let mut view = LogicView::new();
        let lane = view.frame(Vec::new()).center_of("CLK");
        let over_the_lane = Pos2::new(450.0, lane.y);
        let hover = |view: &mut LogicView| {
            view.frame(vec![egui::Event::PointerMoved(over_the_lane)]);
            view.frame(Vec::new()).shape_count
        };

        let without_hint = hover(&mut view);
        view.smart_timing_enabled = true;
        let with_hint = hover(&mut view);

        assert!(
            with_hint > without_hint,
            "{with_hint} shapes with smart timing, {without_hint} without"
        );
    }
}
