//! Shared keyboard-entry baud rate field with a filtered standard-speed popup.

use crate::{egui, RichText, SelectableHoverText, TEXT_MUTED};

pub(crate) const STANDARD_BAUD_RATES: &[u32] = &[
    110, 300, 600, 1_200, 2_400, 4_800, 9_600, 14_400, 19_200, 28_800, 38_400, 57_600, 76_800,
    115_200, 128_000, 153_600, 230_400, 256_000, 460_800, 500_000, 576_000, 921_600, 1_000_000,
    1_152_000, 1_500_000, 2_000_000, 2_500_000, 3_000_000,
];

pub(crate) fn paint_baud_rate_selector(
    ui: &mut egui::Ui,
    baud: &mut u32,
    draft: &mut Option<String>,
    rates: &[u32],
    allow_custom: bool,
) {
    let draft = draft.get_or_insert_with(|| baud.to_string());
    let popup_id = ui.make_persistent_id("baud_rate_popup");
    let filter_id = popup_id.with("show_all");
    let controls = ui.horizontal(|ui| {
        let button_width = 24.0;
        let text_width =
            (ui.available_width() - button_width - ui.spacing().item_spacing.x).max(button_width);
        let text = ui
            .add_sized(
                [text_width, 20.0],
                egui::TextEdit::singleline(draft)
                    .hint_text("Type or select a baud rate")
                    .char_limit(8),
            )
            .on_hover_text("Type a baud rate to filter the available speeds");
        let dropdown = ui
            .add_sized([button_width, 20.0], egui::Button::new("▾"))
            .on_hover_selectable_text("Show the available standard baud rates");
        (text, dropdown)
    });
    let (text_response, dropdown_response) = controls.inner;
    let anchor_response = text_response.clone().union(dropdown_response.clone());
    if dropdown_response.clicked() {
        ui.data_mut(|data| data.insert_temp(filter_id, true));
        ui.memory_mut(|memory| memory.toggle_popup(popup_id));
    } else if text_response.clicked() || text_response.gained_focus() || text_response.changed() {
        ui.data_mut(|data| data.insert_temp(filter_id, false));
        ui.memory_mut(|memory| memory.open_popup(popup_id));
    }
    let valid_baud = draft
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|baud| (1..=12_000_000).contains(baud))
        .filter(|baud| allow_custom || rates.contains(baud));
    if let Some(value) = valid_baud {
        *baud = value;
    }
    if (text_response.has_focus() || text_response.lost_focus())
        && ui.input(|input| input.key_pressed(egui::Key::Enter))
        && valid_baud.is_some()
    {
        text_response.surrender_focus();
        ui.memory_mut(egui::Memory::close_popup);
    }
    egui::popup::popup_below_widget(
        ui,
        popup_id,
        &anchor_response,
        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
        |popup_ui| {
            popup_ui.set_min_width(anchor_response.rect.width());
            popup_ui.set_max_width(anchor_response.rect.width());
            popup_ui.label(
                RichText::new("Standard baud rates")
                    .small()
                    .color(TEXT_MUTED),
            );
            popup_ui.separator();
            let show_all = popup_ui.data(|data| data.get_temp::<bool>(filter_id).unwrap_or(false));
            let query = draft.trim();
            let mut selected = None;
            egui::ScrollArea::vertical()
                .id_salt("baud_rate_results")
                .max_height(240.0)
                .auto_shrink([false, false])
                .show(popup_ui, |ui| {
                    let mut matches = 0;
                    for rate in rates
                        .iter()
                        .filter(|rate| show_all || rate.to_string().contains(query))
                    {
                        matches += 1;
                        if ui
                            .selectable_label(valid_baud == Some(*rate), rate.to_string())
                            .clicked()
                        {
                            selected = Some(*rate);
                        }
                    }
                    if matches == 0 {
                        ui.label(
                            RichText::new(if allow_custom {
                                "No standard match · use the typed value"
                            } else {
                                "No matching supported baud rate"
                            })
                            .small()
                            .color(TEXT_MUTED),
                        );
                    }
                });
            if let Some(rate) = selected {
                *baud = rate;
                *draft = rate.to_string();
                text_response.surrender_focus();
                popup_ui.memory_mut(egui::Memory::close_popup);
            }
        },
    );
    if valid_baud.is_none() {
        ui.colored_label(
            egui::Color32::LIGHT_RED,
            if allow_custom {
                "Enter a baud rate from 1 to 12000000"
            } else {
                "Enter a supported UART baud rate"
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_entry_filters_speeds_and_dropdown_button_shows_all_choices() {
        let context = egui::Context::default();
        crate::configure_style(&context);
        let mut baud = 115200;
        let mut draft = None;
        let mut paint = |events| {
            context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        ui.set_width(super::super::view_shell::CONNECTION_FIELD_WIDTH);
                        paint_baud_rate_selector(
                            ui,
                            &mut baud,
                            &mut draft,
                            STANDARD_BAUD_RATES,
                            true,
                        );
                    });
                },
            )
        };
        let rect = |output: &egui::FullOutput, label: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => {
                        Some(text.visual_bounding_rect())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{label} is visible"))
        };
        let pointer = |pos, pressed| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        let select_all = egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        };
        let output = paint(vec![]);
        let field = rect(&output, "115200");
        let dropdown = rect(&output, "▾");
        assert!(dropdown.left() > field.right());
        assert!(dropdown.top() < field.bottom() && field.top() < dropdown.bottom());
        paint(pointer(field.center(), true));
        paint(pointer(field.center(), false));
        let output = paint(vec![select_all.clone(), egui::Event::Text("576".into())]);
        rect(&output, "57600");
        assert!(!output.shapes.iter().any(
            |shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "9600")
        ));
        paint(vec![
            select_all,
            egui::Event::Text("123456".into()),
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        assert!(!context.memory(|memory| memory.any_popup_open()));
        paint(pointer(dropdown.center(), true));
        paint(pointer(dropdown.center(), false));
        let output = paint(vec![]);
        let choice = rect(&output, "9600").center();
        paint(pointer(choice, true));
        paint(pointer(choice, false));
        assert_eq!(baud, 9600);
        assert_eq!(draft.as_deref(), Some("9600"));
    }
}
