//! Shared instrument chrome. Views supply controls and content; this module
//! owns panel dimensions, typography, scrolling, headers, and card decoration.
//! It has no dependency on application state or device backends.

use crate::{
    apply_setup_panel_typography, egui, toolbar_action_button, Color32, Rect, RichText, ACCENT,
    BACKGROUND, DECODED_PANEL_ACTION_BUTTON_SIZE, GRID, PANEL, TEXT_MUTED,
};

pub(crate) const SETUP_PANEL_WIDTH: f32 = 300.0;
const CARD_MARGIN: f32 = 12.0;
const CARD_STROKE_WIDTH: f32 = 1.0;
const COLLAPSED_PANEL_MARGIN: f32 = 4.0;
pub(crate) const COLLAPSED_SIDE_PANEL_WIDTH: f32 =
    DECODED_PANEL_ACTION_BUTTON_SIZE[0] + 2.0 * (COLLAPSED_PANEL_MARGIN + CARD_STROKE_WIDTH);

fn panel_frame(fill: Color32, margin: f32) -> egui::Frame {
    egui::Frame::none()
        .fill(fill)
        .inner_margin(egui::Margin::same(margin))
}

/// The same application menu and toolbar row in every workspace. The returned
/// flag requests App > Settings, without borrowing a view's application state.
pub(crate) fn view_toolbar(
    context: &egui::Context,
    subtitle: &str,
    controls: impl FnOnce(&mut egui::Ui),
) -> bool {
    let mut settings_clicked = false;
    egui::TopBottomPanel::top("toolbar")
        .frame(panel_frame(PANEL, 12.0))
        .show(context, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("WAVECRAFT").strong().size(18.0).color(ACCENT));
                ui.separator();
                ui.menu_button("App", |ui| {
                    if ui.button("Settings…").clicked() {
                        settings_clicked = true;
                        ui.close_menu();
                    }
                });
                ui.separator();
                if !subtitle.is_empty() {
                    ui.label(RichText::new(subtitle).color(TEXT_MUTED));
                    ui.separator();
                }
                controls(ui);
            });
        });
    settings_clicked
}

/// Left settings panel. A stable ID keeps scroll state isolated per workspace.
pub(crate) struct SetupPanel<'a> {
    id: &'static str,
    title: &'a str,
}

impl<'a> SetupPanel<'a> {
    pub(crate) fn new(id: &'static str, title: &'a str) -> Self {
        Self { id, title }
    }

    /// Every setup panel has collapse/expand and a pinned save footer. The
    /// content returns whether its edited settings may currently be saved.
    pub(crate) fn show(
        self,
        context: &egui::Context,
        open: &mut bool,
        contents: impl FnOnce(&mut egui::Ui) -> bool,
    ) -> bool {
        if !*open {
            egui::SidePanel::left(format!("{}_collapsed", self.id))
                .resizable(false)
                .exact_width(COLLAPSED_SIDE_PANEL_WIDTH)
                .frame(collapsed_side_panel_frame())
                .show(context, |ui| {
                    fill_card_height(ui);
                    if panel_toggle(ui, "▶", &format!("Expand {}", self.title)) {
                        *open = true;
                    }
                });
            return false;
        }
        egui::SidePanel::left(self.id)
            .resizable(false)
            .exact_width(SETUP_PANEL_WIDTH)
            .frame(panel_frame(PANEL, 14.0))
            .show(context, |ui| {
                apply_setup_panel_typography(ui);
                panel_heading(ui, self.title, Some(open), "◀");
                ui.add_space(8.0);
                let available = ui.available_rect_before_wrap();
                let footer_height = 36.0_f32.min(available.height().max(0.0));
                let footer_top = available.bottom() - footer_height;
                let body_rect = Rect::from_min_max(
                    available.min,
                    egui::pos2(available.right(), footer_top.max(available.top())),
                );
                let mut body = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("settings_body")
                        .max_rect(body_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                body.set_clip_rect(ui.clip_rect().intersect(body_rect));
                let save_enabled = settings_scroll(&mut body, contents);
                let mut footer = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("settings_footer")
                        .max_rect(Rect::from_min_max(
                            egui::pos2(available.left(), footer_top),
                            available.max,
                        ))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                footer.separator();
                let clicked = footer
                    .add_enabled(
                        save_enabled,
                        toolbar_action_button("💾 Save settings", ACCENT)
                            .min_size(egui::vec2(footer.available_width(), 24.0)),
                    )
                    .on_hover_text("Save the current settings as defaults for this view")
                    .clicked();
                ui.advance_cursor_after_rect(available);
                clicked
            })
            .inner
    }
}

/// Width-constrained settings scrolling, shared by setup and tool panels.
pub(crate) fn settings_scroll<R>(
    ui: &mut egui::Ui,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let width = ui.available_width();
    egui::ScrollArea::vertical()
        .max_width(width)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(width);
            ui.set_max_width(width);
            contents(ui)
        })
        .inner
}

/// Dark inset used to group the instrument's settings sections.
pub(crate) fn settings_card<R>(
    ui: &mut egui::Ui,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let width = (ui.available_width() - 20.0).max(0.0);
    panel_frame(BACKGROUND, 10.0).rounding(6.0).show(ui, |ui| {
        ui.set_width(width);
        ui.set_max_width(width);
        contents(ui)
    })
}

pub(crate) fn workspace_panel(context: &egui::Context, contents: impl FnOnce(&mut egui::Ui)) {
    egui::CentralPanel::default()
        .frame(panel_frame(BACKGROUND, 16.0))
        .show(context, contents);
}

/// Outline shared by plots, tables, and auxiliary tool panels.
pub(crate) fn card_frame(inner_margin: f32) -> egui::Frame {
    panel_frame(PANEL, inner_margin)
        .stroke(egui::Stroke::new(CARD_STROKE_WIDTH, GRID))
        .rounding(8.0)
}

pub(crate) fn collapsed_side_panel_frame() -> egui::Frame {
    card_frame(COLLAPSED_PANEL_MARGIN)
}

pub(crate) fn fill_card_height(ui: &mut egui::Ui) {
    ui.set_min_height(ui.available_height());
}

/// A full-height content card. The view paints its own header, tabs, and body.
pub(crate) fn view_card<R>(
    ui: &mut egui::Ui,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    card_frame(CARD_MARGIN).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        fill_card_height(ui);
        contents(ui)
    })
}

/// Allocate the remainder of a card for a custom plot and snapshot cropping.
pub(crate) fn remaining_plot_rect(ui: &mut egui::Ui) -> Rect {
    ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ui.available_height().max(64.0)),
        egui::Sense::hover(),
    )
    .0
}

fn panel_toggle(ui: &mut egui::Ui, arrow: &str, hint: &str) -> bool {
    ui.add_sized(
        DECODED_PANEL_ACTION_BUTTON_SIZE,
        toolbar_action_button(arrow, ACCENT),
    )
    .on_hover_text(hint)
    .clicked()
}

fn panel_heading(ui: &mut egui::Ui, title: &str, open: Option<&mut bool>, arrow: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong().color(TEXT_MUTED));
        if let Some(open) = open {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if panel_toggle(ui, arrow, &format!("Collapse {title}")) {
                    *open = false;
                }
            });
        }
    });
}

/// Auxiliary panel on the right of a plot. Its body may supply its own scroll
/// areas (decoded tables) or use `settings_scroll` (settings editors).
pub(crate) fn tools_panel<R>(
    ui: &mut egui::Ui,
    title: &str,
    open: &mut bool,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    if !*open {
        collapsed_side_panel_frame().show(ui, |ui| {
            fill_card_height(ui);
            if panel_toggle(ui, "◀", &format!("Expand {title}")) {
                *open = true;
            }
        });
        return None;
    }
    Some(
        view_card(ui, |ui| {
            panel_heading(ui, title, Some(open), "▶");
            ui.add_space(8.0);
            contents(ui)
        })
        .inner,
    )
}
