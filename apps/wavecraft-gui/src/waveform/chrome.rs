//! Chrome shared by every waveform view: cards, plot background and grid, time
//! ruler, status line, and the common marker drawing helpers.

use crate::{
    egui, format_duration, Color32, Duration, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2,
    WaveformTimeAxis, WaveformViewport, ACCENT, BACKGROUND, DECODED_PANEL_ACTION_BUTTON_SIZE, GRID,
    PANEL, SCOPE_HORIZONTAL_DIVISIONS, TEXT_MUTED, TIME_RULER_HEIGHT, WAVEFORM_STATUS_HEIGHT,
};

/// Width of the card outline.
const CARD_STROKE_WIDTH: f32 = 1.0;
/// Space between the edge of a collapsed side panel and its expand button.
const COLLAPSED_SIDE_PANEL_MARGIN: f32 = 4.0;
/// Width of the strip that stands in for a collapsed side panel: the expand
/// button, its margin and the card outline.
pub(crate) const COLLAPSED_SIDE_PANEL_WIDTH: f32 =
    DECODED_PANEL_ACTION_BUTTON_SIZE[0] + 2.0 * (COLLAPSED_SIDE_PANEL_MARGIN + CARD_STROKE_WIDTH);

/// A floating waveform plate that lets half of the trace beneath it remain
/// visible while retaining the source color.
pub(crate) fn half_transparent_plate(color: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 128)
}

/// The outline every block of the central area shares: the waveform cards and
/// the side panels beside them, so they all have the same corners and edge.
pub(crate) fn card_frame(inner_margin: f32) -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL)
        .stroke(Stroke::new(CARD_STROKE_WIDTH, GRID))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(inner_margin))
}

/// The frame of a collapsed side panel, with the strip's height filled by
/// [`fill_card_height`].
pub(crate) fn collapsed_side_panel_frame() -> egui::Frame {
    card_frame(COLLAPSED_SIDE_PANEL_MARGIN)
}

/// Stretches the contents of a [`card_frame`] to the height the frame was
/// given, so a side panel is as tall as the waveform card beside it.
pub(crate) fn fill_card_height(ui: &mut egui::Ui) {
    ui.set_min_height(ui.available_height());
}

/// A card with a title row whose plot takes all the height that is left, and
/// returns the plot rectangle.
pub(crate) fn waveform_card(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    paint: impl FnOnce(&mut egui::Ui, Rect),
) -> Rect {
    card_frame(12.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(RichText::new(title).strong().color(ACCENT)).selectable(true),
                );
                ui.add(
                    egui::Label::new(RichText::new(subtitle).color(TEXT_MUTED)).selectable(true),
                );
            });
            ui.add_space(8.0);
            let size = Vec2::new(ui.available_width(), ui.available_height().max(64.0));
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            paint(ui, rect);
            rect
        })
        .inner
}

pub(crate) fn paint_plot_background(ui: &egui::Ui, rect: Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, BACKGROUND);
}

pub(crate) fn align_waveform_chrome(status: Rect, ruler: Rect, plot: Rect) -> (Rect, Rect) {
    let ruler = Rect::from_min_max(
        Pos2::new(plot.left(), plot.top() - ruler.height()),
        Pos2::new(plot.right(), plot.top()),
    );
    let status = Rect::from_min_max(
        Pos2::new(plot.left(), ruler.top() - status.height()),
        Pos2::new(plot.right(), ruler.top()),
    );
    (status, ruler)
}

pub(crate) fn paint_plot_grid(ui: &egui::Ui, rect: Rect) {
    let painter = ui.painter();
    for index in 0..=10 {
        let x = egui::lerp(rect.x_range(), index as f32 / 10.0);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0_f32, GRID.gamma_multiply(0.55)),
        );
    }
    for index in 0..=4 {
        let y = egui::lerp(rect.y_range(), index as f32 / 4.0);
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0_f32, GRID.gamma_multiply(0.55)),
        );
    }
}

pub(crate) fn split_waveform_chrome(rect: Rect) -> (Rect, Rect, Rect) {
    let status_bottom = (rect.top() + WAVEFORM_STATUS_HEIGHT).min(rect.bottom());
    let ruler_bottom = (status_bottom + TIME_RULER_HEIGHT).min(rect.bottom());
    (
        Rect::from_min_max(rect.left_top(), Pos2::new(rect.right(), status_bottom)),
        Rect::from_min_max(
            Pos2::new(rect.left(), status_bottom),
            Pos2::new(rect.right(), ruler_bottom),
        ),
        Rect::from_min_max(Pos2::new(rect.left(), ruler_bottom), rect.right_bottom()),
    )
}

pub(crate) fn time_scale_unit(maximum_seconds: f64) -> (&'static str, f64) {
    if maximum_seconds >= 1.0 {
        ("s", 1.0)
    } else if maximum_seconds >= 1.0e-3 {
        ("ms", 1.0e3)
    } else if maximum_seconds >= 1.0e-6 {
        ("µs", 1.0e6)
    } else {
        ("ns", 1.0e9)
    }
}

pub(crate) fn time_scale_precision(label_step: f64) -> usize {
    if !label_step.is_finite() || label_step <= 0.0 {
        return 3;
    }
    if label_step >= 10.0 {
        0
    } else if label_step >= 1.0 {
        1
    } else {
        ((-label_step.log10().floor()) as usize + 1).min(9)
    }
}

pub(crate) fn time_scale_label_stride(width: f32) -> usize {
    const MAJOR_INTERVALS: usize = 10;
    const MINIMUM_LABEL_SPACING: f32 = 68.0;
    let maximum_labels = (width / MINIMUM_LABEL_SPACING).floor() as usize;
    let maximum_labels = maximum_labels.clamp(2, MAJOR_INTERVALS + 1);
    MAJOR_INTERVALS.div_ceil(maximum_labels - 1)
}

pub(crate) fn time_scale_sample_window(
    sample_count: usize,
    sample_offset: u64,
    viewport: WaveformViewport,
) -> Option<(f64, f64)> {
    if sample_count == 0 {
        return None;
    }
    let sample_extent = sample_count.saturating_sub(1).max(1) as f64;
    let first_sample = sample_offset as f64 + viewport.start as f64 * sample_extent;
    let last_sample = sample_offset as f64
        + (f64::from(viewport.start) + f64::from(viewport.span)) * sample_extent;
    Some((first_sample, last_sample))
}

pub(crate) fn paint_time_scale(
    painter: &egui::Painter,
    rect: Rect,
    first_sample: f64,
    last_sample: f64,
    sample_rate_hz: u64,
) {
    painter.rect_filled(rect, 4.0, PANEL.gamma_multiply(0.72));
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0_f32, GRID),
    );
    if sample_rate_hz == 0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "TIME",
            FontId::monospace(9.0),
            TEXT_MUTED,
        );
        return;
    }

    const MAJOR_INTERVALS: usize = 10;
    const SUBDIVISIONS_PER_INTERVAL: usize = 5;
    let maximum_seconds = first_sample.abs().max(last_sample.abs()) / sample_rate_hz as f64;
    let (unit, multiplier) = time_scale_unit(maximum_seconds);
    let label_stride = time_scale_label_stride(rect.width());
    let label_step = (last_sample - first_sample) / MAJOR_INTERVALS as f64 * label_stride as f64
        / sample_rate_hz as f64
        * multiplier;
    let precision = time_scale_precision(label_step);
    let total_subdivisions = MAJOR_INTERVALS * SUBDIVISIONS_PER_INTERVAL;

    for index in 0..=total_subdivisions {
        let fraction = index as f32 / total_subdivisions as f32;
        let x = egui::lerp(rect.x_range(), fraction);
        let is_major = index % SUBDIVISIONS_PER_INTERVAL == 0;
        let major_index = index / SUBDIVISIONS_PER_INTERVAL;
        let is_labelled =
            is_major && (major_index % label_stride == 0 || major_index == MAJOR_INTERVALS);
        let tick_height = if is_labelled {
            7.0
        } else if is_major {
            5.0
        } else {
            2.5
        };
        painter.line_segment(
            [
                Pos2::new(x, rect.bottom() - tick_height),
                Pos2::new(x, rect.bottom()),
            ],
            Stroke::new(
                if is_major { 1.0_f32 } else { 0.7_f32 },
                if is_major {
                    TEXT_MUTED
                } else {
                    TEXT_MUTED.gamma_multiply(0.55)
                },
            ),
        );

        if is_labelled {
            let sample = first_sample + (last_sample - first_sample) * fraction as f64;
            let seconds = sample / sample_rate_hz as f64;
            let alignment = if index == 0 {
                egui::Align2::LEFT_TOP
            } else if index == total_subdivisions {
                egui::Align2::RIGHT_TOP
            } else {
                egui::Align2::CENTER_TOP
            };
            painter.text(
                Pos2::new(x, rect.top() + 2.0),
                alignment,
                format!("{:.*} {unit}", precision, seconds * multiplier),
                FontId::monospace(9.0),
                TEXT_MUTED,
            );
        }
    }
}

pub(crate) fn paint_waveform_time_scale(
    painter: &egui::Painter,
    rect: Rect,
    sample_count: usize,
    sample_rate_hz: u64,
    sample_offset: u64,
    viewport: WaveformViewport,
    time_axis: Option<WaveformTimeAxis>,
) {
    if let Some(time_axis) = time_axis {
        let first_seconds =
            time_axis.start_seconds + f64::from(viewport.start) * time_axis.duration_seconds;
        let last_seconds = first_seconds + f64::from(viewport.span) * time_axis.duration_seconds;
        paint_time_scale(painter, rect, first_seconds, last_seconds, 1);
        return;
    }
    let Some((first_sample, last_sample)) =
        time_scale_sample_window(sample_count, sample_offset, viewport)
    else {
        paint_time_scale(painter, rect, 0.0, 0.0, 0);
        return;
    };
    if sample_rate_hz == 0 {
        paint_time_scale(painter, rect, 0.0, 0.0, 0);
        return;
    }
    let rate = sample_rate_hz as f64;
    paint_time_scale(painter, rect, first_sample / rate, last_sample / rate, 1);
}

pub(crate) fn paint_waveform_status(
    painter: &egui::Painter,
    rect: Rect,
    total_sample_count: usize,
    sample_rate_hz: u64,
    viewport: WaveformViewport,
) {
    painter.rect_filled(rect, 4.0, PANEL.gamma_multiply(0.9));
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0_f32, GRID),
    );
    if let Some(time_per_division) = viewport.time_per_division(total_sample_count, sample_rate_hz)
    {
        let per_division = Duration::from_secs_f64(time_per_division);
        let span = Duration::from_secs_f64(time_per_division * SCOPE_HORIZONTAL_DIVISIONS as f64);
        painter.text(
            rect.left_center() + Vec2::new(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            format!(
                "SPAN {}  ·  {}/div",
                format_duration(span),
                format_duration(per_division)
            ),
            FontId::monospace(10.0),
            TEXT_MUTED,
        );
    }
    let zoom_factor = viewport.zoom_factor();
    let zoom_label = if zoom_factor >= 1.0 {
        format!("ZOOM {zoom_factor:.1}×")
    } else {
        format!("ZOOM {zoom_factor:.2}×")
    };
    painter.text(
        rect.right_center() + Vec2::new(-6.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        zoom_label,
        FontId::monospace(10.0),
        ACCENT,
    );
}

pub(crate) fn marker_text_color(marker_color: Color32) -> Color32 {
    let luminance = (299 * u32::from(marker_color.r())
        + 587 * u32::from(marker_color.g())
        + 114 * u32::from(marker_color.b()))
        / 1_000;
    if luminance >= 140 {
        BACKGROUND
    } else {
        Color32::WHITE
    }
}

pub(crate) fn waveform_sample_x(
    plot: Rect,
    viewport: WaveformViewport,
    sample_index: f64,
    sample_count: usize,
) -> f32 {
    let sample_fraction = sample_index / sample_count.saturating_sub(1).max(1) as f64;
    let screen_fraction =
        (sample_fraction - f64::from(viewport.start)) / f64::from(viewport.span).max(f64::EPSILON);
    plot.left() + screen_fraction as f32 * plot.width()
}

pub(crate) fn paint_dashed_line(painter: &egui::Painter, start: Pos2, end: Pos2, stroke: Stroke) {
    const DASH_LENGTH: f32 = 6.0;
    const GAP_LENGTH: f32 = 4.0;
    let delta = end - start;
    let length = delta.length();
    if length <= f32::EPSILON {
        return;
    }
    let direction = delta / length;
    let mut offset = 0.0;
    while offset < length {
        let dash_end = (offset + DASH_LENGTH).min(length);
        painter.line_segment(
            [start + direction * offset, start + direction * dash_end],
            stroke,
        );
        offset += DASH_LENGTH + GAP_LENGTH;
    }
}

pub(crate) fn paint_marker_handle(
    painter: &egui::Painter,
    center: Pos2,
    label: &str,
    color: Color32,
) {
    let width = (label.chars().count() as f32 * 7.0 + 10.0).max(25.0);
    let rect = Rect::from_center_size(center, Vec2::new(width, 18.0));
    painter.rect_filled(rect, egui::Rounding::same(3.0), color);
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        label,
        FontId::monospace(10.0),
        marker_text_color(color),
    );
}

pub(crate) fn paint_marker_readout(
    painter: &egui::Painter,
    plot: Rect,
    center: Pos2,
    text: &str,
    color: Color32,
) {
    let size = Vec2::new(
        (text.chars().count() as f32 * 7.2 + 16.0).min(plot.width()),
        24.0_f32.min(plot.height()),
    );
    let half = size * 0.5;
    let center = Pos2::new(
        center.x.clamp(plot.left() + half.x, plot.right() - half.x),
        center.y.clamp(plot.top() + half.y, plot.bottom() - half.y),
    );
    let rect = Rect::from_center_size(center, size);
    painter.rect_filled(
        rect,
        egui::Rounding::same(4.0),
        Color32::from_rgba_unmultiplied(8, 12, 18, 230),
    );
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        text,
        FontId::monospace(12.0),
        color,
    );
}

pub(crate) fn paint_empty(painter: &egui::Painter, rect: Rect, text: &str) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        FontId::proportional(14.0),
        TEXT_MUTED,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    #[test]
    fn waveform_status_is_positioned_above_the_time_ruler() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 300.0));
        let (status, ruler, plot) = split_waveform_chrome(rect);

        assert_eq!(status.top(), rect.top());
        assert_eq!(status.bottom(), ruler.top());
        assert_eq!(ruler.bottom(), plot.top());
        assert_eq!(plot.bottom(), rect.bottom());
    }

    #[test]
    fn floating_waveform_plates_are_half_transparent() {
        let fill = half_transparent_plate(Color32::from_rgb(8, 12, 18));

        assert_eq!(fill.to_srgba_unmultiplied(), [8, 12, 18, 128]);
    }
}
