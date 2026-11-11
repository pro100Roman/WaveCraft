//! Oscilloscope FFT view: spectrum calculation, its frequency viewport, and
//! rendering.

use crate::{
    egui, format_frequency, paint_dashed_line, paint_empty, paint_plot_background, paint_plot_grid,
    scope_channel_label, Color32, FontId, Pos2, Range, Rect, ScopeFftSettings, ScopeFftWindow,
    SelectableHoverText, Sense, Stroke, Vec2, VecDeque, WaveformStore, WaveformViewport, ACCENT,
    ORANGE, SCOPE_COLORS, TEXT_MUTED,
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
    pub(crate) analog_bandwidth_hz: u64,
}

fn fft_out_of_band_start_fraction(
    start_frequency_hz: f64,
    end_frequency_hz: f64,
    analog_bandwidth_hz: u64,
) -> Option<f32> {
    if end_frequency_hz <= start_frequency_hz || analog_bandwidth_hz as f64 >= end_frequency_hz {
        return None;
    }
    Some(
        ((analog_bandwidth_hz as f64 - start_frequency_hz)
            / (end_frequency_hz - start_frequency_hz))
            .clamp(0.0, 1.0) as f32,
    )
}

fn paint_scope_fft_bandwidth_limit(
    painter: &egui::Painter,
    rect: Rect,
    start_frequency_hz: f64,
    end_frequency_hz: f64,
    analog_bandwidth_hz: u64,
) {
    let Some(boundary_fraction) =
        fft_out_of_band_start_fraction(start_frequency_hz, end_frequency_hz, analog_bandwidth_hz)
    else {
        return;
    };
    let boundary_x = egui::lerp(rect.x_range(), boundary_fraction);
    let out_of_band = Rect::from_min_max(
        Pos2::new(boundary_x, rect.top()),
        Pos2::new(rect.right(), rect.bottom()),
    );
    painter.rect_filled(out_of_band, 0.0, Color32::from_black_alpha(96));

    let bandwidth = format_frequency(analog_bandwidth_hz as f64);
    if boundary_fraction > f32::EPSILON {
        paint_dashed_line(
            painter,
            Pos2::new(boundary_x, rect.top()),
            Pos2::new(boundary_x, rect.bottom()),
            Stroke::new(1.25_f32, ORANGE),
        );
        let (label_x, alignment) = if boundary_x + 135.0 <= rect.right() {
            (boundary_x + 5.0, egui::Align2::LEFT_TOP)
        } else {
            (boundary_x - 5.0, egui::Align2::RIGHT_TOP)
        };
        painter.text(
            Pos2::new(label_x, rect.top() + 23.0),
            alignment,
            format!("ANALOG BW · {bandwidth}"),
            FontId::monospace(9.0),
            ORANGE,
        );
    } else {
        painter.text(
            rect.left_top() + Vec2::new(8.0, 23.0),
            egui::Align2::LEFT_TOP,
            format!("OUTSIDE {bandwidth} ANALOG BW"),
            FontId::monospace(9.0),
            ORANGE,
        );
    }
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
        analog_bandwidth_hz,
    } = options;
    const MIN_DBV: f32 = -120.0;
    const MAX_DBV: f32 = 20.0;
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
    paint_scope_fft_bandwidth_limit(
        &painter,
        rect,
        start_frequency_hz,
        end_frequency_hz,
        analog_bandwidth_hz,
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    #[test]
    fn fft_marks_only_the_region_beyond_the_analog_bandwidth() {
        assert_eq!(
            fft_out_of_band_start_fraction(0.0, 250_000_000.0, 100_000_000),
            Some(0.4)
        );
        assert_eq!(
            fft_out_of_band_start_fraction(0.0, 250_000_000.0, 20_000_000),
            Some(0.08)
        );
        assert_eq!(
            fft_out_of_band_start_fraction(150_000_000.0, 250_000_000.0, 100_000_000),
            Some(0.0)
        );
        assert_eq!(
            fft_out_of_band_start_fraction(0.0, 50_000_000.0, 100_000_000),
            None
        );
    }

    #[test]
    fn scope_fft_finds_the_input_tone() {
        let length = 1_024;
        let tone_bin = 64;
        let samples = (0..length)
            .map(|index| {
                (std::f32::consts::TAU * tone_bin as f32 * index as f32 / length as f32).sin()
            })
            .collect::<VecDeque<_>>();

        let spectrum = scope_fft_spectrum(&samples, 0..length, length, ScopeFftWindow::Hann);
        let peak_bin = spectrum
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(bin, _)| bin)
            .unwrap();

        assert_eq!(peak_bin, tone_bin);
        assert!((spectrum[peak_bin] + 3.0).abs() < 0.1);
    }

    #[test]
    fn scope_fft_supports_vendor_window_types() {
        let samples = (0..1_024)
            .map(|index| (std::f32::consts::TAU * 32.0 * index as f32 / 1_024.0).sin())
            .collect::<VecDeque<_>>();

        for window in [
            ScopeFftWindow::Rectangular,
            ScopeFftWindow::Hann,
            ScopeFftWindow::Hamming,
            ScopeFftWindow::Blackman,
            ScopeFftWindow::FlatTop,
        ] {
            let spectrum = scope_fft_spectrum(&samples, 0..1_024, 1_024, window);
            assert_eq!(spectrum.len(), 513);
            assert!(spectrum.iter().all(|magnitude| magnitude.is_finite()));
            assert_eq!(
                spectrum
                    .iter()
                    .enumerate()
                    .skip(1)
                    .max_by(|left, right| left.1.total_cmp(right.1))
                    .map(|(bin, _)| bin),
                Some(32),
            );
        }
    }

    #[test]
    fn scope_fft_mouse_position_maps_to_frequency() {
        let plot = Rect::from_min_size(Pos2::new(100.0, 20.0), Vec2::new(800.0, 200.0));

        assert_eq!(fft_frequency_at_x(plot, 100.0, 0.0, 500_000.0), 0.0);
        assert_eq!(fft_frequency_at_x(plot, 500.0, 0.0, 500_000.0), 250_000.0);
        assert_eq!(fft_frequency_at_x(plot, 900.0, 0.0, 500_000.0), 500_000.0);
        assert_eq!(fft_frequency_at_x(plot, 950.0, 0.0, 500_000.0), 500_000.0);
        assert_eq!(
            fft_frequency_at_x(plot, 100.0, 100_000.0, 200_000.0),
            100_000.0
        );
        assert_eq!(
            fft_frequency_at_x(plot, 900.0, 100_000.0, 200_000.0),
            200_000.0
        );
        assert_eq!(fft_bin_at_x(plot, 100.0, 128, 256), 128);
        assert_eq!(fft_bin_at_x(plot, 500.0, 128, 256), 192);
        assert_eq!(fft_bin_at_x(plot, 900.0, 128, 256), 256);
        assert_eq!(fft_bin_at_x(plot, 950.0, 128, 256), 256);
    }

    #[test]
    fn scope_fft_viewport_zooms_at_pointer_and_pans() {
        let mut viewport = ScopeFftViewport::default();

        viewport.zoom(1.0, 0.25, 0.01);
        assert!((viewport.span - 0.8).abs() < f32::EPSILON);
        assert!((viewport.start - 0.05).abs() < f32::EPSILON);

        viewport.pan(-100.0, 1_000.0);
        assert!((viewport.start - 0.13).abs() < f32::EPSILON);

        viewport.zoom(-1.0, 0.5, 0.01);
        assert_eq!(viewport, ScopeFftViewport::default());
    }
}
