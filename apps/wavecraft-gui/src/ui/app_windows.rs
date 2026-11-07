//! Modal windows opened from the application update loop: settings, DSCope
//! auto-calibration confirmation, and the manual calibration editor.

use crate::{
    egui, paint_instrument_settings, paint_radio_capture_settings, paint_radio_settings,
    selected_radio, Color32, DsCopeCalibrationValues, RichText, Sense, Vec2, WaveCraftApp, ACCENT,
    DSCOPE_VOLTS_PER_DIV_MV, TEXT_MUTED,
};

impl WaveCraftApp {
    /// The instruments and radio settings window. `refresh_clicked` and
    /// `radio_device_selection_changed` accumulate across the whole frame, so
    /// they are updated in place rather than returned.
    pub(crate) fn paint_settings_window(
        &mut self,
        context: &egui::Context,
        radio_runtime_controls_enabled: bool,
        radio_trigger_runtime_controls_enabled: bool,
        refresh_clicked: &mut bool,
        radio_device_selection_changed: &mut bool,
    ) {
        let mut settings_open = self.settings_open;
        let mut close_settings = false;
        let settings_locked = self.is_busy();
        if settings_open {
            let screen_rect = context.input(|input| input.screen_rect());
            egui::Area::new(egui::Id::new("settings_modal_backdrop"))
                .order(egui::Order::Middle)
                .fixed_pos(screen_rect.min)
                .show(context, |ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
                    ui.painter().rect_filled(
                        rect,
                        egui::Rounding::ZERO,
                        Color32::from_black_alpha(112),
                    );
                });
        }
        egui::Window::new("Settings")
            .open(&mut settings_open)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size([330.0, 420.0])
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("INSTRUMENTS").strong().color(ACCENT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(!settings_locked, egui::Button::new("↻ Refresh"))
                                .clicked()
                            {
                                *refresh_clicked = true;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    paint_instrument_settings(ui, &self.devices);

                    ui.add_space(14.0);
                    ui.label(RichText::new("RADIO RECEIVER").strong().color(ACCENT));
                    ui.add_space(6.0);
                    *radio_device_selection_changed |= paint_radio_settings(
                        ui,
                        &self.radios,
                        &mut self.selected_radio_id,
                        &self.radio_discovery_failures,
                        !settings_locked,
                    );
                    if *radio_device_selection_changed {
                        self.reconcile_radio_settings();
                    }
                    if let Some(radio) =
                        selected_radio(&self.radios, self.selected_radio_id.as_deref()).cloned()
                    {
                        paint_radio_capture_settings(
                            ui,
                            &radio,
                            &mut self.radio_settings,
                            !settings_locked,
                            radio_runtime_controls_enabled,
                            radio_trigger_runtime_controls_enabled,
                            false,
                        );
                    }

                    if settings_locked {
                        ui.add_space(8.0);
                        ui.small(
                            RichText::new(if radio_runtime_controls_enabled {
                        "Device, sample-rate, IQ-correction, and antenna-power settings remain locked; tuning, frequency correction, gains, and RF trigger level are live"
                            } else if radio_trigger_runtime_controls_enabled {
                                "Settings remain locked while RF Single waits; RF trigger level and Post-trigger are live"
                            } else {
                                "Settings are locked while an operation is running"
                            })
                            .color(TEXT_MUTED),
                        );
                    }

                    ui.add_space(16.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close_settings = true;
                        }
                    });
                });
            });
        if close_settings {
            settings_open = false;
        }
        self.settings_open = settings_open;
    }

    /// Returns whether the user confirmed starting auto calibration.
    pub(crate) fn paint_auto_calibration_confirmation(&mut self, context: &egui::Context) -> bool {
        let mut auto_confirmation_open = self.auto_calibration_confirmation_open;
        let mut auto_confirmed = false;
        let mut auto_cancelled = false;
        egui::Window::new("DSCope Auto Calibration")
            .id(egui::Id::new("auto_calibration_confirmation"))
            .open(&mut auto_confirmation_open)
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label(RichText::new("Disconnect all probes before continuing.").strong());
                ui.add_space(6.0);
                ui.label(
                    "WaveCraft will run the vendor calibration sequence for both channels: offset transfer, all input ranges, and ADC branch balance. It can take a while. Device NVM is changed only if you choose Save when calibration finishes.",
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Keep all probes disconnected during calibration. Do not disconnect the oscilloscope while calibration is running.",
                    )
                    .color(Color32::LIGHT_RED),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        auto_cancelled = true;
                    }
                    if ui.button("Start calibration").clicked() {
                        auto_confirmed = true;
                    }
                });
            });
        if auto_confirmed || auto_cancelled {
            auto_confirmation_open = false;
        }
        self.auto_calibration_confirmation_open = auto_confirmation_open;
        auto_confirmed
    }

    /// Returns the calibration values the user chose to save to the device.
    pub(crate) fn paint_manual_calibration_window(
        &mut self,
        context: &egui::Context,
    ) -> Option<DsCopeCalibrationValues> {
        let mut save_manual_values = None;
        let mut close_manual = false;
        let manual_calibration_actions_enabled = self.pending.is_none();
        if let Some(editor) = self.manual_calibration.as_mut() {
            let mut manual_open = true;
            egui::Window::new("DSCope Manual Calibration")
                .open(&mut manual_open)
                .collapsible(false)
                .resizable(false)
                .default_width(430.0)
                .show(context, |ui| {
                    ui.label(
                        "Adjust VGAIN and VOFF for the selected input range. VCOMB is shared by all ranges of a channel. Active acquisition previews edits; device NVM changes only after Save.",
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("Input range");
                        egui::ComboBox::from_id_salt("manual_calibration_range")
                            .selected_text(format!(
                                "{} mV/div",
                                DSCOPE_VOLTS_PER_DIV_MV[editor.range]
                            ))
                            .show_ui(ui, |ui| {
                                for (range, value) in
                                    DSCOPE_VOLTS_PER_DIV_MV.into_iter().enumerate()
                                {
                                    ui.selectable_value(
                                        &mut editor.range,
                                        range,
                                        format!("{value} mV/div"),
                                    );
                                }
                            });
                    });
                    ui.add_space(8.0);
                    for channel in 0..2 {
                        ui.group(|ui| {
                            ui.label(RichText::new(format!("CH{channel}")).strong());
                            let Some(mut gain) = editor.values.vga_gain(channel, editor.range)
                            else {
                                return;
                            };
                            ui.horizontal(|ui| {
                                ui.label("VGAIN");
                                ui.add(
                                    egui::DragValue::new(&mut gain)
                                        .range(1..=0x1fff)
                                        .speed(1),
                                );
                            });
                            let _ = editor.values.set_vga_gain(channel, editor.range, gain);

                            let Some(mut offset) =
                                editor.values.preoffset(channel, editor.range)
                            else {
                                return;
                            };
                            ui.horizontal(|ui| {
                                ui.label("VOFF");
                                ui.add(egui::Slider::new(&mut offset, 0..=1_023));
                            });
                            let _ = editor.values.set_preoffset(channel, editor.range, offset);

                            let Some(comb_value) = editor.values.comb_comp(channel) else {
                                return;
                            };
                            let mut comb = i16::from(comb_value);
                            ui.horizontal(|ui| {
                                ui.label("VCOMB");
                                ui.add(egui::Slider::new(&mut comb, -127..=127));
                            });
                            let _ = editor.values.set_comb_comp(channel, comb);
                        });
                        ui.add_space(6.0);
                    }
                    ui.label(
                        RichText::new(
                            "Incorrect calibration values can distort measurements. Save only values established with a known reference.",
                        )
                        .color(Color32::LIGHT_RED),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close_manual = true;
                        }
                        if ui
                            .add_enabled(
                                manual_calibration_actions_enabled,
                                egui::Button::new("Save to device"),
                            )
                            .clicked()
                        {
                            save_manual_values = Some(editor.values);
                        }
                    });
                });
            if !manual_open {
                close_manual = true;
            }
        }
        if close_manual {
            self.manual_calibration = None;
        }
        save_manual_values
    }
}
