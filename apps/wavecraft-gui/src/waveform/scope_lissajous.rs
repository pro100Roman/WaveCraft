//! Oscilloscope XY (Lissajous) view.

use crate::{
    egui, format_input_scale, paint_empty, paint_plot_background, scope_channel_label,
    scope_plot_sample_segments, scope_vertical_offset_ratio, Color32, FontId, Pos2, Rect,
    ScopeCaptureSettings, ScopeChannelSettings, ScopeLissajousSettings, Stroke, Vec2,
    WaveformStore, WaveformViewport, ACCENT, GRID, PANEL, SCOPE_VERTICAL_DIVISIONS, TEXT_MUTED,
    WAVEFORM_STATUS_HEIGHT,
};

pub(crate) fn scope_lissajous_position(
    plot: Rect,
    x_channel: ScopeChannelSettings,
    y_channel: ScopeChannelSettings,
    x_voltage: f32,
    y_voltage: f32,
) -> Pos2 {
    let x_volts_per_division =
        f32::from(x_channel.volts_per_div_mv) / 1_000.0 * f32::from(x_channel.probe_attenuation);
    let y_volts_per_division =
        f32::from(y_channel.volts_per_div_mv) / 1_000.0 * f32::from(y_channel.probe_attenuation);
    let x_span = x_volts_per_division * SCOPE_VERTICAL_DIVISIONS as f32;
    let y_span = y_volts_per_division * SCOPE_VERTICAL_DIVISIONS as f32;
    let x_fraction = scope_vertical_offset_ratio(x_channel.vertical_offset) + x_voltage / x_span;
    let y_fraction = scope_vertical_offset_ratio(y_channel.vertical_offset) - y_voltage / y_span;
    Pos2::new(
        plot.left() + x_fraction * plot.width(),
        plot.top() + y_fraction * plot.height(),
    )
}

pub(crate) fn paint_scope_xy_graticule(painter: &egui::Painter, plot: Rect) {
    for index in 0..=SCOPE_VERTICAL_DIVISIONS {
        let fraction = index as f32 / SCOPE_VERTICAL_DIVISIONS as f32;
        let x = egui::lerp(plot.x_range(), fraction);
        let y = egui::lerp(plot.y_range(), fraction);
        let center = index == SCOPE_VERTICAL_DIVISIONS / 2;
        let stroke = Stroke::new(
            if center { 1.2_f32 } else { 0.8_f32 },
            if center {
                TEXT_MUTED.gamma_multiply(0.45)
            } else {
                GRID.gamma_multiply(0.8)
            },
        );
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            stroke,
        );
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            stroke,
        );
    }
}

pub(crate) struct ScopeLissajousPaintOptions<'a> {
    pub(crate) viewport: WaveformViewport,
    pub(crate) settings: ScopeCaptureSettings,
    pub(crate) lissajous: ScopeLissajousSettings,
    pub(crate) channel_colors: [Color32; 2],
    pub(crate) channel_names: &'a [String; 2],
}

pub(crate) fn paint_scope_lissajous(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    options: ScopeLissajousPaintOptions<'_>,
) {
    let ScopeLissajousPaintOptions {
        viewport,
        settings,
        lissajous,
        channel_colors,
        channel_names,
    } = options;
    let panel_side = rect.width().min(rect.height()).max(1.0);
    let panel = Rect::from_center_size(rect.center(), Vec2::splat(panel_side));
    paint_plot_background(ui, panel);
    let painter = ui.painter();
    let header_bottom = (panel.top() + WAVEFORM_STATUS_HEIGHT).min(panel.bottom());
    let header = Rect::from_min_max(panel.left_top(), Pos2::new(panel.right(), header_bottom));
    painter.rect_filled(header, 4.0, PANEL.gamma_multiply(0.9));
    painter.line_segment(
        [header.left_bottom(), header.right_bottom()],
        Stroke::new(1.0_f32, GRID),
    );

    let x_index = lissajous.x_channel.min(settings.channels.len() - 1);
    let y_index = lissajous.y_channel.min(settings.channels.len() - 1);
    let x_channel = settings.channels[x_index];
    let y_channel = settings.channels[y_index];
    painter.text(
        header.left_center() + Vec2::new(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "LISSAJOUS · XY",
        FontId::monospace(10.0),
        ACCENT,
    );
    painter.text(
        header.right_center() + Vec2::new(-6.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        format!(
            "X {} · Y {}",
            scope_channel_label(x_index, channel_names),
            scope_channel_label(y_index, channel_names)
        ),
        FontId::monospace(10.0),
        TEXT_MUTED,
    );

    let available_plot = Rect::from_min_max(
        Pos2::new(panel.left() + 8.0, header.bottom() + 8.0),
        Pos2::new(panel.right() - 8.0, panel.bottom() - 8.0),
    );
    let side = available_plot.width().min(available_plot.height()).max(1.0);
    let plot = Rect::from_center_size(available_plot.center(), Vec2::splat(side));
    let plot_painter = painter.with_clip_rect(plot);
    paint_scope_xy_graticule(&plot_painter, plot);

    let Some(x_samples) = waveforms.scope_channels.get(x_index) else {
        paint_empty(&plot_painter, plot, "Waiting for X source samples");
        return;
    };
    let Some(y_samples) = waveforms.scope_channels.get(y_index) else {
        paint_empty(&plot_painter, plot, "Waiting for Y source samples");
        return;
    };
    let sample_count = x_samples.len().min(y_samples.len());
    let visible_range = if waveforms.scope_rolling {
        viewport.sample_range_in_domain(sample_count, waveforms.scope_capacity)
    } else {
        viewport.sample_range(sample_count)
    };
    if visible_range.is_empty() {
        paint_empty(&plot_painter, plot, "Waiting for two-channel samples");
        return;
    }
    let target_points = (plot.width() * 2.0).max(256.0) as usize;
    let step = (visible_range.len() / target_points).max(1);
    let segments = scope_plot_sample_segments(
        visible_range,
        step,
        usize::MAX,
        None,
        waveforms.scope_sample_offset,
        &waveforms.scope_capture_boundaries,
    );
    for segment in segments {
        let points = segment
            .into_iter()
            .filter_map(|index| {
                Some(scope_lissajous_position(
                    plot,
                    x_channel,
                    y_channel,
                    *x_samples.get(index)?,
                    *y_samples.get(index)?,
                ))
            })
            .collect::<Vec<_>>();
        if points.len() > 1 {
            plot_painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(5.0_f32, ACCENT.gamma_multiply(0.14)),
            ));
            plot_painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, ACCENT)));
        }
    }

    painter.text(
        plot.left_bottom() + Vec2::new(4.0, -4.0),
        egui::Align2::LEFT_BOTTOM,
        format!(
            "X · {} · {} · {}× probe",
            scope_channel_label(x_index, channel_names),
            format_input_scale(x_channel.volts_per_div_mv),
            x_channel.probe_attenuation
        ),
        FontId::monospace(9.0),
        channel_colors[x_index],
    );
    painter.text(
        plot.left_top() + Vec2::new(4.0, 4.0),
        egui::Align2::LEFT_TOP,
        format!(
            "Y · {} · {} · {}× probe",
            scope_channel_label(y_index, channel_names),
            format_input_scale(y_channel.volts_per_div_mv),
            y_channel.probe_attenuation
        ),
        FontId::monospace(9.0),
        channel_colors[y_index],
    );
}
