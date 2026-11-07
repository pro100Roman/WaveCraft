//! The fixed right panel with the oscilloscope tools.

use crate::{
    egui, paint_scope_tools, scope_tools_panel_is_visible, RichText, ScopeToolsOptions, BACKGROUND,
    PANEL, TEXT_MUTED,
};

use crate::WaveCraftApp;

impl WaveCraftApp {
    /// Paints the scope tools panel when the active waveform tab uses it.
    pub(crate) fn paint_scope_tools_panel(
        &mut self,
        context: &egui::Context,
        trigger_controls_enabled: bool,
    ) {
        if scope_tools_panel_is_visible(self.active_waveform_tab) {
            egui::SidePanel::right("scope_tools_fixed")
                .resizable(false)
                .exact_width(300.0)
                .frame(
                    egui::Frame::none()
                        .fill(PANEL)
                        .inner_margin(egui::Margin::same(14.0)),
                )
                .show(context, |ui| {
                    let content_width = ui.available_width();
                    egui::ScrollArea::vertical()
                        .max_width(content_width)
                        .show(ui, |ui| {
                            ui.set_width(content_width);
                            ui.label(RichText::new("SCOPE TOOLS").strong().color(TEXT_MUTED));
                            ui.add_space(8.0);
                            egui::Frame::none()
                                .fill(BACKGROUND)
                                .rounding(egui::Rounding::same(6.0))
                                .inner_margin(egui::Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.set_width((content_width - 20.0).max(0.0));
                                    paint_scope_tools(
                                        ui,
                                        &mut self.scope_settings,
                                        &mut self.scope_function_settings,
                                        &mut self.scope_marker_settings,
                                        &mut self.scope_marker_editor,
                                        &self.scope_channel_names,
                                        ScopeToolsOptions {
                                            viewport: self.scope_viewport,
                                            trigger_controls_enabled,
                                        },
                                    );
                                });
                        });
                });
        }
    }
}
