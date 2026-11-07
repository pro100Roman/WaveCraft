//! The central waveform area of the application window.

use crate::{
    combined_waveform_cards, egui, focus_logic_sample_range, format_radio_value,
    paint_decoded_data_panel, paint_logic, paint_scope, paint_scope_lissajous, scope_channel_label,
    scope_channels_subtitle, toolbar_action_button, ui::frame_state::FrameState,
    waveform_card_fill, CombinedWaveformViewports, LogicWaveformOptions, PendingSnapshot,
    RadioDataMode, RadioViewOptions, RichText, ScopeCaptureSettings, ScopeLissajousPaintOptions,
    ScopeWaveformOptions, SelectableHoverText, Sense, Stroke, Vec2, WaveCraftApp, WaveformStore,
    WaveformTab, ACCENT, BACKGROUND, DECODED_PANEL_ACTION_BUTTON_SIZE, GRID, PANEL, TEXT_MUTED,
};

impl WaveCraftApp {
    /// Paints the waveform views, their decoded-data side panel, and the
    /// empty-state messages, then starts a pending snapshot if requested.
    pub(crate) fn paint_central_panel(
        &mut self,
        context: &egui::Context,
        frame: &FrameState,
        snapshot_clicked: bool,
    ) {
        let FrameState {
            trigger_controls_enabled,
            frontend_controls_enabled,
            rf_trigger_level_controls_enabled,
            scope_settings_before_ui,
            scope_auto_sample_rate_before_ui,
            ..
        } = *frame;
        let mut snapshot_plot_rect = None;
        let protocol_analyzer_for_waveform = self.protocol_analyzer.clone();
        let mut protocol_decode_cache = std::mem::take(&mut self.protocol_decode_cache);
        let streamed_protocol_decode = self
            .active_capture_for_view()
            .filter(|active| !active.preserve_display_until_complete)
            .map(|active| active.protocol_decode_snapshot.clone())
            .or_else(|| {
                (!self.calibration_active)
                    .then(|| self.displayed_completed_capture())
                    .flatten()
                    .map(|capture| capture.protocol_decode.snapshot())
            });
        let mut decoded_data_panel_open = self.decoded_data_panel_open;
        let mut decoded_data_panel_width = self.decoded_data_panel_width;
        let mut radio_trigger_level_dbfs_tenths = self.radio_settings.trigger_level_dbfs_tenths;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::same(16.0)),
            )
            .show(context, |ui| {
                let available_tabs = self.available_waveform_tabs();
                if available_tabs.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(
                                "Connect a supported instrument, then Refresh",
                            )
                            .color(TEXT_MUTED),
                        );
                    });
                    return;
                }

                let empty_waveforms = WaveformStore::default();
                let waveforms = self
                    .active_capture_for_view()
                    .filter(|active| !active.preserve_display_until_complete)
                    .map(|active| &active.waveforms)
                    .or_else(|| {
                        self.calibration_active
                            .then_some(self.calibration_display.as_ref())
                            .flatten()
                            .map(|display| &display.waveforms)
                    })
                    .or_else(|| {
                        self.displayed_completed_capture()
                            .map(|capture| &capture.waveforms)
                    })
                    .unwrap_or(&empty_waveforms);
                let mut combined_logic_ratio = self.combined_logic_ratio;
                let mut logic_channel_colors = self.logic_channel_colors;
                let mut logic_channel_marker_opacities = self.logic_channel_marker_opacities;
                let mut logic_channel_names = self.logic_channel_names.clone();
                let mut logic_channel_settings_drafts = self.logic_channel_settings_drafts.clone();
                let mut logic_marker_settings = self.logic_marker_settings.clone();
                let mut logic_enabled_channels = self.logic_settings.enabled_channels;
                let mut logic_maximum_heights = self.logic_settings.maximum_heights;
                let mut logic_viewport = self.logic_viewport;
                let mut scope_viewport = self.scope_viewport;
                let mut combined_viewport = self.combined_viewport;
                let mut scope_fft_viewport = self.scope_fft_viewport;
                let mut scope_measurement_caches = self.scope_measurement_caches;
                let mut scope_measurement_window_positions =
                    self.scope_measurement_window_positions;
                let mut scope_marker_settings = self.scope_marker_settings;
                let mut scope_quick_time_measurement = self.scope_quick_time_measurement;
                let mut scope_time_per_division_seconds = self.scope_time_per_division_seconds;
                let mut scope_timebase_changed = false;
                let mut scope_vertical_offsets = self
                    .scope_settings
                    .channels
                    .map(|channel| channel.vertical_offset);
                let mut scope_trigger_settings = self.scope_settings.trigger;
                let scope_settings = self
                    .active_capture_for_view()
                    .map(|active| ScopeCaptureSettings::from_capture_config(active.scope_config))
                    .or_else(|| {
                        self.calibration_active
                            .then_some(self.calibration_display.as_ref())
                            .flatten()
                            .map(|display| {
                                ScopeCaptureSettings::from_capture_config(display.scope_config)
                            })
                    })
                    .unwrap_or(self.scope_settings);
                match self.active_waveform_tab {
                    Some(WaveformTab::LogicAnalyzer) => {
                        let decoded_outcomes = protocol_decode_cache.shared_outcomes(
                            waveforms,
                            &protocol_analyzer_for_waveform.analyzers,
                            streamed_protocol_decode.as_ref(),
                        );
                        let sample_offset = waveforms.logic_sample_offset;
                        let sample_count = waveforms.logic_sample_count;
                        let sample_rate_hz = waveforms.logic_rate_hz;
                        let available_size = ui.available_size();
                        let spacing = ui.spacing().item_spacing.x;
                        let splitter_width = 6.0;
                        let minimum_waveform_width = 260.0;
                        let mut selected_range = None;
                        let mut paint_waveform = |waveform_ui: &mut egui::Ui| {
                            waveform_card_fill(
                                waveform_ui,
                                "DIGITAL",
                                "DSLogic · channels 0–15",
                                |ui, rect| {
                                    paint_logic(
                                        ui,
                                        rect,
                                        waveforms,
                                        LogicWaveformOptions {
                                            channel_colors: &mut logic_channel_colors,
                                            channel_marker_opacities:
                                                &mut logic_channel_marker_opacities,
                                            channel_names: &mut logic_channel_names,
                                            channel_settings_drafts:
                                                &mut logic_channel_settings_drafts,
                                            smart_timing_enabled: self.logic_smart_timing_enabled,
                                            marker_settings: &mut logic_marker_settings,
                                            maximum_heights: &mut logic_maximum_heights,
                                            enabled_channels: &mut logic_enabled_channels,
                                            protocol_analyzer: &protocol_analyzer_for_waveform,
                                            protocol_decode_cache: &mut protocol_decode_cache,
                                            protocol_stream: streamed_protocol_decode.as_ref(),
                                            time_axis: None,
                                        },
                                        &mut logic_viewport,
                                    )
                                },
                            )
                        };
                        if decoded_data_panel_open {
                            let maximum_panel_width = (available_size.x
                                - minimum_waveform_width
                                - splitter_width
                                - spacing * 2.0)
                                .max(120.0);
                            decoded_data_panel_width = decoded_data_panel_width.clamp(
                                280.0_f32.min(maximum_panel_width),
                                maximum_panel_width.min(560.0),
                            );
                            let waveform_width = (available_size.x
                                - decoded_data_panel_width
                                - splitter_width
                                - spacing * 2.0)
                                .max(1.0);
                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    Vec2::new(waveform_width, available_size.y),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        snapshot_plot_rect = Some(paint_waveform(ui));
                                    },
                                );
                                let (splitter_rect, splitter_response) = ui.allocate_exact_size(
                                    Vec2::new(splitter_width, available_size.y),
                                    Sense::drag(),
                                );
                                let splitter_response = splitter_response
                                    .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                                ui.painter().line_segment(
                                    [splitter_rect.center_top(), splitter_rect.center_bottom()],
                                    Stroke::new(1.0_f32, GRID),
                                );
                                if splitter_response.dragged() {
                                    let delta = ui.input(|input| input.pointer.delta().x);
                                    decoded_data_panel_width = (decoded_data_panel_width - delta)
                                        .clamp(
                                            280.0_f32.min(maximum_panel_width),
                                            maximum_panel_width.min(560.0),
                                        );
                                }
                                ui.allocate_ui_with_layout(
                                    Vec2::new(decoded_data_panel_width, available_size.y),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        egui::Frame::none()
                                            .fill(PANEL)
                                            .rounding(egui::Rounding::same(6.0))
                                            .inner_margin(egui::Margin::same(12.0))
                                            .show(ui, |ui| {
                                                ui.set_min_height(
                                                    (available_size.y - 24.0).max(0.0),
                                                );
                                                ui.horizontal(|ui| {
                                                    ui.label(
                                                        RichText::new("DECODED DATA")
                                                            .strong()
                                                            .color(TEXT_MUTED),
                                                    );
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            if ui
                                                                .add_sized(
                                                                    DECODED_PANEL_ACTION_BUTTON_SIZE,
                                                                    toolbar_action_button(
                                                                        "▶", ACCENT,
                                                                    ),
                                                                )
                                                                .on_hover_selectable_text(
                                                                    "Collapse decoded data",
                                                                )
                                                                .clicked()
                                                            {
                                                                decoded_data_panel_open = false;
                                                            }
                                                        },
                                                    );
                                                });
                                                ui.add_space(8.0);
                                                ui.set_width(ui.available_width());
                                                selected_range = paint_decoded_data_panel(
                                                    ui,
                                                    &protocol_analyzer_for_waveform.analyzers,
                                                    decoded_outcomes.as_slice(),
                                                    sample_rate_hz,
                                                );
                                            });
                                    },
                                );
                            });
                        } else {
                            let collapsed_width = 34.0;
                            let waveform_width =
                                (available_size.x - collapsed_width - spacing).max(1.0);
                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    Vec2::new(waveform_width, available_size.y),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        snapshot_plot_rect = Some(paint_waveform(ui));
                                    },
                                );
                                ui.allocate_ui_with_layout(
                                    Vec2::new(collapsed_width, available_size.y),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        egui::Frame::none()
                                            .fill(PANEL)
                                            .rounding(egui::Rounding::same(6.0))
                                            .inner_margin(egui::Margin::same(4.0))
                                            .show(ui, |ui| {
                                                ui.set_min_height(
                                                    (available_size.y - 8.0).max(0.0),
                                                );
                                                if ui
                                                    .add_sized(
                                                        DECODED_PANEL_ACTION_BUTTON_SIZE,
                                                        toolbar_action_button("◀", ACCENT),
                                                    )
                                                    .on_hover_selectable_text("Expand decoded data")
                                                    .clicked()
                                                {
                                                    decoded_data_panel_open = true;
                                                }
                                            });
                                    },
                                );
                            });
                        }
                        if let Some((start_sample, end_sample)) = selected_range {
                            focus_logic_sample_range(
                                &mut logic_viewport,
                                sample_offset,
                                sample_count,
                                start_sample,
                                end_sample,
                            );
                        }
                    }
                    Some(WaveformTab::Oscilloscope) => {
                        let subtitle = scope_channels_subtitle(&self.scope_channel_names);
                        snapshot_plot_rect = Some(waveform_card_fill(
                            ui,
                            "OSCILLOSCOPE",
                            &subtitle,
                            |ui, rect| {
                                paint_scope(
                                    ui,
                                    rect,
                                    waveforms,
                                    &mut scope_viewport,
                                    &mut scope_fft_viewport,
                                    true,
                                    ScopeWaveformOptions {
                                        settings: scope_settings,
                                        auto_sample_rate: self.scope_auto_sample_rate,
                                        smart_timing_enabled: self.scope_smart_timing_enabled,
                                        function_settings: self.scope_function_settings,
                                        channel_names: &self.scope_channel_names,
                                        measurement_caches: &mut scope_measurement_caches,
                                        measurement_window_positions:
                                            &mut scope_measurement_window_positions,
                                        marker_settings: &mut scope_marker_settings,
                                        quick_time_measurement: &mut scope_quick_time_measurement,
                                        channel_colors: self.scope_channel_colors,
                                        vertical_offsets: &mut scope_vertical_offsets,
                                        trigger_settings: &mut scope_trigger_settings,
                                        time_per_division_seconds:
                                            &mut scope_time_per_division_seconds,
                                        timebase_changed: &mut scope_timebase_changed,
                                        offset_controls_enabled: frontend_controls_enabled,
                                        trigger_controls_enabled,
                                        time_axis: None,
                                    },
                                )
                            },
                        ));
                    }
                    Some(WaveformTab::Lissajous) => {
                        let lissajous = self.scope_function_settings.lissajous;
                        let subtitle = format!(
                            "DSCope · {} → X / {} → Y",
                            scope_channel_label(lissajous.x_channel, &self.scope_channel_names),
                            scope_channel_label(lissajous.y_channel, &self.scope_channel_names)
                        );
                        let mut lissajous_scope_settings = scope_settings;
                        for (channel, vertical_offset) in lissajous_scope_settings
                            .channels
                            .iter_mut()
                            .zip(scope_vertical_offsets)
                        {
                            channel.vertical_offset = vertical_offset;
                        }
                        snapshot_plot_rect = Some(waveform_card_fill(
                            ui,
                            "LISSAJOUS",
                            &subtitle,
                            |ui, rect| {
                                paint_scope_lissajous(
                                    ui,
                                    rect,
                                    waveforms,
                                    ScopeLissajousPaintOptions {
                                        viewport: scope_viewport,
                                        settings: lissajous_scope_settings,
                                        lissajous,
                                        channel_colors: self.scope_channel_colors,
                                        channel_names: &self.scope_channel_names,
                                    },
                                )
                            },
                        ));
                    }
                    Some(WaveformTab::RadioReceiver) => {
                        let subtitle = waveforms.radio.newest_spectrum().map_or_else(
                            || "Waiting for IQ samples".to_owned(),
                            |spectrum| {
                                format!(
                                    "{} · {} FFT bins",
                                    format_radio_value(spectrum.center_frequency_hz),
                                    spectrum.power_dbfs.len()
                                )
                            },
                        );
                        snapshot_plot_rect = Some(waveform_card_fill(
                            ui,
                            "RF SPECTRUM",
                            &subtitle,
                            |ui, rect| {
                                self.radio_view
                                    .borrow_mut()
                                    .paint(
                                        ui,
                                        rect,
                                        &waveforms.radio,
                                        &waveforms.radio_analysis,
                                        RadioViewOptions {
                                            analysis_enabled: self
                                                .radio_settings
                                                .analyzer
                                                .is_enabled(),
                                            packet_mode: self.radio_settings.analyzer.data_mode
                                                == RadioDataMode::Packets,
                                            sample_rate_hz: self
                                                .radio_settings
                                                .capture
                                                .sample_rate_hz,
                                            trigger_level_dbfs_tenths:
                                                &mut radio_trigger_level_dbfs_tenths,
                                            trigger_controls_enabled:
                                                rf_trigger_level_controls_enabled,
                                        },
                                    );
                            },
                        ));
                    }
                    Some(WaveformTab::Combined) => {
                        snapshot_plot_rect = Some(combined_waveform_cards(
                            ui,
                            waveforms,
                            &mut combined_logic_ratio,
                            LogicWaveformOptions {
                                channel_colors: &mut logic_channel_colors,
                                channel_marker_opacities: &mut logic_channel_marker_opacities,
                                channel_names: &mut logic_channel_names,
                                channel_settings_drafts: &mut logic_channel_settings_drafts,
                                smart_timing_enabled: self.logic_smart_timing_enabled,
                                marker_settings: &mut logic_marker_settings,
                                maximum_heights: &mut logic_maximum_heights,
                                enabled_channels: &mut logic_enabled_channels,
                                protocol_analyzer: &protocol_analyzer_for_waveform,
                                protocol_decode_cache: &mut protocol_decode_cache,
                                protocol_stream: streamed_protocol_decode.as_ref(),
                                time_axis: None,
                            },
                            CombinedWaveformViewports {
                                logic: &mut logic_viewport,
                                scope: &mut scope_viewport,
                                combined: &mut combined_viewport,
                                fft: &mut scope_fft_viewport,
                            },
                            ScopeWaveformOptions {
                                settings: scope_settings,
                                auto_sample_rate: self.scope_auto_sample_rate,
                                smart_timing_enabled: self.scope_smart_timing_enabled,
                                function_settings: self.scope_function_settings,
                                channel_names: &self.scope_channel_names,
                                measurement_caches: &mut scope_measurement_caches,
                                measurement_window_positions:
                                    &mut scope_measurement_window_positions,
                                marker_settings: &mut scope_marker_settings,
                                quick_time_measurement: &mut scope_quick_time_measurement,
                                channel_colors: self.scope_channel_colors,
                                vertical_offsets: &mut scope_vertical_offsets,
                                trigger_settings: &mut scope_trigger_settings,
                                time_per_division_seconds: &mut scope_time_per_division_seconds,
                                timebase_changed: &mut scope_timebase_changed,
                                offset_controls_enabled: frontend_controls_enabled,
                                trigger_controls_enabled,
                                time_axis: None,
                            },
                        ));
                    }
                    None => {}
                }
                self.combined_logic_ratio = combined_logic_ratio;
                self.logic_channel_colors = logic_channel_colors;
                self.logic_channel_marker_opacities = logic_channel_marker_opacities;
                self.logic_channel_names = logic_channel_names;
                self.logic_channel_settings_drafts = logic_channel_settings_drafts;
                self.logic_marker_settings = logic_marker_settings;
                self.logic_settings.enabled_channels = logic_enabled_channels;
                self.logic_settings.maximum_heights = logic_maximum_heights;
                self.logic_viewport = logic_viewport;
                self.scope_viewport = scope_viewport;
                self.combined_viewport = combined_viewport;
                self.scope_fft_viewport = scope_fft_viewport;
                self.scope_measurement_caches = scope_measurement_caches;
                self.scope_measurement_window_positions = scope_measurement_window_positions;
                self.scope_marker_settings = scope_marker_settings;
                self.scope_quick_time_measurement = scope_quick_time_measurement;
                self.scope_time_per_division_seconds = scope_time_per_division_seconds;
                let automatic_sample_rate_inputs_changed = scope_timebase_changed
                    || (!scope_auto_sample_rate_before_ui && self.scope_auto_sample_rate)
                    || self.scope_settings.sample_count != scope_settings_before_ui.sample_count
                    || self.scope_settings.enabled_channel_count()
                        != scope_settings_before_ui.enabled_channel_count();
                if self.scope_auto_sample_rate && automatic_sample_rate_inputs_changed {
                    if let Some(time_per_division) = scope_time_per_division_seconds {
                        self.scope_settings.sample_rate_hz = self
                            .scope_settings
                            .sample_rate_for_timebase(time_per_division);
                    }
                }
                for (channel, vertical_offset) in self
                    .scope_settings
                    .channels
                    .iter_mut()
                    .zip(scope_vertical_offsets)
                {
                    channel.vertical_offset = vertical_offset;
                }
                self.scope_settings.trigger = scope_trigger_settings;
            });
        self.protocol_decode_cache = protocol_decode_cache;
        self.decoded_data_panel_open = decoded_data_panel_open;
        self.decoded_data_panel_width = decoded_data_panel_width;
        self.radio_settings.trigger_level_dbfs_tenths = radio_trigger_level_dbfs_tenths;
        self.snapshot_plot_rect = snapshot_plot_rect;
        if snapshot_clicked {
            if let Some(plot_rect) = snapshot_plot_rect {
                self.pending_snapshot = Some(PendingSnapshot {
                    plot_rect,
                    pixels_per_point: context.pixels_per_point(),
                });
                self.status = "Preparing snapshot…".to_owned();
                context.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            }
        }
    }
}
