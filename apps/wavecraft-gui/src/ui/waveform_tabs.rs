//! The top strip that switches between waveform and System Trace views.

use crate::{egui, workspace_views, RichText, WorkspaceView, PANEL, TEXT_MUTED};

use crate::WaveCraftApp;

impl WaveCraftApp {
    /// Paints the available waveform views and the independent System Trace view.
    pub(crate) fn paint_waveform_tabs(&mut self, context: &egui::Context) {
        let views = workspace_views(&self.available_waveform_tabs());
        egui::TopBottomPanel::top("waveform_tabs")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(14.0, 7.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("VIEW").strong().color(TEXT_MUTED));
                    ui.separator();
                    for view in views {
                        if ui
                            .selectable_label(
                                self.active_workspace_view == Some(view),
                                view.label(),
                            )
                            .clicked()
                        {
                            match view {
                                WorkspaceView::Waveform(tab) => self.select_waveform_tab(tab),
                                WorkspaceView::SystemTrace => self.select_system_trace_view(),
                            }
                        }
                    }
                });
            });
    }
}
