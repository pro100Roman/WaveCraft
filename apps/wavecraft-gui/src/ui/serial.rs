//! Serial / UART workspace, matching the instrument toolbar, setup cards and split panes.

use super::baud_rate::{paint_baud_rate_selector, STANDARD_BAUD_RATES};
use super::serial_graph::paint_graph;
use super::serial_line_style::{line_style_picker, LINE_STYLE_PICKER_WIDTH};
use super::view_shell::{
    settings_card, setup_field_row, setup_section, view_card, view_toolbar, workspace_panel,
    ToolbarSection, CONNECTION_FIELD_WIDTH,
};
use crate::serial::{
    SerialMode, SerialSettings, SerialState, SerialTimestampUnit, TextField, COLORS,
};
use crate::{
    egui, toolbar_action_button, RichText, SelectableHoverText, WaveCraftApp, WorkspaceView,
    ACCENT, GRID, TEXT_MUTED,
};
use wavecraft_serial::BinarySchema;

impl WaveCraftApp {
    pub(crate) fn serial_view_is_active(&self) -> bool {
        self.active_workspace_view
            .and_then(WorkspaceView::serial_index)
            .is_some()
    }

    pub(crate) fn active_serial_index(&self) -> usize {
        self.active_workspace_view
            .and_then(WorkspaceView::serial_index)
            .unwrap_or(0)
    }

    pub(crate) fn select_serial_view(&mut self, view: WorkspaceView) {
        let index = view.serial_index().unwrap_or(0);
        if let Some(active) = self.active_waveform_tab {
            self.save_waveform_view_settings(active);
        }
        self.active_workspace_view = Some(view);
        self.snapshot_plot_rect = None;
        if self.serial[index].ports.is_empty() {
            self.serial[index].refresh();
        }
        self.save_global_settings();
    }

    pub(crate) fn paint_serial_toolbar(
        &mut self,
        context: &egui::Context,
        actions: &mut crate::FrameActions,
    ) {
        let index = self.active_serial_index();
        let running = self.serial[index].session.is_some();
        let toolbar = view_toolbar(
            context,
            "Serial / UART",
            self.pending_snapshot.is_none(),
            |ui, section| match section {
                ToolbarSection::Files => {
                    if ui
                        .add_enabled(!running, toolbar_action_button("↻ Refresh", TEXT_MUTED))
                        .clicked()
                    {
                        self.serial[index].refresh();
                    }
                    if ui.add(toolbar_action_button("Clear", TEXT_MUTED)).clicked() {
                        self.serial[index].clear_data();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(!running, toolbar_action_button("📂 Open…", TEXT_MUTED))
                        .on_hover_text("Open raw serial bytes or a CSV file")
                        .clicked()
                    {
                        self.open_serial_raw(index);
                    }
                    if ui
                        .add_enabled(
                            !self.serial[index].raw.is_empty(),
                            toolbar_action_button("Save raw…", TEXT_MUTED),
                        )
                        .on_hover_text("Save the retained raw bytes (latest 64 KiB)")
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("serial-capture.bin")
                            .save_file()
                        {
                            let bytes: Vec<_> = self.serial[index].raw.iter().copied().collect();
                            if let Err(error) = std::fs::write(path, bytes) {
                                self.serial[index].error = Some(error.to_string());
                            }
                        }
                    }
                }
                ToolbarSection::Controls => {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.serial[index].imported_file.is_some()
                            && ui
                                .add_enabled(!running, toolbar_action_button("Plot", ACCENT))
                                .on_hover_text("Plot the opened file using the current fields, columns and X-axis settings")
                                .clicked()
                        {
                            if let Err(error) = self.serial[index].plot_imported() {
                                self.serial[index].error = Some(error.to_string());
                            }
                        }
                    });
                }
            },
        );
        self.settings_open |= toolbar.settings_clicked;
        actions.snapshot_clicked |= toolbar.snapshot_clicked;
    }

    pub(crate) fn connected_serial_port_owner(
        &self,
        port: &str,
        excluding: WorkspaceView,
    ) -> Option<WorkspaceView> {
        WorkspaceView::SERIAL
            .into_iter()
            .zip(&self.serial)
            .filter(|(_, state)| state.session.is_some())
            .map(|(view, state)| (view, state.settings.port.as_str()))
            .chain(
                WorkspaceView::PD_POWER
                    .into_iter()
                    .zip(&self.pd_power)
                    .filter(|(_, state)| state.session.is_some())
                    .map(|(view, state)| (view, state.settings.port.as_str())),
            )
            .find(|(view, candidate)| {
                *view != excluding && crate::pd_power::same_port(candidate, port)
            })
            .map(|(view, _)| view)
    }

    pub(crate) fn apply_serial_actions(&mut self, actions: &crate::FrameActions) {
        if let Some(index) = actions.serial_toggle_connection {
            let busy_view = self.serial.get(index).and_then(|state| {
                self.connected_serial_port_owner(&state.settings.port, WorkspaceView::SERIAL[index])
            });
            if let Some(state) = self.serial.get_mut(index) {
                if state.session.is_some() {
                    state.stop();
                } else if let Some(view) = busy_view {
                    state.error = Some(format!(
                        "This port is already connected in {}",
                        view.label()
                    ));
                } else if let Err(error) = state.start() {
                    state.error = Some(error.to_string());
                }
            }
        }
    }

    fn open_serial_raw(&mut self, index: usize) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open serial data or CSV")
            .add_filter("Serial data and CSV", &["bin", "txt", "csv", "raw"])
            .add_filter("All files", &["*"])
            .pick_file()
        else {
            return;
        };
        if let Err(error) = self.serial[index].import_raw(&path) {
            self.serial[index].error = Some(error.to_string());
        } else {
            self.save_global_settings();
        }
    }

    pub(crate) fn paint_serial_setup(
        &mut self,
        context: &egui::Context,
        actions: &mut crate::FrameActions,
    ) {
        let index = self.active_serial_index();
        let before = self.serial[index].settings.clone();
        self.show_setup_panel(
            context,
            WorkspaceView::SERIAL[index],
            actions,
            |app, ui, actions| {
                let running = app.serial[index].session.is_some();
                if running {
                    ui.label(
                        RichText::new("Close the port to change connection or decoding settings")
                            .small()
                            .color(TEXT_MUTED),
                    );
                } else {
                    ui.label(
                        RichText::new(
                            "Open the port or plot an imported file to apply decoding changes.",
                        )
                        .small()
                        .color(TEXT_MUTED),
                    );
                }
                settings_card(ui, |ui| {
                    setup_section(ui, "CONNECTION", true, |ui| {
                        port_settings(ui, &mut app.serial[index], index, actions)
                    });
                    ui.add_space(10.0);
                    setup_section(ui, "DATA FORMAT", !running, |ui| {
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut app.serial[index].settings.mode,
                                SerialMode::Text,
                                "Text / ASCII",
                            );
                            ui.selectable_value(
                                &mut app.serial[index].settings.mode,
                                SerialMode::Binary,
                                "Binary",
                            );
                            ui.selectable_value(
                                &mut app.serial[index].settings.mode,
                                SerialMode::Csv,
                                "CSV",
                            );
                        });
                        if app.serial[index].settings.mode == SerialMode::Text {
                            text_settings(ui, &mut app.serial[index].settings, None);
                        } else if app.serial[index].settings.mode == SerialMode::Csv {
                            csv_settings(ui, &mut app.serial[index]);
                        } else {
                            binary_settings(ui, &mut app.serial[index]);
                        }
                    });
                    ui.add_space(10.0);
                    setup_section(ui, "X AXIS", !running, |ui| {
                        x_axis_settings(ui, &mut app.serial[index].settings)
                    });
                });
                ui.add_space(10.0);
                ui.label(
                    RichText::new(
                        "No fields or schema? Raw data is still received below the graph.",
                    )
                    .small()
                    .color(TEXT_MUTED),
                );
                ui.label(
                    RichText::new("History: latest 64 KiB raw bytes and 10,000 points per field.")
                        .small()
                        .color(TEXT_MUTED),
                );
                true
            },
        );

        if before != self.serial[index].settings {
            self.apply_serial_axis_changes(&before);
            for series in &mut self.serial[index].series {
                let color = if self.serial[index].settings.mode != SerialMode::Binary {
                    self.serial[index]
                        .settings
                        .fields
                        .iter()
                        .find(|field| field.name.trim() == series.name)
                        .map(|field| field.color)
                } else {
                    self.serial[index]
                        .settings
                        .binary_colors
                        .get(&series.name)
                        .copied()
                };
                if let Some(color) = color {
                    series.color = color;
                }
            }
            // Keep a stopped capture intact until a new port/file open, but rebuild the raw representation on mode changes.
            self.serial[index].invalidate_raw();
            self.save_global_settings();
        }
    }

    fn apply_serial_axis_changes(&mut self, before: &SerialSettings) {
        let index = self.active_serial_index();
        let settings = &self.serial[index].settings;
        let axis_changed = before.timestamp_field != settings.timestamp_field
            || before.timestamp_unit != settings.timestamp_unit
            || before.timestamp_multiplier != settings.timestamp_multiplier
            || before.sample_rate != settings.sample_rate;
        if axis_changed && self.serial[index].imported_file.is_some() {
            if let Err(error) = self.serial[index].rebuild_imported_axis() {
                self.serial[index].error = Some(error.to_string());
            }
        }
    }

    pub(crate) fn paint_serial_central(&mut self, context: &egui::Context) {
        let index = self.active_serial_index();
        let mut layout_changed = false;
        workspace_panel(context, |ui| {
            ui.push_id(("serial_workspace", index), |ui| {
                let state = &mut self.serial[index];
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("SERIAL / UART").strong().color(ACCENT));
                    ui.separator();
                    ui.label(format!("{} bytes", state.received_bytes));
                    ui.label(format!("{} records", state.decoded_records));
                    if state.rejected_frames > 0 {
                        ui.colored_label(
                            egui::Color32::LIGHT_RED,
                            format!("{} rejected", state.rejected_frames),
                        );
                    }
                    if state.invalid_x_records > 0 {
                        ui.label(format!("{} invalid X values", state.invalid_x_records))
                            .on_hover_text("Missing, nonfinite or decreasing timestamps are excluded from plotting. Raw bytes are preserved.");
                    }
                    if state.discarded_bytes > 0 {
                        ui.label(format!("{} unsynced bytes", state.discarded_bytes));
                    }
                });
                ui.add_space(6.0);
                let total = (ui.available_height() - 10.0 - 2.0 * ui.spacing().item_spacing.y).max(0.0);
                let min_height = 60.0_f32.min(total / 2.0);
                let graph_height = (total * f32::from(state.settings.graph_percent) / 100.0)
                    .clamp(min_height, (total - min_height).max(min_height));
                let width = ui.available_width();
                let before = state.settings.graph_percent;
                ui.allocate_ui_with_layout(
                    egui::vec2(width, graph_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_clip_rect(ui.max_rect());
                        paint_graph(ui, state);
                    },
                );
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(width, 10.0), egui::Sense::drag());
                let response = response
                    .on_hover_cursor(egui::CursorIcon::ResizeVertical)
                    .on_hover_text("Drag to resize Graph and Raw data");
                ui.painter().line_segment(
                    [
                        egui::pos2(rect.left() + 8.0, rect.center().y),
                        egui::pos2(rect.right() - 8.0, rect.center().y),
                    ],
                    egui::Stroke::new(
                        if response.hovered() || response.dragged() {
                            2.0
                        } else {
                            1.0_f32
                        },
                        if response.hovered() || response.dragged() {
                            ACCENT
                        } else {
                            GRID
                        },
                    ),
                );
                if response.drag_started() {
                    state.split_drag_origin = Some(graph_height);
                }
                if response.dragged() && total > 0.0 {
                    let height = (state.split_drag_origin.unwrap_or(graph_height)
                        + response.drag_delta().y)
                        .clamp(min_height, (total - min_height).max(min_height));
                    state.split_drag_origin = Some(height);
                    state.settings.graph_percent =
                        (height / total * 100.0).round().clamp(10.0, 90.0) as u8;
                }
                if response.double_clicked() {
                    state.settings.graph_percent = 65;
                }
                if response.drag_stopped()
                    || before != state.settings.graph_percent && !response.dragged()
                {
                    state.split_drag_origin = None;
                    layout_changed = true;
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(width, (total - graph_height).max(0.0)),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_clip_rect(ui.max_rect());
                        paint_raw(ui, state);
                    },
                );
            });
        });
        if layout_changed {
            self.save_global_settings();
        }
    }
}

fn port_settings(
    ui: &mut egui::Ui,
    state: &mut SerialState,
    index: usize,
    actions: &mut crate::FrameActions,
) {
    let running = state.session.is_some();
    ui.spacing_mut().interact_size.x = CONNECTION_FIELD_WIDTH;
    ui.add_enabled_ui(!running, |ui| {
        setup_field_row(
            ui,
            "Port",
            "",
            "Serial port used for live UART capture",
            |ui| {
                egui::ComboBox::from_id_salt("serial_ports")
                    .width(CONNECTION_FIELD_WIDTH)
                    .truncate()
                    .selected_text(if state.settings.port.is_empty() {
                        "Select port"
                    } else {
                        &state.settings.port
                    })
                    .show_ui(ui, |ui| {
                        if state.ports.is_empty() {
                            ui.add_enabled(false, egui::Label::new("No serial ports found"));
                        }
                        for port in &state.ports {
                            ui.selectable_value(&mut state.settings.port, port.clone(), port);
                        }
                    });
            },
        );
    });
    setup_field_row(ui, "", "", "", |ui| {
        if ui.add_enabled(
            !state.stopping && (running || (state.settings.mode != SerialMode::Csv && !state.settings.port.trim().is_empty())),
            toolbar_action_button(if running { "Close" } else { "Open" }, if running { egui::Color32::LIGHT_RED } else { ACCENT })
                .min_size(egui::vec2(CONNECTION_FIELD_WIDTH, 22.0)),
        )
        .on_hover_selectable_text("Open or close the selected serial port. Live UART capture is available in ASCII or Binary mode.")
        .on_disabled_hover_selectable_text("Select a port and ASCII or Binary mode to open a live UART connection.")
        .clicked() {
            actions.serial_toggle_connection = Some(index);
        }
    });
    ui.add_enabled_ui(!running, |ui| {
        let settings = &mut state.settings;
        setup_field_row(
            ui,
            "Baud rate",
            "",
            "Type a baud rate or select a standard speed",
            |ui| {
                paint_baud_rate_selector(
                    ui,
                    &mut settings.baud,
                    &mut state.baud_draft,
                    STANDARD_BAUD_RATES,
                    true,
                );
            },
        );
        setup_field_row(ui, "Data bits", "", "", |ui| {
            egui::ComboBox::from_id_salt("serial_data_bits")
                .width(CONNECTION_FIELD_WIDTH)
                .selected_text(settings.data_bits.to_string())
                .show_ui(ui, |ui| {
                    for bits in 5..=8 {
                        ui.selectable_value(&mut settings.data_bits, bits, bits.to_string());
                    }
                });
        });
        setup_field_row(ui, "Parity", "", "", |ui| {
            string_combo(
                ui,
                "serial_parity",
                &mut settings.parity,
                &["None", "Even", "Odd"],
            );
        });
        setup_field_row(ui, "Stop bits", "", "", |ui| {
            egui::ComboBox::from_id_salt("serial_stop_bits")
                .width(CONNECTION_FIELD_WIDTH)
                .selected_text(settings.stop_bits.to_string())
                .show_ui(ui, |ui| {
                    for bits in 1..=2 {
                        ui.selectable_value(&mut settings.stop_bits, bits, bits.to_string());
                    }
                });
        });
        setup_field_row(ui, "Flow control", "", "", |ui| {
            string_combo(
                ui,
                "serial_flow",
                &mut settings.flow_control,
                &["None", "RTS/CTS", "XON/XOFF"],
            );
        });
    });
}

fn string_combo(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[&str]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(value.as_str())
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for option in options {
                ui.selectable_value(value, (*option).to_owned(), *option);
            }
        });
}

fn text_settings(ui: &mut egui::Ui, settings: &mut SerialSettings, csv_columns: Option<&[String]>) {
    ui.add_space(8.0);
    ui.label(RichText::new("PLOTTED FIELDS").small().color(TEXT_MUTED));
    ui.label(
        RichText::new(if csv_columns.is_some() { "Select CSV columns to plot. Choose a timestamp explicitly in X AXIS." } else { "Set a separator after each name, or leave it empty for automatic matching. Records end at CR or LF." })
            .small()
            .color(TEXT_MUTED),
    );
    let mut remove = None;
    for (index, field) in settings.fields.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.horizontal(|ui| {
                ui.color_edit_button_srgb(&mut field.color);
                let name_width = (ui.available_width()
                    - LINE_STYLE_PICKER_WIDTH
                    - 24.0
                    - if csv_columns.is_some() { 0.0 } else { 40.0 }
                    - ui.spacing().item_spacing.x * 3.0)
                    .max(32.0);
                if let Some(columns) = csv_columns {
                    csv_column_picker(ui, &mut field.name, columns, name_width);
                } else {
                    ascii_field_inputs(ui, field, name_width);
                }
                line_style_picker(ui, &mut field.line_style, field.color);
                if ui.small_button("×").on_hover_text("Remove field").clicked() {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        settings.fields.remove(index);
    }
    if ui
        .add_enabled(
            settings.fields.len() < 10
                && csv_columns.map_or(true, |columns| {
                    columns
                        .iter()
                        .any(|column| !settings.fields.iter().any(|field| &field.name == column))
                }),
            egui::Button::new(format!("+ Add field ({}/10)", settings.fields.len())),
        )
        .clicked()
    {
        settings.fields.push(TextField {
            separator: String::new(),
            name: csv_columns
                .and_then(|columns| {
                    columns
                        .iter()
                        .find(|column| !settings.fields.iter().any(|field| &field.name == *column))
                })
                .cloned()
                .unwrap_or_default(),
            color: COLORS[settings.fields.len() % COLORS.len()],
            line_style: Default::default(),
        });
    }
}

fn csv_column_picker(
    ui: &mut egui::Ui,
    selected: &mut String,
    columns: &[String],
    width: f32,
) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 20.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            let preview = selected.replace(['\r', '\n'], " ");
            egui::ComboBox::from_id_salt("csv_column")
                .width(width)
                .truncate()
                .selected_text(preview)
                .show_ui(ui, |ui| {
                    for column in columns {
                        ui.selectable_value(selected, column.clone(), column);
                    }
                })
                .response
                .on_hover_text(selected.as_str())
        },
    )
    .inner
}

fn ascii_field_inputs(ui: &mut egui::Ui, field: &mut TextField, name_width: f32) {
    ui.add_sized(
        [name_width, 20.0],
        egui::TextEdit::singleline(&mut field.name)
            .char_limit(64)
            .hint_text("e.g. temperature"),
    );
    let separator = ui.add_sized(
        [40.0, 20.0],
        egui::TextEdit::singleline(&mut field.separator)
            .char_limit(wavecraft_serial::MAX_TEXT_SEPARATOR_BYTES)
            .font(egui::TextStyle::Monospace)
            .hint_text("auto"),
    );
    if separator.changed() {
        field.separator.retain(|character| character.is_ascii());
    }
    separator.on_hover_text("Field/value separator: up to 3 ASCII characters, matched literally after the name. Empty uses automatic matching (=, :, whitespace or JSON). Spaces are significant.");
}

fn csv_settings(ui: &mut egui::Ui, state: &mut SerialState) {
    ui.label("Column separator");
    egui::ComboBox::from_id_salt("serial_csv_delimiter")
        .selected_text(match state.settings.csv_delimiter {
            wavecraft_serial::CsvDelimiter::Auto => "Auto",
            wavecraft_serial::CsvDelimiter::Comma => "Comma (,)",
            wavecraft_serial::CsvDelimiter::Semicolon => "Semicolon (;)",
            wavecraft_serial::CsvDelimiter::Tab => "Tab",
        })
        .show_ui(ui, |ui| {
            for (value, label) in [
                (wavecraft_serial::CsvDelimiter::Auto, "Auto"),
                (wavecraft_serial::CsvDelimiter::Comma, "Comma (,)"),
                (wavecraft_serial::CsvDelimiter::Semicolon, "Semicolon (;)"),
                (wavecraft_serial::CsvDelimiter::Tab, "Tab"),
            ] {
                ui.selectable_value(&mut state.settings.csv_delimiter, value, label);
            }
        });
    ui.checkbox(
        &mut state.settings.csv_has_headers,
        "First row contains column names",
    );
    text_settings(ui, &mut state.settings, Some(&state.csv_columns));
}

fn x_axis_settings(ui: &mut egui::Ui, settings: &mut SerialSettings) {
    let names: Vec<String> = match settings.mode {
        SerialMode::Text | SerialMode::Csv => settings
            .fields
            .iter()
            .map(|field| field.name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect(),
        SerialMode::Binary => BinarySchema::from_json(&settings.schema_json)
            .map(|schema| schema.channel_names())
            .unwrap_or_default(),
    };
    ui.label("Timestamp field");
    egui::ComboBox::from_id_salt("serial_timestamp_field")
        .width(ui.available_width())
        .selected_text(if settings.timestamp_field.is_empty() {
            "None"
        } else {
            &settings.timestamp_field
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut settings.timestamp_field, String::new(), "None");
            for name in names {
                ui.selectable_value(&mut settings.timestamp_field, name.clone(), name);
            }
        });
    if !settings.timestamp_field.is_empty() {
        ui.horizontal(|ui| {
            ui.label("Unit");
            egui::ComboBox::from_id_salt("serial_timestamp_unit")
                .selected_text(settings.timestamp_unit.label())
                .show_ui(ui, |ui| {
                    for unit in SerialTimestampUnit::ALL {
                        ui.selectable_value(&mut settings.timestamp_unit, unit, unit.label());
                    }
                });
        });
        ui.label("Timestamp multiplier");
        ui.add(
            egui::TextEdit::singleline(&mut settings.timestamp_multiplier)
                .desired_width(ui.available_width())
                .hint_text("1"),
        )
        .on_hover_text(
            "Time = timestamp × multiplier × unit. For ticks of 10 ms, choose ms and enter 10.",
        );
    }
    ui.label("Sample rate (Hz)");
    ui.add_enabled(settings.timestamp_field.is_empty(), egui::TextEdit::singleline(&mut settings.sample_rate)
        .desired_width(ui.available_width()).hint_text("Optional — sample number if empty"))
        .on_hover_text("Shared by ASCII and Binary. Without a timestamp, time = sample number / sample rate. A selected timestamp takes precedence.");
    ui.label(
        RichText::new(if settings.timestamp_field.is_empty() {
            "No timestamp: sample numbers from 0, or time from sample rate."
        } else {
            "The selected field supplies X; other fields supply Y."
        })
        .small()
        .color(TEXT_MUTED),
    );
}

fn binary_settings(ui: &mut egui::Ui, state: &mut SerialState) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Load JSON…").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Serial schema", &["json"])
                .pick_file()
            {
                let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                    use std::io::Read;
                    let mut json = String::new();
                    std::fs::File::open(&path)?
                        .take(1_048_577)
                        .read_to_string(&mut json)?;
                    state.load_schema(
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        json,
                    )?;
                    Ok(())
                })();
                state.error = result.err().map(|error| error.to_string());
            }
        }
        if ui
            .add_enabled(
                !state.settings.schema_json.is_empty(),
                egui::Button::new("Remove"),
            )
            .clicked()
        {
            state.settings.schema_json.clear();
            state.settings.schema_name.clear();
            state.settings.binary_colors.clear();
            state.settings.binary_line_styles.clear();
        }
    });
    if state.settings.schema_json.is_empty() {
        ui.label(RichText::new("No schema · raw bytes only").color(TEXT_MUTED));
    } else {
        ui.add(egui::Label::new(&state.settings.schema_name).truncate());
    }
    if ui.small_button("Save example JSON…").clicked() {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("serial-schema.json")
            .save_file()
        {
            if let Err(error) = std::fs::write(
                path,
                include_str!("../../../../docs/examples/serial-schema.json"),
            ) {
                state.error = Some(error.to_string());
            }
        }
    }
    let settings = &mut state.settings;
    if let Ok(schema) = BinarySchema::from_json(&settings.schema_json) {
        ui.label(
            RichText::new(format!(
                "Start: {} B · Stop: {} B · {:?} endian",
                schema.start_word_bytes, schema.stop_word_bytes, schema.byte_order
            ))
            .small()
            .color(TEXT_MUTED),
        );
        paint_binary_plotted_fields(ui, settings, &schema);
    }
    ui.add_enabled_ui(!settings.schema_json.is_empty(), |ui| {
        hex_field(ui, "Start word (hex)", &mut settings.start_word);
        hex_field(ui, "Stop word (hex)", &mut settings.stop_word);
        ui.add_space(6.0);
        ui.label(
            RichText::new("CRC · BEFORE STOP WORD")
                .small()
                .color(TEXT_MUTED),
        );
        egui::ComboBox::from_id_salt("serial_crc_width")
            .selected_text(format!("CRC-{}", settings.crc_width))
            .show_ui(ui, |ui| {
                for width in [8, 16, 32] {
                    ui.selectable_value(&mut settings.crc_width, width, format!("CRC-{width}"));
                }
            });
        egui::ComboBox::from_id_salt("serial_crc_preset")
            .selected_text("Polynomial preset…")
            .show_ui(ui, |ui| {
                for (name, width, poly, seed, xor, reflect, big) in [
                    ("CRC-8 / SMBUS", 8, "07", "00", "00", false, false),
                    ("CRC-8 / MAXIM", 8, "31", "00", "00", true, false),
                    ("CRC-16 / IBM-3740", 16, "1021", "FFFF", "0000", false, true),
                    ("CRC-16 / MODBUS", 16, "8005", "FFFF", "0000", true, false),
                    (
                        "CRC-32 / ISO-HDLC",
                        32,
                        "04C11DB7",
                        "FFFFFFFF",
                        "FFFFFFFF",
                        true,
                        false,
                    ),
                ] {
                    if ui.button(name).clicked() {
                        settings.crc_width = width;
                        settings.polynomial = poly.into();
                        settings.seed = seed.into();
                        settings.xor_output = xor.into();
                        settings.reflect_input = reflect;
                        settings.reflect_output = reflect;
                        settings.crc_big_endian = big;
                        ui.close_menu();
                    }
                }
            });
        hex_field(ui, "Polynomial (normal hex)", &mut settings.polynomial);
        hex_field(ui, "Seed / init (hex)", &mut settings.seed);
        hex_field(ui, "Final XOR (hex)", &mut settings.xor_output);
        ui.checkbox(&mut settings.reflect_input, "Reflect input bits");
        ui.checkbox(&mut settings.reflect_output, "Reflect output bits");
        ui.checkbox(&mut settings.crc_big_endian, "CRC bytes: big endian");
        ui.checkbox(&mut settings.crc_include_start, "Include start word in CRC");
        ui.label(
            RichText::new("CRC covers the payload, optionally including the start word.")
                .small()
                .color(TEXT_MUTED),
        );
    });
    if !settings.schema_json.is_empty() {
        if let Err(error) = settings.binary_decoder() {
            ui.colored_label(egui::Color32::LIGHT_RED, error.to_string());
        }
    }
}

/// Keep the JSON field hierarchy visible: bit fields are grouped under their
/// parent, while each decoded channel owns its color and stroke settings.
fn paint_binary_plotted_fields(
    ui: &mut egui::Ui,
    settings: &mut SerialSettings,
    schema: &BinarySchema,
) {
    ui.add_space(8.0);
    ui.label(RichText::new("PLOTTED FIELDS").small().color(TEXT_MUTED));
    for (index, name) in schema.channel_names().into_iter().enumerate() {
        settings
            .binary_colors
            .entry(name)
            .or_insert(COLORS[index % COLORS.len()]);
    }
    for field in &schema.fields {
        if field.bits.is_empty() {
            binary_plotted_field(ui, settings, &field.name);
        } else {
            ui.label(RichText::new(&field.name).strong().color(TEXT_MUTED));
            ui.indent((&field.name, "binary_bit_fields"), |ui| {
                for bit in &field.bits {
                    binary_plotted_field(ui, settings, &bit.name);
                }
            });
        }
    }
}

fn binary_plotted_field(ui: &mut egui::Ui, settings: &mut SerialSettings, name: &str) {
    ui.push_id(("binary_plot_field", name), |ui| {
        ui.horizontal(|ui| {
            let color = settings
                .binary_colors
                .entry(name.to_owned())
                .or_insert(COLORS[0]);
            ui.color_edit_button_srgb(color);
            let name_width =
                (ui.available_width() - LINE_STYLE_PICKER_WIDTH - ui.spacing().item_spacing.x)
                    .max(32.0);
            ui.add_sized([name_width, 20.0], egui::Label::new(name).truncate());
            line_style_picker(
                ui,
                settings
                    .binary_line_styles
                    .entry(name.to_owned())
                    .or_default(),
                *color,
            );
        });
    });
}

fn hex_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(RichText::new(label).small().color(TEXT_MUTED));
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace)
            .char_limit(96),
    );
}

fn paint_raw(ui: &mut egui::Ui, state: &mut SerialState) {
    view_card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("RAW DATA").strong().color(TEXT_MUTED));
            ui.label(
                RichText::new(if state.settings.mode != SerialMode::Binary {
                    "ASCII"
                } else {
                    "HEX + ASCII"
                })
                .small()
                .color(ACCENT),
            );
        });
        ui.add_space(8.0);
        if state.raw.is_empty() {
            ui.label(
                RichText::new("Open the port or a raw capture to inspect incoming bytes.")
                    .color(TEXT_MUTED),
            );
        } else {
            let lines = state.raw_lines();
            egui::ScrollArea::both()
                .id_salt("serial_raw")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show_rows(ui, 15.0, lines.len(), |ui, rows| {
                    for index in rows {
                        ui.add(
                            egui::Label::new(RichText::new(&lines[index]).monospace().size(12.0))
                                .wrap_mode(egui::TextWrapMode::Extend)
                                .selectable(true),
                        );
                    }
                });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_column_width_and_line_style_position_do_not_depend_on_selected_name() {
        let context = egui::Context::default();
        let columns = vec![
            "x".to_owned(),
            "a_very_long_sensor_column_name_that_must_not_move_the_style_control".into(),
            "two\nlines".into(),
        ];
        let mut previous = None;
        for name in &columns {
            let mut selected = name.clone();
            let _ = context.run(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    ui.horizontal(|ui| {
                        let rect = csv_column_picker(ui, &mut selected, &columns, 100.0).rect;
                        let style_x = ui.cursor().left();
                        assert!((rect.width() - 100.0).abs() < 0.1);
                        let geometry = (rect.size(), style_x);
                        if let Some(previous) = previous {
                            assert_eq!(geometry, previous);
                        }
                        previous = Some(geometry);
                        line_style_picker(ui, &mut Default::default(), COLORS[0]);
                    });
                });
            });
        }
    }

    #[test]
    fn csv_import_paints_graph_raw_ascii_and_column_settings() {
        let mut app = crate::app::test_support::app();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/examples/serial-samples.csv");
        app.serial[0].import_raw(&path).unwrap();
        let last_line = app.serial[0].raw_lines().last().unwrap().clone();
        let context = egui::Context::default();
        let _ = render(&mut app, &context, vec![]);
        let labels = texts(&render(&mut app, &context, vec![]).shapes);
        assert!(labels.iter().any(|label| label == "RAW DATA"));
        assert!(labels.iter().any(|label| label == "ASCII"));
        assert!(labels.contains(&last_line));
        assert!(labels.iter().any(|label| label == "X: Sample"));
        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                ui.set_width(300.0);
                csv_settings(ui, &mut app.serial[0]);
            });
        });
        let labels = texts(&output.shapes);
        assert!(labels
            .iter()
            .any(|label| label == "First row contains column names"));
        assert!(labels.iter().any(|label| label == "timestamp"));
        assert!(labels.iter().any(|label| label == "temperature"));
    }

    #[test]
    fn removing_timestamp_and_rate_rebuilds_opened_files_with_sample_labels() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples");
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let mut app = crate::app::test_support::app();
            app.serial[0].settings.mode = mode;
            let file = if mode == SerialMode::Text {
                app.serial[0].settings.fields = ["temperature", "speed"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| TextField {
                        name: name.into(),
                        separator: String::new(),
                        color: COLORS[index],
                        line_style: Default::default(),
                    })
                    .collect();
                "serial-ascii.txt"
            } else {
                app.serial[0]
                    .load_schema(
                        "schema.json".into(),
                        include_str!("../../../../docs/examples/serial-schema.json").into(),
                    )
                    .unwrap();
                "serial-binary.bin"
            };
            app.serial[0].settings.timestamp_field = "speed".into();
            app.serial[0].settings.timestamp_unit = SerialTimestampUnit::Milliseconds;
            app.serial[0].settings.timestamp_multiplier = "10".into();
            app.serial[0].import_raw(&root.join(file)).unwrap();
            let raw = app.serial[0].raw.clone();
            assert!(app.serial[0].x_axis.is_time());
            let before = app.serial[0].settings.clone();
            app.serial[0].settings.timestamp_field.clear();
            app.apply_serial_axis_changes(&before);
            assert_eq!(app.serial[0].x_axis, crate::serial::SerialXAxis::Samples);
            assert_eq!(app.serial[0].decoded_records, 512);
            let context = egui::Context::default();
            let labels = texts(&render(&mut app, &context, vec![]).shapes);
            assert!(labels.iter().any(|label| label == "X: Sample"));
            assert!(labels.iter().any(|label| label == "samples"));
            assert!(!labels.iter().any(|label| label == "s"
                || label.ends_with("ms")
                || label.ends_with("µs")
                || label.ends_with("ns")));
            let before = app.serial[0].settings.clone();
            app.serial[0].settings.sample_rate = "1000".into();
            app.apply_serial_axis_changes(&before);
            assert!(app.serial[0].x_axis.is_time());
            let before = app.serial[0].settings.clone();
            app.serial[0].settings.sample_rate.clear();
            app.apply_serial_axis_changes(&before);
            assert_eq!(app.serial[0].x_axis, crate::serial::SerialXAxis::Samples);
            assert_eq!(app.serial[0].raw, raw);
            assert!(app.serial[0].error.is_none());
        }
    }

    #[test]
    fn empty_plot_updates_units_without_requiring_fields_or_json() {
        let mut app = crate::app::test_support::app();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples");
        for mode in [SerialMode::Text, SerialMode::Binary] {
            app.serial[0].settings.mode = mode;
            app.serial[0]
                .import_raw(&root.join("serial-ascii.txt"))
                .unwrap();
            let raw = app.serial[0].raw.clone();
            let before = app.serial[0].settings.clone();
            app.serial[0].settings.sample_rate = "1000".into();
            app.apply_serial_axis_changes(&before);
            assert!(app.serial[0].x_axis.is_time());
            let before = app.serial[0].settings.clone();
            app.serial[0].settings.sample_rate.clear();
            app.apply_serial_axis_changes(&before);
            assert!(!app.serial[0].x_axis.is_time());
            assert_eq!(app.serial[0].raw, raw);
            assert!(app.serial[0].error.is_none());
        }
    }

    #[test]
    fn graph_labels_switch_from_sample_numbers_to_sample_rate_time_on_plot() {
        let mut app = crate::app::test_support::app();
        app.serial[0].settings.fields.push(TextField {
            name: "temperature".into(),
            separator: String::new(),
            color: COLORS[0],
            line_style: Default::default(),
        });
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/examples/serial-ascii.txt");
        app.serial[0].import_raw(&path).unwrap();
        app.serial[0].fit_graph();
        let context = egui::Context::default();
        let labels = texts(&render(&mut app, &context, vec![]).shapes);
        assert!(labels.iter().any(|label| label == "X: Sample"));
        assert!(labels.iter().any(|label| label == "samples"));
        app.serial[0].settings.sample_rate = "1000".into();
        // Editing settings does not relabel the old graph before applying Plot.
        assert!(texts(&render(&mut app, &context, vec![]).shapes)
            .iter()
            .any(|label| label == "X: Sample"));
        app.serial[0].plot_imported().unwrap();
        let labels = texts(&render(&mut app, &context, vec![]).shapes);
        assert!(labels.iter().any(|label| label == "X: Time"));
        assert!(labels.iter().any(|label| label.ends_with("ms")));
        assert!(!labels.iter().any(|label| label == "samples"));
    }

    fn connection_frame(
        app: &mut WaveCraftApp,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, crate::FrameActions) {
        let mut actions = crate::FrameActions::default();
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |context| {
                app.paint_serial_toolbar(context, &mut actions);
                app.paint_serial_setup(context, &mut actions);
                app.paint_status_bar(context);
                app.paint_serial_central(context);
            },
        );
        (output, actions)
    }

    fn connection_text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
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
    }

    fn click_connection(
        app: &mut WaveCraftApp,
        context: &egui::Context,
        position: egui::Pos2,
    ) -> crate::FrameActions {
        let mut actions = crate::FrameActions::default();
        for pressed in [true, false] {
            actions = connection_frame(
                app,
                context,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .1;
        }
        actions
    }

    #[test]
    fn connection_has_aligned_fields_and_open_close_below_port() {
        let mut app = crate::app::test_support::app();
        app.active_workspace_view = Some(WorkspaceView::Serial);
        app.serial[0].settings.port = "test-uart".into();
        app.serial[0].ports = vec!["test-uart".into()];
        let context = egui::Context::default();
        crate::configure_style(&context);
        connection_frame(&mut app, &context, vec![]);
        let (output, _) = connection_frame(&mut app, &context, vec![]);
        connection_text_rect(&output, "CONNECTION");
        let port = connection_text_rect(&output, "Port");
        let selected_port = connection_text_rect(&output, "test-uart");
        let open = connection_text_rect(&output, "Open");
        let baud = connection_text_rect(&output, "Baud rate");
        let speed = connection_text_rect(&output, "115200");
        assert!(selected_port.left() > port.right());
        assert!((selected_port.center().y - port.center().y).abs() < 2.0);
        assert!((speed.center().y - baud.center().y).abs() < 2.0);
        assert!((selected_port.left() - speed.left()).abs() < 2.0);
        assert!(open.top() > port.bottom() && open.bottom() < baud.top());
        assert!(open.left() > port.right() && open.right() < 300.0);
        for label in ["Data bits", "Parity", "Stop bits", "Flow control"] {
            let rect = connection_text_rect(&output, label);
            assert!((rect.left() - port.left()).abs() < 2.0, "{label}");
        }
        let labels = texts(&output.shapes);
        assert!(!labels
            .iter()
            .any(|text| text == "PORT" || text == "▶ Start" || text == "■ Stop"));
        assert_eq!(
            labels.iter().filter(|text| text.as_str() == "Open").count(),
            1
        );
        let actions = click_connection(&mut app, &context, open.center());
        assert_eq!(actions.serial_toggle_connection, Some(0));
        assert!(app.serial[0].session.is_none()); // Applied after painting, never from the editor.
        app.serial[0].baud_draft = Some("invalid".into());
        app.apply_serial_actions(&actions);
        assert!(app.serial[0].error.is_some());
        assert!(app.serial[0].session.is_none());

        let ready = crate::app::test_support::ready(
            &app,
            wavecraft_core::InstrumentKind::Serial,
            crate::AcquisitionMode::Run,
            crate::CaptureTarget::LogicAnalyzer,
        );
        app.serial[0].session = Some(ready.session);
        app.serial[0].baud_draft = None;
        let (output, _) = connection_frame(&mut app, &context, vec![]);
        let close = connection_text_rect(&output, "Close");
        let port = connection_text_rect(&output, "Port");
        assert!(close.top() > port.bottom());
        assert!(close.bottom() < connection_text_rect(&output, "Baud rate").top());
        let data_bits = connection_text_rect(&output, "8").center();
        assert!(click_connection(&mut app, &context, data_bits)
            .serial_toggle_connection
            .is_none());
        assert!(!context.memory(|memory| memory.any_popup_open()));
        let actions = click_connection(&mut app, &context, close.center());
        assert_eq!(actions.serial_toggle_connection, Some(0));
        app.apply_serial_actions(&actions);
        assert!(app.serial[0].stopping);
        assert!(click_connection(&mut app, &context, close.center())
            .serial_toggle_connection
            .is_none());
    }

    #[test]
    fn open_is_disabled_without_a_port_or_in_csv_mode() {
        let mut app = crate::app::test_support::app();
        app.active_workspace_view = Some(WorkspaceView::Serial);
        let context = egui::Context::default();
        crate::configure_style(&context);
        for mode in [SerialMode::Text, SerialMode::Binary, SerialMode::Csv] {
            app.serial[0].settings.mode = mode;
            app.serial[0].settings.port = if mode == SerialMode::Csv {
                "test-uart".into()
            } else {
                String::new()
            };
            connection_frame(&mut app, &context, vec![]);
            let (output, _) = connection_frame(&mut app, &context, vec![]);
            let position = connection_text_rect(&output, "Open").center();
            assert!(click_connection(&mut app, &context, position)
                .serial_toggle_connection
                .is_none());
            assert!(app.serial[0].session.is_none());
        }
    }

    #[test]
    fn five_serial_views_restore_independent_settings_layout_and_legacy_profile() {
        let path =
            std::env::temp_dir().join(format!("wavecraft-five-serial-{}.json", std::process::id()));
        let repository = crate::CaptureSettingsRepository::at(path.clone());
        let mut saved = crate::CaptureSettingsDocument::default();
        let legacy = SerialSettings {
            port: "legacy-uart".into(),
            baud: 230400,
            ..Default::default()
        };
        saved.global.serial = Some(legacy.clone());
        let mut app = WaveCraftApp::with_settings(Some(repository.clone()), saved);
        assert_eq!(app.serial[0].settings, legacy);
        assert_eq!(
            app.available_workspace_views()
                .iter()
                .filter(|view| view.serial_index().is_some())
                .count(),
            1
        );
        for (index, view) in WorkspaceView::SERIAL.into_iter().enumerate() {
            app.open_workspace_view(view);
            app.serial[index].settings.port = format!("uart-{index}");
            app.serial[index].settings.baud = 123456 + index as u32;
            app.serial[index].settings.graph_percent = 40 + index as u8 * 5;
            app.serial[index].settings.fields.push(TextField {
                name: format!("value{index}"),
                separator: "=".into(),
                color: COLORS[index],
                line_style: Default::default(),
            });
            app.workspace_view_layout
                .set_setup_panel_open(view, index % 2 == 0);
            app.save_setup_settings(view);
        }
        app.workspace_view_layout.begin_drag(WorkspaceView::Serial5);
        app.workspace_view_layout.drag_to(WorkspaceView::Serial);
        app.workspace_view_layout.finish_drag();
        app.set_workspace_view_visible(WorkspaceView::Serial3, false);
        app.save_global_settings();
        let mut restored = WaveCraftApp::with_settings(None, repository.load().unwrap());
        restored.reconcile_waveform_tab();
        assert_eq!(restored.active_workspace_view, Some(WorkspaceView::Serial5));
        assert_eq!(
            restored.available_workspace_views(),
            app.available_workspace_views()
        );
        assert_eq!(
            restored.workspace_view_layout.order(),
            app.workspace_view_layout.order()
        );
        for (index, view) in WorkspaceView::SERIAL.into_iter().enumerate() {
            assert_eq!(restored.serial[index].settings, app.serial[index].settings);
            assert_eq!(
                restored.workspace_view_layout.setup_panel_open(view),
                index % 2 == 0
            );
            assert!(restored.serial[index].session.is_none());
        }
        app.open_workspace_view(WorkspaceView::Serial3);
        let context = egui::Context::default();
        crate::configure_style(&context);
        connection_frame(&mut app, &context, vec![]);
        let (output, _) = connection_frame(&mut app, &context, vec![]);
        let open = connection_text_rect(&output, "Open").center();
        let actions = click_connection(&mut app, &context, open);
        assert_eq!(actions.serial_toggle_connection, Some(2));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn serial_views_poll_in_background_and_stop_or_fail_independently() {
        use crate::app::test_support::{ready_scripted, Script, SourceEnding};
        use wavecraft_core::{CaptureBatch, InstrumentKind};
        let mut app = crate::app::test_support::app();
        for (index, view) in WorkspaceView::SERIAL.into_iter().enumerate() {
            app.workspace_view_layout.set_visible(view, true);
            app.serial[index].settings.fields.push(TextField {
                name: "value".into(),
                separator: "=".into(),
                color: COLORS[index],
                line_style: Default::default(),
            });
            app.serial[index].prepare().unwrap();
            let ready = ready_scripted(
                &app,
                vec![(
                    InstrumentKind::Serial,
                    Script::sending(
                        vec![CaptureBatch::Serial(
                            format!("value={index}\n").into_bytes(),
                        )],
                        if index == 4 {
                            SourceEnding::Fail("serial slot failure".into())
                        } else {
                            SourceEnding::Idle
                        },
                    ),
                )],
                crate::AcquisitionMode::Run,
                crate::CaptureTarget::LogicAnalyzer,
            );
            app.serial[index].session = Some(ready.session);
        }
        app.active_workspace_view = Some(WorkspaceView::Battery);
        let context = egui::Context::default();
        crate::configure_style(&context);
        let poll_frame = |app: &mut WaveCraftApp| {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, 900.0),
                    )),
                    ..Default::default()
                },
                |context| app.run_frame(context),
            );
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while (app.serial.iter().any(|state| state.raw.is_empty())
            || app.serial[4].session.is_some())
            && std::time::Instant::now() < deadline
        {
            poll_frame(&mut app);
        }
        for (index, state) in app.serial.iter().enumerate() {
            assert_eq!(
                state.raw.iter().copied().collect::<Vec<_>>(),
                format!("value={index}\n").into_bytes()
            );
            assert_eq!(state.decoded_records, 1);
            assert_eq!(state.series[0].points[0][1], index as f64);
        }
        assert!(app.serial[4]
            .error
            .as_deref()
            .unwrap()
            .contains("serial slot failure"));
        assert!(app.serial[..4]
            .iter()
            .all(|state| state.session.is_some() && state.error.is_none()));
        app.set_workspace_view_visible(WorkspaceView::Serial3, false);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.serial[2].session.is_some() && std::time::Instant::now() < deadline {
            poll_frame(&mut app);
        }
        assert!(app.serial[2].session.is_none());
        for index in [0, 1, 3] {
            assert!(app.serial[index].session.is_some());
        }
        app.open_workspace_view(WorkspaceView::Serial2);
        app.apply_serial_actions(&crate::FrameActions {
            serial_toggle_connection: Some(0),
            ..Default::default()
        });
        assert!(app.serial[0].stopping);
        assert!(app.serial[1].session.is_some() && !app.serial[1].stopping);
        app.open_workspace_view(WorkspaceView::Serial3);
        assert_eq!(app.serial[2].decoded_records, 1);
    }

    #[test]
    fn imported_files_clear_actions_viewports_and_errors_stay_with_each_serial_view() {
        let mut app = crate::app::test_support::app();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples");
        let context = egui::Context::default();
        crate::configure_style(&context);
        for (index, view) in WorkspaceView::SERIAL.into_iter().enumerate() {
            app.open_workspace_view(view);
            let file = if index % 2 == 0 {
                "serial-ascii.txt"
            } else {
                "serial-samples.csv"
            };
            app.serial[index].import_raw(&root.join(file)).unwrap();
            app.serial[index].follow = false;
            app.serial[index].time_window = 10.0 + index as f64;
        }
        let raw = app
            .serial
            .iter()
            .map(|state| state.raw.clone())
            .collect::<Vec<_>>();
        connection_frame(&mut app, &context, vec![]);
        let (output, _) = connection_frame(&mut app, &context, vec![]);
        let clear = connection_text_rect(&output, "Clear").center();
        click_connection(&mut app, &context, clear);
        assert!(app.serial[4].raw.is_empty());
        assert!(app.serial[4].imported_file.is_none());
        for (index, expected) in raw.iter().enumerate().take(4) {
            assert_eq!(&app.serial[index].raw, expected);
            assert!(app.serial[index].imported_file.is_some());
            assert_eq!(app.serial[index].time_window, 10.0 + index as f64);
            assert!(!app.serial[index].follow);
        }
        app.serial[4].error = Some("Only serial five failed".into());
        app.workspace_view_layout
            .set_setup_panel_open(WorkspaceView::Serial5, false);
        let (output, _) = connection_frame(&mut app, &context, vec![]);
        assert!(connection_text_rect(&output, "Only serial five failed").top() > 800.0);
        app.select_serial_view(WorkspaceView::Serial);
        let (output, _) = connection_frame(&mut app, &context, vec![]);
        assert!(!texts(&output.shapes)
            .iter()
            .any(|text| text == "Only serial five failed"));
        assert!(texts(&output.shapes)
            .iter()
            .any(|text| text.contains("serial-ascii.txt")));
    }

    #[test]
    fn serial_and_pd_power_views_cannot_open_a_port_owned_by_another_view() {
        use crate::app::test_support::ready;
        let mut app = crate::app::test_support::app();
        let session = |app: &WaveCraftApp| {
            ready(
                app,
                wavecraft_core::InstrumentKind::Serial,
                crate::AcquisitionMode::Run,
                crate::CaptureTarget::LogicAnalyzer,
            )
            .session
        };
        app.serial[0].settings.port = "/dev/cu.test-uart".into();
        app.serial[0].session = Some(session(&app));
        app.serial[1].settings.port = "/dev/tty.test-uart".into();
        app.apply_serial_actions(&crate::FrameActions {
            serial_toggle_connection: Some(1),
            ..Default::default()
        });
        assert!(app.serial[1].session.is_none());
        assert!(app.serial[1]
            .error
            .as_deref()
            .unwrap()
            .contains("Serial / UART"));
        app.pd_power[0].settings.port = "/dev/tty.test-uart".into();
        app.apply_pd_power_actions(&crate::FrameActions {
            pd_power_connect: Some(0),
            ..Default::default()
        });
        assert!(app.pd_power[0].session.is_none());
        assert!(app.pd_power[0]
            .error
            .as_deref()
            .unwrap()
            .contains("Serial / UART"));
        app.pd_power[3].settings.port = "/dev/cu.test-power".into();
        app.pd_power[3].session = Some(session(&app));
        app.serial[4].settings.port = "/dev/tty.test-power".into();
        app.apply_serial_actions(&crate::FrameActions {
            serial_toggle_connection: Some(4),
            ..Default::default()
        });
        assert!(app.serial[4].session.is_none());
        assert!(app.serial[4]
            .error
            .as_deref()
            .unwrap()
            .contains("PD Power 4"));
        assert!(app.serial[0].session.is_some());
    }

    #[test]
    fn plot_button_stays_in_toolbar_and_replots_opened_file_with_name_in_status() {
        let mut app = crate::app::test_support::app();
        app.active_workspace_view = Some(WorkspaceView::Serial);
        let context = egui::Context::default();
        crate::configure_style(&context);
        let frame = |app: &mut WaveCraftApp, events| {
            context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 700.0),
                    )),
                    events,
                    ..Default::default()
                },
                |context| {
                    app.paint_serial_toolbar(context, &mut crate::FrameActions::default());
                    app.paint_status_bar(context);
                    app.paint_serial_central(context);
                },
            )
        };
        assert!(!texts(&frame(&mut app, vec![]).shapes)
            .iter()
            .any(|text| text == "Plot"));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/examples/serial-ascii.txt");
        app.serial[0].import_raw(&path).unwrap();
        let raw = app.serial[0].raw.clone();
        let output = frame(&mut app, vec![]);
        let bounds = |label: &str| {
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
        let plot = bounds("Plot");
        assert!(plot.left() > bounds("📷 Snapshot").right());
        assert!(plot.right() > 1100.0);
        assert!(!texts(&output.shapes).iter().any(|text| text == "▶ Start"
            || text == "■ Stop"
            || text == "Close"
            || text == "Open"));
        assert!(texts(&output.shapes)
            .iter()
            .any(|text| text.contains("serial-ascii.txt")));
        app.serial[0].settings.fields.push(TextField {
            separator: String::new(),
            name: "temperature".into(),
            color: COLORS[0],
            line_style: Default::default(),
        });
        for pressed in [true, false] {
            let position = plot.center();
            let _ = frame(
                &mut app,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert_eq!(app.serial[0].decoded_records, 512);
        assert_eq!(app.serial[0].series[0].points.len(), 512);
        assert_eq!(app.serial[0].raw, raw);
        let labels = texts(&frame(&mut app, vec![]).shapes);
        assert!(labels.iter().any(|text| text.contains("serial-ascii.txt")));
        assert!(labels.iter().any(|text| text == "Plot"));
        app.serial[0].clear_data();
        let labels = texts(&frame(&mut app, vec![]).shapes);
        assert!(!labels
            .iter()
            .any(|text| text == "Plot" || text.contains("serial-ascii.txt")));
    }

    fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn collect(shape: &egui::Shape, output: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => output.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, output);
                    }
                }
                _ => {}
            }
        }
        let mut output = Vec::new();
        for shape in shapes {
            collect(&shape.shape, &mut output);
        }
        output
    }

    fn render(
        app: &mut WaveCraftApp,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |context| app.paint_serial_central(context),
        )
    }

    #[test]
    fn raw_only_modes_paint_bytes_below_an_empty_graph() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let mut app = crate::app::test_support::app();
            app.active_workspace_view = Some(WorkspaceView::Serial);
            app.serial[0].settings.mode = mode;
            app.serial[0].prepare().unwrap();
            app.serial[0].ingest(1.0, b"temperature=42\n");
            let context = egui::Context::default();
            crate::configure_style(&context);
            let _ = render(&mut app, &context, Vec::new());
            let output = render(&mut app, &context, Vec::new());
            let labels = texts(&output.shapes);
            assert!(labels.iter().any(|text| text == "GRAPH"));
            assert!(labels.iter().any(|text| text == "RAW DATA"));
            assert!(labels.iter().any(|text| text.contains("Raw data only")));
            assert!(labels.iter().any(|text| if mode == SerialMode::Text {
                text == "temperature=42"
            } else {
                text.contains("74 65 6D 70")
            }));
        }
    }

    #[test]
    fn imported_files_are_visible_in_raw_panels_without_fields_or_json() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let mut app = crate::app::test_support::app();
            app.active_workspace_view = Some(WorkspaceView::Serial);
            app.serial[0].settings.mode = mode;
            let file = if mode == SerialMode::Text {
                "serial-ascii.txt"
            } else {
                "serial-binary.bin"
            };
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/examples")
                .join(file);
            app.serial[0].import_raw(&path).unwrap();
            let last_line = app.serial[0].raw_lines().last().unwrap().clone();
            let context = egui::Context::default();
            crate::configure_style(&context);
            let _ = render(&mut app, &context, vec![]);
            let labels = texts(&render(&mut app, &context, vec![]).shapes);
            assert!(labels.iter().any(|text| text == "RAW DATA"));
            assert!(labels.iter().any(|text| text
                == if mode == SerialMode::Text {
                    "ASCII"
                } else {
                    "HEX + ASCII"
                }));
            assert!(labels.contains(&last_line));
            assert!(app.serial[0].series.is_empty());
            assert!(app.serial[0].error.is_none());
        }
    }

    #[test]
    fn parsed_binary_json_lists_scalar_and_bit_field_names_with_independent_styles() {
        let mut state = SerialState::default();
        state.settings.mode = SerialMode::Binary;
        state
            .load_schema(
                "example.json".into(),
                include_str!("../../../../docs/examples/serial-schema.json").into(),
            )
            .unwrap();
        state
            .settings
            .binary_line_styles
            .insert("temperature".into(), crate::serial::SerialLineStyle::Star);
        state
            .settings
            .binary_line_styles
            .insert("speed".into(), crate::serial::SerialLineStyle::Triangle);
        let context = egui::Context::default();
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 1200.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| binary_settings(ui, &mut state));
            },
        );
        let labels = texts(&output.shapes);
        for name in [
            "PLOTTED FIELDS",
            "temperature",
            "speed",
            "status",
            "enabled",
            "fault",
            "mode",
        ] {
            assert!(labels.iter().any(|text| text == name), "{name}");
        }
        assert_eq!(
            state.settings.line_style("temperature"),
            crate::serial::SerialLineStyle::Star
        );
        assert_eq!(
            state.settings.line_style("speed"),
            crate::serial::SerialLineStyle::Triangle
        );
        assert_ne!(
            state.settings.binary_colors["temperature"],
            state.settings.binary_colors["speed"]
        );
    }

    #[test]
    fn fit_button_fills_plot_width_for_short_and_long_traces_despite_trailing_raw_bytes() {
        for (first, last) in [(0.0002, 0.0006), (0.3, 1.0), (1.0, 7201.0)] {
            let mut app = crate::app::test_support::app();
            app.serial[0].settings.fields.push(TextField {
                separator: String::new(),
                name: "v".into(),
                color: COLORS[0],
                line_style: Default::default(),
            });
            app.serial[0].settings.fields.push(TextField {
                name: "ts".into(),
                separator: String::new(),
                color: COLORS[1],
                line_style: Default::default(),
            });
            app.serial[0].settings.timestamp_field = "ts".into();
            app.serial[0].prepare().unwrap();
            for (time, value) in [(first, 0), ((first + last) / 2.0, 1), (last, 2)] {
                app.serial[0].ingest(0.0, format!("v={value} ts={time}\n").as_bytes());
            }
            app.serial[0].ingest(last + 30.0, b"no matching values\n");
            let context = egui::Context::default();
            let output = render(&mut app, &context, vec![]);
            let fit = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == "Fit" => {
                        Some(text.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .expect("Fit is visible");
            for pressed in [true, false] {
                let _ = render(
                    &mut app,
                    &context,
                    vec![
                        egui::Event::PointerMoved(fit),
                        egui::Event::PointerButton {
                            pos: fit,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
            // Render again to catch the time-window editor clamping fitted spans
            // back to its old 10 ms minimum or one-hour maximum.
            let output = render(&mut app, &context, vec![]);
            assert!((app.serial[0].time_window - (last - first)).abs() < 1e-9);
            if last < 0.001 {
                assert!(texts(&output.shapes)
                    .iter()
                    .any(|text| text.ends_with("µs")));
            }
            let color = egui::epaint::ColorMode::Solid(egui::Color32::from_rgb(
                COLORS[0][0],
                COLORS[0][1],
                COLORS[0][2],
            ));
            let trace = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Path(path)
                        if !path.closed && path.stroke.color == color && path.points.len() == 3 =>
                    {
                        Some(path)
                    }
                    _ => None,
                })
                .expect("the fitted trace is visible");
            let top = trace
                .points
                .iter()
                .map(|point| point.y)
                .fold(f32::INFINITY, f32::min);
            let bottom = trace
                .points
                .iter()
                .map(|point| point.y)
                .fold(f32::NEG_INFINITY, f32::max);
            let grid = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::LineSegment { points, stroke }
                        if stroke.color == egui::epaint::ColorMode::Solid(GRID)
                            && points[0].y == points[1].y
                            && points[0].y > top
                            && points[0].y < bottom =>
                    {
                        Some(points)
                    }
                    _ => None,
                })
                .expect("plot grid is visible");
            assert!((trace.points[0].x - grid[0].x).abs() < 0.01);
            assert!((trace.points[2].x - grid[1].x).abs() < 0.01);
        }
    }

    #[test]
    fn graph_does_not_repeat_field_controls_or_latest_value_legend() {
        let mut app = crate::app::test_support::app();
        app.serial[0].settings.fields.push(TextField {
            separator: String::new(),
            name: "temperature".into(),
            color: COLORS[0],
            line_style: Default::default(),
        });
        app.serial[0].prepare().unwrap();
        app.serial[0].ingest(1.0, b"temperature=42\n");
        let context = egui::Context::default();
        let _ = render(&mut app, &context, vec![]);
        let output = render(&mut app, &context, vec![]);
        let labels = texts(&output.shapes);
        assert!(labels.iter().any(|text| text == "GRAPH"));
        assert!(!labels
            .iter()
            .any(|text| text == "PLOTTED FIELDS" || text.starts_with("temperature  ")));
        assert!(labels.iter().any(|text| text == "temperature=42"));
    }

    #[test]
    fn raw_data_has_breathing_room_below_both_format_headers() {
        for mode in [SerialMode::Text, SerialMode::Binary] {
            let mut app = crate::app::test_support::app();
            app.serial[0].settings.mode = mode;
            app.serial[0].ingest(1.0, b"temperature=42\n");
            let context = egui::Context::default();
            let _ = render(&mut app, &context, vec![]);
            let output = render(&mut app, &context, vec![]);
            let text_shapes: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => Some(text),
                    _ => None,
                })
                .collect();
            let header_bottom = text_shapes
                .iter()
                .filter(|text| matches!(text.galley.text(), "RAW DATA" | "ASCII" | "HEX + ASCII"))
                .map(|text| text.visual_bounding_rect().bottom())
                .fold(f32::NEG_INFINITY, f32::max);
            let first_data = text_shapes
                .iter()
                .find(|text| {
                    if mode == SerialMode::Text {
                        text.galley.text() == "temperature=42"
                    } else {
                        text.galley.text().contains("74 65 6D 70")
                    }
                })
                .expect("raw data is visible");
            assert!(
                first_data.visual_bounding_rect().top() - header_bottom >= 8.0,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn splitter_accepts_small_drag_steps_and_persists_height() {
        let mut app = crate::app::test_support::app();
        app.active_workspace_view = Some(WorkspaceView::Serial);
        app.serial[0].settings.fields.push(TextField {
            separator: String::new(),
            name: "temperature".into(),
            color: COLORS[0],
            line_style: Default::default(),
        });
        app.serial[0].prepare().unwrap();
        for index in 0..100 {
            app.serial[0].ingest(
                index as f64 * 0.01,
                format!("temperature={index}\n").as_bytes(),
            );
        }
        let context = egui::Context::default();
        crate::configure_style(&context);
        let output = render(&mut app, &context, Vec::new());
        let divider = output
            .shapes
            .iter()
            .find_map(|clipped| {
                if let egui::Shape::LineSegment { points, .. } = &clipped.shape {
                    if (points[0].x - 24.0).abs() < 0.1 && (points[1].x - 976.0).abs() < 0.1 {
                        return Some(points[0].lerp(points[1], 0.5));
                    }
                }
                None
            })
            .expect("divider is painted");
        let _ = render(&mut app, &context, vec![egui::Event::PointerMoved(divider)]);
        let _ = render(
            &mut app,
            &context,
            vec![egui::Event::PointerButton {
                pos: divider,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
        );
        for step in 1..=45 {
            let _ = render(
                &mut app,
                &context,
                vec![egui::Event::PointerMoved(
                    divider + egui::vec2(0.0, step as f32),
                )],
            );
        }
        let end = divider + egui::vec2(0.0, 45.0);
        let _ = render(
            &mut app,
            &context,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        assert!(app.serial[0].settings.graph_percent > 68);
        assert_eq!(
            app.saved_capture_settings
                .global
                .serial
                .as_ref()
                .unwrap()
                .graph_percent,
            app.serial[0].settings.graph_percent
        );
    }

    #[test]
    fn serial_view_paints_full_frames_without_waveform_session_tabs() {
        let mut app = crate::app::test_support::app();
        app.select_waveform_tab(crate::WaveformTab::LogicAnalyzer);
        app.active_workspace_view = Some(WorkspaceView::Serial);
        let context = egui::Context::default();
        crate::configure_style(&context);
        for size in [egui::vec2(1600.0, 900.0), egui::vec2(640.0, 420.0)] {
            for mode in [SerialMode::Text, SerialMode::Binary] {
                app.serial[0].settings.mode = mode;
                let output = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |context| app.run_frame(context),
                );
                let labels = texts(&output.shapes);
                assert!(labels.iter().any(|text| text == "SERIAL / UART SETUP"));
                assert!(labels.iter().any(|text| text == "RAW DATA"));
                assert!(!labels.iter().any(|text| text == "Session 1"));
                assert_eq!(app.frame_state().control_target, None);
            }
        }
    }
}
