//! The central waveform area of the application window.

use crate::{
    combined_waveform_cards, egui, focus_logic_sample_range, format_radio_value,
    paint_decoded_data_panel, paint_logic, paint_scope, paint_scope_lissajous, scope_channel_label,
    scope_channels_subtitle, toolbar_action_button, ui::frame_state::FrameState,
    waveform_card_fill, Color32, CombinedWaveformViewports, LogicChannelSettingsDraft,
    LogicMarkerSettings, LogicWaveformOptions, PendingSnapshot, ProtocolAnalyzerSettings,
    ProtocolDecodeCache, ProtocolDecodeOutcome, ProtocolDecodeSnapshot, RadioDataMode,
    RadioViewOptions, Rect, RichText, ScopeCaptureSettings, ScopeFftViewport,
    ScopeLissajousPaintOptions, ScopeMarkerSettings, ScopeMeasurementCache,
    ScopeQuickTimeMeasurement, ScopeTriggerSettings, ScopeWaveformOptions, SelectableHoverText,
    Sense, Stroke, Vec2, WaveCraftApp, WaveformStore, WaveformTab, WaveformViewport, ACCENT,
    BACKGROUND, DECODED_PANEL_ACTION_BUTTON_SIZE, GRID, LOGIC_CHANNEL_COUNT, PANEL, TEXT_MUTED,
};

/// App state that `paint_central_panel` moves out of [`WaveCraftApp`] while the
/// views paint, so they can borrow it mutably next to the capture data they
/// display; [`PanelState::restore_into`] hands it back afterwards.
struct PanelState {
    protocol_analyzer: ProtocolAnalyzerSettings,
    protocol_decode_cache: ProtocolDecodeCache,
    streamed_protocol_decode: Option<ProtocolDecodeSnapshot>,
    decoded_data_panel_open: bool,
    decoded_data_panel_width: f32,
    radio_trigger_level_dbfs_tenths: i16,
    snapshot_plot_rect: Option<Rect>,
}

impl PanelState {
    fn take_from(app: &mut WaveCraftApp) -> Self {
        let protocol_analyzer = app.protocol_analyzer.clone();
        let protocol_decode_cache = std::mem::take(&mut app.protocol_decode_cache);
        let streamed_protocol_decode = app
            .active_capture_for_view()
            .filter(|active| !active.preserve_display_until_complete)
            .map(|active| active.protocol_decode_snapshot.clone())
            .or_else(|| {
                (!app.calibration_active)
                    .then(|| app.displayed_completed_capture())
                    .flatten()
                    .map(|capture| capture.protocol_decode.snapshot())
            });
        Self {
            protocol_analyzer,
            protocol_decode_cache,
            streamed_protocol_decode,
            decoded_data_panel_open: app.decoded_data_panel_open,
            decoded_data_panel_width: app.decoded_data_panel_width,
            radio_trigger_level_dbfs_tenths: app.radio_settings.trigger_level_dbfs_tenths,
            snapshot_plot_rect: None,
        }
    }

    fn restore_into(self, app: &mut WaveCraftApp) {
        app.protocol_decode_cache = self.protocol_decode_cache;
        app.decoded_data_panel_open = self.decoded_data_panel_open;
        app.decoded_data_panel_width = self.decoded_data_panel_width;
        app.radio_settings.trigger_level_dbfs_tenths = self.radio_trigger_level_dbfs_tenths;
        app.snapshot_plot_rect = self.snapshot_plot_rect;
    }
}

/// Copies of the view state the waveform views edit while painting. They are
/// taken after the empty-state check and written back by
/// [`WaveCraftApp::apply_waveform_view_edits`].
struct WaveformViewEdits {
    combined_logic_ratio: f32,
    logic_channel_colors: [Color32; LOGIC_CHANNEL_COUNT],
    logic_channel_marker_opacities: [u8; LOGIC_CHANNEL_COUNT],
    logic_channel_names: [String; LOGIC_CHANNEL_COUNT],
    logic_channel_settings_drafts: [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT],
    logic_marker_settings: LogicMarkerSettings,
    logic_enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
    logic_maximum_heights: [u8; LOGIC_CHANNEL_COUNT],
    logic_viewport: WaveformViewport,
    scope_viewport: WaveformViewport,
    combined_viewport: WaveformViewport,
    scope_fft_viewport: ScopeFftViewport,
    scope_measurement_caches: [ScopeMeasurementCache; 2],
    scope_measurement_window_positions: [Vec2; 2],
    scope_marker_settings: ScopeMarkerSettings,
    scope_quick_time_measurement: ScopeQuickTimeMeasurement,
    scope_time_per_division_seconds: Option<f64>,
    scope_timebase_changed: bool,
    scope_vertical_offsets: [u16; 2],
    scope_trigger_settings: ScopeTriggerSettings,
}

impl WaveformViewEdits {
    fn copy_from(app: &WaveCraftApp) -> Self {
        Self {
            combined_logic_ratio: app.combined_logic_ratio,
            logic_channel_colors: app.logic_channel_colors,
            logic_channel_marker_opacities: app.logic_channel_marker_opacities,
            logic_channel_names: app.logic_channel_names.clone(),
            logic_channel_settings_drafts: app.logic_channel_settings_drafts.clone(),
            logic_marker_settings: app.logic_marker_settings.clone(),
            logic_enabled_channels: app.logic_settings.enabled_channels,
            logic_maximum_heights: app.logic_settings.maximum_heights,
            logic_viewport: app.logic_viewport,
            scope_viewport: app.scope_viewport,
            combined_viewport: app.combined_viewport,
            scope_fft_viewport: app.scope_fft_viewport,
            scope_measurement_caches: app.scope_measurement_caches,
            scope_measurement_window_positions: app.scope_measurement_window_positions,
            scope_marker_settings: app.scope_marker_settings,
            scope_quick_time_measurement: app.scope_quick_time_measurement,
            scope_time_per_division_seconds: app.scope_time_per_division_seconds,
            scope_timebase_changed: false,
            scope_vertical_offsets: app
                .scope_settings
                .channels
                .map(|channel| channel.vertical_offset),
            scope_trigger_settings: app.scope_settings.trigger,
        }
    }
}

impl WaveCraftApp {
    /// Paints the waveform views, their decoded-data side panel, and the
    /// empty-state messages, then starts a pending snapshot if requested.
    pub(crate) fn paint_central_panel(
        &mut self,
        context: &egui::Context,
        frame: &FrameState,
        snapshot_clicked: bool,
    ) {
        let mut state = PanelState::take_from(self);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::same(16.0)),
            )
            .show(context, |ui| {
                self.paint_waveform_views(ui, frame, &mut state);
            });
        let snapshot_plot_rect = state.snapshot_plot_rect;
        state.restore_into(self);
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

    /// Paints the view of the active waveform tab, or a hint when no instrument
    /// is available, then writes back what the view edited.
    fn paint_waveform_views(
        &mut self,
        ui: &mut egui::Ui,
        frame: &FrameState,
        state: &mut PanelState,
    ) {
        let available_tabs = self.available_waveform_tabs();
        if available_tabs.is_empty() {
            paint_connect_instrument_message(ui);
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
        let mut edits = WaveformViewEdits::copy_from(self);
        let scope_settings = self
            .active_capture_for_view()
            .map(|active| ScopeCaptureSettings::from_capture_config(active.scope_config))
            .or_else(|| {
                self.calibration_active
                    .then_some(self.calibration_display.as_ref())
                    .flatten()
                    .map(|display| ScopeCaptureSettings::from_capture_config(display.scope_config))
            })
            .unwrap_or(self.scope_settings);
        match self.active_waveform_tab {
            Some(WaveformTab::LogicAnalyzer) => {
                self.paint_logic_view(ui, waveforms, state, &mut edits);
            }
            Some(WaveformTab::Oscilloscope) => {
                self.paint_scope_view(ui, waveforms, scope_settings, frame, state, &mut edits);
            }
            Some(WaveformTab::Lissajous) => {
                self.paint_lissajous_view(ui, waveforms, scope_settings, state, &edits);
            }
            Some(WaveformTab::RadioReceiver) => {
                self.paint_radio_view(ui, waveforms, frame, state);
            }
            Some(WaveformTab::Combined) => {
                self.paint_combined_view(ui, waveforms, scope_settings, frame, state, &mut edits);
            }
            None => {}
        }
        self.apply_waveform_view_edits(edits, frame);
    }

    /// The logic waveform with the decoded-data panel beside it; a range
    /// selected in that panel is focused in the waveform.
    fn paint_logic_view(
        &self,
        ui: &mut egui::Ui,
        waveforms: &WaveformStore,
        state: &mut PanelState,
        edits: &mut WaveformViewEdits,
    ) {
        let decoded_outcomes = state.protocol_decode_cache.shared_outcomes(
            waveforms,
            &state.protocol_analyzer.analyzers,
            state.streamed_protocol_decode.as_ref(),
        );
        let sample_offset = waveforms.logic_sample_offset;
        let sample_count = waveforms.logic_sample_count;
        let mut selected_range = None;
        if state.decoded_data_panel_open {
            selected_range = self.paint_logic_with_decoded_data_panel(
                ui,
                waveforms,
                decoded_outcomes.as_slice(),
                state,
                edits,
            );
        } else {
            self.paint_logic_with_collapsed_decoded_data_panel(ui, waveforms, state, edits);
        }
        if let Some((start_sample, end_sample)) = selected_range {
            focus_logic_sample_range(
                &mut edits.logic_viewport,
                sample_offset,
                sample_count,
                start_sample,
                end_sample,
            );
        }
    }

    fn paint_logic_waveform(
        &self,
        waveform_ui: &mut egui::Ui,
        waveforms: &WaveformStore,
        state: &mut PanelState,
        edits: &mut WaveformViewEdits,
    ) -> Rect {
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
                        channel_colors: &mut edits.logic_channel_colors,
                        channel_marker_opacities: &mut edits.logic_channel_marker_opacities,
                        channel_names: &mut edits.logic_channel_names,
                        channel_settings_drafts: &mut edits.logic_channel_settings_drafts,
                        smart_timing_enabled: self.logic_smart_timing_enabled,
                        marker_settings: &mut edits.logic_marker_settings,
                        maximum_heights: &mut edits.logic_maximum_heights,
                        enabled_channels: &mut edits.logic_enabled_channels,
                        protocol_analyzer: &state.protocol_analyzer,
                        protocol_decode_cache: &mut state.protocol_decode_cache,
                        protocol_stream: state.streamed_protocol_decode.as_ref(),
                        time_axis: None,
                    },
                    &mut edits.logic_viewport,
                )
            },
        )
    }

    /// The logic waveform next to the expanded decoded-data panel and a
    /// draggable splitter; returns the sample range selected in the panel.
    fn paint_logic_with_decoded_data_panel(
        &self,
        ui: &mut egui::Ui,
        waveforms: &WaveformStore,
        decoded_outcomes: &[ProtocolDecodeOutcome],
        state: &mut PanelState,
        edits: &mut WaveformViewEdits,
    ) -> Option<(u64, u64)> {
        let sample_rate_hz = waveforms.logic_rate_hz;
        let available_size = ui.available_size();
        let spacing = ui.spacing().item_spacing.x;
        let splitter_width = 6.0;
        let minimum_waveform_width = 260.0;
        let mut selected_range = None;
        let maximum_panel_width =
            (available_size.x - minimum_waveform_width - splitter_width - spacing * 2.0).max(120.0);
        state.decoded_data_panel_width = state.decoded_data_panel_width.clamp(
            280.0_f32.min(maximum_panel_width),
            maximum_panel_width.min(560.0),
        );
        let waveform_width =
            (available_size.x - state.decoded_data_panel_width - splitter_width - spacing * 2.0)
                .max(1.0);
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(waveform_width, available_size.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    state.snapshot_plot_rect =
                        Some(self.paint_logic_waveform(ui, waveforms, state, edits));
                },
            );
            let (splitter_rect, splitter_response) =
                ui.allocate_exact_size(Vec2::new(splitter_width, available_size.y), Sense::drag());
            let splitter_response =
                splitter_response.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            ui.painter().line_segment(
                [splitter_rect.center_top(), splitter_rect.center_bottom()],
                Stroke::new(1.0_f32, GRID),
            );
            if splitter_response.dragged() {
                let delta = ui.input(|input| input.pointer.delta().x);
                state.decoded_data_panel_width = (state.decoded_data_panel_width - delta).clamp(
                    280.0_f32.min(maximum_panel_width),
                    maximum_panel_width.min(560.0),
                );
            }
            ui.allocate_ui_with_layout(
                Vec2::new(state.decoded_data_panel_width, available_size.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    selected_range = paint_decoded_data_side_panel(
                        ui,
                        available_size,
                        state,
                        decoded_outcomes,
                        sample_rate_hz,
                    );
                },
            );
        });
        selected_range
    }

    /// The logic waveform next to the narrow strip that expands the
    /// decoded-data panel.
    fn paint_logic_with_collapsed_decoded_data_panel(
        &self,
        ui: &mut egui::Ui,
        waveforms: &WaveformStore,
        state: &mut PanelState,
        edits: &mut WaveformViewEdits,
    ) {
        let available_size = ui.available_size();
        let spacing = ui.spacing().item_spacing.x;
        let collapsed_width = 34.0;
        let waveform_width = (available_size.x - collapsed_width - spacing).max(1.0);
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(waveform_width, available_size.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    state.snapshot_plot_rect =
                        Some(self.paint_logic_waveform(ui, waveforms, state, edits));
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
                            ui.set_min_height((available_size.y - 8.0).max(0.0));
                            if ui
                                .add_sized(
                                    DECODED_PANEL_ACTION_BUTTON_SIZE,
                                    toolbar_action_button("◀", ACCENT),
                                )
                                .on_hover_selectable_text("Expand decoded data")
                                .clicked()
                            {
                                state.decoded_data_panel_open = true;
                            }
                        });
                },
            );
        });
    }

    fn paint_scope_view(
        &self,
        ui: &mut egui::Ui,
        waveforms: &WaveformStore,
        scope_settings: ScopeCaptureSettings,
        frame: &FrameState,
        state: &mut PanelState,
        edits: &mut WaveformViewEdits,
    ) {
        let FrameState {
            trigger_controls_enabled,
            frontend_controls_enabled,
            ..
        } = *frame;
        let subtitle = scope_channels_subtitle(&self.scope_channel_names);
        state.snapshot_plot_rect = Some(waveform_card_fill(
            ui,
            "OSCILLOSCOPE",
            &subtitle,
            |ui, rect| {
                paint_scope(
                    ui,
                    rect,
                    waveforms,
                    &mut edits.scope_viewport,
                    &mut edits.scope_fft_viewport,
                    true,
                    ScopeWaveformOptions {
                        settings: scope_settings,
                        auto_sample_rate: self.scope_auto_sample_rate,
                        smart_timing_enabled: self.scope_smart_timing_enabled,
                        function_settings: self.scope_function_settings,
                        channel_names: &self.scope_channel_names,
                        measurement_caches: &mut edits.scope_measurement_caches,
                        measurement_window_positions: &mut edits.scope_measurement_window_positions,
                        marker_settings: &mut edits.scope_marker_settings,
                        quick_time_measurement: &mut edits.scope_quick_time_measurement,
                        channel_colors: self.scope_channel_colors,
                        vertical_offsets: &mut edits.scope_vertical_offsets,
                        trigger_settings: &mut edits.scope_trigger_settings,
                        time_per_division_seconds: &mut edits.scope_time_per_division_seconds,
                        timebase_changed: &mut edits.scope_timebase_changed,
                        offset_controls_enabled: frontend_controls_enabled,
                        trigger_controls_enabled,
                        time_axis: None,
                    },
                )
            },
        ));
    }

    fn paint_lissajous_view(
        &self,
        ui: &mut egui::Ui,
        waveforms: &WaveformStore,
        scope_settings: ScopeCaptureSettings,
        state: &mut PanelState,
        edits: &WaveformViewEdits,
    ) {
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
            .zip(edits.scope_vertical_offsets)
        {
            channel.vertical_offset = vertical_offset;
        }
        state.snapshot_plot_rect = Some(waveform_card_fill(
            ui,
            "LISSAJOUS",
            &subtitle,
            |ui, rect| {
                paint_scope_lissajous(
                    ui,
                    rect,
                    waveforms,
                    ScopeLissajousPaintOptions {
                        viewport: edits.scope_viewport,
                        settings: lissajous_scope_settings,
                        lissajous,
                        channel_colors: self.scope_channel_colors,
                        channel_names: &self.scope_channel_names,
                    },
                )
            },
        ));
    }

    fn paint_radio_view(
        &self,
        ui: &mut egui::Ui,
        waveforms: &WaveformStore,
        frame: &FrameState,
        state: &mut PanelState,
    ) {
        let FrameState {
            rf_trigger_level_controls_enabled,
            ..
        } = *frame;
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
        state.snapshot_plot_rect = Some(waveform_card_fill(
            ui,
            "RF SPECTRUM",
            &subtitle,
            |ui, rect| {
                self.radio_view.borrow_mut().paint(
                    ui,
                    rect,
                    &waveforms.radio,
                    &waveforms.radio_analysis,
                    RadioViewOptions {
                        analysis_enabled: self.radio_settings.analyzer.is_enabled(),
                        packet_mode: self.radio_settings.analyzer.data_mode
                            == RadioDataMode::Packets,
                        sample_rate_hz: self.radio_settings.capture.sample_rate_hz,
                        trigger_level_dbfs_tenths: &mut state.radio_trigger_level_dbfs_tenths,
                        trigger_controls_enabled: rf_trigger_level_controls_enabled,
                    },
                );
            },
        ));
    }

    fn paint_combined_view(
        &self,
        ui: &mut egui::Ui,
        waveforms: &WaveformStore,
        scope_settings: ScopeCaptureSettings,
        frame: &FrameState,
        state: &mut PanelState,
        edits: &mut WaveformViewEdits,
    ) {
        let FrameState {
            trigger_controls_enabled,
            frontend_controls_enabled,
            ..
        } = *frame;
        state.snapshot_plot_rect = Some(combined_waveform_cards(
            ui,
            waveforms,
            &mut edits.combined_logic_ratio,
            LogicWaveformOptions {
                channel_colors: &mut edits.logic_channel_colors,
                channel_marker_opacities: &mut edits.logic_channel_marker_opacities,
                channel_names: &mut edits.logic_channel_names,
                channel_settings_drafts: &mut edits.logic_channel_settings_drafts,
                smart_timing_enabled: self.logic_smart_timing_enabled,
                marker_settings: &mut edits.logic_marker_settings,
                maximum_heights: &mut edits.logic_maximum_heights,
                enabled_channels: &mut edits.logic_enabled_channels,
                protocol_analyzer: &state.protocol_analyzer,
                protocol_decode_cache: &mut state.protocol_decode_cache,
                protocol_stream: state.streamed_protocol_decode.as_ref(),
                time_axis: None,
            },
            CombinedWaveformViewports {
                logic: &mut edits.logic_viewport,
                scope: &mut edits.scope_viewport,
                combined: &mut edits.combined_viewport,
                fft: &mut edits.scope_fft_viewport,
            },
            ScopeWaveformOptions {
                settings: scope_settings,
                auto_sample_rate: self.scope_auto_sample_rate,
                smart_timing_enabled: self.scope_smart_timing_enabled,
                function_settings: self.scope_function_settings,
                channel_names: &self.scope_channel_names,
                measurement_caches: &mut edits.scope_measurement_caches,
                measurement_window_positions: &mut edits.scope_measurement_window_positions,
                marker_settings: &mut edits.scope_marker_settings,
                quick_time_measurement: &mut edits.scope_quick_time_measurement,
                channel_colors: self.scope_channel_colors,
                vertical_offsets: &mut edits.scope_vertical_offsets,
                trigger_settings: &mut edits.scope_trigger_settings,
                time_per_division_seconds: &mut edits.scope_time_per_division_seconds,
                timebase_changed: &mut edits.scope_timebase_changed,
                offset_controls_enabled: frontend_controls_enabled,
                trigger_controls_enabled,
                time_axis: None,
            },
        ));
    }

    /// Stores what the views edited and, when a timebase or capture-size change
    /// calls for it, recomputes the automatic sample rate.
    fn apply_waveform_view_edits(&mut self, edits: WaveformViewEdits, frame: &FrameState) {
        let FrameState {
            scope_settings_before_ui,
            scope_auto_sample_rate_before_ui,
            ..
        } = *frame;
        let WaveformViewEdits {
            combined_logic_ratio,
            logic_channel_colors,
            logic_channel_marker_opacities,
            logic_channel_names,
            logic_channel_settings_drafts,
            logic_marker_settings,
            logic_enabled_channels,
            logic_maximum_heights,
            logic_viewport,
            scope_viewport,
            combined_viewport,
            scope_fft_viewport,
            scope_measurement_caches,
            scope_measurement_window_positions,
            scope_marker_settings,
            scope_quick_time_measurement,
            scope_time_per_division_seconds,
            scope_timebase_changed,
            scope_vertical_offsets,
            scope_trigger_settings,
        } = edits;
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
    }
}

fn paint_connect_instrument_message(ui: &mut egui::Ui) {
    ui.centered_and_justified(|ui| {
        ui.label(RichText::new("Connect a supported instrument, then Refresh").color(TEXT_MUTED));
    });
}

/// The expanded "DECODED DATA" panel; returns the sample range the user
/// selected in it. Collapsing it clears `decoded_data_panel_open`.
fn paint_decoded_data_side_panel(
    ui: &mut egui::Ui,
    available_size: Vec2,
    state: &mut PanelState,
    decoded_outcomes: &[ProtocolDecodeOutcome],
    sample_rate_hz: u64,
) -> Option<(u64, u64)> {
    let mut selected_range = None;
    egui::Frame::none()
        .fill(PANEL)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_min_height((available_size.y - 24.0).max(0.0));
            ui.horizontal(|ui| {
                ui.label(RichText::new("DECODED DATA").strong().color(TEXT_MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_sized(
                            DECODED_PANEL_ACTION_BUTTON_SIZE,
                            toolbar_action_button("▶", ACCENT),
                        )
                        .on_hover_selectable_text("Collapse decoded data")
                        .clicked()
                    {
                        state.decoded_data_panel_open = false;
                    }
                });
            });
            ui.add_space(8.0);
            ui.set_width(ui.available_width());
            selected_range = paint_decoded_data_panel(
                ui,
                &state.protocol_analyzer.analyzers,
                decoded_outcomes,
                sample_rate_hz,
            );
        });
    selected_range
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::app;
    use crate::{ProtocolDecodeCacheKey, DSCOPE_SAMPLE_RATES_HZ};

    /// Copies the view state, lets `edit` change it as a view would, and writes
    /// it back for a frame in which nothing else changed.
    fn apply(app: &mut WaveCraftApp, edit: impl FnOnce(&mut WaveformViewEdits)) {
        let frame = app.frame_state();
        apply_in_frame(app, &frame, edit);
    }

    fn apply_in_frame(
        app: &mut WaveCraftApp,
        frame: &FrameState,
        edit: impl FnOnce(&mut WaveformViewEdits),
    ) {
        let mut edits = WaveformViewEdits::copy_from(app);
        edit(&mut edits);
        app.apply_waveform_view_edits(edits, frame);
    }

    /// An app whose scope runs at the lowest rate, so a recomputed automatic
    /// rate for a very fast timebase is visibly different.
    fn app_at_the_lowest_scope_rate() -> (WaveCraftApp, u64) {
        let mut app = app();
        app.scope_auto_sample_rate = true;
        app.scope_settings.sample_rate_hz = DSCOPE_SAMPLE_RATES_HZ[0];
        let recomputed = app.scope_settings.sample_rate_for_timebase(FAST_TIMEBASE);
        assert_ne!(recomputed, app.scope_settings.sample_rate_hz);
        (app, recomputed)
    }

    const FAST_TIMEBASE: f64 = 1.0e-6;

    #[test]
    fn copying_and_applying_unchanged_edits_leaves_the_app_as_it_was() {
        let mut app = app();
        let before = (
            app.combined_logic_ratio,
            app.logic_viewport,
            app.scope_viewport,
            app.combined_viewport,
            app.scope_settings,
            app.scope_time_per_division_seconds,
            app.logic_settings.enabled_channels,
            app.logic_channel_names.clone(),
        );
        apply(&mut app, |_| {});
        assert_eq!(
            before,
            (
                app.combined_logic_ratio,
                app.logic_viewport,
                app.scope_viewport,
                app.combined_viewport,
                app.scope_settings,
                app.scope_time_per_division_seconds,
                app.logic_settings.enabled_channels,
                app.logic_channel_names.clone(),
            )
        );
    }

    #[test]
    fn edits_made_by_the_views_are_written_back_to_the_app() {
        let mut app = app();
        apply(&mut app, |edits| {
            edits.combined_logic_ratio = 0.3;
            edits.logic_enabled_channels[3] = false;
            edits.logic_maximum_heights[2] = 4;
            edits.logic_channel_names[1] = "CLK".to_owned();
            edits.logic_channel_marker_opacities[5] = 7;
            edits.logic_viewport.start = 0.25;
            edits.scope_viewport.span = 0.5;
            edits.combined_viewport.start = 0.125;
            edits.scope_time_per_division_seconds = Some(0.002);
            edits.scope_vertical_offsets = [10, 20];
            edits.scope_trigger_settings.position_percent = 65;
        });
        assert_eq!(app.combined_logic_ratio, 0.3);
        assert!(!app.logic_settings.enabled_channels[3]);
        assert_eq!(app.logic_settings.maximum_heights[2], 4);
        assert_eq!(app.logic_channel_names[1], "CLK");
        assert_eq!(app.logic_channel_marker_opacities[5], 7);
        assert_eq!(app.logic_viewport.start, 0.25);
        assert_eq!(app.scope_viewport.span, 0.5);
        assert_eq!(app.combined_viewport.start, 0.125);
        assert_eq!(app.scope_time_per_division_seconds, Some(0.002));
        assert_eq!(app.scope_settings.channels[0].vertical_offset, 10);
        assert_eq!(app.scope_settings.channels[1].vertical_offset, 20);
        assert_eq!(app.scope_settings.trigger.position_percent, 65);
    }

    #[test]
    fn a_timebase_change_recomputes_the_automatic_sample_rate() {
        let (mut app, recomputed) = app_at_the_lowest_scope_rate();
        apply(&mut app, |edits| {
            edits.scope_timebase_changed = true;
            edits.scope_time_per_division_seconds = Some(FAST_TIMEBASE);
        });
        assert_eq!(app.scope_settings.sample_rate_hz, recomputed);
    }

    #[test]
    fn a_timebase_change_leaves_a_manual_sample_rate_alone() {
        let (mut app, _) = app_at_the_lowest_scope_rate();
        app.scope_auto_sample_rate = false;
        apply(&mut app, |edits| {
            edits.scope_timebase_changed = true;
            edits.scope_time_per_division_seconds = Some(FAST_TIMEBASE);
        });
        assert_eq!(app.scope_settings.sample_rate_hz, DSCOPE_SAMPLE_RATES_HZ[0]);
    }

    #[test]
    fn the_automatic_sample_rate_is_kept_when_no_input_of_it_changed() {
        let (mut app, _) = app_at_the_lowest_scope_rate();
        apply(&mut app, |edits| {
            edits.scope_time_per_division_seconds = Some(FAST_TIMEBASE);
        });
        assert_eq!(app.scope_settings.sample_rate_hz, DSCOPE_SAMPLE_RATES_HZ[0]);
    }

    #[test]
    fn switching_the_sample_rate_to_automatic_recomputes_it() {
        let (mut app, recomputed) = app_at_the_lowest_scope_rate();
        let mut frame = app.frame_state();
        frame.scope_auto_sample_rate_before_ui = false;
        apply_in_frame(&mut app, &frame, |edits| {
            edits.scope_time_per_division_seconds = Some(FAST_TIMEBASE);
        });
        assert_eq!(app.scope_settings.sample_rate_hz, recomputed);
    }

    #[test]
    fn a_changed_capture_length_or_channel_count_recomputes_the_automatic_sample_rate() {
        let (mut app, recomputed) = app_at_the_lowest_scope_rate();

        let mut frame = app.frame_state();
        frame.scope_settings_before_ui.sample_count += 1;
        apply_in_frame(&mut app, &frame, |edits| {
            edits.scope_time_per_division_seconds = Some(FAST_TIMEBASE);
        });
        assert_eq!(app.scope_settings.sample_rate_hz, recomputed);

        app.scope_settings.sample_rate_hz = DSCOPE_SAMPLE_RATES_HZ[0];
        let mut frame = app.frame_state();
        frame.scope_settings_before_ui.channels[1].enabled =
            !frame.scope_settings_before_ui.channels[1].enabled;
        apply_in_frame(&mut app, &frame, |edits| {
            edits.scope_time_per_division_seconds = Some(FAST_TIMEBASE);
        });
        assert_ne!(app.scope_settings.sample_rate_hz, DSCOPE_SAMPLE_RATES_HZ[0]);
    }

    #[test]
    fn the_panel_state_hands_back_everything_it_took() {
        let mut app = app();
        app.decoded_data_panel_open = true;
        app.decoded_data_panel_width = 321.0;
        app.radio_settings.trigger_level_dbfs_tenths = -123;
        app.protocol_decode_cache.key = Some(ProtocolDecodeCacheKey {
            waveform_revision: 42,
            sample_offset: 0,
            sample_count: 0,
            analyzers: Vec::new(),
        });

        let mut state = PanelState::take_from(&mut app);
        assert!(app.protocol_decode_cache.key.is_none());
        assert_eq!(
            state
                .protocol_decode_cache
                .key
                .as_ref()
                .map(|key| key.waveform_revision),
            Some(42)
        );
        assert!(state.decoded_data_panel_open);
        assert_eq!(state.decoded_data_panel_width, 321.0);
        assert_eq!(state.radio_trigger_level_dbfs_tenths, -123);
        assert_eq!(state.snapshot_plot_rect, None);

        state.decoded_data_panel_open = false;
        state.decoded_data_panel_width = 400.0;
        state.radio_trigger_level_dbfs_tenths = -200;
        let plot_rect = Rect::from_min_size(egui::pos2(1.0, 2.0), egui::vec2(30.0, 40.0));
        state.snapshot_plot_rect = Some(plot_rect);
        state.restore_into(&mut app);

        assert_eq!(
            app.protocol_decode_cache
                .key
                .as_ref()
                .map(|key| key.waveform_revision),
            Some(42)
        );
        assert!(!app.decoded_data_panel_open);
        assert_eq!(app.decoded_data_panel_width, 400.0);
        assert_eq!(app.radio_settings.trigger_level_dbfs_tenths, -200);
        assert_eq!(app.snapshot_plot_rect, Some(plot_rect));
    }
}
