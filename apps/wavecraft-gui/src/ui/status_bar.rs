//! The bottom status bar: build version, process load, and the status/error line.

use crate::{
    egui, Color32, RichText, SelectableHoverText, ACCENT, BUILD_VERSION_LABEL, PANEL, TEXT_MUTED,
};

use crate::WaveCraftApp;

impl WaveCraftApp {
    /// Paints the bottom status bar. It only reads application state.
    pub(crate) fn paint_status_bar(&self, context: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(9.0)),
            )
            .show(context, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(BUILD_VERSION_LABEL)
                            .small()
                            .monospace()
                            .color(TEXT_MUTED),
                    )
                    .on_hover_selectable_text("Build version");
                    ui.separator();
                    ui.label(
                        RichText::new(self.memory_usage.label())
                            .small()
                            .monospace()
                            .color(TEXT_MUTED),
                    )
                    .on_hover_selectable_text("Current resident memory used by the WaveCraft process");
                    ui.separator();
                    ui.label(
                        RichText::new(self.cpu_load.label())
                            .small()
                            .monospace()
                            .color(TEXT_MUTED),
                    )
                    .on_hover_selectable_text(
                        "WaveCraft process CPU usage; 100% equals one fully utilized logical CPU, so multi-threaded processing can exceed 100%",
                    );

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let color = if self.last_error.is_some() {
                            Color32::LIGHT_RED
                        } else if self.is_busy() {
                            ACCENT
                        } else {
                            TEXT_MUTED
                        };
                        ui.label(RichText::new("●").color(color));
                        ui.add(
                            egui::Label::new(&self.status)
                                .selectable(true)
                                .truncate(),
                        )
                        .on_hover_selectable_text(&self.status);
                        if let Some(error) = &self.last_error {
                            ui.separator();
                            ui.add(
                                egui::Label::new(RichText::new(error).color(Color32::LIGHT_RED))
                                    .selectable(true)
                                    .truncate(),
                            )
                            .on_hover_selectable_text(error);
                        }
                    });
                });
            });
    }
}
