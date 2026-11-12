//! Independent System Trace workspace chrome and initial empty state.

use crate::{egui, RichText, WaveCraftApp, ACCENT, BACKGROUND, GRID, PANEL, TEXT_MUTED};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemTraceSettings {
    pub(crate) target_name: String,
    pub(crate) speed_khz: u32,
    pub(crate) control_block_address: String,
    pub(crate) up_channel: usize,
    pub(crate) down_channel: usize,
}

impl Default for SystemTraceSettings {
    fn default() -> Self {
        Self {
            target_name: String::new(),
            speed_khz: 4_000,
            control_block_address: String::new(),
            up_channel: 1,
            down_channel: 1,
        }
    }
}

impl WaveCraftApp {
    pub(crate) fn paint_system_trace_toolbar(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(12.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("WAVECRAFT").strong().size(18.0).color(ACCENT));
                    ui.separator();
                    ui.menu_button("App", |ui| {
                        if ui.button("Settings…").clicked() {
                            self.settings_open = true;
                            ui.close_menu();
                        }
                    });
                    ui.separator();
                    ui.label(RichText::new("System and RTOS trace").color(TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled(false, egui::Button::new("▶ Start"));
                        ui.add_enabled(false, egui::Button::new("↻ Scan probes"));
                    });
                });
            });
    }

    pub(crate) fn paint_system_trace_setup_panel(&mut self, context: &egui::Context) {
        egui::SidePanel::left("system_trace_setup")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(context, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("SYSTEM TRACE SETUP").strong().color(TEXT_MUTED));
                        ui.add_space(12.0);

                        setup_label(ui, "Debug probe");
                        egui::ComboBox::from_id_salt("system_trace_probe")
                            .width(ui.available_width())
                            .selected_text("No probe backend in this build")
                            .show_ui(ui, |ui| {
                                ui.add_enabled(false, egui::Label::new("No probes found"));
                            });

                        ui.add_space(10.0);
                        setup_label(ui, "Target MCU");
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.system_trace_settings.target_name,
                            )
                            .hint_text("e.g. STM32F407VGTx"),
                        );

                        ui.add_space(10.0);
                        setup_label(ui, "SWD speed (kHz)");
                        ui.add(
                            egui::DragValue::new(&mut self.system_trace_settings.speed_khz)
                                .range(100..=50_000)
                                .speed(100),
                        );

                        ui.add_space(10.0);
                        setup_label(ui, "RTT control block");
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.system_trace_settings.control_block_address,
                            )
                            .hint_text("Auto scan or 0x20000000"),
                        );

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            setup_label(ui, "Up channel");
                            ui.add(
                                egui::DragValue::new(
                                    &mut self.system_trace_settings.up_channel,
                                )
                                .range(0..=255),
                            );
                            setup_label(ui, "Down");
                            ui.add(
                                egui::DragValue::new(
                                    &mut self.system_trace_settings.down_channel,
                                )
                                .range(0..=255),
                            );
                        });

                        ui.add_space(16.0);
                        ui.add_enabled_ui(false, |ui| {
                            let _ = ui.button("Connect and attach");
                        });
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(
                                "Probe discovery is isolated behind wavecraft-debug-probe. Enable a compatible backend to connect hardware.",
                            )
                            .small()
                            .color(TEXT_MUTED),
                        );
                    });
            });
    }

    pub(crate) fn paint_system_trace_central_panel(&mut self, context: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::same(16.0)),
            )
            .show(context, |ui| {
                let available_height = ui.available_height();
                egui::Frame::none()
                    .fill(PANEL)
                    .stroke(egui::Stroke::new(1.0_f32, GRID))
                    .rounding(egui::Rounding::same(6.0))
                    .inner_margin(egui::Margin::same(20.0))
                    .show(ui, |ui| {
                        ui.set_min_height((available_height - 40.0).max(0.0));
                        ui.label(RichText::new("SYSTEM TRACE").strong().color(ACCENT));
                        ui.add_space(8.0);
                        ui.heading("Connect a debug probe to begin");
                        ui.label(
                            RichText::new(
                                "Task, interrupt, timer, marker, log, and data events will appear in this independent workspace.",
                            )
                            .color(TEXT_MUTED),
                        );
                        ui.add_space(20.0);
                        paint_empty_instrument(ui, "TIMELINE", "Task and ISR execution lanes");
                        ui.add_space(12.0);
                        paint_empty_instrument(ui, "EVENTS", "Decoded SystemView events");
                    });
            });
    }
}

fn setup_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(TEXT_MUTED));
}

fn paint_empty_instrument(ui: &mut egui::Ui, title: &str, description: &str) {
    egui::Frame::none()
        .fill(BACKGROUND)
        .stroke(egui::Stroke::new(1.0_f32, GRID))
        .rounding(egui::Rounding::same(4.0))
        .inner_margin(egui::Margin::same(14.0))
        .show(ui, |ui| {
            ui.set_min_height(100.0);
            ui.label(RichText::new(title).strong().color(TEXT_MUTED));
            ui.label(RichText::new(description).color(TEXT_MUTED));
        });
}
