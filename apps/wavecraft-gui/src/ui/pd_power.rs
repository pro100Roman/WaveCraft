//! WeAct supply workspace, composed entirely with the common instrument shell.
use super::{
    baud_rate::paint_baud_rate_selector,
    frame_actions::FrameActions,
    manual_value::ManualValue,
    plot_hover::{paint_plot_hover, plot_hover_pointer},
    view_shell::{
        primary_toolbar_button, settings_card, setup_field_row, setup_section,
        stacked_view_splitter, view_card, view_toolbar, workspace_panel, ToolbarSection,
        CONNECTION_FIELD_WIDTH, PANE_SPLITTER_HEIGHT, SETUP_FIELD_WIDTH as FIELD_WIDTH,
    },
};
use crate::{
    egui,
    pd_power::{input_mode, output_mode, PowerState},
    toolbar_action_button, RichText, SelectableHoverText, WaveCraftApp, WorkspaceView, ACCENT,
    GRID, SCOPE_COLORS, TEXT_MUTED,
};
use wavecraft_serial::pd_power::{Request, BAUD_RATES, MAX_POLL_HZ, MIN_POLL_HZ, UART_BAUD_RATES};

fn output_button(state: &PowerState) -> (&'static str, egui::Color32) {
    match state.output_is_on().filter(|_| state.connected) {
        Some(true) => ("ON", ACCENT),
        Some(false) => ("OFF", egui::Color32::LIGHT_RED),
        None => ("ON/OFF", TEXT_MUTED),
    }
}

fn section(ui: &mut egui::Ui, title: &str, enabled: bool, contents: impl FnOnce(&mut egui::Ui)) {
    let hint = match title {
        "CONNECTION" => "Select a USB CDC or UART port, then Connect. USB allows typed host baud rates; physical UART must match the rate selected in the device menu.",
        "LIVE STATE" => "Measured output state and input supply information. Input voltage and PD status are unavailable on early firmware. Timer state reports whether a sequence is running, including its OFF phase.",
        "OUTPUT / CC · CV" => "CV holds the target voltage. CC limits current automatically when the load reaches the current limit. Edit and write one of M0–M4, then activate it to use its voltage and current limits.",
        "INPUT / PD" => "PD voltage requests require PD input, output OFF, and measured output below 5 V. Supported from V1 ≥ 1.0.2.0 or V1.1 ≥ 1.0.1.0. Early firmware cannot report input voltage or PD state.",
        "DEVICE SETTINGS" => "Edit device protection, calibration, display and measurement filtering. Apply device settings writes edits; Store setup in device makes them survive power cycles. Unsupported settings are hidden for older firmware.",
        "OUTPUT TIMER" => "Available from V1 ≥ 1.0.3.0 or V1.1 ≥ 1.0.2.0. Apply timer writes the mode and durations. The toolbar ON/OFF button starts or stops the sequence. OFF = 0 is one shot; ON = 0 prevents starting.",
        _ => "Device identity and service controls. Store setup in device saves the current configuration to device memory; the pinned Save settings footer saves this WaveCraft view.",
    };
    setup_section(ui, title, enabled, contents).on_hover_selectable_text(hint);
}

fn control_help(label: &str) -> &'static str {
    match label {
        "Port" => "Serial port used by this view. Each connected PD Power view needs its own port. USB CDC and UART adapters are supported.",
        "Baud rate" => "Type a USB CDC host line-coding rate or select a standard speed. Physical UART must match one of the seven rates selected in the device menu (9600–460800 baud). This is not the graph update frequency.",
        "Protocol" => "USB CDC uses LF-terminated frames. Physical UART uses CRC8 and requires UART mode enabled in the device menu.",
        "Polling rate" => "Requested host polling rate: 0.1–100 Hz. The API guarantees no ADC sampling frequency. Two serial reads per point, plus input state when supported; slow responses reduce the actual rate without catch-up bursts.",
        "Time window" => "Visible graph duration in seconds, from 1 to 3600. Both voltage and current use the same time window.",
        "Actual rate" => "Measured receipt rate over the last 100 points. It may be lower than the requested polling rate and settles after a rate change.",
        "Regulation" | "Voltage target" | "Current limit" => "CV regulates the target voltage; CC automatically limits current when the load reaches this limit. The input supply and buck headroom may constrain the actual voltage/current.",
        "Active preset" | "Edit preset" => "M0–M4 store voltage and current limits. Editing a preset does not activate it: write the preset, then use Activate preset.",
        "PD target" | "PD requested" => "Requested PD supply voltage, not the measured input voltage. Writing requires PD input, output OFF, measured output below 5 V, and supported firmware.",
        "Input voltage" | "Input source" => "Measured input voltage and PD/QC/DC negotiation state. Early firmware has no input-state read command; unavailable readings are shown as dashes.",
        "OCP protection" => "Over-current protection shuts the output off after approximately 200 ms above the current limit. With OCP disabled, the supply can remain in constant-current regulation.",
        "Calibration" => "Enable the stored factory calibration corrections for voltage/current setting and display. This switch does not rewrite the calibration table.",
        "OC discharge" => "Discharge after an over-current condition to reduce voltage overshoot when a constant-current load is suddenly removed. Requires newer firmware.",
        "Auto output" => "Automatically enable output when the device powers on. Apply writes the setting; Store setup in device persists it across power cycles.",
        "Compensation" => "Enable firmware output error compensation (up to ±0.02 V and ±0.003 A). Requires supported firmware.",
        "Brightness" => "Device LCD brightness from 1–100 percent. Apply device settings sends the edited value.",
        "Filter window" => "Moving-average measurement filter length, from 1–200 samples. A larger window smooths displayed voltage/current but responds more slowly to changes; it does not set host polling frequency.",
        "Voltage discharge" => "Write-only voltage-discharge control. The protocol provides no readback, so WaveCraft does not infer its current state from a click.",
        "Timer mode" => "Enable or disable the timed output sequence. Apply timer writes this mode and both durations. ON/OFF in the toolbar starts or stops the sequence. Requires V1 ≥ 1.0.3.0 or V1.1 ≥ 1.0.2.0.",
        "ON duration" => "Time output stays enabled in each timer cycle, in seconds (0–59999). Zero prevents the sequence from starting. Apply timer sends the edited duration.",
        "OFF duration" => "Time output stays disabled between cycles, in seconds (0–59999). Zero ends the sequence after one ON phase; a nonzero duration repeats ON/OFF cycles.",
        "Timer sequence" => "Reported timer activity, including its OFF phase. A running sequence may have its physical output temporarily disabled.",
        "LCD type" => "Select the actual display panel: HAN or BOE. Changing this device-memory setting requires confirmation.",
        _ => "",
    }
}

impl WaveCraftApp {
    pub(crate) fn pd_power_view_is_active(&self) -> bool {
        self.active_workspace_view
            .and_then(WorkspaceView::pd_power_index)
            .is_some()
    }

    pub(crate) fn active_pd_power_index(&self) -> usize {
        self.active_workspace_view
            .and_then(WorkspaceView::pd_power_index)
            .unwrap_or(0)
    }

    pub(crate) fn select_pd_power_view(&mut self, view: WorkspaceView) {
        let index = view.pd_power_index().unwrap_or(0);
        if let Some(active) = self.active_waveform_tab {
            self.save_waveform_view_settings(active);
        }
        self.active_workspace_view = Some(view);
        self.snapshot_plot_rect = None;
        if self.pd_power[index].ports.is_empty() {
            self.pd_power[index].refresh_ports();
        }
        self.save_global_settings();
    }

    pub(crate) fn paint_pd_power_toolbar(
        &mut self,
        context: &egui::Context,
        actions: &mut FrameActions,
    ) {
        let index = self.active_pd_power_index();
        let state = &self.pd_power[index];
        let toolbar = view_toolbar(
            context,
            "WeAct Studio PD Power Mini V1 Buck",
            self.pending_snapshot.is_none(),
            |ui, section| match section {
                ToolbarSection::Files => {
                    if ui
                        .add(toolbar_action_button("Refresh ports", TEXT_MUTED))
                        .clicked()
                    {
                        actions.pd_power_refresh_ports = Some(index);
                    }
                    if ui
                        .add_enabled(
                            state.connected,
                            toolbar_action_button("Read settings", TEXT_MUTED),
                        )
                        .clicked()
                    {
                        actions.pd_power_requests.push((index, Request::Refresh));
                    }
                    if ui
                        .add(toolbar_action_button("Clear graphs", TEXT_MUTED))
                        .clicked()
                    {
                        actions.pd_power_clear = Some(index);
                    }
                }
                ToolbarSection::Controls => {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (label, color) = output_button(state);
                    if ui.add_enabled(state.connected && state.latest.is_some() && !state.output_toggle_pending(), primary_toolbar_button(ui, label, color))
                        .on_hover_selectable_text("Shows the current physical output state read from the device, including changes made with its hardware button. ON is enabled; OFF is disabled, including a timer's OFF phase. Refreshes at the polling rate and changes only after fresh telemetry. Click to toggle output; in timer mode, starts or stops the sequence.")
                        .clicked() { actions.pd_power_toggle_output = Some(index); }
                });
                }
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
        let index = self.active_pd_power_index();
        self.show_setup_panel(
            context,
            WorkspaceView::PD_POWER[index],
            actions,
            |app, ui, actions| {
                editor(ui, &mut app.pd_power[index], index, actions);
                app.pd_power[index].edited_settings().is_ok()
            },
        );
        if let Some((command, data)) = self.pd_power[index].confirmation.clone() {
            let mut dismiss = false;
            egui::Window::new("Confirm device command").id(egui::Id::new(("pd_confirmation", index))).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(context, |ui| {
                ui.set_max_width(380.0);
                ui.label(match command { 0x40 => "Restart the device? The serial connection will close.", 0x45 => "Restore device factory defaults? Saved output settings will be replaced.", 0x47 => "Write factory calibration values? These values change measurement accuracy. Verify every value before continuing.", _ => "Write the LCD panel type to device memory?" });
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() { dismiss = true; }
                    if ui.add_enabled(self.pd_power[index].connected, egui::Button::new("Confirm")).clicked() { write(actions, index, command, data); dismiss = true; }
                });
            });
            if dismiss {
                self.pd_power[index].confirmation = None;
            }
        }
    }

    pub(crate) fn paint_pd_power_central(&mut self, context: &egui::Context) {
        let index = self.active_pd_power_index();
        workspace_panel(context, |ui| {
            let bounds = ui.available_rect_before_wrap();
            let gap = PANE_SPLITTER_HEIGHT + ui.spacing().item_spacing.y * 2.0;
            let total_height = (bounds.height() - gap).max(0.0);
            let minimum = 100.0_f32.min(total_height * 0.5);
            let upper_height = (total_height * self.pd_power[index].plot_fraction)
                .clamp(minimum, total_height - minimum);
            for (channel, title) in ["OUTPUT VOLTAGE", "OUTPUT CURRENT"].into_iter().enumerate() {
                let rect = egui::Rect::from_min_size(
                    bounds.min
                        + egui::vec2(
                            0.0,
                            if channel == 0 {
                                0.0
                            } else {
                                upper_height + gap
                            },
                        ),
                    egui::vec2(
                        bounds.width(),
                        if channel == 0 {
                            upper_height
                        } else {
                            total_height - upper_height
                        },
                    ),
                );
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(("pd_plot", index, channel))
                        .max_rect(rect),
                );
                view_card(&mut child, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(title).strong().color(TEXT_MUTED));
                        let value = self.pd_power[index].latest.map(|s| {
                            if channel == 0 {
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
                                        if channel == 0 { "V" } else { "A" }
                                    )
                                },
                            ))
                            .monospace()
                            .size(22.0)
                            .color(SCOPE_COLORS[channel]),
                        );
                    });
                    if self.pd_power[index].points.is_empty() {
                        ui.label("Connect the supply to display live measurements.");
                    }
                    graph(
                        ui,
                        ui.available_rect_before_wrap(),
                        &self.pd_power[index],
                        channel,
                    );
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
                self.pd_power[index].plot_fraction = height / total_height;
            }
            if response.drag_stopped() {
                self.pd_power[index].layout_changed = true;
            }
            ui.advance_cursor_after_rect(bounds);
        });
    }

    pub(crate) fn apply_pd_power_actions(&mut self, actions: &FrameActions) {
        if let Some(index) = actions.pd_power_refresh_ports {
            if let Some(state) = self.pd_power.get_mut(index) {
                state.refresh_ports();
            }
        }
        if let Some(index) = actions.pd_power_connect {
            let busy_port = self.pd_power.get(index).and_then(|state| {
                self.connected_serial_port_owner(
                    &state.settings.port,
                    WorkspaceView::PD_POWER[index],
                )
            });
            if let Some(state) = self.pd_power.get_mut(index) {
                if state.session.is_some() {
                    state.stop();
                } else if let Some(other) = busy_port {
                    state.error = Some(format!(
                        "This port is already connected in {}",
                        other.label()
                    ));
                } else if let Err(error) = state.start() {
                    state.error = Some(error.to_string());
                }
            }
        }
        if let Some(index) = actions.pd_power_toggle_output {
            if let Some(state) = self.pd_power.get_mut(index) {
                state.toggle_output();
            }
        }
        if let Some(index) = actions.pd_power_clear {
            if let Some(state) = self.pd_power.get_mut(index) {
                state.points.clear();
            }
        }
        for (index, request) in &actions.pd_power_requests {
            if let Some(state) = self.pd_power.get_mut(*index) {
                state.request(request.clone());
            }
        }
    }
}

fn write(actions: &mut FrameActions, index: usize, command: u8, data: Vec<u8>) {
    actions
        .pd_power_requests
        .push((index, Request::Write { command, data }));
}

fn row<R>(
    ui: &mut egui::Ui,
    label: &str,
    unit: &str,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    setup_field_row(ui, label, unit, control_help(label), content)
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
            [ui.available_width(), 22.0],
            ManualValue::new(value)
                .range(range)
                .min_decimals(decimals)
                .max_decimals(decimals),
        )
        .on_hover_selectable_text(control_help(label))
        .on_disabled_hover_selectable_text(control_help(label))
        .changed()
    })
}

fn readout(ui: &mut egui::Ui, label: &str, value: impl Into<String>, unit: &str) {
    row(ui, label, unit, |ui| {
        ui.add_sized(
            [ui.available_width(), 22.0],
            egui::Label::new(RichText::new(value.into()).monospace()).wrap(),
        )
    });
}

fn field_combo(id: &str) -> egui::ComboBox {
    egui::ComboBox::from_id_salt(id)
        .width(FIELD_WIDTH)
        .truncate()
}

fn editor(ui: &mut egui::Ui, state: &mut PowerState, index: usize, actions: &mut FrameActions) {
    settings_card(ui, |ui| {
        section(ui, "CONNECTION", true, |ui| {
            ui.spacing_mut().interact_size.x = CONNECTION_FIELD_WIDTH;
            ui.add_enabled_ui(state.session.is_none(), |ui| {
                row(ui, "Port", "", |ui| {
                    field_combo("port")
                        .width(CONNECTION_FIELD_WIDTH)
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
            });
            row(ui, "", "", |ui| {
                if ui.add_enabled(state.session.is_some() || !state.settings.port.trim().is_empty(), toolbar_action_button(if state.session.is_some() { "Disconnect" } else { "Connect" }, if state.session.is_some() { egui::Color32::LIGHT_RED } else { ACCENT }).min_size(egui::vec2(CONNECTION_FIELD_WIDTH,22.0)))
                    .on_hover_selectable_text("Open or close the selected USB CDC/UART connection. Connecting reads device settings without enabling the output.")
                    .clicked() { actions.pd_power_connect = Some(index); }
            });
            ui.add_enabled_ui(state.session.is_none(), |ui| {
                row(ui, "Baud rate", "", |ui| {
                    paint_baud_rate_selector(
                        ui,
                        &mut state.settings.baud_rate,
                        &mut state.baud_draft,
                        if state.settings.crc8 {
                            UART_BAUD_RATES.as_slice()
                        } else {
                            BAUD_RATES.as_slice()
                        },
                        !state.settings.crc8,
                    );
                });
                row(ui, "Protocol", "", |ui| {
                    field_combo("protocol")
                        .width(CONNECTION_FIELD_WIDTH)
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
                                state.baud_draft = None;
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
                    .push((index, Request::PollRate(state.settings.poll_hz)));
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
        });
        ui.add_space(10.0);
        section(ui, "LIVE STATE", true, |ui| {
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
        section(ui, "OUTPUT / CC · CV", state.connected, |ui| {
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
                    write(actions, index, 4, data);
                }
                if ui.button("Activate preset").clicked() {
                    write(actions, index, 3, vec![state.preset_editor as u8]);
                }
            });
        });
        ui.add_space(10.0);
        section(ui, "INPUT / PD", state.connected, |ui| {
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
                        index,
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
        });
        ui.add_space(10.0);
        section(ui, "DEVICE SETTINGS", state.connected, |ui| {
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
                        ui.checkbox(&mut enabled, "Enabled")
                            .on_hover_selectable_text(control_help(label));
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
            if ui.button("Apply device settings").on_hover_selectable_text("Write the edited protection, calibration, brightness, filter and compensation settings. Store setup in device saves them across power cycles.").clicked() {
                for command in [6, 7, 8, 11, 12, 13, 14] {
                    if let Some(value) = state.settings.device.values[command] {
                        write(actions, index, command as u8, vec![value]);
                    }
                }
            }
            ui.horizontal(|ui| {
                ui.label("Voltage discharge");
                if ui.button("Enable").clicked() {
                    write(actions, index, 9, vec![1]);
                }
                if ui.button("Disable").clicked() {
                    write(actions, index, 9, vec![0]);
                }
            })
            .response
            .on_hover_selectable_text(control_help("Voltage discharge"));
        });
        ui.add_space(10.0);
        section(ui, "OUTPUT TIMER", state.connected, |ui| {
            if let Some(timer) = &mut state.settings.device.timer {
                row(ui, "Timer mode", "", |ui| {
                    let mut enabled = timer[0] != 0;
                    ui.checkbox(&mut enabled, "Enabled")
                        .on_hover_selectable_text(control_help("Timer mode"));
                    timer[0] = u16::from(enabled);
                });
                number(ui, "ON duration", "s", &mut timer[1], 0..=59999, 0);
                number(ui, "OFF duration", "s", &mut timer[2], 0..=59999, 0);
                if ui.button("Apply timer").on_hover_selectable_text("Write timer mode and ON/OFF durations. Use the toolbar ON/OFF button to start or stop the sequence. These settings require V1 ≥ 1.0.3.0 or V1.1 ≥ 1.0.2.0.").clicked() {
                    let mut data = vec![timer[0] as u8];
                    data.extend_from_slice(&timer[1].to_le_bytes());
                    data.extend_from_slice(&timer[2].to_le_bytes());
                    write(actions, index, 15, data);
                }
            } else {
                readout(ui, "Timer mode", "Unavailable", "");
            }
        });
        ui.add_space(10.0);
        section(ui, "DEVICE INFORMATION", state.connected, |ui| {
            ui.label(&state.settings.device.model);
            row(ui, "Firmware", "", |ui| {
                ui.label(&state.settings.device.version);
            });
            ui.label(format!("Serial: {}", state.settings.device.serial));
            if ui.button("Store setup in device").on_hover_selectable_text("Save the current device configuration in its nonvolatile memory. The pinned Save settings footer saves this WaveCraft view instead.").clicked() {
                write(actions, index, 0x44, vec![]);
            }

            let service = ui.collapsing("Service / calibration", |ui| {
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
                    if ui.button("Write calibration…").on_hover_selectable_text("Write raw factory calibration values after confirmation. The reserved word is preserved.").clicked() {
                        state.confirmation =
                            Some((0x47, data.iter().flat_map(|v| v.to_le_bytes()).collect()));
                    }
                }
            });
            service.header_response.on_hover_selectable_text("Device restart, factory reset, LCD panel type, and factory calibration. Calibration fields are raw 16-bit values; the reserved word is preserved. Writes require explicit confirmation.");
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
        app.pd_power[0].latest = Some(PowerSample {
            voltage_mv: 5000,
            current_ma: 250,
            ..Default::default()
        });
        app.pd_power[0].points.push_back(Point {
            time: 1.0,
            sample: app.pd_power[0].latest.unwrap(),
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
        assert!(app.pd_power[0].plot_fraction > 0.55);
        assert_eq!(
            app.saved_capture_settings.global.pd_power_plot_fraction,
            Some(app.pd_power[0].plot_fraction)
        );
        let restored = WaveCraftApp::with_settings(None, app.saved_capture_settings.clone());
        assert_eq!(
            restored.pd_power[0].plot_fraction,
            app.pd_power[0].plot_fraction
        );
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
    fn toolbar_has_one_state_colored_toggle_and_connection_is_below_port() {
        let mut app = app();
        app.active_workspace_view = Some(WorkspaceView::PdPower);
        app.pd_power[0].connected = true;
        app.pd_power[0].latest = Some(PowerSample::default());
        assert_eq!(
            output_button(&app.pd_power[0]),
            ("OFF", egui::Color32::LIGHT_RED)
        );
        let context = egui::Context::default();
        crate::configure_style(&context);
        let _ = frame(&mut app, &context);
        let output = frame(&mut app, &context);
        let port = text_rect(&output, "Port");
        let connect = text_rect(&output, "Connect");
        assert!(connect.top() > port.bottom());
        assert!(connect.right() < 300.0);
        assert!(!output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == "Output ON" || t.galley.text() == "Output OFF")));
        let mut actions = FrameActions::default();
        let output = context.run(egui::RawInput::default(), |ctx| {
            app.paint_pd_power_toolbar(ctx, &mut actions)
        });
        let position = text_rect(&output, "OFF").center();
        for pressed in [true, false] {
            let _ = context.run(
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ctx| app.paint_pd_power_toolbar(ctx, &mut actions),
            );
        }
        assert_eq!(actions.pd_power_toggle_output, Some(0));
        app.pd_power[0].latest = Some(PowerSample {
            output_state: 1,
            ..Default::default()
        });
        assert_eq!(output_button(&app.pd_power[0]), ("ON", ACCENT));
        app.pd_power[0].latest = Some(PowerSample {
            output_state: 8,
            ..Default::default()
        });
        assert_eq!(
            output_button(&app.pd_power[0]),
            ("OFF", egui::Color32::LIGHT_RED)
        );
    }

    #[test]
    fn five_power_views_persist_independent_profiles_layout_and_visibility() {
        let path =
            std::env::temp_dir().join(format!("wavecraft-five-power-{}.json", std::process::id()));
        let repository = crate::CaptureSettingsRepository::at(path.clone());
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        assert_eq!(
            app.available_workspace_views()
                .iter()
                .filter(|v| v.pd_power_index().is_some())
                .count(),
            1
        );
        for (index, view) in WorkspaceView::PD_POWER.into_iter().enumerate() {
            app.open_workspace_view(view);
            app.pd_power[index].settings.port = format!("power-port-{index}");
            app.pd_power[index].settings.poll_hz = 10.0 + index as f64;
            app.pd_power[index].plot_fraction = 0.3 + index as f32 * 0.1;
            app.workspace_view_layout
                .set_setup_panel_open(view, index % 2 == 0);
            app.save_setup_settings(view);
        }
        app.save_global_settings();
        let restored = WaveCraftApp::with_settings(None, repository.load().unwrap());
        assert_eq!(
            restored.active_workspace_view,
            Some(WorkspaceView::PdPower5)
        );
        assert_eq!(
            restored
                .available_workspace_views()
                .iter()
                .filter(|v| v.pd_power_index().is_some())
                .count(),
            5
        );
        for (index, view) in WorkspaceView::PD_POWER.into_iter().enumerate() {
            assert_eq!(
                restored.pd_power[index].settings,
                app.pd_power[index].settings
            );
            assert_eq!(
                restored.pd_power[index].plot_fraction,
                app.pd_power[index].plot_fraction
            );
            assert_eq!(
                restored.workspace_view_layout.setup_panel_open(view),
                index % 2 == 0
            );
        }
        app.pd_power[0].points.push_back(Point {
            time: 1.0,
            sample: PowerSample::default(),
        });
        app.pd_power[4].points.push_back(Point {
            time: 2.0,
            sample: PowerSample::default(),
        });
        let actions = FrameActions {
            pd_power_clear: Some(0),
            ..Default::default()
        };
        app.apply_pd_power_actions(&actions);
        assert!(app.pd_power[0].points.is_empty());
        assert_eq!(app.pd_power[4].points.len(), 1);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn power_views_poll_independently_in_background_and_closing_one_stops_only_it() {
        use crate::app::test_support::{ready_scripted, Script, SourceEnding};
        use crate::{AcquisitionMode, CaptureTarget};
        use wavecraft_core::{CaptureBatch, InstrumentKind};
        let mut app = app();
        for (index, view) in WorkspaceView::PD_POWER.into_iter().enumerate() {
            app.workspace_view_layout.set_visible(view, true);
            let sample = PowerSample {
                voltage_mv: 1000 + index as u16,
                current_ma: 100 + index as u16,
                ..Default::default()
            };
            let ready = ready_scripted(
                &app,
                vec![(
                    InstrumentKind::Serial,
                    Script::sending(vec![CaptureBatch::Power(sample)], SourceEnding::Idle),
                )],
                AcquisitionMode::Run,
                CaptureTarget::LogicAnalyzer,
            );
            app.pd_power[index].session = Some(ready.session);
        }
        app.active_workspace_view = Some(WorkspaceView::Battery);
        let context = egui::Context::default();
        crate::configure_style(&context);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.pd_power.iter().any(|state| state.points.is_empty())
            && std::time::Instant::now() < deadline
        {
            let _ = frame(&mut app, &context);
        }
        for (index, state) in app.pd_power.iter().enumerate() {
            assert_eq!(state.latest.unwrap().voltage_mv, 1000 + index as u16);
        }
        app.pd_power[0].settings.port = "/dev/cu.shared-test".into();
        app.pd_power[2].stop();
        app.pd_power[2].settings.port = "/dev/tty.shared-test".into();
        app.apply_pd_power_actions(&FrameActions {
            pd_power_connect: Some(2),
            ..Default::default()
        });
        assert!(app.pd_power[2].session.is_none());
        assert!(app.pd_power[2]
            .error
            .as_deref()
            .unwrap()
            .contains("already connected"));
        app.set_workspace_view_visible(WorkspaceView::PdPower4, false);
        assert!(app.pd_power[3].session.is_none());
        assert!(app.pd_power[0].session.is_some());
        assert!(app.pd_power[1].session.is_some());
        assert!(app.pd_power[4].session.is_some());
    }

    #[test]
    fn legacy_settings_restore_only_one_power_tab() {
        let legacy = serde_json::json!({"format_version":1,"profiles":{},"global":{"last_active_view":"pd_power","pd_power":{"port":"legacy-port","poll_hz":20.0},"pd_power_plot_fraction":0.65}});
        let restored = WaveCraftApp::with_settings(None, serde_json::from_value(legacy).unwrap());
        assert_eq!(restored.pd_power[0].settings.port, "legacy-port");
        assert_eq!(restored.pd_power[0].plot_fraction, 0.65);
        assert_eq!(
            restored
                .available_workspace_views()
                .iter()
                .filter(|v| v.pd_power_index().is_some())
                .count(),
            1
        );
    }

    #[test]
    fn every_device_setting_and_timer_field_has_help_without_inline_explanations() {
        for label in [
            "OCP protection",
            "Calibration",
            "OC discharge",
            "Auto output",
            "Compensation",
            "Brightness",
            "Filter window",
            "Voltage discharge",
            "Timer mode",
            "ON duration",
            "OFF duration",
        ] {
            assert!(!control_help(label).is_empty(), "{label}");
        }
        let mut app = app();
        app.active_workspace_view = Some(WorkspaceView::PdPower);
        let context = egui::Context::default();
        crate::configure_style(&context);
        let output = frame(&mut app, &context);
        assert!(!output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text().starts_with("CV holds") || t.galley.text().starts_with("0.1–100 Hz requested") || t.galley.text().starts_with("Requires PD"))));
    }

    #[test]
    fn setting_explanation_is_shown_on_hover_only() {
        let context = egui::Context::default();
        crate::configure_style(&context);
        let paint = |time, events| {
            context.run(
                egui::RawInput {
                    time: Some(time),
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 300.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let mut value = 10_u8;
                        number(ui, "Brightness", "%", &mut value, 1..=100, 0);
                    });
                },
            )
        };
        let output = paint(0.0, vec![]);
        let position = text_rect(&output, "Brightness").center();
        assert!(!output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == control_help("Brightness"))));
        let _ = paint(0.1, vec![egui::Event::PointerMoved(position)]);
        let _ = paint(1.0, vec![]);
        let output = paint(2.0, vec![]);
        text_rect(&output, control_help("Brightness"));
    }

    #[test]
    fn typed_baud_rates_persist_per_view_and_invalid_drafts_cannot_save_or_connect() {
        let path =
            std::env::temp_dir().join(format!("wavecraft-pd-baud-{}.json", std::process::id()));
        let repository = crate::CaptureSettingsRepository::at(path.clone());
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        for (index, view) in WorkspaceView::PD_POWER.into_iter().enumerate() {
            app.open_workspace_view(view);
            app.pd_power[index].baud_draft = Some((123456 + index as u32).to_string());
            app.save_setup_settings(view);
        }
        let saved = repository.load().unwrap();
        let restored = WaveCraftApp::with_settings(None, saved.clone());
        for (index, state) in restored.pd_power.iter().enumerate() {
            assert_eq!(state.settings.baud_rate, 123456 + index as u32);
        }
        for draft in ["", "invalid", "0", "12000001"] {
            app.pd_power[4].baud_draft = Some(draft.into());
            app.save_setup_settings(WorkspaceView::PdPower5);
            assert_eq!(repository.load().unwrap(), saved);
            assert!(app.pd_power[4].start().is_err());
            assert!(app.pd_power[4].session.is_none());
            assert!(app.pd_power[0].error.is_none());
        }
        app.pd_power[4].settings.crc8 = true;
        app.pd_power[4].baud_draft = Some("123456".into());
        assert!(app.pd_power[4].edited_settings().is_err());
        app.pd_power[4].baud_draft = Some("460800".into());
        assert_eq!(app.pd_power[4].edited_settings().unwrap().baud_rate, 460800);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn saves_full_workspace_restores_tab_and_keeps_error_visible_when_collapsed() {
        let path =
            std::env::temp_dir().join(format!("wavecraft-pd-settings-{}.json", std::process::id()));
        let repository = crate::CaptureSettingsRepository::at(path.clone());
        let mut app =
            WaveCraftApp::with_settings(Some(repository), CaptureSettingsDocument::default());
        app.active_workspace_view = Some(WorkspaceView::PdPower);
        app.pd_power[0].settings.poll_hz = 25.0;
        app.pd_power[0].settings.device.presets[2] = [12000, 1000];
        app.save_setup_settings(WorkspaceView::PdPower);
        app.workspace_view_layout
            .set_setup_panel_open(WorkspaceView::PdPower, false);
        app.save_global_settings();
        let document: CaptureSettingsDocument =
            serde_json::from_str(&serde_json::to_string(&app.saved_capture_settings).unwrap())
                .unwrap();
        let mut restored = WaveCraftApp::with_settings(None, document);
        assert!(restored.pd_power_view_is_active());
        assert_eq!(restored.pd_power[0].settings, app.pd_power[0].settings);
        assert!(!restored
            .workspace_view_layout
            .setup_panel_open(WorkspaceView::PdPower));
        restored.pd_power[0].error = Some("WeAct connection lost".into());
        let context = egui::Context::default();
        crate::configure_style(&context);
        let output = frame(&mut restored, &context);
        assert!(text_rect(&output, "WeAct connection lost").top() > 800.0);
        text_rect(&output, "OUTPUT VOLTAGE");
        std::fs::remove_file(path).unwrap();
    }
}
