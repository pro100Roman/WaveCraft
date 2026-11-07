//! The logic and scope panes shown together in the combined view, and the paint
//! options shared by the per-instrument waveform views.

use crate::{
    egui, paint_logic, paint_scope, scope_channels_subtitle, waveform_card, Color32,
    LogicChannelSettingsDraft, LogicMarkerSettings, Pos2, ProtocolAnalyzerSettings,
    ProtocolDecodeCache, ProtocolDecodeSnapshot, Rect, ScopeCaptureSettings, ScopeFftViewport,
    ScopeFunctionSettings, ScopeMarkerSettings, ScopeMeasurementCache, ScopeQuickTimeMeasurement,
    ScopeTriggerSettings, Sense, Stroke, Vec2, WaveformStore, WaveformViewport, ACCENT,
    COMBINED_DIVIDER_HEIGHT, GRID, LOGIC_CHANNEL_COUNT, MIN_LOGIC_PANE_HEIGHT,
    MIN_SCOPE_PANE_HEIGHT, SCOPE_HORIZONTAL_DIVISIONS, WAVEFORM_CARD_CHROME_HEIGHT,
};

pub(crate) struct ScopeWaveformOptions<'a> {
    pub(crate) settings: ScopeCaptureSettings,
    pub(crate) auto_sample_rate: bool,
    pub(crate) smart_timing_enabled: bool,
    pub(crate) function_settings: ScopeFunctionSettings,
    pub(crate) channel_names: &'a [String; 2],
    pub(crate) measurement_caches: &'a mut [ScopeMeasurementCache; 2],
    pub(crate) measurement_window_positions: &'a mut [Vec2; 2],
    pub(crate) marker_settings: &'a mut ScopeMarkerSettings,
    pub(crate) quick_time_measurement: &'a mut ScopeQuickTimeMeasurement,
    pub(crate) channel_colors: [Color32; 2],
    pub(crate) vertical_offsets: &'a mut [u16; 2],
    pub(crate) trigger_settings: &'a mut ScopeTriggerSettings,
    pub(crate) time_per_division_seconds: &'a mut Option<f64>,
    pub(crate) timebase_changed: &'a mut bool,
    pub(crate) offset_controls_enabled: bool,
    pub(crate) trigger_controls_enabled: bool,
    pub(crate) time_axis: Option<WaveformTimeAxis>,
}

pub(crate) fn waveform_card_fill(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    paint: impl FnOnce(&mut egui::Ui, Rect),
) -> Rect {
    let plot_height = (ui.available_height() - WAVEFORM_CARD_CHROME_HEIGHT).max(64.0);
    waveform_card(ui, title, subtitle, plot_height, paint)
}

pub(crate) fn combined_pane_heights(total_height: f32, logic_ratio: f32) -> (f32, f32) {
    let usable_height = (total_height - COMBINED_DIVIDER_HEIGHT).max(0.0);
    if usable_height == 0.0 {
        return (0.0, 0.0);
    }
    let minimum_total = MIN_LOGIC_PANE_HEIGHT + MIN_SCOPE_PANE_HEIGHT;
    let minimum_scale = (usable_height / minimum_total).min(1.0);
    let minimum_logic_height = MIN_LOGIC_PANE_HEIGHT * minimum_scale;
    let minimum_scope_height = MIN_SCOPE_PANE_HEIGHT * minimum_scale;
    let logic_height = (usable_height * logic_ratio.clamp(0.0, 1.0))
        .clamp(minimum_logic_height, usable_height - minimum_scope_height);
    (logic_height, usable_height - logic_height)
}

pub(crate) struct CombinedWaveformViewports<'a> {
    pub(crate) logic: &'a mut WaveformViewport,
    pub(crate) scope: &'a mut WaveformViewport,
    pub(crate) combined: &'a mut WaveformViewport,
    pub(crate) fft: &'a mut ScopeFftViewport,
}

pub(crate) struct LogicWaveformOptions<'a> {
    pub(crate) channel_colors: &'a mut [Color32; LOGIC_CHANNEL_COUNT],
    pub(crate) channel_marker_opacities: &'a mut [u8; LOGIC_CHANNEL_COUNT],
    pub(crate) channel_names: &'a mut [String; LOGIC_CHANNEL_COUNT],
    pub(crate) channel_settings_drafts: &'a mut [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT],
    pub(crate) smart_timing_enabled: bool,
    pub(crate) marker_settings: &'a mut LogicMarkerSettings,
    pub(crate) maximum_heights: &'a mut [u8; LOGIC_CHANNEL_COUNT],
    pub(crate) enabled_channels: &'a mut [bool; LOGIC_CHANNEL_COUNT],
    pub(crate) protocol_analyzer: &'a ProtocolAnalyzerSettings,
    pub(crate) protocol_decode_cache: &'a mut ProtocolDecodeCache,
    pub(crate) protocol_stream: Option<&'a ProtocolDecodeSnapshot>,
    pub(crate) time_axis: Option<WaveformTimeAxis>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WaveformTimeAxis {
    pub(crate) start_seconds: f64,
    pub(crate) duration_seconds: f64,
}

pub(crate) fn combined_waveform_cards(
    ui: &mut egui::Ui,
    waveforms: &WaveformStore,
    logic_ratio: &mut f32,
    mut logic_options: LogicWaveformOptions<'_>,
    viewports: CombinedWaveformViewports<'_>,
    mut scope_options: ScopeWaveformOptions<'_>,
) -> Rect {
    let CombinedWaveformViewports {
        logic: logic_viewport,
        scope: scope_viewport,
        combined: combined_viewport,
        fft: scope_fft_viewport,
    } = viewports;
    let timeline = waveforms.combined_timeline();
    if let Some(timeline) = timeline {
        *logic_viewport = timeline.source_viewport(*combined_viewport, timeline.logic);
        *scope_viewport = timeline.source_viewport(*combined_viewport, timeline.scope);
        logic_options.time_axis = Some(WaveformTimeAxis {
            start_seconds: timeline.logic.start_seconds - timeline.shared.start_seconds,
            duration_seconds: timeline.logic.duration_seconds(),
        });
        scope_options.time_axis = Some(WaveformTimeAxis {
            start_seconds: timeline.scope.start_seconds - timeline.shared.start_seconds,
            duration_seconds: timeline.scope.duration_seconds(),
        });
    }
    let available = ui.available_rect_before_wrap();
    let (logic_height, _) = combined_pane_heights(available.height(), *logic_ratio);
    let divider_y = available.top() + logic_height;
    let divider_hit_rect = Rect::from_min_max(
        Pos2::new(available.left(), divider_y),
        Pos2::new(
            available.right(),
            (divider_y + COMBINED_DIVIDER_HEIGHT).min(available.bottom()),
        ),
    );
    let divider_response = ui
        .interact(
            divider_hit_rect,
            ui.make_persistent_id("combined_waveform_divider"),
            Sense::drag(),
        )
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if divider_response.dragged() {
        if let Some(pointer) = divider_response.interact_pointer_pos() {
            let usable_height = (available.height() - COMBINED_DIVIDER_HEIGHT).max(1.0);
            let minimum_total = MIN_LOGIC_PANE_HEIGHT + MIN_SCOPE_PANE_HEIGHT;
            let minimum_scale = (usable_height / minimum_total).min(1.0);
            let minimum_logic_ratio = MIN_LOGIC_PANE_HEIGHT * minimum_scale / usable_height;
            let minimum_scope_ratio = MIN_SCOPE_PANE_HEIGHT * minimum_scale / usable_height;
            *logic_ratio = ((pointer.y - available.top()) / usable_height)
                .clamp(minimum_logic_ratio, 1.0 - minimum_scope_ratio);
        }
    }

    let (logic_height, scope_height) = combined_pane_heights(available.height(), *logic_ratio);
    let logic_rect = Rect::from_min_size(available.min, Vec2::new(available.width(), logic_height));
    let scope_rect = Rect::from_min_size(
        Pos2::new(
            available.left(),
            logic_rect.bottom() + COMBINED_DIVIDER_HEIGHT,
        ),
        Vec2::new(available.width(), scope_height),
    );
    ui.advance_cursor_after_rect(available);

    let mut logic_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("combined_logic_waveform")
            .max_rect(logic_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let logic_plot_rect = waveform_card_fill(
        &mut logic_ui,
        "DIGITAL",
        "DSLogic · channels 0–15",
        |ui, rect| paint_logic(ui, rect, waveforms, logic_options, logic_viewport),
    );

    if let Some(timeline) = timeline {
        *combined_viewport = timeline.shared_viewport(*logic_viewport, timeline.logic);
        *scope_viewport = timeline.source_viewport(*combined_viewport, timeline.scope);
        *scope_options.time_per_division_seconds = Some(
            f64::from(combined_viewport.span) * timeline.shared.duration_seconds()
                / SCOPE_HORIZONTAL_DIVISIONS as f64,
        );
    }

    let mut scope_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("combined_scope_waveform")
            .max_rect(scope_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let scope_subtitle = scope_channels_subtitle(scope_options.channel_names);
    let scope_plot_rect = waveform_card_fill(
        &mut scope_ui,
        "OSCILLOSCOPE",
        &scope_subtitle,
        |ui, rect| {
            paint_scope(
                ui,
                rect,
                waveforms,
                scope_viewport,
                scope_fft_viewport,
                false,
                scope_options,
            )
        },
    );

    if let Some(timeline) = timeline {
        *combined_viewport = timeline.shared_viewport(*scope_viewport, timeline.scope);
        *logic_viewport = timeline.source_viewport(*combined_viewport, timeline.logic);
        *scope_viewport = timeline.source_viewport(*combined_viewport, timeline.scope);
    }

    let divider_color = if divider_response.hovered() || divider_response.dragged() {
        ACCENT
    } else {
        GRID
    };
    let divider_center = logic_rect.bottom() + COMBINED_DIVIDER_HEIGHT * 0.5;
    ui.painter().line_segment(
        [
            Pos2::new(available.left(), divider_center),
            Pos2::new(available.right(), divider_center),
        ],
        Stroke::new(2.0_f32, divider_color),
    );
    logic_plot_rect.union(scope_plot_rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_panes_scale_with_window_and_respect_ratio() {
        let (logic, scope) = combined_pane_heights(610.0, 0.6);
        assert!((logic - 360.0).abs() < f32::EPSILON);
        assert!((scope - 240.0).abs() < f32::EPSILON);

        let (logic, scope) = combined_pane_heights(1_010.0, 0.6);
        assert!((logic - 600.0).abs() < f32::EPSILON);
        assert!((scope - 400.0).abs() < f32::EPSILON);

        let (logic, scope) = combined_pane_heights(310.0, 0.99);
        let minimum_scale = 300.0 / (MIN_LOGIC_PANE_HEIGHT + MIN_SCOPE_PANE_HEIGHT);
        assert!((logic - MIN_LOGIC_PANE_HEIGHT * minimum_scale).abs() < 0.001);
        assert!((scope - MIN_SCOPE_PANE_HEIGHT * minimum_scale).abs() < 0.001);
    }
}
