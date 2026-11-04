use eframe::egui::{self, Color32, Stroke};

pub(crate) const BACKGROUND: Color32 = Color32::from_rgb(12, 17, 25);
pub(crate) const PANEL: Color32 = Color32::from_rgb(20, 27, 38);
pub(crate) const GRID: Color32 = Color32::from_rgb(43, 55, 72);
pub(crate) const TEXT_MUTED: Color32 = Color32::from_rgb(145, 160, 180);
pub(crate) const ACCENT: Color32 = Color32::from_rgb(70, 210, 185);
pub(crate) const SCOPE_COLORS: [Color32; 2] = [
    Color32::from_rgb(255, 204, 74),
    Color32::from_rgb(99, 179, 255),
];
pub(crate) const MATH_COLOR: Color32 = Color32::from_rgb(213, 126, 255);
const TOOLTIP_DELAY_SECONDS: f32 = 0.1;

pub(crate) fn toolbar_action_button(label: &str, color: Color32) -> egui::Button<'static> {
    egui::Button::new(label.to_owned())
        .fill(color.gamma_multiply(0.24))
        .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.85)))
}

pub(crate) fn configure_style(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BACKGROUND;
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    visuals.widgets.active.bg_fill = ACCENT.gamma_multiply(0.6);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(45, 61, 77);
    context.set_visuals(visuals);
    context.style_mut(|style| {
        style.interaction.tooltip_delay = TOOLTIP_DELAY_SECONDS;
        style.interaction.selectable_labels = true;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_labels_are_selectable_for_copying() {
        let context = egui::Context::default();

        configure_style(&context);

        assert!(context.style().interaction.selectable_labels);
    }
}
