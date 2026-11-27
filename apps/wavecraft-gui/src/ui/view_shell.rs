//! Shared instrument chrome. Views supply controls and content; this module
//! owns panel dimensions, typography, scrolling, headers, and card decoration.
//! It has no dependency on application state or device backends.

use crate::{
    apply_setup_panel_typography, egui, toolbar_action_button, Color32, Rect, RichText,
    SelectableHoverText, ACCENT, BACKGROUND, DECODED_PANEL_ACTION_BUTTON_SIZE, GRID, PANEL,
    TEXT_MUTED,
};

pub(crate) const SETUP_PANEL_WIDTH: f32 = 300.0;
const CARD_MARGIN: f32 = 12.0;
const CARD_STROKE_WIDTH: f32 = 1.0;
pub(crate) const PANE_SPLITTER_HEIGHT: f32 = 10.0;
const COLLAPSED_PANEL_MARGIN: f32 = 4.0;
pub(crate) const COLLAPSED_SIDE_PANEL_WIDTH: f32 =
    DECODED_PANEL_ACTION_BUTTON_SIZE[0] + 2.0 * (COLLAPSED_PANEL_MARGIN + CARD_STROKE_WIDTH);

fn panel_frame(fill: Color32, margin: f32) -> egui::Frame {
    egui::Frame::none()
        .fill(fill)
        .inner_margin(egui::Margin::same(margin))
}

/// Shared workspace navigation, visually joined to the instrument below it.
pub(crate) fn workspace_tab_bar(
    context: &egui::Context,
    contents: impl FnOnce(&mut egui::Ui),
    add_menu: impl FnOnce(&mut egui::Ui),
) {
    egui::TopBottomPanel::top("waveform_tabs")
        .show_separator_line(false)
        .frame(
            egui::Frame::none()
                .fill(BACKGROUND)
                .inner_margin(egui::Margin {
                    left: 14.0,
                    right: 14.0,
                    top: 6.0,
                    bottom: 0.0,
                }),
        )
        .show(context, |ui| {
            let rect = ui.available_rect_before_wrap();
            let baseline = rect.top() + 34.0;
            ui.painter().hline(
                ui.max_rect().x_range().expand(14.0),
                baseline,
                egui::Stroke::new(1.0_f32, GRID),
            );
            ui.horizontal(|ui| {
                let scroll_width = (ui.available_width() - 40.0).max(0.0);
                egui::ScrollArea::horizontal()
                    .id_salt("workspace_tabs_scroll")
                    .max_width(scroll_width)
                    .auto_shrink([true, true])
                    .drag_to_scroll(false)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            contents(ui);
                        });
                    });
                workspace_add_menu(ui, add_menu);
            });
        });
}

pub(crate) struct WorkspaceTabResponse {
    pub response: egui::Response,
    pub close_clicked: bool,
}

/// Tab styling stays in the shell; selection and reordering belong to the app.
pub(crate) fn workspace_tab(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    close_enabled: bool,
) -> WorkspaceTabResponse {
    let label_width = ui.fonts(|fonts| {
        fonts
            .layout_no_wrap(
                label.to_owned(),
                egui::TextStyle::Button.resolve(ui.style()),
                TEXT_MUTED,
            )
            .size()
            .x
    });
    let response = ui
        .scope(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(16.0, 8.0);
            let visuals = ui.visuals_mut();
            visuals.selection.bg_fill = PANEL;
            visuals.selection.stroke = egui::Stroke::new(1.0_f32, GRID);
            visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
            visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
            visuals.widgets.hovered.expansion = 0.0;
            visuals.widgets.active.bg_stroke = egui::Stroke::NONE;
            visuals.widgets.active.expansion = 0.0;
            ui.add(
                egui::Button::new(RichText::new(label).color(if selected {
                    ACCENT
                } else {
                    TEXT_MUTED
                }))
                .selected(selected)
                .rounding(egui::Rounding {
                    nw: 7.0,
                    ne: 7.0,
                    sw: 0.0,
                    se: 0.0,
                })
                .min_size(egui::vec2(label_width + 56.0, 34.0))
                .sense(egui::Sense::click_and_drag()),
            )
        })
        .inner;
    if selected {
        let rect = response.rect;
        ui.painter().hline(
            (rect.left() + 1.0)..=(rect.right() - 1.0),
            rect.bottom(),
            egui::Stroke::new(2.0_f32, PANEL),
        );
        ui.painter().hline(
            (rect.left() + 8.0)..=(rect.right() - 8.0),
            rect.top() + 1.0,
            egui::Stroke::new(2.0_f32, ACCENT),
        );
    }
    let close_rect = Rect::from_center_size(
        egui::pos2(response.rect.right() - 15.0, response.rect.center().y),
        egui::vec2(20.0, 22.0),
    );
    let close_clicked = ui
        .push_id(label, |ui| {
            ui.add_enabled_ui(close_enabled, |ui| {
                ui.put(close_rect, egui::Button::new("×").frame(false))
                    .on_hover_text(format!("Close {label}"))
                    .clicked()
            })
            .inner
        })
        .inner;
    if !close_enabled {
        ui.interact(
            close_rect,
            response.id.with("last_tab_hint"),
            egui::Sense::hover(),
        )
        .on_hover_text("Keep at least one workspace tab open");
    }
    WorkspaceTabResponse {
        response,
        close_clicked,
    }
}

/// Keep the add-menu control consistent with the workspace strip.
fn workspace_add_menu<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) {
    ui.menu_button(RichText::new("+").size(20.0).color(ACCENT), contents)
        .response
        .on_hover_text("Add a workspace tab");
}

#[derive(Default)]
pub(crate) struct ViewToolbarResponse {
    pub settings_clicked: bool,
    pub snapshot_clicked: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum ToolbarSection {
    Files,
    Controls,
}

/// Every workspace receives the same App menu and Snapshot after its file actions.
/// The callback supplies each section; the shell owns the separating chrome.
pub(crate) fn view_toolbar(
    context: &egui::Context,
    subtitle: &str,
    snapshot_enabled: bool,
    mut contents: impl FnMut(&mut egui::Ui, ToolbarSection),
) -> ViewToolbarResponse {
    let mut response = ViewToolbarResponse::default();
    egui::TopBottomPanel::top("toolbar")
        .frame(panel_frame(PANEL, 12.0))
        .show(context, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("WAVECRAFT").strong().size(18.0).color(ACCENT));
                ui.separator();
                ui.menu_button("App", |ui| {
                    if ui.button("Settings…").clicked() {
                        response.settings_clicked = true;
                        ui.close_menu();
                    }
                });
                ui.separator();
                if !subtitle.is_empty() {
                    ui.label(RichText::new(subtitle).color(TEXT_MUTED));
                    ui.separator();
                }
                contents(ui, ToolbarSection::Files);
                ui.separator();
                response.snapshot_clicked = ui
                    .add_enabled(
                        snapshot_enabled,
                        toolbar_action_button("📷 Snapshot", TEXT_MUTED),
                    )
                    .on_hover_selectable_text("Save the visible view content as a PNG snapshot")
                    .clicked();
                ui.separator();
                contents(ui, ToolbarSection::Controls);
            });
        });
    response
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

/// A collapsible section inside a shared settings card. IDs are scoped to the
/// caller's UI, so each workspace retains its own expanded sections.
pub(crate) fn setup_section(
    ui: &mut egui::Ui,
    title: &str,
    enabled: bool,
    contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    egui::CollapsingHeader::new(RichText::new(title).strong().color(TEXT_MUTED))
        .id_salt(("setup_section", title))
        .default_open(true)
        .show_unindented(ui, |ui| {
            ui.add_space(6.0);
            ui.add_enabled_ui(enabled, contents);
        })
        .header_response
}

pub(crate) fn workspace_panel(context: &egui::Context, contents: impl FnOnce(&mut egui::Ui)) {
    egui::CentralPanel::default()
        .frame(panel_frame(BACKGROUND, 16.0))
        .show(context, contents);
}

/// Common horizontal divider for vertically stacked, resizable instruments.
/// Views own the pane sizes and apply the response's drag delta.
pub(crate) fn stacked_view_splitter(ui: &mut egui::Ui, hint: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), PANE_SPLITTER_HEIGHT),
        egui::Sense::drag(),
    );
    let response = response
        .on_hover_cursor(egui::CursorIcon::ResizeVertical)
        .on_hover_selectable_text(hint);
    let active = response.hovered() || response.dragged();
    ui.painter().line_segment(
        [
            egui::pos2(rect.left() + 8.0, rect.center().y),
            egui::pos2(rect.right() - 8.0, rect.center().y),
        ],
        egui::Stroke::new(
            if active { 2.0_f32 } else { 1.0_f32 },
            if active { ACCENT } else { GRID },
        ),
    );
    response
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
