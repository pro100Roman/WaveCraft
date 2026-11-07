//! Protocol analyzer list and the add/edit analyzer dialog.

use crate::{
    analyzer_channels, analyzer_decoder_chain, analyzer_long_name, analyzer_name, channel_label,
    choose_custom_protocol_to_open, egui, matching_protocols, Color32, ProtocolAnalyzerSettings,
    ProtocolDataFormat, RichText, SelectableHoverText, Sense, Vec2, ACCENT, LOGIC_CHANNEL_COUNT,
    PANEL, PROTOCOLS, TEXT_MUTED,
};

pub(crate) fn paint_protocol_analyzer_settings(
    ui: &mut egui::Ui,
    settings: &mut ProtocolAnalyzerSettings,
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
) {
    let heading = format!("Protocol analyzer ({})", settings.analyzers.len());
    egui::CollapsingHeader::new(RichText::new(heading).strong())
        .id_salt("logic_protocol_analyzer_section")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let add_label = if settings.picker_open {
                    "Close list"
                } else {
                    "+ Add protocol"
                };
                if ui.button(add_label).clicked() {
                    settings.picker_open = !settings.picker_open;
                }
            });

            if let Some(error) = &settings.custom_load_error {
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(RichText::new(error).color(Color32::LIGHT_RED))
                        .selectable(true),
                );
            }

            if settings.picker_open {
                ui.add_space(5.0);
                ui.add(
                    egui::TextEdit::singleline(&mut settings.search)
                        .desired_width(ui.available_width())
                        .hint_text(format!("Search {} protocols…", PROTOCOLS.len())),
                );
                let protocols = matching_protocols(&settings.search);
                ui.small(RichText::new(format!("{} matches", protocols.len())).color(TEXT_MUTED));
                let mut selected_protocol = None;
                egui::Frame::none()
                    .fill(PANEL)
                    .rounding(egui::Rounding::same(4.0))
                    .inner_margin(egui::Margin::same(4.0))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("protocol_catalog_scroll")
                            .max_height(210.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                for protocol in protocols {
                                    let response = ui
                                        .selectable_label(false, protocol.name)
                                        .on_hover_selectable_text(format!(
                                            "{}\nDecoder ID: {}",
                                            protocol.long_name, protocol.id
                                        ));
                                    if response.clicked() {
                                        selected_protocol = Some(protocol.id);
                                    }
                                }
                            });
                    });
                if let Some(protocol_id) = selected_protocol {
                    settings.begin_add(protocol_id);
                }
            }

            if settings.analyzers.is_empty() {
                ui.add_space(4.0);
                ui.small(RichText::new("No protocols configured").color(TEXT_MUTED));
                return;
            }

            let mut remove = None;
            let mut edit = None;
            let mut load_yaml = None;
            let mut clear_yaml = None;
            for (analyzer_index, analyzer) in settings.analyzers.iter().enumerate() {
                let channels = analyzer_channels(analyzer);
                let assigned_channels = analyzer
                    .channel_mappings
                    .iter()
                    .filter(|mapping| mapping.is_some())
                    .count();
                let missing_required = channels.iter().enumerate().any(|(index, channel)| {
                    channel.required
                        && analyzer
                            .channel_mappings
                            .get(index)
                            .copied()
                            .flatten()
                            .is_none()
                });
                let disabled_mapping = analyzer.channel_mappings.iter().any(|mapping| {
                    mapping
                        .map(|channel| !enabled_channels[channel])
                        .unwrap_or(false)
                });
                ui.add_space(6.0);
                egui::Frame::none()
                    .fill(PANEL)
                    .rounding(egui::Rounding::same(4.0))
                    .inner_margin(egui::Margin::same(6.0))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.small(RichText::new(analyzer_long_name(analyzer)).color(TEXT_MUTED));
                            ui.label(RichText::new(analyzer_name(analyzer)).strong());
                            ui.small(
                                RichText::new(format!(
                                    "{} · {assigned_channels}/{} inputs",
                                    analyzer.data_format.label(),
                                    channels.len()
                                ))
                                .color(TEXT_MUTED),
                            );
                            if missing_required {
                                ui.small(
                                    RichText::new("Assign every required input")
                                        .color(Color32::LIGHT_RED),
                                );
                            } else if disabled_mapping {
                                ui.small(
                                    RichText::new(
                                        "One or more mapped capture channels are disabled",
                                    )
                                    .color(Color32::YELLOW),
                                );
                            }
                            ui.add_space(3.0);
                            ui.horizontal(|ui| {
                                if ui.small_button("Edit…").clicked() {
                                    edit = Some(analyzer_index);
                                }
                                if ui
                                    .small_button("×")
                                    .on_hover_selectable_text("Remove analyzer")
                                    .clicked()
                                {
                                    remove = Some(analyzer_index);
                                }
                                ui.separator();
                                if ui.small_button("Load YAML").clicked() {
                                    load_yaml = Some(analyzer_index);
                                }
                                if ui
                                    .add_enabled(
                                        analyzer.custom_protocol.is_some(),
                                        egui::Button::new("Clear").small(),
                                    )
                                    .on_hover_selectable_text("Clear the attached YAML protocol")
                                    .clicked()
                                {
                                    clear_yaml = Some(analyzer_index);
                                }
                            });
                        })
                        .response
                        .on_hover_selectable_text(format!(
                            "Decoder chain: {}",
                            analyzer_decoder_chain(analyzer).join(" → ")
                        ));
                    });
            }
            if let Some(index) = remove {
                settings.analyzers.remove(index);
            } else if let Some(index) = edit {
                settings.begin_edit(index);
            } else if let Some(index) = load_yaml {
                settings.custom_load_error = None;
                if let Some(path) = choose_custom_protocol_to_open() {
                    match std::fs::read_to_string(&path) {
                        Ok(source) => {
                            if let Err(error) =
                                settings.begin_attach_custom_yaml(index, path.clone(), source)
                            {
                                settings.custom_load_error =
                                    Some(format!("Failed to load {}: {error}", path.display()));
                            }
                        }
                        Err(error) => {
                            settings.custom_load_error =
                                Some(format!("Failed to read {}: {error}", path.display()));
                        }
                    }
                }
            } else if let Some(index) = clear_yaml {
                settings.clear_custom_protocol(index);
            }
        });
}

pub(crate) fn paint_protocol_analyzer_modal(
    context: &egui::Context,
    settings: &mut ProtocolAnalyzerSettings,
    logic_channel_names: &[String; LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
) {
    let Some(mut editor) = settings.editor.clone() else {
        return;
    };
    let channels = analyzer_channels(&editor.analyzer);
    editor
        .analyzer
        .channel_mappings
        .resize(channels.len(), None);

    let screen_rect = context.input(|input| input.screen_rect());
    egui::Area::new(egui::Id::new("protocol_settings_modal_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen_rect.min)
        .show(context, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, Color32::from_black_alpha(112));
        });

    let mut open = true;
    let mut cancel = false;
    let mut save = false;
    let title = if editor.target_index.is_some() {
        "Protocol settings"
    } else {
        "Add protocol"
    };
    egui::Window::new(title)
        .id(egui::Id::new("protocol_settings_modal"))
        .open(&mut open)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(440.0)
        .min_width(380.0)
        .show(context, |ui| {
            ui.label(
                RichText::new(analyzer_name(&editor.analyzer))
                    .strong()
                    .color(ACCENT),
            );
            ui.label(RichText::new(analyzer_long_name(&editor.analyzer)).color(TEXT_MUTED));
            if let Some(custom) = &editor.analyzer.custom_protocol {
                ui.small(
                    RichText::new(format!("Schema: {}", custom.source_path.display()))
                        .color(TEXT_MUTED),
                );
            }
            let decoder_chain = analyzer_decoder_chain(&editor.analyzer);
            if decoder_chain.len() > 1 {
                ui.small(
                    RichText::new(format!("Decoder chain: {}", decoder_chain.join(" → ")))
                        .color(TEXT_MUTED),
                );
            }
            ui.add_space(10.0);

            let maximum_height = (screen_rect.height() * 0.62).max(220.0);
            egui::ScrollArea::vertical()
                .id_salt("protocol_settings_modal_scroll")
                .max_height(maximum_height)
                .show(ui, |ui| {
                    egui::Grid::new("protocol_settings_modal_grid")
                        .num_columns(2)
                        .min_col_width(120.0)
                        .spacing([16.0, 7.0])
                        .show(ui, |ui| {
                            ui.label("Output format");
                            egui::ComboBox::from_id_salt("protocol_modal_format")
                                .width(210.0)
                                .selected_text(editor.analyzer.data_format.label())
                                .show_ui(ui, |ui| {
                                    for format in ProtocolDataFormat::ALL {
                                        ui.selectable_value(
                                            &mut editor.analyzer.data_format,
                                            format,
                                            format.label(),
                                        );
                                    }
                                });
                            ui.end_row();

                            for (channel_index, channel) in channels.iter().enumerate() {
                                let label = if channel.required {
                                    format!("{} *", channel.name)
                                } else {
                                    channel.name.to_owned()
                                };
                                ui.label(label)
                                    .on_hover_selectable_text(if channel.required {
                                        "Required decoder input"
                                    } else {
                                        "Optional decoder input"
                                    });
                                let selected_text = editor.analyzer.channel_mappings[channel_index]
                                    .map(|logic_channel| {
                                        channel_label(
                                            "D",
                                            logic_channel,
                                            &logic_channel_names[logic_channel],
                                        )
                                    })
                                    .unwrap_or_else(|| "Not assigned".to_owned());
                                egui::ComboBox::from_id_salt((
                                    "protocol_modal_channel",
                                    channel_index,
                                ))
                                .width(210.0)
                                .selected_text(selected_text)
                                .show_ui(ui, |ui| {
                                    if !channel.required {
                                        ui.selectable_value(
                                            &mut editor.analyzer.channel_mappings[channel_index],
                                            None,
                                            "Not assigned",
                                        );
                                    }
                                    for (logic_channel, channel_name) in
                                        logic_channel_names.iter().enumerate()
                                    {
                                        ui.selectable_value(
                                            &mut editor.analyzer.channel_mappings[channel_index],
                                            Some(logic_channel),
                                            channel_label("D", logic_channel, channel_name),
                                        );
                                    }
                                });
                                ui.end_row();
                            }
                        });
                });

            let missing_required = channels.iter().enumerate().any(|(index, channel)| {
                channel.required && editor.analyzer.channel_mappings[index].is_none()
            });
            let disabled_mapping = editor.analyzer.channel_mappings.iter().any(|mapping| {
                mapping
                    .map(|channel| !enabled_channels[channel])
                    .unwrap_or(false)
            });
            ui.add_space(8.0);
            if missing_required {
                ui.label(
                    RichText::new("Assign every required input before saving")
                        .color(Color32::LIGHT_RED),
                );
            } else if disabled_mapping {
                ui.label(
                    RichText::new("One or more mapped capture channels are disabled")
                        .color(Color32::YELLOW),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui
                    .add_enabled(!missing_required, egui::Button::new("Save"))
                    .clicked()
                {
                    save = true;
                }
            });
        });

    if save {
        settings.save_editor(editor);
    } else if cancel || !open {
        settings.cancel_editor();
    } else {
        settings.editor = Some(editor);
    }
}
