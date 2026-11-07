//! Oscilloscope FFT view: spectrum calculation, its frequency viewport, and
//! rendering.

use crate::{
    egui, fit_rect_to_grid, format_frequency, paint_empty, paint_plot_background, paint_plot_grid,
    scope_channel_label, FontId, Pos2, Range, Rect, ScopeFftSettings, ScopeFftWindow,
    SelectableHoverText, Sense, Stroke, Vec2, VecDeque, WaveformStore, WaveformViewport, ACCENT,
    SCOPE_COLORS, TEXT_MUTED,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScopeFftViewport {
    pub(crate) start: f32,
    pub(crate) span: f32,
}

impl Default for ScopeFftViewport {
    fn default() -> Self {
        Self {
            start: 0.0,
            span: 1.0,
        }
    }
}

impl ScopeFftViewport {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn zoom(&mut self, scroll_delta: f32, pointer_fraction: f32, minimum_span: f32) {
        if scroll_delta == 0.0 {
            return;
        }
        let pointer_fraction = pointer_fraction.clamp(0.0, 1.0);
        let anchor = self.start + pointer_fraction * self.span;
        let factor = if scroll_delta > 0.0 { 0.8 } else { 1.25 };
        self.span = (self.span * factor).clamp(minimum_span, 1.0);
        self.start = (anchor - pointer_fraction * self.span).clamp(0.0, 1.0 - self.span);
    }

    pub(crate) fn pan(&mut self, delta_x: f32, plot_width: f32) {
        if plot_width <= 0.0 {
            return;
        }
        self.start = (self.start - delta_x / plot_width * self.span).clamp(0.0, 1.0 - self.span);
    }
}

pub(crate) fn fft_in_place(values: &mut [(f64, f64)]) {
    let count = values.len();
    let mut target = 0;
    for source in 1..count {
        let mut bit = count >> 1;
        while target & bit != 0 {
            target ^= bit;
            bit >>= 1;
        }
        target ^= bit;
        if source < target {
            values.swap(source, target);
        }
    }

    let mut width = 2;
    while width <= count {
        let angle = -std::f64::consts::TAU / width as f64;
        let base = (angle.cos(), angle.sin());
        for start in (0..count).step_by(width) {
            let mut twiddle = (1.0, 0.0);
            for offset in 0..width / 2 {
                let even = values[start + offset];
                let odd = values[start + offset + width / 2];
                let rotated = (
                    odd.0 * twiddle.0 - odd.1 * twiddle.1,
                    odd.0 * twiddle.1 + odd.1 * twiddle.0,
                );
                values[start + offset] = (even.0 + rotated.0, even.1 + rotated.1);
                values[start + offset + width / 2] = (even.0 - rotated.0, even.1 - rotated.1);
                twiddle = (
                    twiddle.0 * base.0 - twiddle.1 * base.1,
                    twiddle.0 * base.1 + twiddle.1 * base.0,
                );
            }
        }
        width *= 2;
    }
}

pub(crate) fn scope_fft_spectrum(
    samples: &VecDeque<f32>,
    visible_range: Range<usize>,
    requested_length: usize,
    window_type: ScopeFftWindow,
) -> Vec<f32> {
    let available = visible_range.len().min(requested_length);
    if available < 8 {
        return Vec::new();
    }
    let length = 1_usize << (usize::BITS - 1 - available.leading_zeros());
    let start = visible_range.start + (visible_range.len() - length) / 2;
    let mut values = Vec::with_capacity(length);
    let mut window_sum = 0.0;
    for index in 0..length {
        let window = window_type.coefficient(index, length);
        window_sum += window;
        values.push((f64::from(samples[start + index]) * window, 0.0));
    }
    fft_in_place(&mut values);

    values[..=length / 2]
        .iter()
        .enumerate()
        .map(|(bin, (real, imaginary))| {
            let factor = if bin == 0 || bin == length / 2 {
                1.0
            } else {
                2.0_f64.sqrt()
            };
            let magnitude = (real * real + imaginary * imaginary).sqrt() * factor / window_sum;
            (20.0 * magnitude.max(1.0e-12).log10()) as f32
        })
        .collect()
}

pub(crate) fn fft_frequency_at_x(
    plot: Rect,
    x: f32,
    start_frequency_hz: f64,
    end_frequency_hz: f64,
) -> f64 {
    let fraction = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    start_frequency_hz + fraction as f64 * (end_frequency_hz - start_frequency_hz)
}

pub(crate) fn fft_bin_at_x(plot: Rect, x: f32, first_bin: usize, last_bin: usize) -> usize {
    let fraction = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    first_bin + (fraction * (last_bin - first_bin) as f32).round() as usize
}

pub(crate) fn update_scope_fft_viewport(
    ui: &egui::Ui,
    rect: Rect,
    viewport: &mut ScopeFftViewport,
    max_bin: usize,
) -> egui::Response {
    let minimum_span = (10.0 / max_bin as f32).clamp(0.001, 1.0);
    viewport.span = viewport.span.clamp(minimum_span, 1.0);
    viewport.start = viewport.start.clamp(0.0, 1.0 - viewport.span);

    let response = ui.interact(
        rect,
        ui.make_persistent_id("scope_fft_viewport"),
        Sense::click_and_drag(),
    );
    let cursor = if response.dragged_by(egui::PointerButton::Primary) {
        egui::CursorIcon::Grabbing
    } else {
        egui::CursorIcon::Crosshair
    };
    let response = response.on_hover_cursor(cursor);
    if response.double_clicked() {
        viewport.reset();
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        let delta_x = ui.input(|input| input.pointer.delta().x);
        viewport.pan(delta_x, rect.width());
        ui.ctx().request_repaint();
    }
    if response.hovered() {
        let scroll_delta = ui.input(|input| input.raw_scroll_delta.y);
        if scroll_delta != 0.0 {
            let pointer_fraction = response
                .hover_pos()
                .map(|pointer| (pointer.x - rect.left()) / rect.width().max(1.0))
                .unwrap_or(0.5);
            viewport.zoom(scroll_delta, pointer_fraction, minimum_span);
            ui.ctx().request_repaint();
        }
    }
    response
}

pub(crate) struct ScopeFftPaintOptions<'a> {
    pub(crate) viewport: WaveformViewport,
    pub(crate) fft_viewport: &'a mut ScopeFftViewport,
    pub(crate) settings: ScopeFftSettings,
    pub(crate) channel_names: &'a [String; 2],
    pub(crate) square_grid_cells: bool,
}

pub(crate) fn paint_scope_fft(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    options: ScopeFftPaintOptions<'_>,
) {
    let ScopeFftPaintOptions {
        viewport,
        fft_viewport,
        settings,
        channel_names,
        square_grid_cells,
    } = options;
    const MIN_DBV: f32 = -120.0;
    const MAX_DBV: f32 = 20.0;
    let rect = if square_grid_cells {
        fit_rect_to_grid(rect, 10, 4)
    } else {
        rect
    };
    paint_plot_background(ui, rect);
    let painter = ui.painter().with_clip_rect(rect);
    paint_plot_grid(ui, rect);
    let Some(samples) = waveforms.scope_channels.get(settings.channel) else {
        paint_empty(&painter, rect, "Waiting for FFT source samples");
        return;
    };
    let visible_range = if waveforms.scope_rolling {
        viewport.sample_range_in_domain(samples.len(), waveforms.scope_capacity)
    } else {
        viewport.sample_range(samples.len())
    };
    let spectrum = scope_fft_spectrum(samples, visible_range, settings.length, settings.window);
    if spectrum.len() < 2 || waveforms.scope_rate_hz == 0 {
        paint_empty(&painter, rect, "Waiting for FFT source samples");
        return;
    }

    let nyquist = waveforms.scope_rate_hz as f64 / 2.0;
    let last_bin = spectrum.len() - 1;
    let max_bin = last_bin;
    let response = update_scope_fft_viewport(ui, rect, fft_viewport, max_bin);
    let first_visible_bin = (fft_viewport.start * max_bin as f32).floor() as usize;
    let last_visible_bin = ((fft_viewport.start + fft_viewport.span) * max_bin as f32)
        .ceil()
        .clamp((first_visible_bin + 1) as f32, max_bin as f32) as usize;
    let first_trace_bin = first_visible_bin.max(1);
    let usable_bins = &spectrum[first_trace_bin..=last_visible_bin];
    let bins_per_pixel = (usable_bins.len() as f32 / rect.width().max(1.0))
        .ceil()
        .max(1.0) as usize;
    let points = usable_bins
        .chunks(bins_per_pixel)
        .enumerate()
        .map(|(chunk_index, chunk)| {
            let bin = first_trace_bin + chunk_index * bins_per_pixel;
            let value = chunk.iter().copied().fold(MIN_DBV, f32::max);
            let x = rect.left()
                + (bin - first_visible_bin) as f32 / (last_visible_bin - first_visible_bin) as f32
                    * rect.width();
            let y = rect.bottom()
                - ((value.clamp(MIN_DBV, MAX_DBV) - MIN_DBV) / (MAX_DBV - MIN_DBV)) * rect.height();
            Pos2::new(x, y)
        })
        .collect::<Vec<_>>();
    if points.len() > 1 {
        painter.add(egui::Shape::line(
            points.clone(),
            Stroke::new(4.0_f32, SCOPE_COLORS[settings.channel].gamma_multiply(0.16)),
        ));
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.5_f32, SCOPE_COLORS[settings.channel]),
        ));
    }

    let fft_length = (spectrum.len() - 1) * 2;
    let start_frequency_hz = nyquist * fft_viewport.start as f64;
    let end_frequency_hz = nyquist * (fft_viewport.start + fft_viewport.span) as f64;
    painter.text(
        rect.left_top() + Vec2::new(8.0, 7.0),
        egui::Align2::LEFT_TOP,
        format!(
            "FFT · {} · {} · {fft_length} pts",
            scope_channel_label(settings.channel, channel_names),
            settings.window.label(),
        ),
        FontId::monospace(10.0),
        SCOPE_COLORS[settings.channel],
    );
    painter.text(
        rect.left_bottom() + Vec2::new(5.0, -4.0),
        egui::Align2::LEFT_BOTTOM,
        format_frequency(start_frequency_hz),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.center_bottom() + Vec2::new(0.0, -4.0),
        egui::Align2::CENTER_BOTTOM,
        format_frequency((start_frequency_hz + end_frequency_hz) * 0.5),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.right_bottom() + Vec2::new(-5.0, -4.0),
        egui::Align2::RIGHT_BOTTOM,
        format_frequency(end_frequency_hz),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.right_top() + Vec2::new(-5.0, 5.0),
        egui::Align2::RIGHT_TOP,
        format!("{MAX_DBV:.0} dBV"),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.right_bottom() + Vec2::new(-5.0, -18.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{MIN_DBV:.0} dBV"),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );

    if let Some(pointer) = response.hover_pos() {
        let frequency = fft_frequency_at_x(rect, pointer.x, start_frequency_hz, end_frequency_hz);
        let bin = fft_bin_at_x(rect, pointer.x, first_visible_bin, last_visible_bin);
        let magnitude_dbv = spectrum[bin];
        painter.line_segment(
            [
                Pos2::new(pointer.x, rect.top()),
                Pos2::new(pointer.x, rect.bottom()),
            ],
            Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.75)),
        );
        response.on_hover_selectable_text(format!(
            "Frequency: {}\nMagnitude: {magnitude_dbv:.2} dBV\nScroll to zoom · drag to pan · double-click to reset",
            format_frequency(frequency),
        ));
    }
}
