//! The top toolbar: refresh, start/stop, recording, and snapshot controls.

use crate::{
    egui, toolbar_action_button, ui::frame_state::FrameState, AcquisitionMode, CaptureTarget,
    Color32, DsCopeTriggerSource, RichText, SelectableHoverText, Vec2, ACCENT,
    CAPTURE_FILE_BUTTON_WIDTH, ORANGE, PANEL, SCOPE_COLORS, TEXT_MUTED,
};

use crate::{ui::frame_actions::FrameActions, WaveCraftApp};

impl WaveCraftApp {
    /// File operations stay disabled for the whole acquisition request, not
    /// only while a worker or source happens to be active. Re-Arm briefly has
    /// neither between sweeps, which previously made Open and Export blink.
    fn capture_file_actions_enabled(&self) -> bool {
        !self.is_busy() && self.capture_requests.is_empty()
    }

    /// Paints the toolbar and records the user's requests in `actions`.
    pub(crate) fn paint_toolbar(
        &mut self,
        context: &egui::Context,
        frame: &FrameState,
        actions: &mut FrameActions,
    ) {
        if self.system_trace_view_is_active() {
            self.paint_system_trace_toolbar(context);
            return;
        }
        let FrameState {
            control_target,
            control_target_available,
            can_attach_run_capture_recording,
            can_attach_run_decoded_recording,
            ..
        } = *frame;
        let capture_file_actions_enabled = self.capture_file_actions_enabled();
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
                            capture_file_actions_enabled,
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
                            capture_file_actions_enabled
                                && self.has_displayed_completed_capture(),
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
                        let scope_auto = control_target == Some(CaptureTarget::Oscilloscope)
                            && self.scope_settings.trigger.source == DsCopeTriggerSource::Auto;
                        let scope_update = control_target == Some(CaptureTarget::Oscilloscope)
                            && !scope_auto;
                        let start_label = if scope_auto {
                            "▶ Auto"
                        } else if scope_update {
                            "▶ Update"
                        } else {
                            "▶ Run"
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
                                    toolbar_action_button(start_label, ACCENT)
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
                            } else if scope_auto {
                                "Continuously acquire trigger-synchronized oscilloscope sweeps"
                            } else if scope_update {
                                "Keep the current trace until the next trigger updates the display"
                            } else {
                                "Run continuously"
                            })
                            .clicked();
                        if run_stop_clicked {
                            if acquisition_active {
                                actions.stop_clicked = true;
                            } else if let Some(target) = control_target {
                                actions.start_request = Some((start_mode, target));
                            }
                        }
                        if control_target == Some(CaptureTarget::Oscilloscope)
                            && ui
                                .add_enabled(
                                    self.capture_target_can_start(CaptureTarget::Oscilloscope)
                                        && control_target_available
                                        && !self.record_capture
                                        && !self.record_decoded_data,
                                    toolbar_action_button("▶ Re-Arm", SCOPE_COLORS[1]),
                                )
                                .on_hover_selectable_text(
                                    "Acquire one complete sweep, then automatically re-arm for the next trigger",
                                )
                                .clicked()
                        {
                            actions.start_request = Some((
                                AcquisitionMode::Repetitive,
                                CaptureTarget::Oscilloscope,
                            ));
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app::test_support::{app, request};

    #[test]
    fn capture_file_actions_stay_disabled_between_rearmed_sweeps() {
        let mut app = app();
        assert!(app.capture_file_actions_enabled());

        request(&mut app, CaptureTarget::Oscilloscope);

        assert!(app.pending.is_none());
        assert!(app.captures.is_empty());
        assert!(!app.capture_file_actions_enabled());
    }
}
