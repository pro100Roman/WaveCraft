use eframe::egui::{self, Color32, Stroke};

pub(crate) const BACKGROUND: Color32 = Color32::from_rgb(12, 17, 25);
pub(crate) const PANEL: Color32 = Color32::from_rgb(20, 27, 38);
pub(crate) const GRID: Color32 = Color32::from_rgb(43, 55, 72);
pub(crate) const TEXT_MUTED: Color32 = Color32::from_rgb(145, 160, 180);
pub(crate) const ACCENT: Color32 = Color32::from_rgb(70, 210, 185);
pub(crate) const ORANGE: Color32 = Color32::from_rgb(255, 145, 77);
pub(crate) const SCOPE_COLORS: [Color32; 2] = [
    Color32::from_rgb(255, 204, 74),
    Color32::from_rgb(99, 179, 255),
];
pub(crate) const MATH_COLOR: Color32 = Color32::from_rgb(213, 126, 255);
pub(crate) const SETUP_PANEL_BODY_FONT_SIZE: f32 = 12.5;
pub(crate) const SETUP_PANEL_SMALL_FONT_SIZE: f32 = 9.0;
pub(crate) const SETUP_PANEL_MONOSPACE_FONT_SIZE: f32 = 12.0;
const TOOLTIP_DELAY_SECONDS: f32 = 0.1;
const SYMBOL_FONT_NAME: &str = "Noto Sans Symbols2";
const SYMBOL_FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/NotoSansSymbols2-Regular.ttf");

/// Keeps every instrument setup panel on the typography established by the RF
/// settings, even when a nested panel customizes its local egui style.
pub(crate) fn apply_setup_panel_typography(ui: &mut egui::Ui) {
    let text_styles = &mut ui.style_mut().text_styles;
    text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::proportional(SETUP_PANEL_SMALL_FONT_SIZE),
    );
    text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::proportional(SETUP_PANEL_BODY_FONT_SIZE),
    );
    text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(SETUP_PANEL_BODY_FONT_SIZE),
    );
    text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::monospace(SETUP_PANEL_MONOSPACE_FONT_SIZE),
    );
}

pub(crate) fn toolbar_action_button(label: &str, color: Color32) -> egui::Button<'static> {
    egui::Button::new(label.to_owned())
        .fill(color.gamma_multiply(0.24))
        .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.85)))
}

pub(crate) fn configure_style(context: &egui::Context) {
    context.set_fonts(wavecraft_font_definitions());
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

fn wavecraft_font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default();
    let hack = "Hack".to_owned();
    if !proportional.contains(&hack) {
        proportional.push(hack);
    }
    fonts.font_data.insert(
        SYMBOL_FONT_NAME.to_owned(),
        egui::FontData::from_static(SYMBOL_FONT_BYTES).tweak(egui::FontTweak {
            y_offset_factor: 0.0625,
            ..egui::FontTweak::default()
        }),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let family_fonts = fonts.families.entry(family).or_default();
        let symbol_font = SYMBOL_FONT_NAME.to_owned();
        if !family_fonts.contains(&symbol_font) {
            family_fonts.push(symbol_font);
        }
    }
    fonts
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

    #[test]
    fn symbol_font_is_the_last_fallback_for_both_default_families() {
        let fonts = wavecraft_font_definitions();

        assert!(fonts.font_data.contains_key(SYMBOL_FONT_NAME));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            assert_eq!(
                fonts.families[&family].last().map(String::as_str),
                Some(SYMBOL_FONT_NAME)
            );
        }
    }

    #[test]
    fn configured_fonts_contain_the_trace_drag_handle_glyph() {
        let context = egui::Context::default();
        configure_style(&context);
        let mut supported = [false; 2];

        let _ = context.run(egui::RawInput::default(), |context| {
            supported = context.fonts(|fonts| {
                [
                    egui::FontId::proportional(14.0),
                    egui::FontId::monospace(14.0),
                ]
                .map(|font_id| fonts.has_glyph(&font_id, '⠿'))
            });
        });

        assert_eq!(supported, [true, true]);
    }

    #[test]
    fn setup_panel_typography_matches_the_rf_baseline() {
        let context = egui::Context::default();
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                apply_setup_panel_typography(ui);
                let styles = &ui.style().text_styles;
                assert_eq!(
                    styles[&egui::TextStyle::Body].size,
                    SETUP_PANEL_BODY_FONT_SIZE
                );
                assert_eq!(
                    styles[&egui::TextStyle::Button].size,
                    SETUP_PANEL_BODY_FONT_SIZE
                );
                assert_eq!(
                    styles[&egui::TextStyle::Small].size,
                    SETUP_PANEL_SMALL_FONT_SIZE
                );
                assert_eq!(
                    styles[&egui::TextStyle::Monospace].size,
                    SETUP_PANEL_MONOSPACE_FONT_SIZE
                );
            });
        });
    }
}
