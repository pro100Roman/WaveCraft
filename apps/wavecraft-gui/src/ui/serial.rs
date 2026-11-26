//! Serial / UART workspace, matching the instrument toolbar, setup cards and split panes.

use super::view_shell::{settings_card, view_card, view_toolbar, workspace_panel, SetupPanel};
use crate::serial::{SerialMode, SerialSettings, SerialState, TextField, COLORS};
use crate::{
    egui, toolbar_action_button, RichText, WaveCraftApp, WorkspaceView, ACCENT, BACKGROUND, GRID,
    TEXT_MUTED,
};
use wavecraft_serial::BinarySchema;

const BAUD_RATES: &[u32] = &[
    110, 300, 600, 1_200, 2_400, 4_800, 9_600, 14_400, 19_200, 28_800, 38_400, 57_600, 76_800,
    115_200, 128_000, 153_600, 230_400, 256_000, 460_800, 500_000, 576_000, 921_600, 1_000_000,
    1_152_000, 1_500_000, 2_000_000, 2_500_000, 3_000_000,
];

impl WaveCraftApp {
    pub(crate) fn serial_view_is_active(&self) -> bool {
        self.active_workspace_view == Some(WorkspaceView::Serial)
    }

    pub(crate) fn select_serial_view(&mut self) {
        if let Some(active) = self.active_waveform_tab {
            self.save_waveform_view_settings(active);
        }
        self.active_workspace_view = Some(WorkspaceView::Serial);
        self.snapshot_plot_rect = None;
        if self.serial.ports.is_empty() {
            self.serial.refresh();
        }
        self.save_global_settings();
    }

    pub(crate) fn paint_serial_toolbar(&mut self, context: &egui::Context) {
        let settings_clicked = view_toolbar(context, "Serial / UART", |ui| {
            let running = self.serial.session.is_some();
            if ui
                .add_enabled(!running, toolbar_action_button("↻ Refresh", TEXT_MUTED))
                .clicked()
            {
                self.serial.refresh();
            }
            if ui.add(toolbar_action_button("Clear", TEXT_MUTED)).clicked() {
                self.serial.clear_data();
            }
            ui.separator();
            if ui
                .add_enabled(!running, toolbar_action_button("📂 Open raw…", TEXT_MUTED))
                .clicked()
            {
                self.open_serial_raw();
            }
            if ui
                .add_enabled(
                    !self.serial.raw.is_empty(),
                    toolbar_action_button("Save raw…", TEXT_MUTED),
                )
                .on_hover_text("Save the retained raw bytes (latest 64 KiB)")
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name("serial-capture.bin")
                    .save_file()
                {
                    let bytes: Vec<_> = self.serial.raw.iter().copied().collect();
                    if let Err(error) = std::fs::write(path, bytes) {
                        self.serial.error = Some(error.to_string());
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if running {
                    if ui
                        .add_enabled(
                            !self.serial.stopping,
                            toolbar_action_button("■ Stop", egui::Color32::LIGHT_RED),
                        )
                        .clicked()
                    {
                        self.serial.stop();
                    }
                } else if ui.add(toolbar_action_button("▶ Start", ACCENT)).clicked() {
                    if let Err(error) = self.serial.start() {
                        self.serial.error = Some(error.to_string());
                    }
                }
            });
        });
        self.settings_open |= settings_clicked;
    }

    fn open_serial_raw(&mut self) {
        use std::io::Read;
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open raw serial bytes")
            .pick_file()
        else {
            return;
        };
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let file = std::fs::File::open(&path)?;
            if file.metadata()?.len() > 8 * 1024 * 1024 {
                return Err("Raw imports are limited to 8 MiB".into());
            }
            self.serial.prepare()?;
            let mut reader = file.take(8 * 1024 * 1024);
            let mut buffer = [0; 4096];
            let mut count = 0;
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                count += read;
                let bits_per_byte = 1
                    + self.serial.settings.data_bits
                    + self.serial.settings.stop_bits
                    + u8::from(self.serial.settings.parity != "None");
                self.serial.ingest(
                    count as f64 * f64::from(bits_per_byte)
                        / f64::from(self.serial.settings.baud.max(1)),
                    &buffer[..read],
                );
            }
            self.serial.status = "Imported · estimated UART timing".into();
            Ok(())
        })();
        if let Err(error) = result {
            self.serial.error = Some(error.to_string());
        }
    }

    pub(crate) fn paint_serial_setup(&mut self, context: &egui::Context) {
        let before = self.serial.settings.clone();
        let mut open = self.serial.setup_open;
        SetupPanel::new("serial_setup", "SERIAL / UART SETUP").show_collapsible(
            context,
            &mut open,
            |ui| {
                let running = self.serial.session.is_some();
                if running {
                    ui.label(
                        RichText::new("Stop to change port or decoding settings")
                            .small()
                            .color(TEXT_MUTED),
                    );
                } else {
                    ui.label(
                        RichText::new("Decoding changes apply on the next Start / Open.")
                            .small()
                            .color(TEXT_MUTED),
                    );
                }
                settings_card(ui, |ui| {
                    setup_section(ui, "PORT", !running, |ui| {
                        port_settings(ui, &mut self.serial)
                    });
                    ui.add_space(10.0);
                    setup_section(ui, "DATA FORMAT", !running, |ui| {
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.serial.settings.mode,
                                SerialMode::Text,
                                "Text / ASCII",
                            );
                            ui.selectable_value(
                                &mut self.serial.settings.mode,
                                SerialMode::Binary,
                                "Binary",
                            );
                        });
                        if self.serial.settings.mode == SerialMode::Text {
                            text_settings(ui, &mut self.serial.settings);
                        } else {
                            binary_settings(ui, &mut self.serial);
                        }
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
            },
        );
        self.serial.setup_open = open;
        if before != self.serial.settings {
            for series in &mut self.serial.series {
                let color = if self.serial.settings.mode == SerialMode::Text {
                    self.serial
                        .settings
                        .fields
                        .iter()
                        .find(|field| field.name.trim() == series.name)
                        .map(|field| field.color)
                } else {
                    self.serial
                        .settings
                        .binary_colors
                        .get(&series.name)
                        .copied()
                };
                if let Some(color) = color {
                    series.color = color;
                }
            }
            // Keep a stopped capture intact until a new Start/Open, but rebuild the raw representation on mode changes.
            self.serial.invalidate_raw();
            self.save_global_settings();
        }
    }

    pub(crate) fn paint_serial_central(&mut self, context: &egui::Context) {
        let colors_before: Vec<_> = self
            .serial
            .series
            .iter()
            .map(|series| series.color)
            .collect();
        workspace_panel(context, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("SERIAL / UART").strong().color(ACCENT));
                ui.separator();
                ui.label(format!("{} bytes", self.serial.received_bytes));
                ui.label(format!("{} records", self.serial.decoded_records));
                if self.serial.rejected_frames > 0 {
                    ui.colored_label(
                        egui::Color32::LIGHT_RED,
                        format!("{} rejected", self.serial.rejected_frames),
                    );
                }
                if self.serial.discarded_bytes > 0 {
                    ui.label(format!("{} unsynced bytes", self.serial.discarded_bytes));
                }
            });
            ui.add_space(6.0);
            let total = (ui.available_height() - 10.0 - 2.0 * ui.spacing().item_spacing.y).max(0.0);
            let min_height = 60.0_f32.min(total / 2.0);
            let graph_height = (total * f32::from(self.serial.settings.graph_percent) / 100.0)
                .clamp(min_height, (total - min_height).max(min_height));
            let width = ui.available_width();
            let before = self.serial.settings.graph_percent;
            ui.allocate_ui_with_layout(
                egui::vec2(width, graph_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_clip_rect(ui.max_rect());
                    paint_graph(ui, &mut self.serial);
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
                self.serial.split_drag_origin = Some(graph_height);
            }
            if response.dragged() && total > 0.0 {
                let height = (self.serial.split_drag_origin.unwrap_or(graph_height)
                    + response.drag_delta().y)
                    .clamp(min_height, (total - min_height).max(min_height));
                self.serial.split_drag_origin = Some(height);
                self.serial.settings.graph_percent =
                    (height / total * 100.0).round().clamp(10.0, 90.0) as u8;
            }
            if response.double_clicked() {
                self.serial.settings.graph_percent = 65;
            }
            if response.drag_stopped()
                || before != self.serial.settings.graph_percent && !response.dragged()
            {
                self.serial.split_drag_origin = None;
                self.save_global_settings();
            }
            ui.allocate_ui_with_layout(
                egui::vec2(width, (total - graph_height).max(0.0)),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_clip_rect(ui.max_rect());
                    paint_raw(ui, &mut self.serial);
                },
            );
        });
        if self
            .serial
            .series
            .iter()
            .map(|series| series.color)
            .ne(colors_before)
        {
            for series in &self.serial.series {
                if self.serial.settings.mode == SerialMode::Text {
                    if let Some(field) = self
                        .serial
                        .settings
                        .fields
                        .iter_mut()
                        .find(|field| field.name.trim() == series.name)
                    {
                        field.color = series.color;
                    }
                } else {
                    self.serial
                        .settings
                        .binary_colors
                        .insert(series.name.clone(), series.color);
                }
            }
            self.save_global_settings();
        }
    }
}

fn setup_section(
    ui: &mut egui::Ui,
    title: &str,
    enabled: bool,
    contents: impl FnOnce(&mut egui::Ui),
) {
    egui::CollapsingHeader::new(RichText::new(title).strong().color(TEXT_MUTED))
        .id_salt(("serial_setup_card", title))
        .default_open(true)
        .show_unindented(ui, |ui| {
            ui.add_space(6.0);
            ui.add_enabled_ui(enabled, contents);
        });
}

fn port_settings(ui: &mut egui::Ui, state: &mut SerialState) {
    let settings = &mut state.settings;
    let width = ui.available_width();
    ui.label("Port");
    egui::ComboBox::from_id_salt("serial_ports")
        .width(width)
        .truncate()
        .selected_text(if settings.port.is_empty() {
            "Select port"
        } else {
            &settings.port
        })
        .show_ui(ui, |ui| {
            if state.ports.is_empty() {
                ui.add_enabled(false, egui::Label::new("No serial ports found"));
            }
            for port in &state.ports {
                ui.selectable_value(&mut settings.port, port.clone(), port);
            }
        });

    ui.label("Baud rate");
    paint_baud_rate_selector(ui, &mut settings.baud, &mut state.baud_draft);

    ui.label("Data bits");
    egui::ComboBox::from_id_salt("serial_data_bits")
        .width(width)
        .selected_text(settings.data_bits.to_string())
        .show_ui(ui, |ui| {
            for bits in 5..=8 {
                ui.selectable_value(&mut settings.data_bits, bits, bits.to_string());
            }
        });
    ui.label("Parity");
    string_combo(
        ui,
        "serial_parity",
        &mut settings.parity,
        &["None", "Even", "Odd"],
    );
    ui.label("Stop bits");
    egui::ComboBox::from_id_salt("serial_stop_bits")
        .width(width)
        .selected_text(settings.stop_bits.to_string())
        .show_ui(ui, |ui| {
            for bits in 1..=2 {
                ui.selectable_value(&mut settings.stop_bits, bits, bits.to_string());
            }
        });
    ui.label("Flow control");
    string_combo(
        ui,
        "serial_flow",
        &mut settings.flow_control,
        &["None", "RTS/CTS", "XON/XOFF"],
    );
}

fn paint_baud_rate_selector(ui: &mut egui::Ui, baud: &mut u32, draft: &mut Option<String>) {
    let draft = draft.get_or_insert_with(|| baud.to_string());
    let popup_id = ui.make_persistent_id("serial_baud_rate_popup");
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
            .on_hover_text(
                "Type a baud rate to filter the standard speeds, or enter a custom value",
            );
        let dropdown = ui
            .add_sized([button_width, 20.0], egui::Button::new("▾"))
            .on_hover_text("Show standard baud rates up to 3000000");
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
        .filter(|baud| (1..=12_000_000).contains(baud));
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
                .id_salt("serial_baud_rate_results")
                .max_height(240.0)
                .auto_shrink([false, false])
                .show(popup_ui, |ui| {
                    let mut matches = 0;
                    for rate in BAUD_RATES
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
                            RichText::new("No standard match · use the typed value")
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
            "Enter a baud rate from 1 to 12000000",
        );
    }
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

fn text_settings(ui: &mut egui::Ui, settings: &mut SerialSettings) {
    ui.add_space(8.0);
    ui.label(RichText::new("PLOTTED FIELDS").small().color(TEXT_MUTED));
    ui.label(
        RichText::new("Match name=value or name: value. Records end at CR or LF.")
            .small()
            .color(TEXT_MUTED),
    );
    let mut remove = None;
    for (index, field) in settings.fields.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.horizontal(|ui| {
                ui.color_edit_button_srgb(&mut field.color);
                ui.add(
                    egui::TextEdit::singleline(&mut field.name)
                        .desired_width((ui.available_width() - 30.0).max(20.0))
                        .char_limit(64)
                        .hint_text("e.g. temperature"),
                );
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
            settings.fields.len() < 10,
            egui::Button::new(format!("+ Add field ({}/10)", settings.fields.len())),
        )
        .clicked()
    {
        settings.fields.push(TextField {
            name: String::new(),
            color: COLORS[settings.fields.len() % COLORS.len()],
        });
    }
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
        for (index, name) in schema.channel_names().into_iter().enumerate() {
            ui.horizontal(|ui| {
                ui.color_edit_button_srgb(
                    settings
                        .binary_colors
                        .entry(name.clone())
                        .or_insert(COLORS[index % COLORS.len()]),
                );
                ui.label(name);
            });
        }
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

fn hex_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(RichText::new(label).small().color(TEXT_MUTED));
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace)
            .char_limit(96),
    );
}

fn paint_graph(ui: &mut egui::Ui, state: &mut SerialState) {
    view_card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("GRAPH").strong().color(TEXT_MUTED));
            if ui.checkbox(&mut state.follow, "Follow").changed() {
                state.graph_end = state.latest_time;
            }
            ui.add(
                egui::DragValue::new(&mut state.time_window)
                    .range(0.01..=3600.0)
                    .speed(0.1)
                    .suffix(" s"),
            );
            if ui.small_button("Fit").clicked() {
                let first = state
                    .series
                    .iter()
                    .filter_map(|series| series.points.front().map(|point| point[0]))
                    .fold(state.latest_time, f64::min);
                state.time_window = (state.latest_time - first).max(0.01);
                state.follow = true;
            }
        });
        if !state.series.is_empty() {
            egui::ScrollArea::horizontal()
                .id_salt("serial_legend")
                .max_height(24.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for series in &mut state.series {
                            ui.color_edit_button_srgb(&mut series.color);
                            ui.label(
                                RichText::new(format!(
                                    "{}  {}",
                                    series.name,
                                    series
                                        .points
                                        .back()
                                        .map(|point| format!("{:.3}", point[1]))
                                        .unwrap_or_else(|| "—".into())
                                ))
                                .color(egui::Color32::from_rgb(
                                    series.color[0],
                                    series.color[1],
                                    series.color[2],
                                )),
                            );
                        }
                    });
                });
        }
        let (surface, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ui.available_height().max(0.0)),
            egui::Sense::drag(),
        );
        ui.painter().rect_filled(surface, 3.0, BACKGROUND);
        if surface.width() < 100.0 || surface.height() < 55.0 {
            return;
        }
        let plot = egui::Rect::from_min_max(
            surface.min + egui::vec2(66.0, 10.0),
            surface.max - egui::vec2(12.0, 24.0),
        );
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll != 0.0 {
                state.time_window =
                    (state.time_window * (-f64::from(scroll) * 0.005).exp()).clamp(0.01, 3600.0);
            }
        }
        if response.dragged() {
            if state.follow {
                state.graph_end = state.latest_time;
                state.follow = false;
            }
            state.graph_end = (state.graph_end
                - f64::from(ui.input(|input| input.pointer.delta().x) / plot.width())
                    * state.time_window)
                .max(0.0);
        }
        let end = if state.follow {
            state.latest_time
        } else {
            state.graph_end
        };
        let start = (end - state.time_window).max(0.0);
        let end = (start + state.time_window).max(end);
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for series in &state.series {
            for point in series
                .points
                .iter()
                .filter(|point| point[0] >= start && point[0] <= end)
            {
                min = min.min(point[1]);
                max = max.max(point[1]);
            }
        }
        if !min.is_finite() {
            min = 0.0;
            max = 1.0;
        }
        let scale = min.abs().max(max.abs()).max(1.0);
        min /= scale;
        max /= scale;
        let span = (max - min).max(min.abs() * 0.01).max(0.001);
        min -= span * 0.08;
        max += span * 0.08;
        let painter = ui.painter().with_clip_rect(surface);
        for step in 0..=4 {
            let fraction = step as f32 / 4.0;
            let y = egui::lerp(plot.y_range(), fraction);
            painter.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                egui::Stroke::new(1.0_f32, GRID),
            );
            let value = (max - f64::from(fraction) * (max - min)) * scale;
            let label = if value.abs() >= 1e5 || value != 0.0 && value.abs() < 0.001 {
                format!("{value:.1e}")
            } else {
                format!("{value:.3}")
            };
            painter.text(
                egui::pos2(plot.left() - 6.0, y),
                egui::Align2::RIGHT_CENTER,
                label,
                egui::FontId::monospace(10.0),
                TEXT_MUTED,
            );
            let x = egui::lerp(plot.x_range(), fraction);
            painter.line_segment(
                [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
                egui::Stroke::new(1.0_f32, GRID),
            );
            painter.text(
                egui::pos2(x, plot.bottom() + 5.0),
                if step == 0 {
                    egui::Align2::LEFT_TOP
                } else if step == 4 {
                    egui::Align2::RIGHT_TOP
                } else {
                    egui::Align2::CENTER_TOP
                },
                format!("{:.2}s", start + f64::from(fraction) * (end - start)),
                egui::FontId::monospace(10.0),
                TEXT_MUTED,
            );
        }
        let painter = painter.with_clip_rect(plot);
        for series in &state.series {
            let color = egui::Color32::from_rgb(series.color[0], series.color[1], series.color[2]);
            let points: Vec<_> = series
                .points
                .iter()
                .filter(|point| point[0] >= start && point[0] <= end)
                .map(|point| {
                    egui::pos2(
                        plot.left() + ((point[0] - start) / (end - start)) as f32 * plot.width(),
                        plot.bottom()
                            - ((point[1] / scale - min) / (max - min)) as f32 * plot.height(),
                    )
                })
                .collect();
            if points.len() == 1 {
                painter.circle_filled(points[0], 2.5, color);
            } else if !points.is_empty() {
                painter.add(egui::Shape::line(points, egui::Stroke::new(1.5_f32, color)));
            }
        }
        if state.series.iter().all(|series| series.points.is_empty()) {
            painter.text(
                plot.center(),
                egui::Align2::CENTER_CENTER,
                if state.series.is_empty()
                    && (state.settings.mode == SerialMode::Text && state.settings.fields.is_empty()
                        || state.settings.mode == SerialMode::Binary
                            && state.settings.schema_json.is_empty())
                {
                    "Raw data only · add text fields or load a binary schema to plot"
                } else if state.series.is_empty() {
                    "Start receiving or open a raw capture to plot configured fields"
                } else {
                    "Waiting for matching values…"
                },
                egui::FontId::proportional(13.0),
                TEXT_MUTED,
            );
        }
        if let Some(pointer) = response
            .hover_pos()
            .filter(|position| plot.contains(*position))
        {
            let time = start + f64::from((pointer.x - plot.left()) / plot.width()) * (end - start);
            painter.line_segment(
                [
                    egui::pos2(pointer.x, plot.top()),
                    egui::pos2(pointer.x, plot.bottom()),
                ],
                egui::Stroke::new(1.0_f32, TEXT_MUTED),
            );
            response.on_hover_ui(|ui| {
                ui.label(format!("{time:.4} s"));
                for series in &state.series {
                    if let Some(point) = series
                        .points
                        .iter()
                        .min_by(|a, b| (a[0] - time).abs().total_cmp(&(b[0] - time).abs()))
                    {
                        ui.label(format!("{}: {:.6}", series.name, point[1]));
                    }
                }
            });
        }
    });
}

fn paint_raw(ui: &mut egui::Ui, state: &mut SerialState) {
    view_card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("RAW DATA").strong().color(TEXT_MUTED));
            ui.label(
                RichText::new(if state.settings.mode == SerialMode::Text {
                    "ASCII"
                } else {
                    "HEX + ASCII"
                })
                .small()
                .color(ACCENT),
            );
        });
        if state.raw.is_empty() {
            ui.label(
                RichText::new("Start receiving or open a raw capture to inspect incoming bytes.")
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
            app.serial.settings.mode = mode;
            app.serial.prepare().unwrap();
            app.serial.ingest(1.0, b"temperature=42\n");
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
    fn splitter_accepts_small_drag_steps_and_persists_height() {
        let mut app = crate::app::test_support::app();
        app.active_workspace_view = Some(WorkspaceView::Serial);
        app.serial.settings.fields.push(TextField {
            name: "temperature".into(),
            color: COLORS[0],
        });
        app.serial.prepare().unwrap();
        for index in 0..100 {
            app.serial.ingest(
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
        assert!(app.serial.settings.graph_percent > 68);
        assert_eq!(
            app.saved_capture_settings
                .global
                .serial
                .as_ref()
                .unwrap()
                .graph_percent,
            app.serial.settings.graph_percent
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
                app.serial.settings.mode = mode;
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
