//! WeAct supply workspace, composed entirely with the common instrument shell.
use super::{
    frame_actions::FrameActions,
    manual_value::ManualValue,
    plot_hover::{paint_plot_hover, plot_hover_pointer},
    view_shell::{
        settings_card, setup_section, stacked_view_splitter, view_card, view_toolbar,
        workspace_panel, PANE_SPLITTER_HEIGHT,
    },
};
use crate::{
    egui,
    pd_power::{input_mode, output_mode, PowerState},
    toolbar_action_button, RichText, SelectableHoverText, WaveCraftApp, WorkspaceView, ACCENT,
    GRID, SCOPE_COLORS, TEXT_MUTED,
};
use wavecraft_serial::pd_power::{Request, BAUD_RATES, MAX_POLL_HZ, MIN_POLL_HZ, UART_BAUD_RATES};

const FIELD_WIDTH: f32 = 96.0;
const LABEL_WIDTH: f32 = 112.0;

impl WaveCraftApp {
    pub(crate) fn pd_power_view_is_active(&self) -> bool {
        self.active_workspace_view == Some(WorkspaceView::PdPower)
    }

    pub(crate) fn select_pd_power_view(&mut self) {
        if let Some(active) = self.active_waveform_tab {
            self.save_waveform_view_settings(active);
        }
        self.active_workspace_view = Some(WorkspaceView::PdPower);
        self.snapshot_plot_rect = None;
        if self.pd_power.ports.is_empty() {
            self.pd_power.refresh_ports();
        }
        self.save_global_settings();
    }

    pub(crate) fn paint_pd_power_toolbar(
        &mut self,
        context: &egui::Context,
        actions: &mut FrameActions,
    ) {
        let state = &self.pd_power;
        let toolbar = view_toolbar(
            context,
            "WeAct Studio PD Power Mini V1 Buck",
            self.pending_snapshot.is_none(),
            |ui| {
                actions.pd_power_refresh_ports |= ui
                    .add(toolbar_action_button("Refresh ports", TEXT_MUTED))
                    .clicked();
                if ui
                    .add_enabled(
                        state.connected,
                        toolbar_action_button("Read settings", TEXT_MUTED),
                    )
                    .clicked()
                {
                    actions.pd_power_requests.push(Request::Refresh);
                }
                actions.pd_power_clear |= ui
                    .add(toolbar_action_button("Clear graphs", TEXT_MUTED))
                    .clicked();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            state.connected,
                            toolbar_action_button("Output OFF", egui::Color32::LIGHT_RED),
                        )
                        .clicked()
                    {
                        write(actions, 2, vec![0]);
                    }
                    if ui
                        .add_enabled(state.connected, toolbar_action_button("Output ON", ACCENT))
                        .clicked()
                    {
                        write(actions, 2, vec![1]);
                    }
                    actions.pd_power_connect |= ui
                        .add_enabled(
                            state.session.is_some() || !state.settings.port.trim().is_empty(),
                            toolbar_action_button(
                                if state.session.is_some() {
                                    "Disconnect"
                                } else {
                                    "Connect"
                                },
                                ACCENT,
                            ),
                        )
                        .clicked();
                });
            },
        );
        self.settings_open |= toolbar.settings_clicked;
        actions.snapshot_clicked |= toolbar.snapshot_clicked;
    }

    pub(crate) fn paint_pd_power_setup(
        &mut self,
        context: &egui::Context,
        actions: &mut FrameActions,
    ) {
        self.show_setup_panel(
            context,
            WorkspaceView::PdPower,
            actions,
            |app, ui, actions| {
                editor(ui, &mut app.pd_power, actions);
                app.pd_power.settings.validate().is_ok()
            },
        );
        if let Some((command, data)) = self.pd_power.confirmation.clone() {
            let mut dismiss = false;
            egui::Window::new("Confirm device command").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(context, |ui| {
                ui.set_max_width(380.0);
                ui.label(match command { 0x40 => "Restart the device? The serial connection will close.", 0x45 => "Restore device factory defaults? Saved output settings will be replaced.", 0x47 => "Write factory calibration values? These values change measurement accuracy. Verify every value before continuing.", _ => "Write the LCD panel type to device memory?" });
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() { dismiss = true; }
                    if ui.add_enabled(self.pd_power.connected, egui::Button::new("Confirm")).clicked() { write(actions, command, data); dismiss = true; }
                });
            });
            if dismiss {
                self.pd_power.confirmation = None;
            }
        }
    }

    pub(crate) fn paint_pd_power_central(&mut self, context: &egui::Context) {
        workspace_panel(context, |ui| {
            let bounds = ui.available_rect_before_wrap();
            let gap = PANE_SPLITTER_HEIGHT + ui.spacing().item_spacing.y * 2.0;
            let total_height = (bounds.height() - gap).max(0.0);
            let minimum = 100.0_f32.min(total_height * 0.5);
            let upper_height =
                (total_height * self.pd_power.plot_fraction).clamp(minimum, total_height - minimum);
            for (index, title) in ["OUTPUT VOLTAGE", "OUTPUT CURRENT"].into_iter().enumerate() {
                let rect = egui::Rect::from_min_size(
                    bounds.min + egui::vec2(0.0, if index == 0 { 0.0 } else { upper_height + gap }),
                    egui::vec2(
                        bounds.width(),
                        if index == 0 {
                            upper_height
                        } else {
                            total_height - upper_height
                        },
                    ),
                );
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(("pd_plot", index))
                        .max_rect(rect),
                );
                view_card(&mut child, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(title).strong().color(TEXT_MUTED));
                        let value = self.pd_power.latest.map(|s| {
                            if index == 0 {
                                s.voltage_mv
                            } else {
                                s.current_ma
                            }
                        });
                        ui.label(
                            RichText::new(value.map_or_else(
                                || "—".into(),
                                |v| {
                                    format!(
                                        "{:.3} {}",
                                        f64::from(v) / 1000.0,
                                        if index == 0 { "V" } else { "A" }
                                    )
                                },
                            ))
                            .monospace()
                            .size(22.0)
                            .color(SCOPE_COLORS[index]),
                        );
                    });
                    if self.pd_power.points.is_empty() {
                        ui.label("Connect the supply to display live measurements.");
                    }
                    graph(ui, ui.available_rect_before_wrap(), &self.pd_power, index);
                });
            }
            let split_rect = egui::Rect::from_min_size(
                egui::pos2(
                    bounds.left(),
                    bounds.top() + upper_height + ui.spacing().item_spacing.y,
                ),
                egui::vec2(bounds.width(), PANE_SPLITTER_HEIGHT),
            );
            let mut split_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("pd_power_splitter")
                    .max_rect(split_rect),
            );
            let response =
                stacked_view_splitter(&mut split_ui, "Drag to resize Voltage and Current");
            if response.dragged() && total_height > 0.0 {
                let height = (upper_height + ui.input(|input| input.pointer.delta().y))
                    .clamp(minimum, total_height - minimum);
                self.pd_power.plot_fraction = height / total_height;
            }
            if response.drag_stopped() {
                self.pd_power.layout_changed = true;
            }
            ui.advance_cursor_after_rect(bounds);
        });
    }

    pub(crate) fn apply_pd_power_actions(&mut self, actions: &FrameActions) {
        if actions.pd_power_refresh_ports {
            self.pd_power.refresh_ports();
        }
        if actions.pd_power_connect {
            if self.pd_power.session.is_some() {
                self.pd_power.stop();
            } else if let Err(error) = self.pd_power.start() {
                self.pd_power.error = Some(error.to_string());
            }
        }
        if actions.pd_power_clear {
            self.pd_power.points.clear();
        }
        for request in &actions.pd_power_requests {
            self.pd_power.request(request.clone());
        }
    }
}

fn write(actions: &mut FrameActions, command: u8, data: Vec<u8>) {
    actions
        .pd_power_requests
        .push(Request::Write { command, data });
}

fn row<R>(
    ui: &mut egui::Ui,
    label: &str,
    unit: &str,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.push_id(label, |ui| {
        ui.horizontal(|ui| {
            ui.add_sized([LABEL_WIDTH, 22.0], egui::Label::new(label).wrap());
            let result = ui
                .allocate_ui_with_layout(
                    egui::vec2(FIELD_WIDTH, 22.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_width(FIELD_WIDTH);
                        ui.spacing_mut().interact_size.x = FIELD_WIDTH;
                        content(ui)
                    },
                )
                .inner;
            ui.label(unit);
            result
        })
        .inner
    })
    .inner
}

fn number<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    unit: &str,
    value: &mut N,
    range: std::ops::RangeInclusive<N>,
    decimals: usize,
) -> bool {
    row(ui, label, unit, |ui| {
        ui.add_sized(
            [FIELD_WIDTH, 22.0],
            ManualValue::new(value)
                .range(range)
                .min_decimals(decimals)
                .max_decimals(decimals),
        )
        .changed()
    })
}

fn readout(ui: &mut egui::Ui, label: &str, value: impl Into<String>, unit: &str) {
    row(ui, label, unit, |ui| {
        ui.add_sized(
            [FIELD_WIDTH, 22.0],
            egui::Label::new(RichText::new(value.into()).monospace()).wrap(),
        )
    });
}

fn field_combo(id: &str) -> egui::ComboBox {
    egui::ComboBox::from_id_salt(id)
        .width(FIELD_WIDTH)
        .truncate()
}

fn editor(ui: &mut egui::Ui, state: &mut PowerState, actions: &mut FrameActions) {
    settings_card(ui, |ui| {
        setup_section(ui, "CONNECTION", true, |ui| {
            ui.add_enabled_ui(state.session.is_none(), |ui| {
                row(ui, "Port", "", |ui| {
                    field_combo("port")
                        .selected_text(if state.settings.port.is_empty() {
                            "Select…"
                        } else {
                            &state.settings.port
                        })
                        .show_ui(ui, |ui| {
                            for port in &state.ports {
                                ui.selectable_value(&mut state.settings.port, port.clone(), port);
                            }
                        });
                });
                row(ui, "Baud rate", "", |ui| {
                    field_combo("baud")
                        .selected_text(state.settings.baud_rate.to_string())
                        .show_ui(ui, |ui| {
                            for &baud in if state.settings.crc8 {
                                UART_BAUD_RATES.as_slice()
                            } else {
                                BAUD_RATES.as_slice()
                            } {
                                ui.selectable_value(
                                    &mut state.settings.baud_rate,
                                    baud,
                                    baud.to_string(),
                                );
                            }
                        });
                });
                row(ui, "Protocol", "", |ui| {
                    field_combo("protocol")
                        .selected_text(if state.settings.crc8 {
                            "UART CRC8"
                        } else {
                            "USB CDC"
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut state.settings.crc8, false, "USB CDC");
                            if ui
                                .selectable_value(&mut state.settings.crc8, true, "UART CRC8")
                                .changed()
                                && !UART_BAUD_RATES.contains(&state.settings.baud_rate)
                            {
                                state.settings.baud_rate = 115200;
                            }
                        });
                });
            });
            if number(
                ui,
                "Polling rate",
                "Hz",
                &mut state.settings.poll_hz,
                MIN_POLL_HZ..=MAX_POLL_HZ,
                1,
            ) {
                actions
                    .pd_power_requests
                    .push(Request::PollRate(state.settings.poll_hz));
            }
            number(
                ui,
                "Time window",
                "s",
                &mut state.settings.window_seconds,
                1.0..=3600.0,
                1,
            );
            readout(
                ui,
                "Actual rate",
                state
                    .measured_hz()
                    .map_or_else(|| "—".into(), |v| format!("{v:.1}")),
                "Hz",
            );
            ui.label("0.1–100 Hz requested; actual rate depends on the device and port. The API specifies no guaranteed sampling rate.").on_hover_selectable_text("Two serial reads per point, plus input state when supported by the firmware. Firmware filtering also affects the measurement response.");
        });
        ui.add_space(10.0);
        setup_section(ui, "LIVE STATE", true, |ui| {
            let latest = state.latest;
            readout(ui, "Regulation", latest.map_or("—", output_mode), "");
            readout(
                ui,
                "Output",
                latest.map_or("—", |s| if s.output_state & 1 != 0 { "ON" } else { "OFF" }),
                "",
            );
            readout(
                ui,
                "Input source",
                latest.map_or("—", |s| input_mode(s.input_state)),
                "",
            );
            readout(
                ui,
                "Input voltage",
                latest.filter(|s| s.input_state != u8::MAX).map_or_else(
                    || "—".into(),
                    |s| format!("{:.3}", f64::from(s.input_voltage_mv) / 1000.0),
                ),
                "V",
            );
            readout(
                ui,
                "PD requested",
                latest.filter(|s| s.input_state != u8::MAX).map_or_else(
                    || "—".into(),
                    |s| format!("{:.1}", f64::from(s.requested_pd_decivolts) / 10.0),
                ),
                "V",
            );
            readout(
                ui,
                "Power",
                latest.map_or_else(
                    || "—".into(),
                    |s| {
                        format!(
                            "{:.3}",
                            f64::from(s.voltage_mv) * f64::from(s.current_ma) / 1e6
                        )
                    },
                ),
                "W",
            );
            readout(
                ui,
                "Timer sequence",
                latest.map_or("—", |s| {
                    if s.output_state & 8 != 0 {
                        "Running"
                    } else {
                        "Stopped"
                    }
                }),
                "",
            );
        });
        ui.add_space(10.0);
        setup_section(ui, "OUTPUT / CC · CV", state.connected, |ui| {
            ui.label("CV holds the target voltage. CC limits current automatically when the load reaches the current limit.");
            let device = &mut state.settings.device;
            readout(ui, "Active preset", format!("M{}", device.preset), "");
            row(ui, "Edit preset", "", |ui| {
                field_combo("preset")
                    .selected_text(format!("M{}", state.preset_editor))
                    .show_ui(ui, |ui| {
                        for index in 0..5 {
                            ui.selectable_value(
                                &mut state.preset_editor,
                                index,
                                format!("M{index}"),
                            );
                        }
                    });
            });
            let preset = &mut device.presets[state.preset_editor];
            let mut voltage = f64::from(preset[0]) / 1000.0;
            let mut current = f64::from(preset[1]) / 1000.0;
            if number(ui, "Voltage target", "V", &mut voltage, 0.5..=20.0, 3) {
                preset[0] = (voltage * 1000.0).round() as u16;
            }
            if number(ui, "Current limit", "A", &mut current, 0.005..=3.1, 3) {
                preset[1] = (current * 1000.0).round() as u16;
            }
            ui.horizontal(|ui| {
                if ui.button("Write preset").clicked() {
                    let mut data = vec![state.preset_editor as u8];
                    data.extend_from_slice(&preset[0].to_le_bytes());
                    data.extend_from_slice(&preset[1].to_le_bytes());
                    write(actions, 4, data);
                }
                if ui.button("Activate preset").clicked() {
                    write(actions, 3, vec![state.preset_editor as u8]);
                }
            });
        });
        ui.add_space(10.0);
        setup_section(ui, "INPUT / PD", state.connected, |ui| {
            if state.connected && !state.settings.device.input_state_supported {
                ui.label("Input voltage and PD state are unavailable in this firmware.");
            }
            let can_pd = state.settings.device.supports_pd()
                && state.latest.is_some_and(|s| {
                    s.output_state & 1 == 0 && s.voltage_mv < 5000 && s.input_state == 5
                });
            ui.add_enabled_ui(can_pd, |ui| {
                let mut voltage =
                    f64::from(state.settings.device.pd_decivolts.unwrap_or(200)) / 10.0;
                if number(ui, "PD target", "V", &mut voltage, 8.0..=24.0, 1) {
                    state.settings.device.pd_decivolts = Some((voltage * 10.0).round() as u16);
                }
                if ui.button("Request PD voltage").clicked() {
                    write(
                        actions,
                        10,
                        state
                            .settings
                            .device
                            .pd_decivolts
                            .unwrap_or(200)
                            .to_le_bytes()
                            .to_vec(),
                    );
                }
            });
            ui.label("Requires PD, output OFF, measured output < 5 V and supported firmware.");
        });
        ui.add_space(10.0);
        setup_section(ui, "DEVICE SETTINGS", state.connected, |ui| {
            for (command, label) in [
                (6, "OCP protection"),
                (7, "Calibration"),
                (11, "OC discharge"),
                (12, "Auto output"),
                (14, "Compensation"),
            ] {
                if let Some(value) = &mut state.settings.device.values[command] {
                    row(ui, label, "", |ui| {
                        let mut enabled = *value != 0;
                        ui.checkbox(&mut enabled, "Enabled");
                        *value = u8::from(enabled);
                    });
                }
            }
            if let Some(value) = &mut state.settings.device.values[8] {
                number(ui, "Brightness", "%", value, 1..=100, 0);
            }
            if let Some(value) = &mut state.settings.device.values[13] {
                number(ui, "Filter window", "", value, 1..=200, 0);
            }
            if ui.button("Apply device settings").clicked() {
                for command in [6, 7, 8, 11, 12, 13, 14] {
                    if let Some(value) = state.settings.device.values[command] {
                        write(actions, command as u8, vec![value]);
                    }
                }
            }
            ui.horizontal(|ui| {
                ui.label("Voltage discharge");
                if ui.button("Enable").clicked() {
                    write(actions, 9, vec![1]);
                }
                if ui.button("Disable").clicked() {
                    write(actions, 9, vec![0]);
                }
            });
            ui.label("Voltage discharge has a write command only; its state cannot be read.");
        });
        ui.add_space(10.0);
        setup_section(ui, "OUTPUT TIMER", state.connected, |ui| {
            if let Some(timer) = &mut state.settings.device.timer {
                row(ui, "Timer mode", "", |ui| {
                    let mut enabled = timer[0] != 0;
                    ui.checkbox(&mut enabled, "Enabled");
                    timer[0] = u16::from(enabled);
                });
                number(ui, "ON duration", "s", &mut timer[1], 0..=59999, 0);
                number(ui, "OFF duration", "s", &mut timer[2], 0..=59999, 0);
                if ui.button("Apply timer").clicked() {
                    let mut data = vec![timer[0] as u8];
                    data.extend_from_slice(&timer[1].to_le_bytes());
                    data.extend_from_slice(&timer[2].to_le_bytes());
                    write(actions, 15, data);
                }
                ui.label("OFF = 0: one shot; ON = 0: no start. Output ON/OFF starts or stops the sequence.");
            } else {
                ui.label("Requires V1 ≥ 1.0.3.0 or V1.1 ≥ 1.0.2.0 firmware.");
            }
        });
        ui.add_space(10.0);
        setup_section(ui, "DEVICE INFORMATION", state.connected, |ui| {
            ui.label(&state.settings.device.model);
            row(ui, "Firmware", "", |ui| {
                ui.label(&state.settings.device.version);
            });
            ui.label(format!("Serial: {}", state.settings.device.serial));
            if ui.button("Store setup in device").clicked() {
                write(actions, 0x44, vec![]);
            }
            ui.label("Stores the current device configuration in its memory. Save settings below saves the WaveCraft workspace.");
            ui.collapsing("Service / calibration", |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Restart device").clicked() {
                        state.confirmation = Some((0x40, vec![]));
                    }
                    if ui.button("Factory reset").clicked() {
                        state.confirmation = Some((0x45, vec![]));
                    }
                });
                if let Some(lcd) = &mut state.settings.device.lcd_type {
                    row(ui, "LCD type", "", |ui| {
                        field_combo("lcd")
                            .selected_text(if *lcd == 0 { "HAN" } else { "BOE" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(lcd, 0, "HAN");
                                ui.selectable_value(lcd, 1, "BOE");
                            });
                    });
                    if ui.button("Write LCD type…").clicked() {
                        state.confirmation = Some((0x46, vec![*lcd]));
                    }
                }
                if let Some(data) = &mut state.settings.device.factory_data {
                    ui.label("Factory calibration: raw 16-bit values. Reserved word is preserved.");
                    for (i, value) in data.iter_mut().enumerate().take(31) {
                        let label = if i < 14 {
                            format!("V{} {}", i / 2, if i % 2 == 0 { "display" } else { "set" })
                        } else if i < 28 {
                            format!(
                                "I{} {}",
                                (i - 14) / 2,
                                if i % 2 == 0 { "display" } else { "set" }
                            )
                        } else {
                            ["Zero current S1", "Zero current S0", "LCD calibration"][i - 28]
                                .to_owned()
                        };
                        number(ui, &label, "", value, 0..=u16::MAX, 0);
                    }
                    if ui.button("Write calibration…").clicked() {
                        state.confirmation =
                            Some((0x47, data.iter().flat_map(|v| v.to_le_bytes()).collect()));
                    }
                }
            });
        });
    });
}

fn graph(ui: &mut egui::Ui, bounds: egui::Rect, state: &PowerState, channel: usize) {
    if bounds.width() < 100.0 || bounds.height() < 50.0 {
        return;
    }
    let response = ui.allocate_rect(bounds, egui::Sense::hover());
    let plot = bounds.shrink2(egui::vec2(12.0, 14.0));
    let plot = egui::Rect::from_min_max(
        plot.min + egui::vec2(48.0, 0.0),
        plot.max - egui::vec2(4.0, 14.0),
    );
    if !plot.is_positive() {
        return;
    }
    let end = state
        .points
        .back()
        .map_or(state.settings.window_seconds, |p| {
            p.time.max(state.settings.window_seconds)
        });
    let start = end - state.settings.window_seconds;
    let value = |s: wavecraft_core::PowerSample| {
        f64::from(if channel == 0 {
            s.voltage_mv
        } else {
            s.current_ma
        }) / 1000.0
    };
    let max = state
        .points
        .iter()
        .filter(|p| p.time >= start)
        .map(|p| value(p.sample))
        .fold(if channel == 0 { 1.0_f64 } else { 0.1_f64 }, f64::max)
        * 1.1;
    let painter = ui.painter_at(bounds);
    for i in 0..=4 {
        let ratio = i as f32 / 4.0;
        let y = egui::lerp(plot.bottom()..=plot.top(), ratio);
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            egui::Stroke::new(1.0_f32, GRID),
        );
        ui.put(
            egui::Rect::from_min_size(egui::pos2(bounds.left(), y - 9.0), egui::vec2(50.0, 18.0)),
            egui::Label::new(
                RichText::new(format!("{:.2}", max * f64::from(ratio)))
                    .monospace()
                    .size(11.0),
            ),
        );
        let x = egui::lerp(plot.left()..=plot.right(), ratio);
        painter.line_segment(
            [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
            egui::Stroke::new(1.0_f32, GRID),
        );
        ui.put(
            egui::Rect::from_min_size(
                egui::pos2(
                    (x - 30.0).clamp(bounds.left(), bounds.right() - 60.0),
                    plot.bottom() + 3.0,
                ),
                egui::vec2(60.0, 18.0),
            ),
            egui::Label::new(
                RichText::new(format!(
                    "{:.1} s",
                    start + f64::from(ratio) * state.settings.window_seconds
                ))
                .size(11.0),
            ),
        );
    }
    let mut points = Vec::new();
    let mut previous_time = None;
    let mut pixel = i32::MIN;
    let gap_limit = (3.0 / state.settings.poll_hz).max(0.5);
    let flush = |points: &mut Vec<egui::Pos2>| {
        if points.len() > 1 {
            painter.with_clip_rect(plot).add(egui::Shape::line(
                std::mem::take(points),
                egui::Stroke::new(1.5_f32, SCOPE_COLORS[channel]),
            ));
        } else {
            points.clear();
        }
    };
    for point in state
        .points
        .iter()
        .filter(|p| p.time >= start && p.time <= end)
    {
        if previous_time.is_some_and(|time| point.time - time > gap_limit) {
            flush(&mut points);
            pixel = i32::MIN;
        }
        previous_time = Some(point.time);
        let x = plot.left()
            + ((point.time - start) / state.settings.window_seconds) as f32 * plot.width();
        let y = plot.bottom() - (value(point.sample) / max) as f32 * plot.height();
        // Bound drawing work to screen width while retaining the latest point.
        if x as i32 != pixel {
            points.push(egui::pos2(x, y));
            pixel = x as i32;
        } else if let Some(last) = points.last_mut() {
            *last = egui::pos2(x, y);
        }
    }
    flush(&mut points);
    if let Some(pointer) = plot_hover_pointer(ui, &response, plot) {
        let time = start
            + f64::from((pointer.x - plot.left()) / plot.width()) * state.settings.window_seconds;
        if let Some(point) = state
            .points
            .iter()
            .filter(|p| p.time >= start)
            .min_by(|a, b| (a.time - time).abs().total_cmp(&(b.time - time).abs()))
        {
            paint_plot_hover(
                ui,
                pointer,
                bounds,
                &[(
                    format!(
                        "{:.3} s · {:.3} {}",
                        point.time,
                        value(point.sample),
                        if channel == 0 { "V" } else { "A" }
                    ),
                    SCOPE_COLORS[channel],
                )],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::test_support::app, capture_settings::CaptureSettingsDocument, pd_power::Point,
    };
    use wavecraft_core::PowerSample;

    fn frame(app: &mut WaveCraftApp, context: &egui::Context) -> egui::FullOutput {
        frame_with_events(app, context, vec![])
    }

    fn frame_with_events(
        app: &mut WaveCraftApp,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |context| app.run_frame(context),
        )
    }
    fn text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
        output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == label => Some(t.visual_bounding_rect()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing label {label}"))
    }

    #[test]
    fn uses_shared_setup_footer_and_stacks_voltage_above_current() {
        let mut app = app();
        app.active_workspace_view = Some(WorkspaceView::PdPower);
        app.pd_power.latest = Some(PowerSample {
            voltage_mv: 5000,
            current_ma: 250,
            ..Default::default()
        });
        app.pd_power.points.push_back(Point {
            time: 1.0,
            sample: app.pd_power.latest.unwrap(),
        });
        let context = egui::Context::default();
        crate::configure_style(&context);
        let _ = frame(&mut app, &context);
        let output = frame(&mut app, &context);
        let voltage = text_rect(&output, "OUTPUT VOLTAGE");
        let current = text_rect(&output, "OUTPUT CURRENT");
        assert!(voltage.top() < current.top());
        assert_eq!(voltage.left(), current.left());
        assert!(voltage.left() > 300.0);
        text_rect(&output, "5.000 V");
        text_rect(&output, "0.250 A");
        let save = text_rect(&output, "💾 Save settings");
        assert!(save.top() > 800.0 && save.left() < 300.0);
        assert!(app.snapshot_plot_rect.unwrap().contains_rect(current));
        assert_eq!(output.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text().contains("Snapshot"))).count(), 1);
    }

    #[test]
    fn plot_cards_retain_workspace_margins_with_setup_open_or_collapsed() {
        for open in [true, false] {
            let mut app = app();
            app.active_workspace_view = Some(WorkspaceView::PdPower);
            app.workspace_view_layout
                .set_setup_panel_open(WorkspaceView::PdPower, open);
            let context = egui::Context::default();
            crate::configure_style(&context);
            let _ = frame(&mut app, &context);
            let output = frame(&mut app, &context);
            let cards = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.stroke.color == GRID
                            && rect.rect.width() > 500.0
                            && rect.rect.height() > 200.0 =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(cards.len(), 2);
            for card in &cards {
                assert!((card.right() - 1384.0).abs() < 1.0, "{card:?}");
            }
            assert!(cards[0].bottom() < cards[1].top(), "cards must not touch");
        }
    }

    #[test]
    fn shared_splitter_resizes_both_graphs_and_persists_on_release() {
        let mut app = app();
        app.active_workspace_view = Some(WorkspaceView::PdPower);
        let context = egui::Context::default();
        crate::configure_style(&context);
        let _ = frame(&mut app, &context);
        let output = frame(&mut app, &context);
        let splitter = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::LineSegment { points, stroke }
                    if stroke.color == egui::epaint::ColorMode::Solid(GRID)
                        && (points[0].x - 324.0).abs() < 1.0
                        && (points[1].x - points[0].x) > 500.0
                        && points[0].y == points[1].y =>
                {
                    Some(points[0].lerp(points[1], 0.5))
                }
                _ => None,
            })
            .expect("shared splitter line");
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = frame_with_events(
            &mut app,
            &context,
            vec![egui::Event::PointerMoved(splitter), button(splitter, true)],
        );
        let moved = splitter + egui::vec2(0.0, 60.0);
        let _ = frame_with_events(&mut app, &context, vec![egui::Event::PointerMoved(moved)]);
        let _ = frame_with_events(&mut app, &context, vec![button(moved, false)]);
        assert!(app.pd_power.plot_fraction > 0.55);
        assert_eq!(
            app.saved_capture_settings.global.pd_power_plot_fraction,
            Some(app.pd_power.plot_fraction)
        );
        let restored = WaveCraftApp::with_settings(None, app.saved_capture_settings.clone());
        assert_eq!(restored.pd_power.plot_fraction, app.pd_power.plot_fraction);
    }

    #[test]
    fn numeric_fields_keep_fixed_dimensions_for_different_values() {
        let context = egui::Context::default();
        crate::configure_style(&context);
        let mut rectangles = Vec::new();
        for mut value in [0.5_f64, 20.0] {
            let _ = context.run(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    row(ui, "Voltage target", "V", |ui| {
                        rectangles.push(
                            ui.add_sized(
                                [FIELD_WIDTH, 22.0],
                                ManualValue::new(&mut value).min_decimals(3).max_decimals(3),
                            )
                            .rect,
                        );
                    });
                });
            });
        }
        assert_eq!(rectangles[0], rectangles[1]);
        assert_eq!(rectangles[0].width(), FIELD_WIDTH);
    }

    #[test]
    fn dropdowns_and_numeric_editors_use_the_same_fixed_width() {
        let context = egui::Context::default();
        crate::configure_style(&context);
        let mut rectangles = Vec::new();
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                for (id, text) in [
                    ("short", "M2"),
                    ("long", "/dev/cu.usbmodemacdec616b2961"),
                    ("baud", "3000000"),
                    ("protocol", "UART CRC8"),
                ] {
                    row(ui, id, "", |ui| {
                        rectangles.push(
                            field_combo(id)
                                .selected_text(text)
                                .show_ui(ui, |_| {})
                                .response
                                .rect,
                        );
                    });
                }
                let mut value = 5.0;
                row(ui, "numeric", "V", |ui| {
                    rectangles.push(
                        ui.add_sized(
                            [FIELD_WIDTH, 22.0],
                            ManualValue::new(&mut value).min_decimals(3),
                        )
                        .rect,
                    );
                });
            });
        });
        for rect in &rectangles {
            assert_eq!(rect.width(), FIELD_WIDTH, "{rect:?}");
            assert_eq!(rect.left(), rectangles[0].left());
        }
    }

    #[test]
    fn setup_sections_share_one_background_and_collapse_while_disconnected() {
        let mut app = app();
        app.active_workspace_view = Some(WorkspaceView::PdPower);
        let context = egui::Context::default();
        crate::configure_style(&context);
        context.style_mut(|style| style.animation_time = 0.0);
        let _ = frame(&mut app, &context);
        let output = frame(&mut app, &context);
        assert!(!output.shapes.iter().any(
            |shape| matches!(&shape.shape, egui::Shape::Text(t) if t.galley.text() == "Port path")
        ));
        let backgrounds = output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == crate::BACKGROUND && rect.rect.left() < 300.0 && rect.rect.width() > 240.0 && rect.rect.height() > 100.0)).count();
        assert_eq!(backgrounds, 1);
        let position = text_rect(&output, "CONNECTION").center();
        for pressed in [true, false] {
            let _ = frame_with_events(
                &mut app,
                &context,
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
        let output = frame(&mut app, &context);
        assert!(!output.shapes.iter().any(
            |shape| matches!(&shape.shape, egui::Shape::Text(t) if t.galley.text() == "Port")
        ));
        text_rect(&output, "CONNECTION");
        text_rect(&output, "LIVE STATE");
        text_rect(&output, "💾 Save settings");
    }

    #[test]
    fn saves_full_workspace_restores_tab_and_keeps_error_visible_when_collapsed() {
        let path =
            std::env::temp_dir().join(format!("wavecraft-pd-settings-{}.json", std::process::id()));
        let repository = crate::CaptureSettingsRepository::at(path.clone());
        let mut app =
            WaveCraftApp::with_settings(Some(repository), CaptureSettingsDocument::default());
        app.active_workspace_view = Some(WorkspaceView::PdPower);
        app.pd_power.settings.poll_hz = 25.0;
        app.pd_power.settings.device.presets[2] = [12000, 1000];
        app.save_setup_settings(WorkspaceView::PdPower);
        app.workspace_view_layout
            .set_setup_panel_open(WorkspaceView::PdPower, false);
        app.save_global_settings();
        let document: CaptureSettingsDocument =
            serde_json::from_str(&serde_json::to_string(&app.saved_capture_settings).unwrap())
                .unwrap();
        let mut restored = WaveCraftApp::with_settings(None, document);
        assert!(restored.pd_power_view_is_active());
        assert_eq!(restored.pd_power.settings, app.pd_power.settings);
        assert!(!restored
            .workspace_view_layout
            .setup_panel_open(WorkspaceView::PdPower));
        restored.pd_power.error = Some("WeAct connection lost".into());
        let context = egui::Context::default();
        crate::configure_style(&context);
        let output = frame(&mut restored, &context);
        assert!(text_rect(&output, "WeAct connection lost").top() > 800.0);
        text_rect(&output, "OUTPUT VOLTAGE");
        std::fs::remove_file(path).unwrap();
    }
}
