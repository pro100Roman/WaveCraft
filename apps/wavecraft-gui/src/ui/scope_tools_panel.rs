//! The oscilloscope tools panel beside the waveform card.

use crate::{
    egui, paint_scope_tools, toolbar_action_button, Rect, RichText, ScopeToolsOptions,
    SelectableHoverText, ACCENT, BACKGROUND, DECODED_PANEL_ACTION_BUTTON_SIZE, PANEL, TEXT_MUTED,
};

use crate::WaveCraftApp;

/// Width of the expanded panel.
const SCOPE_TOOLS_PANEL_WIDTH: f32 = 300.0;
/// Width of the strip that stands in for the collapsed panel.
const SCOPE_TOOLS_COLLAPSED_WIDTH: f32 = 34.0;
/// Space around the contents of the expanded panel.
const SCOPE_TOOLS_PANEL_MARGIN: f32 = 14.0;
/// Space around the expand button of the collapsed strip.
const SCOPE_TOOLS_COLLAPSED_MARGIN: f32 = 4.0;

/// How the central area is divided between the waveform view and the tools.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScopeToolsLayout {
    pub(crate) views: Rect,
    pub(crate) tools: Rect,
}

/// Reserves the right edge of `available` for the tools panel (or its
/// collapsed strip) and leaves the rest, minus `spacing`, to the waveform view.
pub(crate) fn scope_tools_layout(available: Rect, open: bool, spacing: f32) -> ScopeToolsLayout {
    let wanted_width = if open {
        SCOPE_TOOLS_PANEL_WIDTH
    } else {
        SCOPE_TOOLS_COLLAPSED_WIDTH
    };
    // Keep at least a sliver for the view in a window too narrow for both.
    let tools_width = wanted_width.min((available.width() - spacing - 1.0).max(0.0));
    let tools_left = available.right() - tools_width;
    ScopeToolsLayout {
        views: Rect::from_min_max(
            available.min,
            egui::pos2(tools_left - spacing, available.bottom()),
        ),
        tools: Rect::from_min_max(egui::pos2(tools_left, available.top()), available.max),
    }
}

impl WaveCraftApp {
    /// Paints the scope tools into `ui`, which covers the area reserved for
    /// them: the full panel, or a narrow strip with a button that expands it.
    pub(crate) fn paint_scope_tools_panel(
        &mut self,
        ui: &mut egui::Ui,
        trigger_controls_enabled: bool,
    ) {
        if self.scope_tools_panel_open {
            self.paint_expanded_scope_tools_panel(ui, trigger_controls_enabled);
        } else {
            self.paint_collapsed_scope_tools_panel(ui);
        }
    }

    /// The "SCOPE TOOLS" panel; its header button collapses it.
    fn paint_expanded_scope_tools_panel(
        &mut self,
        ui: &mut egui::Ui,
        trigger_controls_enabled: bool,
    ) {
        let available_height = ui.available_height();
        let mut collapse_clicked = false;
        egui::Frame::none()
            .fill(PANEL)
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(SCOPE_TOOLS_PANEL_MARGIN))
            .show(ui, |ui| {
                ui.set_min_height((available_height - SCOPE_TOOLS_PANEL_MARGIN * 2.0).max(0.0));
                let content_width = ui.available_width();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SCOPE TOOLS").strong().color(TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_sized(
                                DECODED_PANEL_ACTION_BUTTON_SIZE,
                                toolbar_action_button("▶", ACCENT),
                            )
                            .on_hover_selectable_text("Collapse scope tools")
                            .clicked()
                        {
                            collapse_clicked = true;
                        }
                    });
                });
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_width(content_width)
                    .show(ui, |ui| {
                        ui.set_width(content_width);
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
        if collapse_clicked {
            self.scope_tools_panel_open = false;
        }
    }

    /// The narrow strip that replaces the collapsed panel; its button expands it.
    fn paint_collapsed_scope_tools_panel(&mut self, ui: &mut egui::Ui) {
        let available_height = ui.available_height();
        let mut expand_clicked = false;
        egui::Frame::none()
            .fill(PANEL)
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(SCOPE_TOOLS_COLLAPSED_MARGIN))
            .show(ui, |ui| {
                ui.set_min_height((available_height - SCOPE_TOOLS_COLLAPSED_MARGIN * 2.0).max(0.0));
                if ui
                    .add_sized(
                        DECODED_PANEL_ACTION_BUTTON_SIZE,
                        toolbar_action_button("◀", ACCENT),
                    )
                    .on_hover_selectable_text("Expand scope tools")
                    .clicked()
                {
                    expand_clicked = true;
                }
            });
        if expand_clicked {
            self.scope_tools_panel_open = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_max(egui::pos2(16.0, 80.0), egui::pos2(1016.0, 780.0))
    }

    #[test]
    fn the_expanded_panel_takes_the_right_edge_and_the_view_the_rest() {
        let layout = scope_tools_layout(area(), true, 8.0);
        assert_eq!(layout.tools.width(), SCOPE_TOOLS_PANEL_WIDTH);
        assert_eq!(layout.tools.right(), area().right());
        assert_eq!(layout.views.left(), area().left());
        assert_eq!(layout.views.right() + 8.0, layout.tools.left());
        assert_eq!(layout.tools.top(), area().top());
        assert_eq!(layout.tools.bottom(), area().bottom());
        assert_eq!(layout.views.top(), area().top());
        assert_eq!(layout.views.bottom(), area().bottom());
    }

    #[test]
    fn the_collapsed_strip_leaves_the_view_wider() {
        let expanded = scope_tools_layout(area(), true, 8.0);
        let collapsed = scope_tools_layout(area(), false, 8.0);
        assert_eq!(collapsed.tools.width(), SCOPE_TOOLS_COLLAPSED_WIDTH);
        assert_eq!(
            collapsed.views.width() - expanded.views.width(),
            SCOPE_TOOLS_PANEL_WIDTH - SCOPE_TOOLS_COLLAPSED_WIDTH
        );
    }

    #[test]
    fn a_narrow_area_keeps_a_sliver_for_the_view() {
        let narrow = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 400.0));
        let layout = scope_tools_layout(narrow, true, 8.0);
        assert!(layout.views.width() >= 1.0);
        assert!(layout.tools.width() <= 200.0 - 8.0 - 1.0);
        assert_eq!(layout.tools.right(), narrow.right());
    }
}
