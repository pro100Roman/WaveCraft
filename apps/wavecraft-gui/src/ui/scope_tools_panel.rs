//! The oscilloscope tools panel beside the waveform card.

use super::view_shell::{settings_card, settings_scroll, tools_panel, SETUP_PANEL_WIDTH};
use crate::{egui, paint_scope_tools, Rect, ScopeToolsOptions, COLLAPSED_SIDE_PANEL_WIDTH};

use crate::WaveCraftApp;

/// Width of the expanded panel.
const SCOPE_TOOLS_PANEL_WIDTH: f32 = SETUP_PANEL_WIDTH;

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
        COLLAPSED_SIDE_PANEL_WIDTH
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
        let mut open = self.scope_tools_panel_open;
        tools_panel(ui, "SCOPE TOOLS", &mut open, |ui| {
            settings_scroll(ui, |ui| {
                settings_card(ui, |ui| {
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
        self.scope_tools_panel_open = open;
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
        assert_eq!(collapsed.tools.width(), COLLAPSED_SIDE_PANEL_WIDTH);
        assert_eq!(
            collapsed.views.width() - expanded.views.width(),
            SCOPE_TOOLS_PANEL_WIDTH - COLLAPSED_SIDE_PANEL_WIDTH
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
