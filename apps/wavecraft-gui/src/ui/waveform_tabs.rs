//! The top strip that switches between the logic, scope, radio, and combined views.

use crate::{egui, RichText, PANEL, TEXT_MUTED};

use crate::WaveCraftApp;

impl WaveCraftApp {
    /// Paints the view tabs for the instruments that are available, and
    /// switches the active waveform view when one is clicked.
    pub(crate) fn paint_waveform_tabs(&mut self, context: &egui::Context) {
        let available_waveform_tabs = self.available_waveform_tabs();
        if !available_waveform_tabs.is_empty() {
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
                        for tab in &available_waveform_tabs {
                            if ui
                                .selectable_label(
                                    self.active_waveform_tab == Some(*tab),
                                    tab.label(),
                                )
                                .clicked()
                            {
                                self.select_waveform_tab(*tab);
                            }
                        }
                    });
                });
        }
    }
}
