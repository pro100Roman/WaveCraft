//! Oscilloscope time-domain rendering: graticule, traces, trigger position and
//! level handles, timing measurements, markers, and the math trace.

use crate::{
    align_waveform_chrome, apply_time_per_division, egui, format_duration, format_frequency,
    format_trigger_voltage, half_transparent_plate, marker_text_color, paint_dashed_line,
    paint_empty, paint_marker_handle, paint_marker_readout, paint_plot_background, paint_scope_fft,
    paint_waveform_status, paint_waveform_time_scale, resolve_scope_timebase, scope_channel_label,
    split_waveform_chrome, update_waveform_viewport, waveform_sample_x, Color32,
    DsCopeBandwidthLimit, DsCopeTriggerSlope, DsCopeTriggerSource, Duration, FontId, HashSet, Pos2,
    Range, Rect, ScopeCaptureSettings, ScopeChannelSettings, ScopeFftPaintOptions,
    ScopeFftViewport, ScopeFunctionSettings, ScopeMarkerSettings, ScopeMathOperation,
    ScopeMathSettings, ScopeMeasurementCache, ScopeQuickTimeMeasurement, ScopeTriggerSettings,
    ScopeWaveformOptions, SelectableHoverText, Sense, Stroke, Vec2, VecDeque, WaveformPlotClick,
    WaveformStore, WaveformTimeAxis, WaveformViewport, WaveformViewportOptions, WaveformZoomAnchor,
    BACKGROUND, DSCOPE_TRIGGER_LEVEL_CENTER, DSCOPE_TRIGGER_LEVEL_MAX, DSCOPE_TRIGGER_LEVEL_MIN,
    DSCOPE_TRIGGER_POSITION_MAX_PERCENT, DSCOPE_TRIGGER_POSITION_MIN_PERCENT,
    DSCOPE_VERTICAL_OFFSET_CENTER, DSCOPE_VERTICAL_OFFSET_MAX, DSCOPE_VERTICAL_OFFSET_MIN, GRID,
    MATH_COLOR, PANEL, SCOPE_ADC_VERTICAL_DIVISIONS, SCOPE_DOWNSAMPLE_MIN_SAMPLES_PER_PIXEL,
    SCOPE_HORIZONTAL_DIVISIONS, SCOPE_MARKER_LIMIT, SCOPE_MEASUREMENT_KINDS,
    SCOPE_VERTICAL_DIVISIONS, TEXT_MUTED,
};

const DSCOPE_U3P100_FULL_ANALOG_BANDWIDTH_HZ: u64 = 100_000_000;
const DSCOPE_LIMITED_ANALOG_BANDWIDTH_HZ: u64 = 20_000_000;

pub(crate) fn paint_scope_graticule(painter: &egui::Painter, rect: Rect) {
    for index in 0..=SCOPE_HORIZONTAL_DIVISIONS {
        let x = egui::lerp(
            rect.x_range(),
            index as f32 / SCOPE_HORIZONTAL_DIVISIONS as f32,
        );
        let center = index == SCOPE_HORIZONTAL_DIVISIONS / 2;
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(
                if center { 1.2_f32 } else { 0.8_f32 },
                if center {
                    TEXT_MUTED.gamma_multiply(0.45)
                } else {
                    GRID.gamma_multiply(0.8)
                },
            ),
        );
    }
    for index in 0..=SCOPE_VERTICAL_DIVISIONS {
        let y = egui::lerp(
            rect.y_range(),
            index as f32 / SCOPE_VERTICAL_DIVISIONS as f32,
        );
        let center = index == SCOPE_VERTICAL_DIVISIONS / 2;
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(
                if center { 1.2_f32 } else { 0.8_f32 },
                if center {
                    TEXT_MUTED.gamma_multiply(0.45)
                } else {
                    GRID.gamma_multiply(0.8)
                },
            ),
        );
    }

    let center = rect.center();
    for index in 0..=SCOPE_HORIZONTAL_DIVISIONS * 5 {
        let x = egui::lerp(
            rect.x_range(),
            index as f32 / (SCOPE_HORIZONTAL_DIVISIONS * 5) as f32,
        );
        painter.circle_filled(Pos2::new(x, center.y), 0.8, TEXT_MUTED.gamma_multiply(0.65));
    }
    for index in 0..=SCOPE_VERTICAL_DIVISIONS * 5 {
        let y = egui::lerp(
            rect.y_range(),
            index as f32 / (SCOPE_VERTICAL_DIVISIONS * 5) as f32,
        );
        painter.circle_filled(Pos2::new(center.x, y), 0.8, TEXT_MUTED.gamma_multiply(0.65));
    }
}

pub(crate) fn scope_vertical_offset_ratio(vertical_offset: u16) -> f32 {
    let span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    f32::from(
        vertical_offset.clamp(DSCOPE_VERTICAL_OFFSET_MIN, DSCOPE_VERTICAL_OFFSET_MAX)
            - DSCOPE_VERTICAL_OFFSET_MIN,
    ) / span
}

pub(crate) fn scope_vertical_offset_y(rect: Rect, vertical_offset: u16) -> f32 {
    egui::lerp(rect.y_range(), scope_vertical_offset_ratio(vertical_offset))
}

pub(crate) fn scope_vertical_offset_from_y(rect: Rect, y: f32) -> u16 {
    let ratio = ((y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
    let span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    (f32::from(DSCOPE_VERTICAL_OFFSET_MIN) + ratio * span).round() as u16
}

pub(crate) fn scope_sample_y(plot: Rect, channel: ScopeChannelSettings, voltage: f32) -> f32 {
    let volts_per_div =
        f32::from(channel.volts_per_div_mv) / 1_000.0 * f32::from(channel.probe_attenuation);
    let voltage_span = volts_per_div * SCOPE_VERTICAL_DIVISIONS as f32;
    scope_vertical_offset_y(plot, channel.vertical_offset) - voltage / voltage_span * plot.height()
}

pub(crate) fn scope_vertical_offset_divisions(rect: Rect, vertical_offset: u16) -> f32 {
    let center_y = scope_vertical_offset_y(rect, DSCOPE_VERTICAL_OFFSET_CENTER);
    (center_y - scope_vertical_offset_y(rect, vertical_offset)) / rect.height().max(1.0)
        * SCOPE_VERTICAL_DIVISIONS as f32
}

pub(crate) fn scope_offset_arrow_at_pointer(
    plot: Rect,
    pointer_y: f32,
    settings: ScopeCaptureSettings,
    vertical_offsets: &[u16; 2],
) -> Option<usize> {
    const HIT_RADIUS: f32 = 18.0;
    let mut closest: Option<(usize, f32)> = None;
    for (channel, vertical_offset) in vertical_offsets.iter().copied().enumerate() {
        if !settings.channels[channel].enabled {
            continue;
        }
        let distance = (pointer_y - scope_vertical_offset_y(plot, vertical_offset)).abs();
        let is_closer = match closest {
            Some((_, closest_distance)) => distance < closest_distance,
            None => true,
        };
        if is_closer {
            closest = Some((channel, distance));
        }
    }
    closest
        .filter(|(_, distance)| *distance <= HIT_RADIUS)
        .map(|(channel, _)| channel)
}

pub(crate) struct ScopeOffsetArrowOptions<'a> {
    pub(crate) settings: ScopeCaptureSettings,
    pub(crate) channel_colors: [Color32; 2],
    pub(crate) channel_names: &'a [String; 2],
    pub(crate) vertical_offsets: &'a mut [u16; 2],
    pub(crate) controls_enabled: bool,
}

pub(crate) fn paint_scope_offset_arrows(
    ui: &egui::Ui,
    plot: Rect,
    options: ScopeOffsetArrowOptions<'_>,
) {
    let ScopeOffsetArrowOptions {
        settings,
        channel_colors,
        channel_names,
        vertical_offsets,
        controls_enabled,
    } = options;
    const ARROW_BODY_WIDTH: f32 = 15.0;
    const ARROW_TIP_WIDTH: f32 = 8.0;
    const ARROW_HALF_HEIGHT: f32 = 9.0;
    const DOT_SPACING: f32 = 7.0;
    let painter = ui.painter().with_clip_rect(plot);
    let arrow_x = plot.left() + 2.0;
    let arrow_width = ARROW_BODY_WIDTH + ARROW_TIP_WIDTH;
    let interaction_rect = Rect::from_min_max(
        plot.left_top(),
        Pos2::new(
            (plot.left() + arrow_width + 4.0).min(plot.right()),
            plot.bottom(),
        ),
    );
    let interaction_id = ui.make_persistent_id("scope_vertical_offset_arrows");
    let response = ui.interact(
        interaction_rect,
        interaction_id,
        if controls_enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let hovered_channel = response.hover_pos().and_then(|pointer| {
        scope_offset_arrow_at_pointer(plot, pointer.y, settings, vertical_offsets)
    });
    let response = if let Some(channel) = hovered_channel {
        response
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_selectable_text(format!(
                "{} vertical offset: {:+.2} div\nDrag to move · double-click to center",
                scope_channel_label(channel, channel_names),
                scope_vertical_offset_divisions(plot, vertical_offsets[channel])
            ))
    } else {
        response
    };

    if controls_enabled {
        if response.drag_started() {
            let channel = response.interact_pointer_pos().and_then(|pointer| {
                scope_offset_arrow_at_pointer(plot, pointer.y, settings, vertical_offsets)
            });
            ui.data_mut(|data| {
                data.remove_temp::<usize>(interaction_id);
                if let Some(channel) = channel {
                    data.insert_temp(interaction_id, channel);
                }
            });
        }
        if response.double_clicked() {
            if let Some(channel) = response.interact_pointer_pos().and_then(|pointer| {
                scope_offset_arrow_at_pointer(plot, pointer.y, settings, vertical_offsets)
            }) {
                vertical_offsets[channel] = DSCOPE_VERTICAL_OFFSET_CENTER;
            }
        } else if response.dragged() {
            let channel = ui.data(|data| data.get_temp::<usize>(interaction_id));
            if let (Some(channel), Some(pointer)) = (channel, response.interact_pointer_pos()) {
                vertical_offsets[channel] = scope_vertical_offset_from_y(plot, pointer.y);
            }
        }
        if response.drag_stopped() {
            ui.data_mut(|data| data.remove_temp::<usize>(interaction_id));
        }
    }

    for (channel, color) in channel_colors.iter().enumerate() {
        if !settings.channels[channel].enabled {
            continue;
        }
        let arrow_y = scope_vertical_offset_y(plot, vertical_offsets[channel]);
        let mut x = plot.left();
        while x <= plot.right() {
            painter.circle_filled(Pos2::new(x, arrow_y), 0.8, color.gamma_multiply(0.35));
            x += DOT_SPACING;
        }
    }

    for channel in (0..channel_colors.len()).rev() {
        if !settings.channels[channel].enabled {
            continue;
        }
        let display_color = channel_colors[channel];
        let arrow_y = scope_vertical_offset_y(plot, vertical_offsets[channel]);
        let body_right = arrow_x + ARROW_BODY_WIDTH;
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(arrow_x, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_right, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_right + ARROW_TIP_WIDTH, arrow_y),
                Pos2::new(body_right, arrow_y + ARROW_HALF_HEIGHT),
                Pos2::new(arrow_x, arrow_y + ARROW_HALF_HEIGHT),
            ],
            display_color,
            Stroke::new(1.0_f32, BACKGROUND),
        ));
        painter.text(
            Pos2::new(arrow_x + ARROW_BODY_WIDTH * 0.5, arrow_y),
            egui::Align2::CENTER_CENTER,
            channel.to_string(),
            FontId::monospace(10.0),
            marker_text_color(display_color),
        );
    }
}

pub(crate) fn scope_trigger_level_y(
    plot: Rect,
    channel: ScopeChannelSettings,
    trigger_level: u8,
) -> f32 {
    scope_sample_y(
        plot,
        channel,
        scope_trigger_threshold_voltage(channel, trigger_level),
    )
}

pub(crate) fn scope_trigger_level_from_y(plot: Rect, y: f32, channel: ScopeChannelSettings) -> u8 {
    let zero_y = scope_vertical_offset_y(plot, channel.vertical_offset);
    let screen_divisions = (y - zero_y) / plot.height().max(1.0) * SCOPE_VERTICAL_DIVISIONS as f32;
    let reference_span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    let level = (f32::from(DSCOPE_VERTICAL_OFFSET_CENTER)
        + screen_divisions / SCOPE_ADC_VERTICAL_DIVISIONS * reference_span)
        .round()
        .clamp(
            f32::from(DSCOPE_TRIGGER_LEVEL_MIN),
            f32::from(DSCOPE_TRIGGER_LEVEL_MAX),
        );
    level as u8
}

pub(crate) fn scope_trigger_channels(settings: ScopeTriggerSettings) -> &'static [usize] {
    match settings.source {
        DsCopeTriggerSource::Auto if settings.auto_channel == 0 => &[0],
        DsCopeTriggerSource::Auto => &[1],
        DsCopeTriggerSource::ChannelZero => &[0],
        DsCopeTriggerSource::ChannelOne => &[1],
        DsCopeTriggerSource::ChannelZeroAndOne | DsCopeTriggerSource::ChannelZeroOrOne => &[0, 1],
    }
}

pub(crate) fn scope_trigger_threshold_voltage(
    channel: ScopeChannelSettings,
    trigger_level: u8,
) -> f32 {
    let reference_span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    let input_span = f32::from(channel.volts_per_div_mv) / 1_000.0
        * SCOPE_ADC_VERTICAL_DIVISIONS
        * f32::from(channel.probe_attenuation);
    (f32::from(DSCOPE_VERTICAL_OFFSET_CENTER) - f32::from(trigger_level)) / reference_span
        * input_span
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScopeTriggerCrossing {
    pub(crate) sample: f64,
    pub(crate) channel_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScopeSmartTimingMeasurement {
    pub(crate) start_sample: f64,
    pub(crate) end_sample: f64,
    pub(crate) channel_index: usize,
    pub(crate) end_voltage: f32,
}

const SCOPE_SMART_TIMING_VOLTAGE_FILTER_BLEND: f32 = 0.06;
const SCOPE_SMART_TIMING_POINTER_RESET_DISTANCE: f32 = 4.0;

#[derive(Clone, Copy, Debug, Default)]
struct ScopeSmartTimingVoltageFilter {
    initialized: bool,
    channel_index: usize,
    pointer_anchor_x: f32,
    waveform_revision: u64,
    voltage: f32,
}

impl ScopeSmartTimingVoltageFilter {
    fn update(
        &mut self,
        channel_index: usize,
        pointer_x: f32,
        waveform_revision: u64,
        voltage: f32,
    ) -> f32 {
        let should_reset = !self.initialized
            || self.channel_index != channel_index
            || (self.pointer_anchor_x - pointer_x).abs()
                > SCOPE_SMART_TIMING_POINTER_RESET_DISTANCE
            || !self.voltage.is_finite()
            || !voltage.is_finite();
        if should_reset {
            *self = Self {
                initialized: true,
                channel_index,
                pointer_anchor_x: pointer_x,
                waveform_revision,
                voltage,
            };
        } else if self.waveform_revision != waveform_revision {
            self.waveform_revision = waveform_revision;
            self.voltage += (voltage - self.voltage) * SCOPE_SMART_TIMING_VOLTAGE_FILTER_BLEND;
        }
        self.voltage
    }
}

impl ScopeSmartTimingMeasurement {
    pub(crate) fn duration(self, sample_rate_hz: u64) -> Option<Duration> {
        (sample_rate_hz != 0).then(|| {
            Duration::from_secs_f64(
                (self.end_sample - self.start_sample).abs() / sample_rate_hz as f64,
            )
        })
    }
}

pub(crate) fn scope_trigger_crossing_sample(
    waveforms: &WaveformStore,
    settings: ScopeCaptureSettings,
    trigger: ScopeTriggerSettings,
) -> Option<ScopeTriggerCrossing> {
    // The FPGA trigger pointer may lead or trail the analog threshold crossing
    // slightly because of the ADC/trigger pipeline and the footer byte offset.
    // Select the closest matching edge instead of requiring it to be exactly
    // one sample before the reported position.
    const TRIGGER_SEARCH_RADIUS: usize = 512;
    let hardware_trigger = waveforms
        .scope_trigger_sample_index
        .and_then(|trigger_sample| trigger_sample.checked_sub(waveforms.scope_sample_offset))
        .and_then(|trigger_sample| usize::try_from(trigger_sample).ok());
    if hardware_trigger.is_none() && trigger.source != DsCopeTriggerSource::Auto {
        return None;
    }
    let capture_boundaries = waveforms
        .scope_capture_boundaries
        .iter()
        .filter_map(|boundary| boundary.checked_sub(waveforms.scope_sample_offset))
        .filter_map(|boundary| usize::try_from(boundary).ok())
        .collect::<HashSet<_>>();
    let mut closest_crossing = None::<(ScopeTriggerCrossing, f32)>;

    for &channel_index in scope_trigger_channels(trigger) {
        let channel_settings = settings.channels[channel_index];
        if !channel_settings.enabled {
            continue;
        }
        let Some(samples) = waveforms.scope_channels.get(channel_index) else {
            continue;
        };
        if samples.len() < 2 {
            continue;
        }
        let reference_sample = match hardware_trigger {
            Some(sample) if sample < samples.len() => sample as f32,
            Some(_) => continue,
            None => f32::from(trigger.position_percent) / 100.0 * (samples.len() - 1) as f32,
        };
        let threshold =
            scope_trigger_threshold_voltage(channel_settings, trigger.levels[channel_index]);
        let (first_left, last_left) = if trigger.source == DsCopeTriggerSource::Auto {
            // Auto capture can return a free-running frame without setting the
            // hardware trigger flag. If that frame nevertheless contains the
            // requested edge, lock it to the trigger position just like a
            // hardware-triggered frame. Only free-run when no edge exists.
            (0, samples.len() - 1)
        } else {
            // A non-auto trigger without a hardware position returned above.
            let Some(sample) = hardware_trigger else {
                continue;
            };
            (
                sample.saturating_sub(TRIGGER_SEARCH_RADIUS),
                sample
                    .saturating_add(TRIGGER_SEARCH_RADIUS)
                    .min(samples.len() - 1),
            )
        };
        for left in first_left..last_left {
            let right = left + 1;
            if capture_boundaries.contains(&right) {
                continue;
            }
            let (left_value, right_value) = (samples[left], samples[right]);
            let crosses = match trigger.slope {
                DsCopeTriggerSlope::Rising => {
                    (left_value < threshold && right_value >= threshold)
                        || (left_value <= threshold && right_value > threshold)
                }
                DsCopeTriggerSlope::Falling => {
                    (left_value > threshold && right_value <= threshold)
                        || (left_value >= threshold && right_value < threshold)
                }
            };
            if !crosses {
                continue;
            }
            let delta = right_value - left_value;
            let interpolation = if delta.abs() <= f32::EPSILON {
                0.0
            } else {
                ((threshold - left_value) / delta).clamp(0.0, 1.0)
            };
            let crossing = ScopeTriggerCrossing {
                sample: waveforms.scope_sample_offset as f64
                    + left as f64
                    + f64::from(interpolation),
                channel_index,
            };
            let distance = (left as f32 + interpolation - reference_sample).abs();
            if closest_crossing.map_or(true, |(_, current_distance)| distance < current_distance) {
                closest_crossing = Some((crossing, distance));
            }
        }
    }
    closest_crossing.map(|(crossing, _)| crossing)
}

pub(crate) fn scope_sample_fraction(sample: f64, sample_offset: u64, sample_count: usize) -> f32 {
    let local_sample = sample - sample_offset as f64;
    (local_sample / sample_count.saturating_sub(1).max(1) as f64) as f32
}

pub(crate) fn scope_smart_timing_measurement(
    waveforms: &WaveformStore,
    trigger_crossing: ScopeTriggerCrossing,
    plot: Rect,
    viewport: WaveformViewport,
    display_sample_count: usize,
    pointer: Pos2,
) -> Option<ScopeSmartTimingMeasurement> {
    if !plot.contains(pointer) || display_sample_count == 0 {
        return None;
    }
    let samples = waveforms
        .scope_channels
        .get(trigger_crossing.channel_index)?;
    if samples.is_empty() {
        return None;
    }
    let screen_fraction = (pointer.x - plot.left()) / plot.width().max(1.0);
    let domain_fraction = viewport.domain_fraction_at_screen(screen_fraction);
    let end_sample_index =
        (f64::from(domain_fraction) * display_sample_count.saturating_sub(1) as f64).round();
    if end_sample_index < 0.0 || end_sample_index > samples.len().saturating_sub(1) as f64 {
        return None;
    }
    let end_sample_index = end_sample_index as usize;
    Some(ScopeSmartTimingMeasurement {
        start_sample: trigger_crossing.sample,
        end_sample: waveforms.scope_sample_offset as f64 + end_sample_index as f64,
        channel_index: trigger_crossing.channel_index,
        end_voltage: samples[end_sample_index],
    })
}

pub(crate) fn scope_smart_timing_has_nonzero_trigger(
    settings: ScopeCaptureSettings,
    trigger_crossing: ScopeTriggerCrossing,
) -> bool {
    settings
        .channels
        .get(trigger_crossing.channel_index)
        .is_some_and(|channel| {
            scope_trigger_threshold_voltage(
                *channel,
                settings.trigger.levels[trigger_crossing.channel_index],
            )
            .abs()
                > f32::EPSILON
        })
}

pub(crate) struct ScopeSmartTimingPaintOptions {
    pub(crate) settings: ScopeCaptureSettings,
    pub(crate) viewport: WaveformViewport,
    pub(crate) display_sample_count: usize,
    pub(crate) sample_rate_hz: u64,
    pub(crate) color: Color32,
}

pub(crate) fn paint_scope_timing_hint(
    painter: &egui::Painter,
    plot: Rect,
    waveforms: &WaveformStore,
    measurement: ScopeSmartTimingMeasurement,
    options: ScopeSmartTimingPaintOptions,
) {
    let ScopeSmartTimingPaintOptions {
        settings,
        viewport,
        display_sample_count,
        sample_rate_hz,
        color,
    } = options;
    let painter = painter.with_clip_rect(plot);
    let Some(duration) = measurement.duration(sample_rate_hz) else {
        return;
    };
    let Some(channel) = settings.channels.get(measurement.channel_index).copied() else {
        return;
    };
    let start_x = waveform_sample_x(
        plot,
        viewport,
        measurement.start_sample - waveforms.scope_sample_offset as f64,
        display_sample_count,
    );
    let end_x = waveform_sample_x(
        plot,
        viewport,
        measurement.end_sample - waveforms.scope_sample_offset as f64,
        display_sample_count,
    );
    let start_y = scope_trigger_level_y(
        plot,
        channel,
        settings.trigger.levels[measurement.channel_index],
    );
    let end_y = scope_sample_y(plot, channel, measurement.end_voltage);

    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(start_x.min(end_x), plot.top()),
            Pos2::new(start_x.max(end_x), plot.bottom()),
        ),
        0.0,
        color.gamma_multiply(0.08),
    );
    for (x, opacity) in [(start_x, 0.3), (end_x, 0.5)] {
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(0.75_f32, color.gamma_multiply(opacity)),
        );
    }
    for point in [Pos2::new(start_x, start_y), Pos2::new(end_x, end_y)] {
        painter.circle_filled(point, 2.0, color.gamma_multiply(0.5));
    }

    if plot.width() < 120.0 || plot.height() < 70.0 {
        return;
    }
    let width = 190.0_f32.min(plot.width() - 16.0);
    let hint = Rect::from_min_size(
        Pos2::new(plot.right() - width - 8.0, plot.top() + 26.0),
        Vec2::new(width, 42.0),
    );
    painter.rect_filled(hint, 5.0, half_transparent_plate(PANEL));
    let border = Stroke::new(0.75_f32, color.gamma_multiply(0.65));
    painter.line_segment([hint.left_top(), hint.right_top()], border);
    painter.line_segment([hint.right_top(), hint.right_bottom()], border);
    painter.line_segment([hint.right_bottom(), hint.left_bottom()], border);
    painter.line_segment([hint.left_bottom(), hint.left_top()], border);

    let left = hint.left() + 8.0;
    painter.text(
        Pos2::new(left, hint.top() + 6.0),
        egui::Align2::LEFT_TOP,
        format!("DURATION   {}", format_duration(duration)),
        FontId::monospace(10.0),
        Color32::WHITE,
    );
    painter.text(
        Pos2::new(left, hint.top() + 22.0),
        egui::Align2::LEFT_TOP,
        format!(
            "AMPLITUDE  {}",
            format_trigger_voltage(measurement.end_voltage)
        ),
        FontId::monospace(10.0),
        color,
    );
}

pub(crate) fn should_align_scope_trigger(
    anchor_locked: bool,
    scope_rolling: bool,
    has_trigger_crossing: bool,
) -> bool {
    anchor_locked && (!scope_rolling || has_trigger_crossing)
}

pub(crate) fn scope_plot_sample_indices(
    visible_range: Range<usize>,
    step: usize,
    channel_index: usize,
    trigger_crossing: Option<ScopeTriggerCrossing>,
    sample_offset: u64,
) -> Vec<usize> {
    let mut indices = visible_range
        .clone()
        .step_by(step.max(1))
        .collect::<Vec<_>>();
    if let Some(last) = visible_range.end.checked_sub(1) {
        indices.push(last);
    }
    if let Some(crossing) = trigger_crossing.filter(|crossing| {
        crossing.channel_index == channel_index && crossing.sample >= sample_offset as f64
    }) {
        let local_crossing = crossing.sample - sample_offset as f64;
        for index in [
            local_crossing.floor() as usize,
            local_crossing.ceil() as usize,
        ] {
            if visible_range.contains(&index) {
                indices.push(index);
            }
        }
    }
    indices.sort_unstable();
    indices.dedup();
    indices
}

pub(crate) fn scope_plot_sample_segments(
    visible_range: Range<usize>,
    step: usize,
    channel_index: usize,
    trigger_crossing: Option<ScopeTriggerCrossing>,
    sample_offset: u64,
    capture_boundaries: &VecDeque<u64>,
) -> Vec<Vec<usize>> {
    let mut indices = scope_plot_sample_indices(
        visible_range.clone(),
        step,
        channel_index,
        trigger_crossing,
        sample_offset,
    );
    let local_boundaries = capture_boundaries
        .iter()
        .filter_map(|boundary| boundary.checked_sub(sample_offset))
        .filter_map(|boundary| usize::try_from(boundary).ok())
        .filter(|boundary| *boundary > visible_range.start && *boundary < visible_range.end)
        .collect::<Vec<_>>();
    for boundary in &local_boundaries {
        indices.extend([boundary - 1, *boundary]);
    }
    indices.sort_unstable();
    indices.dedup();

    let mut segments = Vec::new();
    let mut current = Vec::new();
    for index in indices {
        if local_boundaries.binary_search(&index).is_ok() && !current.is_empty() {
            segments.push(std::mem::take(&mut current));
        }
        current.push(index);
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScopeDisplayBucket {
    pub(crate) center_index: f64,
    pub(crate) minimum: f32,
    pub(crate) maximum: f32,
}

pub(crate) fn scope_downsampled_segments(
    samples: &VecDeque<f32>,
    visible_range: Range<usize>,
    step: usize,
    sample_offset: u64,
    capture_boundaries: &VecDeque<u64>,
) -> Vec<Vec<ScopeDisplayBucket>> {
    let end = visible_range.end.min(samples.len());
    let mut segment_start = visible_range.start.min(end);
    let mut segment_ends = capture_boundaries
        .iter()
        .filter_map(|boundary| boundary.checked_sub(sample_offset))
        .filter_map(|boundary| usize::try_from(boundary).ok())
        .filter(|boundary| *boundary > segment_start && *boundary < end)
        .collect::<Vec<_>>();
    segment_ends.push(end);

    let mut segments = Vec::with_capacity(segment_ends.len());
    for segment_end in segment_ends {
        let mut buckets =
            Vec::with_capacity(segment_end.saturating_sub(segment_start).div_ceil(step));
        for bucket_start in (segment_start..segment_end).step_by(step.max(1)) {
            let bucket_end = bucket_start.saturating_add(step).min(segment_end);
            let mut values = samples.range(bucket_start..bucket_end).copied();
            let Some(first) = values.next() else {
                continue;
            };
            let (minimum, maximum) = values.fold((first, first), |(minimum, maximum), value| {
                (minimum.min(value), maximum.max(value))
            });
            buckets.push(ScopeDisplayBucket {
                center_index: (bucket_start as f64 + (bucket_end - 1) as f64) * 0.5,
                minimum,
                maximum,
            });
        }
        if !buckets.is_empty() {
            segments.push(buckets);
        }
        segment_start = segment_end;
    }
    segments
}

pub(crate) fn paint_scope_downsampled_segments(
    painter: &egui::Painter,
    plot: Rect,
    segments: &[Vec<ScopeDisplayBucket>],
    channel: ScopeChannelSettings,
    viewport: WaveformViewport,
    sample_count: usize,
    color: Color32,
) {
    for segment in segments {
        for (index, bucket) in segment.iter().enumerate() {
            let next = segment.get(index + 1).unwrap_or(bucket);
            // Overlap neighbouring envelopes like DSView so steep edges do
            // not leave vertical gaps between adjacent screen columns.
            let minimum = bucket.minimum.min(next.maximum);
            let maximum = bucket.maximum.max(next.minimum);
            let x = waveform_sample_x(plot, viewport, bucket.center_index, sample_count);
            let line = [
                Pos2::new(x, scope_sample_y(plot, channel, maximum)),
                Pos2::new(x, scope_sample_y(plot, channel, minimum)),
            ];
            painter.line_segment(line, Stroke::new(4.0_f32, color.gamma_multiply(0.16)));
            painter.line_segment(line, Stroke::new(1.5_f32, color));
        }
    }
}

pub(crate) fn scope_trigger_position_x(plot: Rect, position_percent: u8) -> f32 {
    egui::lerp(plot.x_range(), f32::from(position_percent) / 100.0)
}

pub(crate) fn scope_trigger_position_from_x(plot: Rect, x: f32) -> u8 {
    let ratio = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    (ratio * 100.0).round().clamp(
        f32::from(DSCOPE_TRIGGER_POSITION_MIN_PERCENT),
        f32::from(DSCOPE_TRIGGER_POSITION_MAX_PERCENT),
    ) as u8
}

pub(crate) fn scope_trigger_position_in_view(
    trigger_sample_fraction: f32,
    viewport: WaveformViewport,
) -> u8 {
    let position_percent =
        ((trigger_sample_fraction - viewport.start) / viewport.span.max(f32::EPSILON) * 100.0)
            .round() as i32;
    position_percent.clamp(
        i32::from(DSCOPE_TRIGGER_POSITION_MIN_PERCENT),
        i32::from(DSCOPE_TRIGGER_POSITION_MAX_PERCENT),
    ) as u8
}

pub(crate) fn finish_scope_viewport_pan(
    trigger_sample_fraction: f32,
    viewport: &mut WaveformViewport,
    trigger_settings: &mut ScopeTriggerSettings,
) {
    trigger_settings.position_percent =
        scope_trigger_position_in_view(trigger_sample_fraction, *viewport);
    // A manual waveform pan must remain detached from the trigger. Relocking
    // here causes the next frame to recenter the viewport and makes time
    // markers appear to jump away from their capture positions.
    viewport.anchor_locked = false;
}

pub(crate) fn paint_scope_trigger_position(
    ui: &egui::Ui,
    time_ruler: Rect,
    plot: Rect,
    settings: &mut ScopeTriggerSettings,
    controls_enabled: bool,
    viewport: &mut WaveformViewport,
) {
    const COLOR: Color32 = Color32::from_rgb(235, 72, 72);
    const ARROW_HALF_WIDTH: f32 = 10.0;
    const ARROW_HEIGHT: f32 = 19.0;
    const ARROW_SHOULDER_HEIGHT: f32 = 9.0;
    const DOT_SPACING: f32 = 7.0;
    let x = scope_trigger_position_x(plot, settings.position_percent);
    let arrow_top = plot.top() - ARROW_HEIGHT;
    let marker_rect = Rect::from_min_max(
        Pos2::new(x - ARROW_HALF_WIDTH - 4.0, arrow_top - 3.0),
        Pos2::new(x + ARROW_HALF_WIDTH + 4.0, plot.top() + 8.0),
    );
    let response = ui.interact(
        marker_rect,
        ui.make_persistent_id("scope_trigger_position"),
        if controls_enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let response = if controls_enabled {
        response.on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
    } else {
        response
    }
    .on_hover_selectable_text(format!("{}%", settings.position_percent));

    if controls_enabled {
        let old_position = settings.position_percent;
        if response.double_clicked() {
            settings.position_percent = 50;
        } else if response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                settings.position_percent = scope_trigger_position_from_x(plot, pointer.x);
            }
        }
        if settings.position_percent != old_position {
            viewport.move_screen_anchor(
                f32::from(old_position) / 100.0,
                f32::from(settings.position_percent) / 100.0,
            );
            ui.ctx().request_repaint();
        }
    }

    let x = scope_trigger_position_x(plot, settings.position_percent);
    let clip_rect = Rect::from_min_max(
        Pos2::new(plot.left(), time_ruler.top()),
        plot.right_bottom(),
    );
    let painter = ui.painter().with_clip_rect(clip_rect);
    let mut y = plot.top();
    while y <= plot.bottom() {
        painter.circle_filled(Pos2::new(x, y), 0.85, COLOR.gamma_multiply(0.62));
        y += DOT_SPACING;
    }
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(x - ARROW_HALF_WIDTH, plot.top() - ARROW_HEIGHT),
            Pos2::new(x + ARROW_HALF_WIDTH, plot.top() - ARROW_HEIGHT),
            Pos2::new(x + ARROW_HALF_WIDTH, plot.top() - ARROW_SHOULDER_HEIGHT),
            Pos2::new(x, plot.top()),
            Pos2::new(x - ARROW_HALF_WIDTH, plot.top() - ARROW_SHOULDER_HEIGHT),
        ],
        COLOR,
        Stroke::new(1.0_f32, BACKGROUND),
    ));
    painter.text(
        Pos2::new(x, plot.top() - ARROW_HEIGHT + 5.5),
        egui::Align2::CENTER_CENTER,
        "T",
        FontId::monospace(9.0),
        marker_text_color(COLOR),
    );
}

pub(crate) fn paint_scope_trigger_level(
    ui: &egui::Ui,
    plot: Rect,
    scope_settings: ScopeCaptureSettings,
    settings: &mut ScopeTriggerSettings,
    channel_colors: [Color32; 2],
    vertical_offsets: &[u16; 2],
    controls_enabled: bool,
) {
    const ARROW_BODY_WIDTH: f32 = 19.0;
    const ARROW_TIP_WIDTH: f32 = 8.0;
    const ARROW_HALF_HEIGHT: f32 = 9.0;
    const DOT_SPACING: f32 = 7.0;
    let painter = ui.painter().with_clip_rect(plot);
    let active_channels = scope_trigger_channels(*settings)
        .iter()
        .copied()
        .filter(|channel| scope_settings.channels[*channel].enabled)
        .collect::<Vec<_>>();

    for channel in active_channels.iter().copied() {
        let color = channel_colors[channel];
        let mut channel_settings = scope_settings.channels[channel];
        channel_settings.vertical_offset = vertical_offsets[channel];
        let trigger_voltage =
            scope_trigger_threshold_voltage(channel_settings, settings.levels[channel]);
        let arrow_x = plot.right() - 2.0;
        let arrow_width = ARROW_BODY_WIDTH + ARROW_TIP_WIDTH;
        let arrow_y = scope_trigger_level_y(plot, channel_settings, settings.levels[channel]);
        let hit_rect = Rect::from_center_size(
            Pos2::new(arrow_x - arrow_width * 0.5, arrow_y),
            Vec2::new(arrow_width + 4.0, ARROW_HALF_HEIGHT * 2.0 + 10.0),
        );
        let response = ui.interact(
            hit_rect,
            ui.make_persistent_id(("scope_trigger_level", channel)),
            if controls_enabled {
                Sense::click_and_drag()
            } else {
                Sense::hover()
            },
        );
        let response = if controls_enabled {
            response.on_hover_cursor(egui::CursorIcon::ResizeVertical)
        } else {
            response
        }
        .on_hover_selectable_text(format!(
            "Trigger level: {}",
            format_trigger_voltage(trigger_voltage)
        ));

        if controls_enabled {
            if response.double_clicked() {
                settings.levels[channel] = DSCOPE_TRIGGER_LEVEL_CENTER;
            } else if response.dragged() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    settings.levels[channel] =
                        scope_trigger_level_from_y(plot, pointer.y, channel_settings);
                }
            }
        }

        let arrow_y = scope_trigger_level_y(plot, channel_settings, settings.levels[channel]);
        let mut x = plot.left();
        while x <= plot.right() {
            painter.circle_filled(Pos2::new(x, arrow_y), 0.8, color.gamma_multiply(0.45));
            x += DOT_SPACING;
        }
        let body_left = arrow_x - ARROW_BODY_WIDTH;
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(arrow_x, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_left, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_left - ARROW_TIP_WIDTH, arrow_y),
                Pos2::new(body_left, arrow_y + ARROW_HALF_HEIGHT),
                Pos2::new(arrow_x, arrow_y + ARROW_HALF_HEIGHT),
            ],
            color,
            Stroke::new(1.0_f32, BACKGROUND),
        ));
        painter.text(
            Pos2::new(arrow_x - ARROW_BODY_WIDTH * 0.5, arrow_y),
            egui::Align2::CENTER_CENTER,
            format!("T{channel}"),
            FontId::monospace(9.0),
            marker_text_color(color),
        );
    }
}

pub(crate) struct ScopeMeasurementPaintOptions<'a> {
    pub(crate) visible_range: Range<usize>,
    pub(crate) sample_rate_hz: u64,
    pub(crate) settings: ScopeFunctionSettings,
    pub(crate) enabled_channels: [bool; 2],
    pub(crate) channel_colors: [Color32; 2],
    pub(crate) channel_names: &'a [String; 2],
    pub(crate) caches: &'a mut [ScopeMeasurementCache; 2],
    pub(crate) window_positions: &'a mut [Vec2; 2],
}

pub(crate) fn paint_scope_measurements(
    ui: &egui::Ui,
    painter: &egui::Painter,
    plot: Rect,
    waveforms: &WaveformStore,
    options: ScopeMeasurementPaintOptions<'_>,
) {
    let ScopeMeasurementPaintOptions {
        visible_range,
        sample_rate_hz,
        settings,
        enabled_channels,
        channel_colors,
        channel_names,
        caches,
        window_positions,
    } = options;
    const ROW_HEIGHT: f32 = 20.0;
    const COLUMN_WIDTH: f32 = 176.0;
    const PADDING: f32 = 8.0;
    const HEADER_HEIGHT: f32 = 25.0;
    const HEADER_FONT_SIZE: f32 = 10.0;
    const LABEL_FONT_SIZE: f32 = 10.0;
    const VALUE_FONT_SIZE: f32 = 12.0;

    for channel in 0..2 {
        let channel_settings = settings.measurements.channels[channel];
        if !enabled_channels[channel]
            || !channel_settings.enabled
            || waveforms.scope_channels.get(channel).is_none()
        {
            continue;
        }
        let selected = SCOPE_MEASUREMENT_KINDS
            .into_iter()
            .filter(|kind| channel_settings.selected[kind.index()])
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let results =
            caches[channel].results(waveforms, channel, visible_range.clone(), sample_rate_hz);
        let rows = selected.len();
        let desired_size = Vec2::new(
            COLUMN_WIDTH + PADDING * 2.0,
            HEADER_HEIGHT + rows as f32 * ROW_HEIGHT + PADDING * 2.0,
        );
        let panel_size = Vec2::new(
            desired_size.x.min(plot.width()),
            desired_size.y.min(plot.height()),
        );
        let mut panel = scope_measurement_window_rect(plot, panel_size, window_positions[channel]);
        let header = Rect::from_min_size(
            panel.min,
            Vec2::new(panel.width(), HEADER_HEIGHT.min(panel.height())),
        );
        let response = ui
            .interact(
                header,
                ui.make_persistent_id(("scope_measurement_window", channel)),
                Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab)
            .on_hover_selectable_text(format!(
                "Drag the {} measurement window",
                scope_channel_label(channel, channel_names)
            ));
        if response.dragged() {
            let delta = ui.input(|input| input.pointer.delta());
            panel = panel.translate(delta);
            panel = panel.translate(Vec2::new(
                panel
                    .left()
                    .clamp(plot.left(), plot.right() - panel.width())
                    - panel.left(),
                panel
                    .top()
                    .clamp(plot.top(), plot.bottom() - panel.height())
                    - panel.top(),
            ));
            window_positions[channel] = scope_measurement_window_position(plot, panel);
            ui.ctx().request_repaint();
        }

        let color = channel_colors[channel];
        painter.rect_filled(
            panel,
            egui::Rounding::same(5.0),
            half_transparent_plate(Color32::from_rgb(8, 12, 18)),
        );
        painter.rect_filled(
            Rect::from_min_size(panel.min, Vec2::new(panel.width(), HEADER_HEIGHT)),
            egui::Rounding::same(5.0),
            color.gamma_multiply(0.22),
        );
        painter.text(
            panel.left_top() + Vec2::new(PADDING, 5.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{} MEASUREMENTS",
                scope_channel_label(channel, channel_names)
            ),
            FontId::monospace(HEADER_FONT_SIZE),
            color,
        );
        let grip_origin = panel.right_top() + Vec2::new(-PADDING - 7.0, 7.0);
        for row in 0..3 {
            for column in 0..2 {
                painter.circle_filled(
                    grip_origin + Vec2::new(column as f32 * 4.0, row as f32 * 4.0),
                    1.0,
                    color,
                );
            }
        }

        for (ordinal, kind) in selected.into_iter().enumerate() {
            let row = ordinal;
            let origin = panel.left_top()
                + Vec2::new(PADDING, PADDING + HEADER_HEIGHT + row as f32 * ROW_HEIGHT);
            painter.text(
                origin,
                egui::Align2::LEFT_TOP,
                kind.label(),
                FontId::monospace(LABEL_FONT_SIZE),
                TEXT_MUTED,
            );
            painter.text(
                origin + Vec2::new(COLUMN_WIDTH - PADDING * 2.0, -1.0),
                egui::Align2::RIGHT_TOP,
                results.formatted(kind),
                FontId::monospace(VALUE_FONT_SIZE),
                color,
            );
        }
    }
}

pub(crate) fn scope_measurement_window_rect(plot: Rect, size: Vec2, position: Vec2) -> Rect {
    let travel = Vec2::new(
        (plot.width() - size.x).max(0.0),
        (plot.height() - size.y).max(0.0),
    );
    Rect::from_min_size(
        plot.min
            + Vec2::new(
                position.x.clamp(0.0, 1.0) * travel.x,
                position.y.clamp(0.0, 1.0) * travel.y,
            ),
        size,
    )
}

pub(crate) fn scope_measurement_window_position(plot: Rect, panel: Rect) -> Vec2 {
    let travel = Vec2::new(
        (plot.width() - panel.width()).max(0.0),
        (plot.height() - panel.height()).max(0.0),
    );
    Vec2::new(
        if travel.x > 0.0 {
            (panel.left() - plot.left()) / travel.x
        } else {
            0.0
        },
        if travel.y > 0.0 {
            (panel.top() - plot.top()) / travel.y
        } else {
            0.0
        },
    )
}

pub(crate) struct ScopeMarkerPaintOptions<'a> {
    pub(crate) settings: &'a mut ScopeMarkerSettings,
    pub(crate) scope_settings: ScopeCaptureSettings,
    pub(crate) channel_names: &'a [String; 2],
    pub(crate) sample_count: usize,
    pub(crate) sample_rate_hz: u64,
    pub(crate) viewport: WaveformViewport,
}

pub(crate) const SCOPE_MARKER_LINE_WIDTH: f32 = 0.5;

pub(crate) fn paint_scope_markers(
    ui: &egui::Ui,
    painter: &egui::Painter,
    plot: Rect,
    options: ScopeMarkerPaintOptions<'_>,
) {
    let ScopeMarkerPaintOptions {
        settings: marker_settings,
        scope_settings,
        channel_names,
        sample_count,
        sample_rate_hz,
        viewport,
    } = options;
    for channel in 0..2 {
        if !scope_settings.channels[channel].enabled {
            continue;
        }
        let channel_markers = &mut marker_settings.channels[channel];
        let horizontal_count = channel_markers.horizontal_count.min(SCOPE_MARKER_LIMIT);
        for (index, position) in channel_markers.horizontal_positions[..horizontal_count]
            .iter_mut()
            .enumerate()
        {
            let color = channel_markers.horizontal_colors[index];
            let x = egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain(position.clamp(0.0, 1.0)),
            );
            if (plot.left()..=plot.right()).contains(&x) {
                let hit_rect = Rect::from_min_max(
                    Pos2::new(x - 7.0, plot.top()),
                    Pos2::new(x + 7.0, plot.bottom()),
                );
                let response = ui
                    .interact(
                        hit_rect,
                        ui.make_persistent_id(("scope_time_marker", channel, index)),
                        Sense::drag(),
                    )
                    .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                    .on_hover_selectable_text(format!(
                        "Drag {} T{} horizontally",
                        scope_channel_label(channel, channel_names),
                        index + 1
                    ));
                if response.dragged() {
                    if let Some(pointer) = response.interact_pointer_pos() {
                        let screen_fraction = (pointer.x - plot.left()) / plot.width().max(1.0);
                        *position = viewport
                            .domain_fraction_at_screen(screen_fraction)
                            .clamp(0.0, 1.0);
                    }
                }
            }
            let x = egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain(position.clamp(0.0, 1.0)),
            );
            paint_dashed_line(
                painter,
                Pos2::new(x, plot.top()),
                Pos2::new(x, plot.bottom()),
                Stroke::new(SCOPE_MARKER_LINE_WIDTH, color),
            );
            paint_marker_handle(
                painter,
                Pos2::new(x, plot.top() + 10.0),
                &format!("T{channel}{}", index + 1),
                color,
            );
        }
        for index in 0..horizontal_count {
            let Some(other) = channel_markers.horizontal_comparisons[index]
                .filter(|other| *other < horizontal_count && *other != index)
            else {
                continue;
            };
            if other < index && channel_markers.horizontal_comparisons[other] == Some(index) {
                continue;
            }
            let first = channel_markers.horizontal_positions[index];
            let second = channel_markers.horizontal_positions[other];
            let Some(delta_seconds) =
                scope_time_marker_delta(first, second, sample_count, sample_rate_hz)
            else {
                continue;
            };
            let frequency = (delta_seconds > 0.0).then(|| 1.0 / delta_seconds);
            let text = match frequency {
                Some(frequency) => format!(
                    "{} T{}–T{}  {}  ·  {}",
                    scope_channel_label(channel, channel_names),
                    index + 1,
                    other + 1,
                    format_duration(Duration::from_secs_f64(delta_seconds)),
                    format_frequency(frequency)
                ),
                None => format!(
                    "{} T{}–T{}  {}",
                    scope_channel_label(channel, channel_names),
                    index + 1,
                    other + 1,
                    format_duration(Duration::from_secs_f64(delta_seconds))
                ),
            };
            let center_x = egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain((first + second) * 0.5),
            );
            paint_marker_readout(
                painter,
                plot,
                Pos2::new(
                    center_x,
                    plot.top() + 33.0 + index as f32 * 27.0 + channel as f32 * 13.0,
                ),
                &text,
                channel_markers.horizontal_colors[index],
            );
        }

        let vertical_count = channel_markers.vertical_count.min(SCOPE_MARKER_LIMIT);
        for (index, position) in channel_markers.vertical_positions[..vertical_count]
            .iter_mut()
            .enumerate()
        {
            let color = channel_markers.vertical_colors[index];
            let y = egui::lerp(plot.y_range(), position.clamp(0.0, 1.0));
            let hit_rect = Rect::from_min_max(
                Pos2::new(plot.left(), y - 7.0),
                Pos2::new(plot.right(), y + 7.0),
            );
            let response = ui
                .interact(
                    hit_rect,
                    ui.make_persistent_id(("scope_voltage_marker", channel, index)),
                    Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeVertical)
                .on_hover_selectable_text(format!(
                    "Drag {} V{} vertically",
                    scope_channel_label(channel, channel_names),
                    index + 1
                ));
            if response.dragged() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    *position = ((pointer.y - plot.top()) / plot.height().max(1.0)).clamp(0.0, 1.0);
                }
            }
            let y = egui::lerp(plot.y_range(), position.clamp(0.0, 1.0));
            paint_dashed_line(
                painter,
                Pos2::new(plot.left(), y),
                Pos2::new(plot.right(), y),
                Stroke::new(SCOPE_MARKER_LINE_WIDTH, color),
            );
            paint_marker_handle(
                painter,
                Pos2::new(plot.left() + 17.0, y),
                &format!("V{channel}{}", index + 1),
                color,
            );
        }
        for index in 0..vertical_count {
            let Some(other) = channel_markers.vertical_comparisons[index]
                .filter(|other| *other < vertical_count && *other != index)
            else {
                continue;
            };
            if other < index && channel_markers.vertical_comparisons[other] == Some(index) {
                continue;
            }
            let first = channel_markers.vertical_positions[index];
            let second = channel_markers.vertical_positions[other];
            let delta_voltage =
                scope_voltage_marker_delta(first, second, scope_settings.channels[channel]);
            let center_y = egui::lerp(plot.y_range(), (first + second) * 0.5);
            let center_x = if channel == 0 {
                plot.left() + 125.0
            } else {
                plot.right() - 125.0
            };
            paint_marker_readout(
                painter,
                plot,
                Pos2::new(center_x, center_y),
                &format!(
                    "{} V{}–V{}  {}",
                    scope_channel_label(channel, channel_names),
                    index + 1,
                    other + 1,
                    format_marker_voltage(delta_voltage)
                ),
                channel_markers.vertical_colors[index],
            );
        }
    }
}

pub(crate) fn paint_scope_quick_time_measurement(
    painter: &egui::Painter,
    plot: Rect,
    measurement: ScopeQuickTimeMeasurement,
    sample_count: usize,
    sample_rate_hz: u64,
    viewport: WaveformViewport,
) {
    const COLOR: Color32 = Color32::from_rgb(255, 214, 76);

    let (first_position, second_position) = match measurement {
        ScopeQuickTimeMeasurement::Idle => return,
        ScopeQuickTimeMeasurement::AwaitingSecond { first_position } => (first_position, None),
        ScopeQuickTimeMeasurement::Complete {
            first_position,
            second_position,
        } => (first_position, Some(second_position)),
    };
    let first_x = egui::lerp(
        plot.x_range(),
        viewport.screen_fraction_at_domain(first_position),
    );
    paint_dashed_line(
        painter,
        Pos2::new(first_x, plot.top()),
        Pos2::new(first_x, plot.bottom()),
        Stroke::new(SCOPE_MARKER_LINE_WIDTH, COLOR),
    );
    paint_marker_handle(
        painter,
        Pos2::new(first_x, plot.bottom() - 11.0),
        "Δt1",
        COLOR,
    );

    let Some(second_position) = second_position else {
        paint_marker_readout(
            painter,
            plot,
            Pos2::new(first_x, plot.bottom() - 39.0),
            "Click to place Δt2",
            COLOR,
        );
        return;
    };

    let second_x = egui::lerp(
        plot.x_range(),
        viewport.screen_fraction_at_domain(second_position),
    );
    paint_dashed_line(
        painter,
        Pos2::new(second_x, plot.top()),
        Pos2::new(second_x, plot.bottom()),
        Stroke::new(SCOPE_MARKER_LINE_WIDTH, COLOR),
    );
    paint_marker_handle(
        painter,
        Pos2::new(second_x, plot.bottom() - 11.0),
        "Δt2",
        COLOR,
    );
    let Some(delta_seconds) = scope_time_marker_delta(
        first_position,
        second_position,
        sample_count,
        sample_rate_hz,
    ) else {
        return;
    };
    paint_marker_readout(
        painter,
        plot,
        Pos2::new((first_x + second_x) * 0.5, plot.bottom() - 39.0),
        &format!(
            "Δt  {}",
            format_duration(Duration::from_secs_f64(delta_seconds))
        ),
        COLOR,
    );
}

pub(crate) fn scope_marker_color(channel: usize, marker: usize) -> Color32 {
    const CH0_COLORS: [Color32; SCOPE_MARKER_LIMIT] = [
        Color32::from_rgb(255, 204, 74),
        Color32::from_rgb(255, 132, 172),
        Color32::from_rgb(255, 145, 77),
        Color32::from_rgb(213, 126, 255),
        Color32::from_rgb(180, 230, 80),
        Color32::from_rgb(255, 225, 120),
        Color32::from_rgb(255, 105, 105),
        Color32::from_rgb(232, 170, 255),
        Color32::from_rgb(255, 180, 95),
        Color32::from_rgb(205, 240, 120),
    ];
    const CH1_COLORS: [Color32; SCOPE_MARKER_LIMIT] = [
        Color32::from_rgb(99, 179, 255),
        Color32::from_rgb(70, 210, 185),
        Color32::from_rgb(174, 130, 255),
        Color32::from_rgb(100, 205, 255),
        Color32::from_rgb(245, 100, 100),
        Color32::from_rgb(80, 235, 205),
        Color32::from_rgb(135, 205, 255),
        Color32::from_rgb(205, 165, 255),
        Color32::from_rgb(85, 225, 240),
        Color32::from_rgb(255, 135, 180),
    ];
    if channel == 0 {
        CH0_COLORS[marker % SCOPE_MARKER_LIMIT]
    } else {
        CH1_COLORS[marker % SCOPE_MARKER_LIMIT]
    }
}

pub(crate) fn scope_time_marker_delta(
    first_position: f32,
    second_position: f32,
    sample_count: usize,
    sample_rate_hz: u64,
) -> Option<f64> {
    if sample_count == 0 || sample_rate_hz == 0 {
        return None;
    }
    let capture_seconds = sample_count as f64 / sample_rate_hz as f64;
    Some(f64::from((second_position - first_position).abs()) * capture_seconds)
}

pub(crate) fn scope_voltage_marker_delta(
    first_position: f32,
    second_position: f32,
    channel: ScopeChannelSettings,
) -> f32 {
    let volts_per_division =
        f32::from(channel.volts_per_div_mv) / 1_000.0 * f32::from(channel.probe_attenuation);
    (second_position - first_position).abs() * volts_per_division * SCOPE_VERTICAL_DIVISIONS as f32
}

pub(crate) fn format_marker_voltage(value: f32) -> String {
    if value >= 1.0 {
        format!("{value:.3} V")
    } else if value >= 1.0e-3 {
        format!("{:.2} mV", value * 1.0e3)
    } else {
        format!("{:.2} µV", value * 1.0e6)
    }
}

pub(crate) fn scope_math_value(
    waveforms: &WaveformStore,
    settings: ScopeMathSettings,
    sample_index: usize,
) -> Option<f32> {
    let left = *waveforms
        .scope_channels
        .get(settings.left_channel)?
        .get(sample_index)?;
    let right = *waveforms
        .scope_channels
        .get(settings.right_channel)?
        .get(sample_index)?;
    settings.operation.apply(left, right)
}

pub(crate) fn scope_math_unit(operation: ScopeMathOperation) -> &'static str {
    match operation {
        ScopeMathOperation::Add | ScopeMathOperation::Subtract => "V",
        ScopeMathOperation::Multiply => "V²",
        ScopeMathOperation::Divide => "ratio",
    }
}

pub(crate) struct ScopeMathPaintOptions<'a> {
    pub(crate) viewport: WaveformViewport,
    pub(crate) sample_count: usize,
    pub(crate) visible_range: Range<usize>,
    pub(crate) step: usize,
    pub(crate) settings: ScopeMathSettings,
    pub(crate) channel_names: &'a [String; 2],
}

pub(crate) fn paint_scope_math_trace(
    painter: &egui::Painter,
    plot: Rect,
    waveforms: &WaveformStore,
    options: ScopeMathPaintOptions<'_>,
) {
    let ScopeMathPaintOptions {
        viewport,
        sample_count,
        visible_range,
        step,
        settings,
        channel_names,
    } = options;
    if !settings.enabled || visible_range.is_empty() {
        return;
    }
    let segments = scope_plot_sample_segments(
        visible_range,
        step,
        usize::MAX,
        None,
        waveforms.scope_sample_offset,
        &waveforms.scope_capture_boundaries,
    );
    let maximum = segments
        .iter()
        .flatten()
        .filter_map(|index| scope_math_value(waveforms, settings, *index))
        .map(f32::abs)
        .fold(0.0_f32, f32::max)
        .max(f32::EPSILON);
    for segment in segments {
        let points = segment
            .into_iter()
            .filter_map(|index| {
                scope_math_value(waveforms, settings, index).map(|value| {
                    let x = waveform_sample_x(plot, viewport, index as f64, sample_count);
                    let y = plot.center().y - value / maximum * plot.height() * 0.44;
                    Pos2::new(x, y)
                })
            })
            .collect::<Vec<_>>();
        if points.len() > 1 {
            painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(4.0_f32, MATH_COLOR.gamma_multiply(0.16)),
            ));
            painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, MATH_COLOR)));
        }
    }
    painter.text(
        plot.right_top() + Vec2::new(-8.0, 7.0),
        egui::Align2::RIGHT_TOP,
        format!(
            "M  {} {} {}  auto ±{:.3} {}",
            scope_channel_label(settings.left_channel, channel_names),
            settings.operation.symbol(),
            scope_channel_label(settings.right_channel, channel_names),
            maximum,
            scope_math_unit(settings.operation),
        ),
        FontId::monospace(10.0),
        MATH_COLOR,
    );
}

pub(crate) fn paint_scope(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    viewport: &mut WaveformViewport,
    fft_viewport: &mut ScopeFftViewport,
    scope_options: ScopeWaveformOptions<'_>,
) {
    let fft_settings = scope_options.function_settings.fft;
    let channel_names = scope_options.channel_names;
    let analog_bandwidth_hz = match scope_options.settings.bandwidth_limit {
        DsCopeBandwidthLimit::Full => DSCOPE_U3P100_FULL_ANALOG_BANDWIDTH_HZ,
        DsCopeBandwidthLimit::Mhz20 => DSCOPE_LIMITED_ANALOG_BANDWIDTH_HZ,
    };
    if !fft_settings.enabled {
        paint_scope_time_domain(ui, rect, waveforms, viewport, scope_options);
        return;
    }

    const FFT_GAP: f32 = 8.0;
    let desired_fft_height = (rect.height() * 0.36).clamp(80.0, 220.0);
    let fft_height = desired_fft_height.min((rect.height() - FFT_GAP - 64.0).max(32.0));
    let time_height = (rect.height() - FFT_GAP - fft_height).max(0.0);
    let time_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), time_height));
    let fft_rect = Rect::from_min_size(
        Pos2::new(rect.left(), time_rect.bottom() + FFT_GAP),
        Vec2::new(rect.width(), fft_height),
    );
    paint_scope_time_domain(ui, time_rect, waveforms, viewport, scope_options);
    paint_scope_fft(
        ui,
        fft_rect,
        waveforms,
        ScopeFftPaintOptions {
            viewport: *viewport,
            fft_viewport,
            settings: fft_settings,
            channel_names,
            analog_bandwidth_hz,
        },
    );
}

/// How many samples the scope view spans and how fast they were taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeDisplayExtent {
    /// Samples held by the shortest enabled channel.
    sample_count: usize,
    /// Samples the horizontal axis is divided into; this is the capture size
    /// until the first samples arrive, and the ring capacity while rolling.
    display_sample_count: usize,
    display_sample_rate_hz: u64,
}

fn scope_display_extent(
    waveforms: &WaveformStore,
    settings: ScopeCaptureSettings,
) -> ScopeDisplayExtent {
    let sample_count = waveforms
        .scope_channels
        .iter()
        .enumerate()
        .filter(|(index, _)| settings.channels[*index].enabled)
        .map(|(_, samples)| samples)
        .map(VecDeque::len)
        .min()
        .unwrap_or(0);
    let display_sample_count = if waveforms.scope_rolling {
        waveforms.scope_capacity
    } else if sample_count == 0 {
        settings.sample_count as usize
    } else {
        sample_count
    };
    let display_sample_rate_hz = if waveforms.scope_rate_hz == 0 {
        settings.sample_rate_hz
    } else {
        waveforms.scope_rate_hz
    };
    ScopeDisplayExtent {
        sample_count,
        display_sample_count,
        display_sample_rate_hz,
    }
}

/// Where the trigger sits in the sample domain and on screen, as fractions.
fn scope_trigger_fractions(
    waveforms: &WaveformStore,
    settings: ScopeCaptureSettings,
    trigger_crossing: Option<ScopeTriggerCrossing>,
    trigger_settings: ScopeTriggerSettings,
    display_sample_count: usize,
) -> (f32, f32) {
    let trigger_sample_fraction = trigger_crossing
        .map(|crossing| crossing.sample)
        .or_else(|| {
            waveforms
                .scope_trigger_sample_index
                .map(|sample| sample as f64)
        })
        .map(|sample| {
            scope_sample_fraction(sample, waveforms.scope_sample_offset, display_sample_count)
        })
        .unwrap_or_else(|| f32::from(settings.trigger.position_percent) / 100.0);
    let trigger_screen_fraction = f32::from(trigger_settings.position_percent) / 100.0;
    (trigger_sample_fraction, trigger_screen_fraction)
}

/// Geometry and sample extent shared by the parts of the time-domain view.
#[derive(Clone, Copy)]
struct ScopeTimeDomainView {
    plot: Rect,
    time_ruler: Rect,
    status_bar: Rect,
    extent: ScopeDisplayExtent,
    viewport: WaveformViewport,
}

pub(crate) fn paint_scope_time_domain(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    viewport: &mut WaveformViewport,
    scope_options: ScopeWaveformOptions<'_>,
) {
    let mut options = scope_options;
    for (channel, vertical_offset) in options
        .settings
        .channels
        .iter_mut()
        .zip(*options.vertical_offsets)
    {
        channel.vertical_offset = vertical_offset;
    }
    let extent = scope_display_extent(waveforms, options.settings);
    let ScopeDisplayExtent {
        sample_count,
        display_sample_count,
        display_sample_rate_hz,
    } = extent;
    if let Some(selected_timebase) = *options.time_per_division_seconds {
        apply_time_per_division(
            viewport,
            display_sample_count,
            display_sample_rate_hz,
            selected_timebase,
        );
    }
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    let (status_area, ruler_area, plot) = split_waveform_chrome(rect);
    let (status_bar, time_ruler) = align_waveform_chrome(status_area, ruler_area, plot);
    let scope_painter = painter.with_clip_rect(plot);
    paint_scope_graticule(&scope_painter, plot);
    let trigger_crossing =
        scope_trigger_crossing_sample(waveforms, options.settings, *options.trigger_settings);
    let (trigger_sample_fraction, trigger_screen_fraction) = scope_trigger_fractions(
        waveforms,
        options.settings,
        trigger_crossing,
        *options.trigger_settings,
        display_sample_count,
    );
    if should_align_scope_trigger(
        viewport.anchor_locked,
        waveforms.scope_rolling,
        trigger_crossing.is_some(),
    ) {
        viewport.align_trigger_to_screen(trigger_sample_fraction, trigger_screen_fraction);
    }
    update_scope_viewport(
        ui,
        plot,
        waveforms,
        extent,
        (trigger_sample_fraction, trigger_screen_fraction),
        &mut options,
        viewport,
    );
    for (channel, vertical_offset) in options
        .settings
        .channels
        .iter_mut()
        .zip(*options.vertical_offsets)
    {
        channel.vertical_offset = vertical_offset;
    }
    options.settings.trigger = *options.trigger_settings;

    let visible_range = viewport.sample_range_in_domain(sample_count, display_sample_count);
    let visible_count = visible_range.len();
    let view = ScopeTimeDomainView {
        plot,
        time_ruler,
        status_bar,
        extent,
        viewport: *viewport,
    };
    if visible_count == 0 {
        paint_empty(&scope_painter, plot, "Waiting for oscilloscope samples");
        paint_scope_references(ui, &scope_painter, &view, &mut options);
        paint_scope_footer(ui, painter, &view, waveforms, &mut options, viewport);
        return;
    }

    let step = (visible_count as f32 / plot.width().max(1.0))
        .floor()
        .max(1.0) as usize;

    paint_scope_traces(
        &scope_painter,
        waveforms,
        &view,
        &options,
        &visible_range,
        step,
        trigger_crossing,
    );

    paint_scope_math_trace(
        &scope_painter,
        plot,
        waveforms,
        ScopeMathPaintOptions {
            viewport: *viewport,
            sample_count: display_sample_count,
            visible_range: visible_range.clone(),
            step,
            settings: options.function_settings.math,
            channel_names: options.channel_names,
        },
    );

    // Keep channel and trigger references in the foreground so dense or bright
    // waveforms never obscure their arrows, labels, or dotted level lines.
    paint_scope_references(ui, &scope_painter, &view, &mut options);
    paint_scope_analysis_overlays(
        ui,
        &scope_painter,
        waveforms,
        &view,
        &mut options,
        visible_range,
        trigger_crossing,
    );
    paint_scope_footer(ui, painter, &view, waveforms, &mut options, viewport);
}

/// Applies pointer and wheel input to the scope viewport and the quick time
/// measurement, and settles the displayed timebase.
fn update_scope_viewport(
    ui: &egui::Ui,
    plot: Rect,
    waveforms: &WaveformStore,
    extent: ScopeDisplayExtent,
    trigger_fractions: (f32, f32),
    options: &mut ScopeWaveformOptions<'_>,
    viewport: &mut WaveformViewport,
) {
    let (trigger_sample_fraction, trigger_screen_fraction) = trigger_fractions;
    let ScopeDisplayExtent {
        display_sample_count,
        display_sample_rate_hz,
        ..
    } = extent;
    let (viewport_panned, viewport_timebase_changed, waveform_click, requested_timebase) =
        update_waveform_viewport(
            ui,
            plot,
            "scope_waveform_zoom",
            viewport,
            display_sample_count,
            display_sample_rate_hz,
            WaveformViewportOptions {
                reset_on_double_click: false,
                primary_pan_enabled: true,
                zoom_anchor: WaveformZoomAnchor::Sample {
                    domain_fraction: trigger_sample_fraction,
                    screen_fraction: trigger_screen_fraction,
                },
                timebase_basis: *options.time_per_division_seconds,
                timebase_reconfigures_capture: options.timebase_reconfigures_capture,
            },
        );
    if let Some(click) = waveform_click {
        match click {
            WaveformPlotClick::Primary {
                double_clicked: true,
                position,
            } => options
                .quick_time_measurement
                .start(viewport.domain_fraction_at_screen(position).clamp(0.0, 1.0)),
            WaveformPlotClick::Primary {
                double_clicked: false,
                position,
            } => options
                .quick_time_measurement
                .finish(viewport.domain_fraction_at_screen(position).clamp(0.0, 1.0)),
            WaveformPlotClick::Secondary => options.quick_time_measurement.clear(),
        }
    }
    *options.timebase_changed |= viewport_timebase_changed || requested_timebase.is_some();
    if viewport_panned && !waveforms.scope_rolling {
        finish_scope_viewport_pan(trigger_sample_fraction, viewport, options.trigger_settings);
    }
    let displayed_timebase =
        viewport.time_per_division(display_sample_count, display_sample_rate_hz);
    *options.time_per_division_seconds = resolve_scope_timebase(
        *options.time_per_division_seconds,
        displayed_timebase,
        requested_timebase,
        viewport.span,
        options.timebase_reconfigures_capture,
    );
}

/// The channel traces: min/max columns when many samples share a pixel,
/// otherwise a glowing polyline per capture segment.
fn paint_scope_traces(
    scope_painter: &egui::Painter,
    waveforms: &WaveformStore,
    view: &ScopeTimeDomainView,
    options: &ScopeWaveformOptions<'_>,
    visible_range: &Range<usize>,
    step: usize,
    trigger_crossing: Option<ScopeTriggerCrossing>,
) {
    let ScopeTimeDomainView {
        plot,
        extent,
        viewport,
        ..
    } = *view;
    let display_sample_count = extent.display_sample_count;
    let settings = options.settings;
    let channel_colors = options.channel_colors;
    for (channel_index, channel) in waveforms.scope_channels.iter().enumerate() {
        let Some(channel_settings) = settings.channels.get(channel_index).copied() else {
            continue;
        };
        if !channel_settings.enabled {
            continue;
        }
        let color = channel_colors[channel_index.min(channel_colors.len() - 1)];
        if step >= SCOPE_DOWNSAMPLE_MIN_SAMPLES_PER_PIXEL {
            let segments = scope_downsampled_segments(
                channel,
                visible_range.clone(),
                step,
                waveforms.scope_sample_offset,
                &waveforms.scope_capture_boundaries,
            );
            paint_scope_downsampled_segments(
                scope_painter,
                plot,
                &segments,
                channel_settings,
                viewport,
                display_sample_count,
                color,
            );
            continue;
        }
        let segments = scope_plot_sample_segments(
            visible_range.clone(),
            1,
            channel_index,
            trigger_crossing,
            waveforms.scope_sample_offset,
            &waveforms.scope_capture_boundaries,
        );
        for segment in segments {
            let points = segment
                .into_iter()
                .map(|index| {
                    let x = waveform_sample_x(plot, viewport, index as f64, display_sample_count);
                    let y = scope_sample_y(plot, channel_settings, channel[index]);
                    Pos2::new(x, y)
                })
                .collect::<Vec<_>>();
            if points.len() > 1 {
                scope_painter.add(egui::Shape::line(
                    points.clone(),
                    Stroke::new(4.0_f32, color.gamma_multiply(0.16)),
                ));
                scope_painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, color)));
            }
        }
    }
}

/// The offset arrows, trigger level, markers, and quick time measurement
/// that refer to the channels and the trigger.
fn paint_scope_references(
    ui: &egui::Ui,
    scope_painter: &egui::Painter,
    view: &ScopeTimeDomainView,
    options: &mut ScopeWaveformOptions<'_>,
) {
    let ScopeTimeDomainView {
        plot,
        extent,
        viewport,
        ..
    } = *view;
    let ScopeDisplayExtent {
        display_sample_count,
        display_sample_rate_hz,
        ..
    } = extent;
    paint_scope_offset_arrows(
        ui,
        plot,
        ScopeOffsetArrowOptions {
            settings: options.settings,
            channel_colors: options.channel_colors,
            channel_names: options.channel_names,
            vertical_offsets: options.vertical_offsets,
            controls_enabled: options.offset_controls_enabled,
        },
    );
    paint_scope_trigger_level(
        ui,
        plot,
        options.settings,
        options.trigger_settings,
        options.channel_colors,
        options.vertical_offsets,
        options.trigger_controls_enabled,
    );
    paint_scope_markers(
        ui,
        scope_painter,
        plot,
        ScopeMarkerPaintOptions {
            settings: options.marker_settings,
            scope_settings: options.settings,
            channel_names: options.channel_names,
            sample_count: display_sample_count,
            sample_rate_hz: display_sample_rate_hz,
            viewport,
        },
    );
    paint_scope_quick_time_measurement(
        scope_painter,
        plot,
        *options.quick_time_measurement,
        display_sample_count,
        display_sample_rate_hz,
        viewport,
    );
}

/// The automatic measurements and, when the pointer is near a trigger edge,
/// the smart timing hint.
fn paint_scope_analysis_overlays(
    ui: &egui::Ui,
    scope_painter: &egui::Painter,
    waveforms: &WaveformStore,
    view: &ScopeTimeDomainView,
    options: &mut ScopeWaveformOptions<'_>,
    visible_range: Range<usize>,
    trigger_crossing: Option<ScopeTriggerCrossing>,
) {
    let ScopeTimeDomainView {
        plot,
        extent,
        viewport,
        ..
    } = *view;
    let ScopeDisplayExtent {
        display_sample_count,
        display_sample_rate_hz,
        ..
    } = extent;
    let settings = options.settings;
    let channel_colors = options.channel_colors;
    paint_scope_measurements(
        ui,
        scope_painter,
        plot,
        waveforms,
        ScopeMeasurementPaintOptions {
            visible_range,
            sample_rate_hz: display_sample_rate_hz,
            settings: options.function_settings,
            enabled_channels: settings.channels.map(|channel| channel.enabled),
            channel_colors,
            channel_names: options.channel_names,
            caches: options.measurement_caches,
            window_positions: options.measurement_window_positions,
        },
    );
    let smart_timing = options
        .smart_timing_enabled
        .then(|| ui.input(|input| input.pointer.hover_pos()))
        .flatten()
        .zip(
            trigger_crossing
                .filter(|crossing| scope_smart_timing_has_nonzero_trigger(settings, *crossing)),
        )
        .and_then(|(pointer, crossing)| {
            scope_smart_timing_measurement(
                waveforms,
                crossing,
                plot,
                viewport,
                display_sample_count,
                pointer,
            )
            .map(|measurement| (measurement, pointer))
        });
    let voltage_filter_id = ui.make_persistent_id("scope_smart_timing_voltage_filter");
    if let Some((mut measurement, pointer)) = smart_timing {
        measurement.end_voltage = ui.data_mut(|data| {
            data.get_temp_mut_or_default::<ScopeSmartTimingVoltageFilter>(voltage_filter_id)
                .update(
                    measurement.channel_index,
                    pointer.x,
                    waveforms.scope_revision,
                    measurement.end_voltage,
                )
        });
        paint_scope_timing_hint(
            scope_painter,
            plot,
            waveforms,
            measurement,
            ScopeSmartTimingPaintOptions {
                settings,
                viewport,
                display_sample_count,
                sample_rate_hz: display_sample_rate_hz,
                color: channel_colors[measurement.channel_index],
            },
        );
    } else {
        ui.data_mut(|data| {
            data.remove_temp::<ScopeSmartTimingVoltageFilter>(voltage_filter_id);
        });
    }
}

/// The time ruler, the status line, and the trigger position handle.
fn scope_timebase_axis(time_per_division: f64, viewport: WaveformViewport) -> WaveformTimeAxis {
    let duration_seconds = time_per_division * SCOPE_HORIZONTAL_DIVISIONS as f64
        / f64::from(viewport.span).max(f64::EPSILON);
    WaveformTimeAxis {
        start_seconds: -f64::from(viewport.start) * duration_seconds,
        duration_seconds,
    }
}

fn paint_scope_footer(
    ui: &egui::Ui,
    painter: &egui::Painter,
    view: &ScopeTimeDomainView,
    waveforms: &WaveformStore,
    options: &mut ScopeWaveformOptions<'_>,
    viewport: &mut WaveformViewport,
) {
    let ScopeTimeDomainView {
        plot,
        time_ruler,
        status_bar,
        extent,
        ..
    } = *view;
    let ScopeDisplayExtent {
        display_sample_count,
        display_sample_rate_hz,
        ..
    } = extent;
    let selected_timebase = (options.time_axis.is_none() && options.timebase_reconfigures_capture)
        .then_some(*options.time_per_division_seconds)
        .flatten()
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0);
    let time_axis = options
        .time_axis
        .or_else(|| selected_timebase.map(|timebase| scope_timebase_axis(timebase, *viewport)));
    paint_waveform_time_scale(
        painter,
        time_ruler,
        display_sample_count,
        display_sample_rate_hz,
        waveforms.scope_sample_offset,
        *viewport,
        time_axis,
    );
    paint_waveform_status(
        painter,
        status_bar,
        display_sample_count,
        display_sample_rate_hz,
        *viewport,
        selected_timebase,
    );
    paint_scope_trigger_position(
        ui,
        time_ruler,
        plot,
        options.trigger_settings,
        options.trigger_controls_enabled,
        viewport,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    fn scope_channels(lengths: [usize; 2]) -> Vec<VecDeque<f32>> {
        lengths
            .iter()
            .map(|length| VecDeque::from(vec![0.0; *length]))
            .collect()
    }

    fn capture_settings(enabled: [bool; 2]) -> ScopeCaptureSettings {
        let mut settings = ScopeCaptureSettings {
            sample_count: 10_000,
            sample_rate_hz: 1_000_000,
            ..ScopeCaptureSettings::default()
        };
        for (channel, enabled) in settings.channels.iter_mut().zip(enabled) {
            channel.enabled = enabled;
        }
        settings
    }

    #[test]
    fn the_display_extent_follows_the_shortest_enabled_channel() {
        let waveforms = WaveformStore {
            scope_channels: scope_channels([500, 300]),
            scope_rate_hz: 2_000_000,
            ..WaveformStore::default()
        };

        assert_eq!(
            scope_display_extent(&waveforms, capture_settings([true, true])),
            ScopeDisplayExtent {
                sample_count: 300,
                display_sample_count: 300,
                display_sample_rate_hz: 2_000_000,
            }
        );
        // A disabled channel does not shorten the view.
        let extent = scope_display_extent(&waveforms, capture_settings([true, false]));
        assert_eq!(extent.sample_count, 500);
        assert_eq!(extent.display_sample_count, 500);
    }

    #[test]
    fn without_samples_the_display_extent_is_the_configured_capture() {
        let extent =
            scope_display_extent(&WaveformStore::default(), capture_settings([true, true]));

        assert_eq!(
            extent,
            ScopeDisplayExtent {
                sample_count: 0,
                display_sample_count: 10_000,
                display_sample_rate_hz: 1_000_000,
            }
        );
    }

    #[test]
    fn a_rolling_display_spans_the_ring_capacity() {
        let waveforms = WaveformStore {
            scope_channels: scope_channels([500, 500]),
            scope_rolling: true,
            scope_capacity: 4_096,
            ..WaveformStore::default()
        };

        let extent = scope_display_extent(&waveforms, capture_settings([true, true]));

        assert_eq!(extent.sample_count, 500);
        assert_eq!(extent.display_sample_count, 4_096);
    }

    #[test]
    fn automatic_capture_timebase_changes_the_plate_divisions_not_its_viewport_span() {
        let viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            anchor_locked: true,
        };
        let time_per_division = 0.2;
        let axis = scope_timebase_axis(time_per_division, viewport);
        let visible_start = axis.start_seconds + f64::from(viewport.start) * axis.duration_seconds;
        let visible_duration = f64::from(viewport.span) * axis.duration_seconds;

        assert_eq!(visible_start, 0.0);
        assert_eq!(visible_duration, 2.0);
        assert_eq!(viewport.span, 0.5);
    }

    #[test]
    fn the_trigger_fraction_prefers_the_found_crossing_then_the_reported_sample_then_the_setting() {
        let mut settings = capture_settings([true, true]);
        settings.trigger.position_percent = 40;
        let editing_trigger = ScopeTriggerSettings {
            position_percent: 25,
            ..ScopeTriggerSettings::default()
        };
        let waveforms = WaveformStore {
            scope_trigger_sample_index: Some(2_000),
            scope_sample_offset: 1_000,
            ..WaveformStore::default()
        };
        let crossing = ScopeTriggerCrossing {
            sample: 5_000.0,
            channel_index: 0,
        };

        let (found, screen) = scope_trigger_fractions(
            &waveforms,
            settings,
            Some(crossing),
            editing_trigger,
            10_001,
        );
        assert!((found - 0.4).abs() < 1e-6, "{found}");
        assert_eq!(screen, 0.25);

        let (reported, _) =
            scope_trigger_fractions(&waveforms, settings, None, editing_trigger, 10_001);
        assert!((reported - 0.1).abs() < 1e-6, "{reported}");

        let without_index = WaveformStore {
            scope_trigger_sample_index: None,
            ..waveforms
        };
        let (configured, _) =
            scope_trigger_fractions(&without_index, settings, None, editing_trigger, 10_001);
        assert_eq!(configured, 0.4);
    }

    #[test]
    fn scope_plot_segments_do_not_join_independent_captures() {
        let segments = scope_plot_sample_segments(0..10, 4, 0, None, 100, &VecDeque::from([105]));

        assert_eq!(segments, vec![vec![0, 4], vec![5, 8, 9]]);
    }

    #[test]
    fn floating_measurement_windows_stay_relative_to_the_plot() {
        let plot = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 400.0));
        let size = Vec2::new(200.0, 100.0);

        let [channel_zero, channel_one] = default_scope_measurement_window_positions();
        let bottom_left = scope_measurement_window_rect(plot, size, channel_zero);
        assert_eq!(bottom_left.left_bottom(), plot.left_bottom());
        let bottom_right = scope_measurement_window_rect(plot, size, channel_one);
        assert_eq!(bottom_right.right_bottom(), plot.right_bottom());

        let top_right = scope_measurement_window_rect(plot, size, Vec2::new(1.0, 0.0));
        assert_eq!(top_right.right(), plot.right());
        assert_eq!(top_right.top(), plot.top());
        assert_eq!(
            scope_measurement_window_position(plot, top_right),
            Vec2::new(1.0, 0.0)
        );

        let center = scope_measurement_window_rect(plot, size, Vec2::new(0.5, 0.5));
        assert_eq!(center.center(), plot.center());
        assert_eq!(
            scope_measurement_window_position(plot, center),
            Vec2::new(0.5, 0.5)
        );
    }

    #[test]
    fn scope_markers_measure_horizontal_time_and_vertical_amplitude() {
        let markers = ScopeMarkerSettings::default();

        assert!(
            (scope_time_marker_delta(
                markers.channels[0].horizontal_positions[0],
                markers.channels[0].horizontal_positions[1],
                1_000,
                1_000,
            )
            .unwrap()
                - 0.1)
                .abs()
                < 0.000_001
        );
        assert!(
            (scope_voltage_marker_delta(
                markers.channels[0].vertical_positions[0],
                markers.channels[0].vertical_positions[1],
                ScopeCaptureSettings::default().channels[0]
            ) - 0.8)
                .abs()
                < 0.000_001
        );

        let mut scope_settings = ScopeCaptureSettings::default();
        scope_settings.channels[1].volts_per_div_mv = 500;
        scope_settings.channels[1].probe_attenuation = 10;
        assert!(
            (scope_voltage_marker_delta(
                markers.channels[1].vertical_positions[0],
                markers.channels[1].vertical_positions[1],
                scope_settings.channels[1]
            ) - 4.0)
                .abs()
                < 0.000_001
        );

        assert_ne!(
            markers.channels[0].horizontal_positions,
            markers.channels[1].horizontal_positions
        );
        assert_eq!(
            markers.channels[0].horizontal_positions.len(),
            SCOPE_MARKER_LIMIT
        );
        assert_eq!(
            markers.channels[1].vertical_positions.len(),
            SCOPE_MARKER_LIMIT
        );
        assert_eq!(format_marker_voltage(0.125), "125.00 mV");
    }

    #[test]
    fn rolling_scope_aligns_when_auto_trigger_finds_an_edge() {
        assert!(should_align_scope_trigger(true, true, true));
        assert!(!should_align_scope_trigger(true, true, false));
        assert!(should_align_scope_trigger(true, false, false));
        assert!(!should_align_scope_trigger(false, true, true));
    }

    #[test]
    fn auto_scope_finds_the_edge_used_to_synchronize_each_sweep() {
        let waveforms = WaveformStore {
            scope_channels: vec![VecDeque::from([-0.5, -0.25, 0.25, 0.5]), VecDeque::new()],
            ..WaveformStore::default()
        };
        let mut settings = ScopeCaptureSettings::default();
        settings.channels[1].enabled = false;
        settings.trigger.source = DsCopeTriggerSource::Auto;
        settings.trigger.auto_channel = 0;
        settings.trigger.levels[0] = DSCOPE_TRIGGER_LEVEL_CENTER;

        let crossing = scope_trigger_crossing_sample(&waveforms, settings, settings.trigger)
            .expect("the Auto sweep contains the configured rising edge");

        assert_eq!(crossing.channel_index, 0);
        assert!((crossing.sample - 1.5).abs() < 1.0e-6);
    }

    #[test]
    fn scope_smart_timing_ignores_a_zero_volt_trigger_level() {
        let crossing = ScopeTriggerCrossing {
            sample: 10.0,
            channel_index: 0,
        };
        let mut settings = ScopeCaptureSettings::default();

        assert!(!scope_smart_timing_has_nonzero_trigger(settings, crossing));

        settings.trigger.levels[0] = 104;
        assert!(scope_smart_timing_has_nonzero_trigger(settings, crossing));
    }

    #[test]
    fn scope_smart_timing_filters_voltage_between_waveform_updates() {
        let mut filter = ScopeSmartTimingVoltageFilter::default();

        assert_eq!(filter.update(0, 100.0, 1, 1.0), 1.0);
        let filtered = filter.update(0, 100.0, 2, 3.0);
        assert!((filtered - 1.12).abs() < 0.000_001);
        assert_eq!(filter.update(0, 100.0, 2, -3.0), filtered);
        assert_eq!(filter.update(0, 105.0, 3, -2.0), -2.0);
        assert_eq!(filter.update(1, 105.0, 4, 0.5), 0.5);
    }

    #[test]
    fn scope_trigger_coordinates_preserve_sub_sample_precision_at_large_offsets() {
        let sample_offset = 10_000_000_000_u64;
        let fraction = scope_sample_fraction(sample_offset as f64 + 50.25, sample_offset, 101);

        assert!((fraction - 0.5025).abs() < 0.000_001);
    }

    #[test]
    fn scope_downsampling_preserves_trigger_crossing_samples() {
        let indices = scope_plot_sample_indices(
            0..1_000,
            100,
            0,
            Some(ScopeTriggerCrossing {
                sample: 512.25,
                channel_index: 0,
            }),
            0,
        );

        assert!(indices.contains(&512));
        assert!(indices.contains(&513));
        assert_eq!(indices.last(), Some(&999));
    }

    #[test]
    fn scope_downsampling_preserves_each_screen_buckets_extrema() {
        let samples = VecDeque::from(vec![
            0.0, 4.0, -3.0, 1.0, 0.5, 0.25, -0.5, 0.0, 1.0, -5.0, 6.0, 2.0, 1.5, 0.75, 0.5, 1.0,
        ]);

        let segments =
            scope_downsampled_segments(&samples, 0..samples.len(), 8, 0, &VecDeque::new());

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].len(), 2);
        assert_eq!(segments[0][0].minimum, -3.0);
        assert_eq!(segments[0][0].maximum, 4.0);
        assert_eq!(segments[0][1].minimum, -5.0);
        assert_eq!(segments[0][1].maximum, 6.0);
    }

    #[test]
    fn wide_time_span_keeps_a_one_kilohertz_signal_at_full_amplitude() {
        const SAMPLE_RATE_HZ: f32 = 1_000_000.0;
        const SIGNAL_FREQUENCY_HZ: f32 = 1_000.0;
        const SAMPLE_COUNT: usize = 200_000;
        const DISPLAY_BUCKET_SAMPLES: usize = 200;
        let samples = (0..SAMPLE_COUNT)
            .map(|index| {
                (std::f32::consts::TAU * SIGNAL_FREQUENCY_HZ * index as f32 / SAMPLE_RATE_HZ).sin()
            })
            .collect::<VecDeque<_>>();

        let segments = scope_downsampled_segments(
            &samples,
            0..samples.len(),
            DISPLAY_BUCKET_SAMPLES,
            0,
            &VecDeque::new(),
        );
        let minimum = segments[0]
            .iter()
            .map(|bucket| bucket.minimum)
            .fold(f32::INFINITY, f32::min);
        let maximum = segments[0]
            .iter()
            .map(|bucket| bucket.maximum)
            .fold(f32::NEG_INFINITY, f32::max);

        assert!(minimum < -0.999);
        assert!(maximum > 0.999);
    }

    #[test]
    fn scope_downsampling_does_not_join_independent_captures() {
        let samples = VecDeque::from(vec![-1.0, -0.5, 4.0, 5.0]);

        let segments = scope_downsampled_segments(&samples, 0..4, 4, 100, &VecDeque::from([102]));

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0][0].minimum, -1.0);
        assert_eq!(segments[0][0].maximum, -0.5);
        assert_eq!(segments[1][0].minimum, 4.0);
        assert_eq!(segments[1][0].maximum, 5.0);
    }

    #[test]
    fn scope_vertical_offset_maps_hardware_limits_to_the_plot() {
        let rect = Rect::from_min_max(Pos2::new(0.0, 20.0), Pos2::new(100.0, 220.0));

        assert_eq!(
            scope_vertical_offset_y(rect, DSCOPE_VERTICAL_OFFSET_MIN),
            rect.top()
        );
        assert_eq!(
            scope_vertical_offset_y(rect, DSCOPE_VERTICAL_OFFSET_MAX),
            rect.bottom()
        );
        assert_eq!(
            scope_vertical_offset_from_y(rect, rect.top()),
            DSCOPE_VERTICAL_OFFSET_MIN
        );
        assert_eq!(
            scope_vertical_offset_from_y(rect, rect.bottom()),
            DSCOPE_VERTICAL_OFFSET_MAX
        );
    }

    #[test]
    fn scope_input_range_rescales_captured_volts() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1_000.0, 800.0));
        let one_volt_per_div = ScopeChannelSettings {
            volts_per_div_mv: 1_000,
            ..ScopeChannelSettings::default()
        };
        let half_volt_per_div = ScopeChannelSettings {
            volts_per_div_mv: 500,
            ..one_volt_per_div
        };
        let zero_y = scope_sample_y(plot, one_volt_per_div, 0.0);
        let one_volt_y = scope_sample_y(plot, one_volt_per_div, 1.0);
        let half_volt_range_y = scope_sample_y(plot, half_volt_per_div, 1.0);

        let pixels_per_division = plot.height() / SCOPE_VERTICAL_DIVISIONS as f32;
        assert!((zero_y - one_volt_y - pixels_per_division).abs() < 0.001);
        assert!((zero_y - half_volt_range_y - pixels_per_division * 2.0).abs() < 0.001);
    }

    #[test]
    fn overlapping_scope_offset_arrows_remain_independently_selectable() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 200.0));
        let mut settings = ScopeCaptureSettings::default();
        let mut offsets = [DSCOPE_VERTICAL_OFFSET_CENTER; 2];
        let center_y = scope_vertical_offset_y(plot, DSCOPE_VERTICAL_OFFSET_CENTER);

        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            Some(0)
        );

        offsets[0] = DSCOPE_VERTICAL_OFFSET_MIN;
        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            Some(1)
        );

        settings.channels[0].enabled = false;
        offsets[0] = DSCOPE_VERTICAL_OFFSET_CENTER;
        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            Some(1)
        );

        settings.channels[1].enabled = false;
        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            None
        );
    }
}
