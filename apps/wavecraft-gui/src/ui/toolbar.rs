//! The top toolbar: refresh, start/stop, recording, and snapshot controls.

use crate::{
    egui, toolbar_action_button, ui::frame_state::FrameState, AcquisitionMode, CaptureTarget,
    Color32, RichText, SelectableHoverText, Vec2, ACCENT, CAPTURE_FILE_BUTTON_WIDTH, ORANGE, PANEL,
    SCOPE_COLORS, TEXT_MUTED,
};

use crate::{ui::frame_actions::FrameActions, WaveCraftApp};

impl WaveCraftApp {
    /// Paints the toolbar and records the user's requests in `actions`.
    pub(crate) fn paint_toolbar(
        &mut self,
        context: &egui::Context,
        frame: &FrameState,
        actions: &mut FrameActions,
    ) {
        let FrameState {
            control_target,
            control_target_available,
            can_attach_run_capture_recording,
            can_attach_run_decoded_recording,
            ..
        } = *frame;
        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(12.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("WAVECRAFT").strong().size(18.0).color(ACCENT));
                    ui.separator();
                    ui.menu_button("App", |ui| {
                        if ui.button("Settings…").clicked() {
                            self.settings_open = true;
                            ui.close_menu();
                        }
                    });
                    ui.separator();
                    ui.label(RichText::new("USB and SDR capture workbench").color(TEXT_MUTED));
                    ui.separator();
                    actions.refresh_clicked = ui
                        .add_enabled(
                            !self.is_busy(),
                            toolbar_action_button("↻ Refresh", SCOPE_COLORS[1]),
                        )
                        .clicked();
                    ui.separator();
                    if ui
                        .add_enabled(
                            (control_target.is_some_and(|target| {
                                self.capture_target_can_start(target)
                            }) && control_target_available)
                                || can_attach_run_capture_recording,
                            toolbar_action_button("⏺ Record", Color32::LIGHT_RED)
                                .min_size(Vec2::new(CAPTURE_FILE_BUTTON_WIDTH, 0.0)),
                        )
                        .on_hover_selectable_text(if can_attach_run_capture_recording {
                            "Start recording the active Run acquisition to a .dsv file until Stop"
                        } else if control_target == Some(CaptureTarget::RadioReceiver) {
                            "Record continuous Run-mode raw IQ to a .dsv file until Stop"
                        } else {
                            "Record a complete Single capture to a .dsv file"
                        })
                        .clicked()
                    {
                        actions.record_now_clicked = true;
                    }
                    if control_target == Some(CaptureTarget::RadioReceiver)
                        && ui
                            .add_enabled(
                                (self.capture_target_can_start(CaptureTarget::RadioReceiver)
                                    && control_target_available)
                                    || can_attach_run_decoded_recording,
                                toolbar_action_button("⏺ Data", ORANGE)
                                    .min_size(Vec2::new(CAPTURE_FILE_BUTTON_WIDTH, 0.0)),
                            )
                            .on_hover_selectable_text(
                                "Record decoded RAW messages or PACKETS in Run mode to JSON Lines until Stop",
                            )
                            .clicked()
                    {
                        actions.record_decoded_data_clicked = true;
                    }
                    if ui
                        .add_enabled(
                            self.pending.is_none(),
                            toolbar_action_button("📂 Open", TEXT_MUTED)
                                .min_size(Vec2::new(CAPTURE_FILE_BUTTON_WIDTH, 0.0)),
                        )
                        .on_hover_selectable_text("Open a saved .dsv capture")
                        .clicked()
                    {
                        actions.open_recording_clicked = true;
                    }
                    if ui
                        .add_enabled(
                            self.pending.is_none() && self.has_displayed_completed_capture(),
                            toolbar_action_button("Export", TEXT_MUTED)
                                .min_size(Vec2::new(CAPTURE_FILE_BUTTON_WIDTH, 0.0)),
                        )
                        .on_hover_selectable_text("Export the completed capture as a Saleae Logic 2 .sal file")
                        .clicked()
                    {
                        actions.export_saleae_clicked = true;
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.snapshot_plot_rect.is_some()
                                && self.pending_snapshot.is_none(),
                            toolbar_action_button("📷", TEXT_MUTED),
                        )
                        .on_hover_selectable_text("Save the visible waveform plot as a PNG snapshot")
                        .clicked()
                    {
                        actions.snapshot_clicked = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let logic_only = control_target == Some(CaptureTarget::LogicAnalyzer);
                        let acquisition_active = control_target.is_some_and(|control| {
                            self.capture_for_target(control).is_some()
                                || self
                                    .capture_requests
                                    .iter()
                                    .any(|requested| requested.includes(control))
                        });
                        let start_mode = if logic_only {
                            self.logic_acquisition_mode
                        } else {
                            AcquisitionMode::Run
                        };
                        let run_stop_clicked = ui
                            .add_enabled(
                                acquisition_active
                                    || (control_target.is_some_and(|target| {
                                        self.capture_target_can_start(target)
                                    })
                                        && control_target_available
                                        && !self.record_capture
                                        && !self.record_decoded_data),
                                if acquisition_active {
                                    toolbar_action_button("■ Stop", Color32::LIGHT_RED)
                                } else {
                                    toolbar_action_button("▶ Run", ACCENT)
                                },
                            )
                            .on_hover_selectable_text(if acquisition_active {
                                "Stop the active acquisition"
                            } else if logic_only {
                                match start_mode {
                                    AcquisitionMode::Single => {
                                        "Wait for one trigger, acquire once, then stop"
                                    }
                                    AcquisitionMode::Repetitive => {
                                        "Repeatedly acquire complete captures"
                                    }
                                    AcquisitionMode::Loop => {
                                        "Continuously append stream captures to the rolling display"
                                    }
                                    AcquisitionMode::Run => "Start the selected acquisition",
                                }
                            } else {
                                "Run continuously; Auto trigger rolls, edge triggers update the display"
                            })
                            .clicked();
                        if run_stop_clicked {
                            if acquisition_active {
                                actions.stop_clicked = true;
                            } else if let Some(target) = control_target {
                                actions.start_request = Some((start_mode, target));
                            }
                        }
                        if ui
                            .add_enabled(
                                control_target.is_some_and(|target| {
                                    self.capture_target_can_start(target)
                                }) && control_target_available,
                                toolbar_action_button("▶ Single", SCOPE_COLORS[0]),
                            )
                            .on_hover_selectable_text("Wait for one trigger, acquire once, then stop")
                            .clicked()
                        {
                            if let Some(target) = control_target {
                                actions.start_request = Some((AcquisitionMode::Single, target));
                            }
                        }
                    });
                });
            });
    }
}
