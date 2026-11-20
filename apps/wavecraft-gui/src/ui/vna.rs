//! LiteVNA setup controls and Smith chart rendering.

use crate::{
    apply_setup_panel_typography, card_frame, egui, paint_capture_settings_save_button,
    toolbar_action_button, CalibrationStandard, Color32, FontId, FrameActions, PendingSnapshot,
    Pos2, Rect, RichText, SavedCaptureSettingsKind, Sense, Stroke, Vec2, VnaPoint, WaveCraftApp,
    ACCENT, BACKGROUND, GRID, ORANGE, PANEL, SCOPE_COLORS, TEXT_MUTED,
};

const VNA_PANEL_WIDTH: f32 = 300.0;
const SMITH_GRID: Color32 = Color32::from_rgb(54, 68, 81);
const SMITH_TRACE: Color32 = Color32::from_rgb(78, 205, 196);
const SMITH_MIN_ZOOM: f32 = 0.5;
const SMITH_MAX_ZOOM: f32 = 32.0;
const POINT_COUNTS: [u16; 6] = [51, 101, 201, 401, 801, 1024];
const AVERAGING_VALUES: [u8; 7] = [1, 2, 4, 8, 16, 32, 64];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum VnaPlotTab {
    #[default]
    SmithChart,
    LogMagnitudePhase,
}

impl VnaPlotTab {
    const ALL: [Self; 2] = [Self::SmithChart, Self::LogMagnitudePhase];

    fn label(self) -> &'static str {
        match self {
            Self::SmithChart => "SMITH CHART",
            Self::LogMagnitudePhase => "LOG MAG / PHASE",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum VnaChannel {
    #[default]
    S11,
    S21,
}

impl VnaChannel {
    const ALL: [Self; 2] = [Self::S11, Self::S21];

    fn label(self) -> &'static str {
        match self {
            Self::S11 => "S11",
            Self::S21 => "S21",
        }
    }

    fn complex(self, point: VnaPoint) -> (f64, f64) {
        match self {
            Self::S11 => (point.s11_real, point.s11_imaginary),
            Self::S21 => (point.s21_real, point.s21_imaginary),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SmithChartViewport {
    zoom: f32,
    center_gamma: (f64, f64),
}

impl Default for SmithChartViewport {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            center_gamma: (0.0, 0.0),
        }
    }
}

impl SmithChartViewport {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn zoom_about(&mut self, plot_center: Pos2, radius: f32, pointer: Pos2, factor: f32) {
        if radius <= f32::EPSILON || !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let old_zoom = self.zoom.clamp(SMITH_MIN_ZOOM, SMITH_MAX_ZOOM);
        let new_zoom = (old_zoom * factor).clamp(SMITH_MIN_ZOOM, SMITH_MAX_ZOOM);
        if (new_zoom - old_zoom).abs() <= f32::EPSILON {
            self.zoom = new_zoom;
            return;
        }
        let anchor_gamma = self.gamma_at(plot_center, radius, pointer);
        let new_scale = f64::from(radius * new_zoom);
        let pointer_dx = f64::from(pointer.x - plot_center.x);
        let pointer_dy = f64::from(pointer.y - plot_center.y);
        self.center_gamma = (
            anchor_gamma.0 - pointer_dx / new_scale,
            anchor_gamma.1 + pointer_dy / new_scale,
        );
        self.zoom = new_zoom;
    }

    fn gamma_at(self, plot_center: Pos2, radius: f32, position: Pos2) -> (f64, f64) {
        let scale = f64::from((radius * self.zoom).max(f32::EPSILON));
        (
            self.center_gamma.0 + f64::from(position.x - plot_center.x) / scale,
            self.center_gamma.1 - f64::from(position.y - plot_center.y) / scale,
        )
    }
}

impl WaveCraftApp {
    pub(crate) fn paint_vna_calibration_notice(&mut self, context: &egui::Context) {
        if !self.vna_calibration_notice_open {
            return;
        }
        let calibration_state = self.vna_calibration.as_ref().map(|active| {
            self.vna_settings
                .sweep_config()
                .is_ok_and(|config| active.calibration.matches_config(config))
        });
        let sweep_grid_description = self.vna_settings.sweep_config().ok().and_then(|config| {
            let step_hz = config.step_hz().ok()?;
            Some(format!(
                "With the current settings, calibration uses {} evenly spaced points from {:.6} to {:.6} MHz (approximately {:.6} MHz between points).",
                config.points,
                config.start_hz as f64 / 1_000_000.0,
                config.stop_hz as f64 / 1_000_000.0,
                step_hz as f64 / 1_000_000.0,
            ))
        });
        let mut open = self.vna_calibration_notice_open;
        let mut acknowledged = false;
        egui::Window::new("VNA calibration recommended")
            .id(egui::Id::new("vna_calibration_notice_compact"))
            .open(&mut open)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .auto_sized()
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(context, |ui| {
                ui.set_width(680.0);
                ui.set_max_width(680.0);
                ui.label(
                    RichText::new("Calibrate before precision measurements")
                        .strong()
                        .color(ACCENT),
                );
                ui.add_space(8.0);
                ui.label(
                    "For reliable S11 results, connect the calibration standards and measure OPEN, SHORT, and LOAD using the same frequency range and point count as the measurement.",
                );
                ui.add_space(12.0);
                ui.label(RichText::new("How calibration uses frequency points").strong());
                ui.add_space(4.0);
                ui.label(
                    "“For each frequency” means for each discrete sweep point selected between Start and Stop—not for every possible frequency in the range.",
                );
                if let Some(description) = &sweep_grid_description {
                    ui.label(description);
                }
                ui.add_space(10.0);
                ui.label(RichText::new("What is calculated at every point").strong());
                ui.add_space(4.0);
                ui.label("• Directivity — receiver leakage and residual offset measured mainly from LOAD.");
                ui.label("• Source match — error caused by imperfect matching at the VNA test port.");
                ui.label("• Reflection tracking — frequency-dependent amplitude and phase response of the reflection path.");
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "WaveCraft applies the corresponding three coefficients to each S11 point. Calibration is applied only when Start, Stop, and Points match exactly; coefficients are not interpolated onto another grid.",
                    )
                    .color(TEXT_MUTED),
                );
                ui.add_space(8.0);
                match calibration_state {
                    Some(true) => {
                        ui.label(
                            RichText::new(
                                "The saved WaveCraft calibration matches the current sweep and will be applied automatically.",
                            )
                            .color(ACCENT),
                        );
                    }
                    Some(false) => {
                        ui.label(
                            RichText::new(
                                "A calibration is saved, but it does not match the current sweep. Measurements will remain raw until the sweep settings match or you recalibrate.",
                            )
                            .color(Color32::LIGHT_RED),
                        );
                    }
                    None => {
                        ui.label(
                            RichText::new(
                                "No VNA calibration is currently saved. Uncalibrated measurements are displayed as raw S11.",
                            )
                            .color(Color32::LIGHT_RED),
                        );
                    }
                }
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    if ui
                        .add_sized(
                            [160.0, 30.0],
                            toolbar_action_button("Got it", ORANGE),
                        )
                        .clicked()
                    {
                        acknowledged = true;
                    }
                });
            });
        if acknowledged {
            open = false;
        }
        self.vna_calibration_notice_open = open;
    }

    pub(crate) fn paint_vna_toolbar(
        &mut self,
        context: &egui::Context,
        actions: &mut FrameActions,
    ) {
        egui::TopBottomPanel::top("vna_toolbar")
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
                    ui.label(RichText::new("Vector network analyzer").color(TEXT_MUTED));
                    ui.separator();
                    actions.refresh_clicked = ui
                        .add_enabled(
                            !self.is_busy() && self.vna_session.is_none(),
                            toolbar_action_button("↻ Refresh", SCOPE_COLORS[1]),
                        )
                        .clicked();
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.snapshot_plot_rect.is_some() && self.pending_snapshot.is_none(),
                            toolbar_action_button("📷", TEXT_MUTED),
                        )
                        .clicked()
                    {
                        actions.snapshot_clicked = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_start = self.vna_session.is_some() && !self.is_busy();
                        if ui
                            .add_enabled(
                                (self.vna_run_active && !self.vna_run_stop_requested) || can_start,
                                if self.vna_run_active {
                                    toolbar_action_button("■ Stop", Color32::LIGHT_RED)
                                } else {
                                    toolbar_action_button("▶ Run", ACCENT)
                                },
                            )
                            .clicked()
                        {
                            actions.vna_run_stop_clicked = true;
                        }
                        if ui
                            .add_enabled(
                                can_start,
                                toolbar_action_button("▶ Single", SCOPE_COLORS[0]),
                            )
                            .clicked()
                        {
                            actions.vna_sweep_clicked = true;
                        }
                    });
                });
            });
    }

    pub(crate) fn paint_vna_setup_panel(
        &mut self,
        context: &egui::Context,
        actions: &mut FrameActions,
    ) {
        egui::SidePanel::left("vna_setup")
            .resizable(false)
            .exact_width(VNA_PANEL_WIDTH)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(context, |ui| {
                apply_setup_panel_typography(ui);
                let content_width = ui.available_width();
                egui::ScrollArea::vertical()
                    .max_width(content_width)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(content_width);
                        ui.set_max_width(content_width);
                        ui.label(RichText::new("VNA SETUP").strong().color(TEXT_MUTED));
                        ui.add_space(10.0);
                        ui.label(RichText::new("Instrument").color(TEXT_MUTED));
                        let selected_text = if self.vna_discovery_error.is_some() {
                            "LiteVNA discovery failed".to_owned()
                        } else {
                            self.selected_vna()
                                .map(|device| device.display_name.clone())
                                .unwrap_or_else(|| "No LiteVNA detected".to_owned())
                        };
                        ui.add_enabled_ui(
                            self.vna_session.is_none() && self.vna_connection_pending.is_none(),
                            |ui| {
                                egui::ComboBox::from_id_salt("vna_device")
                                    .width(ui.available_width())
                                    .selected_text(selected_text)
                                    .show_ui(ui, |ui| {
                                        for device in &self.vna_devices {
                                            ui.selectable_value(
                                                &mut self.selected_vna_port,
                                                Some(device.port_name.clone()),
                                                &device.display_name,
                                            );
                                        }
                                    });
                            },
                        );
                        ui.add_space(6.0);
                        let connection_pending = self.vna_connection_pending.is_some();
                        let connection_open = self.vna_session.is_some();
                        let connection_label = if connection_pending {
                            if self.vna_connection_closing {
                                "Closing…"
                            } else {
                                "Opening…"
                            }
                        } else if connection_open {
                            "■ Close"
                        } else {
                            "▶ Open"
                        };
                        let connection_color = if connection_open || self.vna_connection_closing {
                            Color32::LIGHT_RED
                        } else {
                            ACCENT
                        };
                        if ui
                            .add_enabled(
                                !connection_pending
                                    && !self.is_busy()
                                    && (connection_open || self.selected_vna().is_some()),
                                toolbar_action_button(connection_label, connection_color),
                            )
                            .clicked()
                        {
                            actions.vna_connection_clicked = true;
                        }

                        ui.add_space(14.0);
                        paint_vna_settings_card(ui, self, actions);
                    });
            });
    }

    pub(crate) fn paint_vna_central_panel(
        &mut self,
        context: &egui::Context,
        snapshot_clicked: bool,
    ) {
        let mut plot_rect = None;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::same(16.0)),
            )
            .show(context, |ui| {
                card_frame(12.0).show(ui, |ui| {
                    ui.set_min_height(ui.available_height());
                    ui.horizontal(|ui| {
                        for tab in VnaPlotTab::ALL {
                            let selected = self.vna_plot_tab == tab;
                            ui.selectable_value(
                                &mut self.vna_plot_tab,
                                tab,
                                RichText::new(tab.label()).strong().color(if selected {
                                    ACCENT
                                } else {
                                    TEXT_MUTED
                                }),
                            );
                        }
                        if let Some(sweep) = &self.vna_sweep {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "FW {}.{} · HW {} · {} points",
                                            sweep.identity.firmware_major,
                                            sweep.identity.firmware_minor,
                                            sweep.identity.hardware_revision,
                                            sweep.points.len()
                                        ))
                                        .monospace()
                                        .color(TEXT_MUTED),
                                    );
                                },
                            );
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let subtitle = match self.vna_plot_tab {
                            VnaPlotTab::SmithChart if self.vna_sweep_calibrated => {
                                "SOL-calibrated S11 · Z₀ = 50 Ω"
                            }
                            VnaPlotTab::SmithChart => "Raw S11 reflection coefficient · Z₀ = 50 Ω",
                            VnaPlotTab::LogMagnitudePhase => {
                                "Log magnitude in dB and phase in degrees"
                            }
                        };
                        ui.label(RichText::new(subtitle).color(TEXT_MUTED));
                        if self.vna_plot_tab == VnaPlotTab::LogMagnitudePhase {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    egui::ComboBox::from_id_salt("vna_plot_channel")
                                        .width(72.0)
                                        .selected_text(self.vna_plot_channel.label())
                                        .show_ui(ui, |ui| {
                                            for channel in VnaChannel::ALL {
                                                ui.selectable_value(
                                                    &mut self.vna_plot_channel,
                                                    channel,
                                                    channel.label(),
                                                );
                                            }
                                        });
                                    ui.label(RichText::new("Source").color(TEXT_MUTED));
                                },
                            );
                        }
                    });
                    ui.add_space(12.0);
                    let available = ui.available_size();
                    let plot_size = Vec2::new(available.x.max(120.0), available.y.max(120.0));
                    let (rect, response) = ui.allocate_exact_size(plot_size, Sense::click());
                    plot_rect = Some(rect);
                    match self.vna_plot_tab {
                        VnaPlotTab::SmithChart => paint_smith_chart(
                            ui,
                            rect,
                            self.vna_sweep.as_ref(),
                            &response,
                            &mut self.vna_smith_viewport,
                            &mut self.vna_smith_pinned_point,
                        ),
                        VnaPlotTab::LogMagnitudePhase => paint_log_magnitude_phase_chart(
                            ui,
                            rect,
                            self.vna_sweep.as_ref(),
                            &response,
                            self.vna_plot_channel,
                            (
                                self.vna_settings.start_mhz * 1_000_000.0,
                                self.vna_settings.stop_mhz * 1_000_000.0,
                            ),
                        ),
                    }
                });
            });
        self.snapshot_plot_rect = plot_rect;
        if snapshot_clicked {
            if let Some(plot_rect) = plot_rect {
                self.pending_snapshot = Some(PendingSnapshot {
                    plot_rect,
                    pixels_per_point: context.pixels_per_point(),
                });
                self.status = "Preparing VNA snapshot…".to_owned();
                context.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            }
        }
    }
}

fn paint_vna_settings_card(ui: &mut egui::Ui, app: &mut WaveCraftApp, actions: &mut FrameActions) {
    let card_width = ui.available_width();
    egui::Frame::none()
        .fill(BACKGROUND)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            let content_width = (card_width - 20.0).max(0.0);
            ui.set_width(content_width);
            ui.set_max_width(content_width);
            ui.label(RichText::new("LiteVNA 64").strong());
            ui.add_space(10.0);
            ui.label(RichText::new("SWEEP").strong().color(TEXT_MUTED));
            ui.add_space(8.0);
            ui.add_enabled_ui(app.vna_pending.is_none() && !app.vna_run_active, |ui| {
                ui.label(RichText::new("Start frequency").color(TEXT_MUTED));
                let start_changed = paint_frequency_input(
                    ui,
                    "vna_start_frequency",
                    &mut app.vna_start_frequency_input,
                );
                if start_changed {
                    update_frequency_setting(
                        &app.vna_start_frequency_input,
                        &mut app.vna_settings.start_mhz,
                    );
                }
                ui.add_space(7.0);
                ui.label(RichText::new("Stop frequency").color(TEXT_MUTED));
                let stop_changed = paint_frequency_input(
                    ui,
                    "vna_stop_frequency",
                    &mut app.vna_stop_frequency_input,
                );
                if stop_changed {
                    update_frequency_setting(
                        &app.vna_stop_frequency_input,
                        &mut app.vna_settings.stop_mhz,
                    );
                }
                ui.add_space(7.0);
                ui.label(RichText::new("Sweep points").color(TEXT_MUTED));
                egui::ComboBox::from_id_salt("vna_points")
                    .width(ui.available_width())
                    .selected_text(app.vna_settings.points.to_string())
                    .show_ui(ui, |ui| {
                        for points in POINT_COUNTS {
                            ui.selectable_value(
                                &mut app.vna_settings.points,
                                points,
                                points.to_string(),
                            );
                        }
                    });
                ui.add_space(7.0);
                ui.label(RichText::new("Averaging").color(TEXT_MUTED));
                egui::ComboBox::from_id_salt("vna_averaging")
                    .width(ui.available_width())
                    .selected_text(format!("{}×", app.vna_settings.averaging))
                    .show_ui(ui, |ui| {
                        for averaging in AVERAGING_VALUES {
                            ui.selectable_value(
                                &mut app.vna_settings.averaging,
                                averaging,
                                format!("{averaging}×"),
                            );
                        }
                    });
            });

            paint_calibration_controls(ui, app, actions);
            ui.add_space(12.0);
            ui.small(
                RichText::new(
                    "USB sweeps contain raw data. WaveCraft applies its own calibration and returns LiteVNA to local mode afterward.",
                )
                .color(TEXT_MUTED),
            );
            let settings_valid = vna_frequency_inputs_are_valid(app);
            if paint_capture_settings_save_button(
                ui,
                SavedCaptureSettingsKind::VectorNetworkAnalyzer,
                !app.is_busy() && settings_valid,
            ) {
                actions.save_capture_settings_kind =
                    Some(SavedCaptureSettingsKind::VectorNetworkAnalyzer);
            }
        });
}

fn vna_frequency_inputs_are_valid(app: &WaveCraftApp) -> bool {
    let Ok(start_mhz) = app.vna_start_frequency_input.trim().parse::<f64>() else {
        return false;
    };
    let Ok(stop_mhz) = app.vna_stop_frequency_input.trim().parse::<f64>() else {
        return false;
    };
    crate::VnaSettings {
        start_mhz,
        stop_mhz,
        points: app.vna_settings.points,
        averaging: app.vna_settings.averaging,
    }
    .sweep_config()
    .is_ok()
}

fn paint_frequency_input(ui: &mut egui::Ui, id: &'static str, input: &mut String) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let edit_width = (ui.available_width() - 40.0).max(64.0);
        changed = ui
            .add_sized(
                [edit_width, 24.0],
                egui::TextEdit::singleline(input).id_source(id),
            )
            .changed();
        ui.label(RichText::new("MHz").color(TEXT_MUTED));
    });
    changed
}

fn update_frequency_setting(input: &str, setting: &mut f64) {
    if let Ok(value) = input.trim().parse::<f64>() {
        if value.is_finite() && (0.05..=6_300.0).contains(&value) {
            *setting = value;
        }
    }
}

fn paint_calibration_controls(ui: &mut egui::Ui, app: &WaveCraftApp, actions: &mut FrameActions) {
    ui.add_space(20.0);
    ui.label(
        RichText::new("CALIBRATION (S11)")
            .strong()
            .color(TEXT_MUTED),
    );
    ui.add_space(6.0);
    ui.small(
        RichText::new("Connect each SOL standard, then measure OPEN, SHORT, and LOAD.")
            .color(TEXT_MUTED),
    );
    ui.add_space(8.0);
    let enabled = app.vna_session.is_some() && !app.is_busy();
    for standard in [
        CalibrationStandard::Open,
        CalibrationStandard::Short,
        CalibrationStandard::Load,
    ] {
        let captured = app.vna_calibration_captures.has(standard);
        let label = if captured {
            format!("✓ {} captured", standard.label())
        } else {
            format!("Measure {}", standard.label())
        };
        if ui
            .add_enabled(
                enabled,
                toolbar_action_button(&label, if captured { ACCENT } else { SCOPE_COLORS[0] })
                    .min_size(Vec2::new(ui.available_width(), 30.0)),
            )
            .clicked()
        {
            actions.vna_calibration_standard = Some(standard);
        }
    }
    ui.add_space(6.0);
    if let Some(active) = &app.vna_calibration {
        let calibration = &active.calibration;
        let matches = app
            .vna_settings
            .sweep_config()
            .is_ok_and(|config| calibration.matches_config(config));
        let range = format!(
            "{:.3}–{:.3} MHz · {} points",
            calibration.start_hz().unwrap_or_default() as f64 / 1_000_000.0,
            calibration.stop_hz().unwrap_or_default() as f64 / 1_000_000.0,
            calibration.point_count()
        );
        let state = if matches {
            "Matches current sweep"
        } else {
            "Current sweep differs"
        };
        ui.small(RichText::new(format!("Active: {}", active.name)).color(ACCENT));
        ui.small(
            RichText::new(format!(
                "{state} · {range} · averaging {}×",
                active.averaging
            ))
            .color(if matches { ACCENT } else { TEXT_MUTED }),
        );
    } else {
        ui.small(RichText::new("No active calibration").color(TEXT_MUTED));
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let button_width = (ui.available_width() - ui.spacing().item_spacing.x) * 0.5;
        if ui
            .add_enabled(
                app.vna_calibration.is_some() && !app.is_busy(),
                toolbar_action_button("Save to file", ACCENT)
                    .min_size(Vec2::new(button_width, 28.0)),
            )
            .clicked()
        {
            actions.vna_save_calibration = true;
        }
        if ui
            .add_enabled(
                !app.is_busy(),
                toolbar_action_button("Load from file", ORANGE)
                    .min_size(Vec2::new(button_width, 28.0)),
            )
            .clicked()
        {
            actions.vna_load_calibration = true;
        }
    });
    if app.vna_calibration.is_some()
        && ui
            .add_enabled(
                !app.is_busy(),
                toolbar_action_button("Clear active calibration", Color32::LIGHT_RED)
                    .min_size(Vec2::new(ui.available_width(), 28.0)),
            )
            .clicked()
    {
        actions.vna_clear_calibration = true;
    }
    ui.small(
        RichText::new(
            "The active calibration survives app restarts. Clear does not modify calibration files.",
        )
        .color(TEXT_MUTED),
    );
}

fn paint_smith_chart(
    ui: &mut egui::Ui,
    rect: Rect,
    sweep: Option<&wavecraft_vna::VnaSweep>,
    response: &egui::Response,
    viewport: &mut SmithChartViewport,
    pinned_point: &mut Option<usize>,
) {
    let radius = rect.width().min(rect.height()) * 0.43;
    if response.double_clicked() {
        viewport.reset();
        ui.ctx().request_repaint();
    }
    if response.hovered() {
        let (scroll_delta, native_zoom) = ui.input_mut(|input| {
            let scroll_delta = input.raw_scroll_delta.y;
            let native_zoom = input.zoom_delta();
            if scroll_delta.abs() > f32::EPSILON {
                input.raw_scroll_delta.y = 0.0;
                input.smooth_scroll_delta.y = 0.0;
            }
            (scroll_delta, native_zoom)
        });
        let factor = if (native_zoom - 1.0).abs() > f32::EPSILON {
            native_zoom
        } else {
            (scroll_delta * 0.002).exp()
        };
        if (factor - 1.0).abs() > f32::EPSILON {
            if let Some(pointer) = response.hover_pos() {
                viewport.zoom_about(rect.center(), radius, pointer, factor);
                ui.ctx().request_repaint();
            }
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        egui::Rounding::same(8.0),
        Color32::from_rgb(14, 19, 25),
    );
    let center = rect.center();
    let unit_center = chart_position(center, radius, (0.0, 0.0), *viewport);
    let unit_radius = radius * viewport.zoom;
    painter.circle_stroke(unit_center, unit_radius, Stroke::new(1.5_f32, GRID));
    painter.line_segment(
        [
            chart_position(center, radius, (-1.0, 0.0), *viewport),
            chart_position(center, radius, (1.0, 0.0), *viewport),
        ],
        Stroke::new(1.0_f32, SMITH_GRID),
    );

    for resistance in [0.2_f64, 0.5, 1.0, 2.0, 5.0] {
        let points = (-160..=160)
            .map(|index| f64::from(index) / 8.0)
            .map(|reactance| impedance_to_gamma(resistance, reactance))
            .map(|gamma| chart_position(center, radius, gamma, *viewport))
            .collect::<Vec<_>>();
        painter.add(egui::Shape::line(points, Stroke::new(0.75_f32, SMITH_GRID)));
    }
    for reactance in [0.2_f64, 0.5, 1.0, 2.0, 5.0] {
        for sign in [-1.0_f64, 1.0] {
            let points = (0..=240)
                .map(|index| 30.0 * f64::from(index) / 240.0)
                .map(|resistance| impedance_to_gamma(resistance, sign * reactance))
                .map(|gamma| chart_position(center, radius, gamma, *viewport))
                .collect::<Vec<_>>();
            painter.add(egui::Shape::line(points, Stroke::new(0.75_f32, SMITH_GRID)));
        }
    }

    for resistance in [0.0_f64, 0.2, 0.5, 1.0, 2.0, 5.0] {
        let gamma = impedance_to_gamma(resistance, 0.0);
        let position = chart_position(center, radius, gamma, *viewport);
        painter.text(
            position + Vec2::new(3.0, 3.0),
            egui::Align2::LEFT_TOP,
            format_resistance(resistance),
            FontId::monospace(10.0),
            TEXT_MUTED,
        );
    }

    let Some(sweep) = sweep else {
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            "Run a sweep to display S11",
            FontId::proportional(15.0),
            TEXT_MUTED,
        );
        return;
    };
    if sweep.points.is_empty() {
        return;
    }
    let trace = sweep
        .points
        .iter()
        .map(|point| {
            chart_position(
                center,
                radius,
                (point.s11_real, point.s11_imaginary),
                *viewport,
            )
        })
        .collect::<Vec<_>>();
    painter.add(egui::Shape::line(
        trace.clone(),
        Stroke::new(2.0_f32, SMITH_TRACE),
    ));
    for point in trace.iter().step_by((trace.len() / 80).max(1)) {
        painter.circle_filled(*point, 1.8, SMITH_TRACE);
    }
    if let Some(first) = trace.first() {
        painter.circle_filled(*first, 4.0, SCOPE_COLORS[0]);
    }
    if let Some(last) = trace.last() {
        painter.circle_filled(*last, 4.0, SCOPE_COLORS[1]);
    }

    if pinned_point.is_some_and(|index| index >= trace.len()) {
        *pinned_point = None;
    }
    let hovered_point = response.hover_pos().and_then(|pointer| {
        trace
            .iter()
            .enumerate()
            .map(|(index, position)| (index, position.distance(pointer)))
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .filter(|(_, distance)| *distance <= 16.0)
            .map(|(index, _)| index)
    });
    if response.clicked() && !response.double_clicked() {
        *pinned_point = match hovered_point {
            Some(index) if *pinned_point == Some(index) => None,
            Some(index) => Some(index),
            None => None,
        };
    }
    if let Some(index) = hovered_point.or(*pinned_point) {
        let point = sweep.points[index];
        let position = trace[index];
        painter.circle_stroke(position, 5.5, Stroke::new(1.5_f32, Color32::WHITE));
        let instruction = if *pinned_point == Some(index) {
            "Pinned · select text and copy"
        } else {
            "Click trace to pin · select text to copy"
        };
        let text = format!("{}\n\n{instruction}", point_label(point));
        let galley = painter.layout_no_wrap(text.clone(), FontId::monospace(11.0), Color32::WHITE);
        let hint_rect = smith_hint_rect(rect, position, galley.size());
        paint_selectable_smith_hint(ui.ctx(), hint_rect, text);
    }
}

fn paint_selectable_smith_hint(context: &egui::Context, rect: Rect, text: String) {
    egui::Area::new(egui::Id::new("vna_smith_selectable_hint"))
        .order(egui::Order::Tooltip)
        .fixed_pos(rect.min)
        .movable(false)
        .interactable(true)
        .show(context, |ui| {
            egui::Frame::none()
                .fill(Color32::from_rgba_unmultiplied(8, 12, 16, 238))
                .stroke(Stroke::new(1.0_f32, SMITH_GRID))
                .rounding(egui::Rounding::same(4.0))
                .inner_margin(egui::Margin::same(SMITH_HINT_PADDING))
                .show(ui, |ui| {
                    ui.style_mut().interaction.selectable_labels = true;
                    ui.add(
                        egui::Label::new(
                            RichText::new(text)
                                .font(FontId::monospace(11.0))
                                .color(Color32::WHITE),
                        )
                        .selectable(true),
                    );
                });
        });
}

const LOG_MAG_DB_FLOOR: f64 = -200.0;
const LOG_MAG_COLOR: Color32 = SMITH_TRACE;
const PHASE_COLOR: Color32 = ORANGE;
const CARTESIAN_GRID_DIVISIONS_X: usize = 5;
const CARTESIAN_GRID_DIVISIONS_Y: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq)]
struct LogMagnitudePhasePoint {
    frequency_hz: f64,
    log_magnitude_db: f64,
    phase_degrees: f64,
}

fn log_magnitude_phase_point(point: VnaPoint, channel: VnaChannel) -> LogMagnitudePhasePoint {
    let (real, imaginary) = channel.complex(point);
    let magnitude = real.hypot(imaginary);
    let log_magnitude_db = if magnitude > 0.0 {
        (20.0 * magnitude.log10()).max(LOG_MAG_DB_FLOOR)
    } else {
        LOG_MAG_DB_FLOOR
    };
    LogMagnitudePhasePoint {
        frequency_hz: point.frequency_hz as f64,
        log_magnitude_db,
        phase_degrees: imaginary.atan2(real).to_degrees(),
    }
}

fn paint_log_magnitude_phase_chart(
    ui: &egui::Ui,
    rect: Rect,
    sweep: Option<&wavecraft_vna::VnaSweep>,
    response: &egui::Response,
    channel: VnaChannel,
    frequency_hint_hz: (f64, f64),
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        egui::Rounding::same(8.0),
        Color32::from_rgb(14, 19, 25),
    );
    let plot = Rect::from_min_max(
        rect.min + Vec2::new(70.0, 34.0),
        rect.max - Vec2::new(62.0, 46.0),
    );
    if plot.width() < 40.0 || plot.height() < 40.0 {
        return;
    }

    let points = sweep
        .into_iter()
        .flat_map(|sweep| sweep.points.iter().copied())
        .map(|point| log_magnitude_phase_point(point, channel))
        .filter(|point| {
            point.frequency_hz.is_finite()
                && point.log_magnitude_db.is_finite()
                && point.phase_degrees.is_finite()
        })
        .collect::<Vec<_>>();
    let (frequency_min, frequency_max) = if points.is_empty() {
        frequency_hint_hz
    } else {
        (
            points
                .iter()
                .map(|point| point.frequency_hz)
                .fold(f64::INFINITY, f64::min),
            points
                .iter()
                .map(|point| point.frequency_hz)
                .fold(f64::NEG_INFINITY, f64::max),
        )
    };
    let (frequency_min, frequency_max) = nonzero_axis_range(frequency_min, frequency_max, 1.0);
    let (log_magnitude_min, log_magnitude_max) = if points.is_empty() {
        (-120.0, 0.0)
    } else {
        log_magnitude_axis_range(&points)
    };

    paint_log_magnitude_phase_grid(
        &painter,
        plot,
        frequency_min,
        frequency_max,
        log_magnitude_min,
        log_magnitude_max,
    );
    paint_log_magnitude_phase_legend(&painter, plot, channel);
    if points.is_empty() {
        painter.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            format!("Run a sweep to display {}", channel.label()),
            FontId::proportional(15.0),
            TEXT_MUTED,
        );
        return;
    }

    let magnitude_trace = points
        .iter()
        .map(|point| {
            Pos2::new(
                frequency_x(plot, point.frequency_hz, frequency_min, frequency_max),
                magnitude_y(
                    plot,
                    point.log_magnitude_db,
                    log_magnitude_min,
                    log_magnitude_max,
                ),
            )
        })
        .collect::<Vec<_>>();
    let phase_trace = points
        .iter()
        .map(|point| {
            Pos2::new(
                frequency_x(plot, point.frequency_hz, frequency_min, frequency_max),
                phase_y(plot, point.phase_degrees),
            )
        })
        .collect::<Vec<_>>();
    painter.add(egui::Shape::line(
        magnitude_trace.clone(),
        Stroke::new(2.0_f32, LOG_MAG_COLOR),
    ));
    painter.add(egui::Shape::line(
        phase_trace.clone(),
        Stroke::new(1.75_f32, PHASE_COLOR),
    ));
    paint_cartesian_trace_endpoints(&painter, &magnitude_trace, LOG_MAG_COLOR);
    paint_cartesian_trace_endpoints(&painter, &phase_trace, PHASE_COLOR);

    if let Some(pointer) = response
        .hover_pos()
        .filter(|pointer| plot.contains(*pointer))
    {
        if let Some((index, _)) = magnitude_trace
            .iter()
            .enumerate()
            .map(|(index, position)| (index, (position.x - pointer.x).abs()))
            .min_by(|left, right| left.1.total_cmp(&right.1))
        {
            let magnitude_position = magnitude_trace[index];
            let phase_position = phase_trace[index];
            let point = points[index];
            painter.line_segment(
                [
                    Pos2::new(magnitude_position.x, plot.top()),
                    Pos2::new(magnitude_position.x, plot.bottom()),
                ],
                Stroke::new(1.0_f32, TEXT_MUTED.gamma_multiply(0.65)),
            );
            painter.circle_filled(magnitude_position, 4.0, LOG_MAG_COLOR);
            painter.circle_filled(phase_position, 4.0, PHASE_COLOR);
            let text = format!(
                "{:.6} MHz\n{}  {:+.2} dB\nPhase  {:+.2}°",
                point.frequency_hz / 1_000_000.0,
                channel.label(),
                point.log_magnitude_db,
                point.phase_degrees,
            );
            let galley = painter.layout_no_wrap(text, FontId::monospace(11.0), Color32::WHITE);
            let hint_rect = smith_hint_rect(rect, magnitude_position, galley.size());
            painter.rect_filled(
                hint_rect,
                egui::Rounding::same(4.0),
                Color32::from_rgba_unmultiplied(8, 12, 16, 238),
            );
            painter.rect_stroke(
                hint_rect,
                egui::Rounding::same(4.0),
                Stroke::new(1.0_f32, SMITH_GRID),
            );
            painter.galley(
                hint_rect.min + Vec2::splat(SMITH_HINT_PADDING),
                galley,
                Color32::WHITE,
            );
        }
    }
}

fn paint_log_magnitude_phase_grid(
    painter: &egui::Painter,
    plot: Rect,
    frequency_min: f64,
    frequency_max: f64,
    log_magnitude_min: f64,
    log_magnitude_max: f64,
) {
    for index in 0..=CARTESIAN_GRID_DIVISIONS_Y {
        let fraction = index as f64 / CARTESIAN_GRID_DIVISIONS_Y as f64;
        let y = plot.bottom() - plot.height() * fraction as f32;
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(0.75_f32, SMITH_GRID),
        );
        let magnitude = log_magnitude_min + fraction * (log_magnitude_max - log_magnitude_min);
        let phase = -180.0 + fraction * 360.0;
        painter.text(
            Pos2::new(plot.left() - 8.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("{magnitude:.0}"),
            FontId::monospace(10.0),
            LOG_MAG_COLOR,
        );
        painter.text(
            Pos2::new(plot.right() + 8.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{phase:.0}°"),
            FontId::monospace(10.0),
            PHASE_COLOR,
        );
    }
    for index in 0..=CARTESIAN_GRID_DIVISIONS_X {
        let fraction = index as f64 / CARTESIAN_GRID_DIVISIONS_X as f64;
        let x = plot.left() + plot.width() * fraction as f32;
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(0.75_f32, SMITH_GRID),
        );
        let frequency_hz = frequency_min + fraction * (frequency_max - frequency_min);
        painter.text(
            Pos2::new(x, plot.bottom() + 8.0),
            egui::Align2::CENTER_TOP,
            format_frequency_mhz(frequency_hz),
            FontId::monospace(10.0),
            TEXT_MUTED,
        );
    }
    painter.rect_stroke(plot, egui::Rounding::ZERO, Stroke::new(1.0_f32, GRID));
    painter.text(
        Pos2::new(plot.left(), plot.top() - 8.0),
        egui::Align2::LEFT_BOTTOM,
        "LOG MAG (dB)",
        FontId::monospace(10.0),
        LOG_MAG_COLOR,
    );
    painter.text(
        Pos2::new(plot.right(), plot.top() - 8.0),
        egui::Align2::RIGHT_BOTTOM,
        "PHASE (°)",
        FontId::monospace(10.0),
        PHASE_COLOR,
    );
    painter.text(
        Pos2::new(plot.center().x, plot.bottom() + 30.0),
        egui::Align2::CENTER_CENTER,
        "FREQUENCY (MHz)",
        FontId::monospace(10.0),
        TEXT_MUTED,
    );
}

fn paint_log_magnitude_phase_legend(painter: &egui::Painter, plot: Rect, channel: VnaChannel) {
    let y = plot.top() + 14.0;
    let first_x = plot.left() + 14.0;
    painter.line_segment(
        [Pos2::new(first_x, y), Pos2::new(first_x + 24.0, y)],
        Stroke::new(2.0_f32, LOG_MAG_COLOR),
    );
    painter.text(
        Pos2::new(first_x + 30.0, y),
        egui::Align2::LEFT_CENTER,
        format!("{} LOG MAG", channel.label()),
        FontId::monospace(10.0),
        LOG_MAG_COLOR,
    );
    let second_x = first_x + 126.0;
    painter.line_segment(
        [Pos2::new(second_x, y), Pos2::new(second_x + 24.0, y)],
        Stroke::new(1.75_f32, PHASE_COLOR),
    );
    painter.text(
        Pos2::new(second_x + 30.0, y),
        egui::Align2::LEFT_CENTER,
        "PHASE",
        FontId::monospace(10.0),
        PHASE_COLOR,
    );
}

fn paint_cartesian_trace_endpoints(painter: &egui::Painter, trace: &[Pos2], color: Color32) {
    if let Some(first) = trace.first() {
        painter.circle_filled(*first, 3.5, color);
    }
    if let Some(last) = trace.last() {
        painter.circle_filled(*last, 3.5, color);
    }
}

fn nonzero_axis_range(minimum: f64, maximum: f64, minimum_span: f64) -> (f64, f64) {
    if maximum - minimum >= minimum_span {
        (minimum, maximum)
    } else {
        let center = (minimum + maximum) * 0.5;
        (center - minimum_span * 0.5, center + minimum_span * 0.5)
    }
}

fn log_magnitude_axis_range(points: &[LogMagnitudePhasePoint]) -> (f64, f64) {
    let minimum = points
        .iter()
        .map(|point| point.log_magnitude_db)
        .fold(f64::INFINITY, f64::min);
    let maximum = points
        .iter()
        .map(|point| point.log_magnitude_db)
        .fold(f64::NEG_INFINITY, f64::max);
    let span = maximum - minimum;
    let padding = (span * 0.08).max(2.0);
    let required_lower = ((minimum - padding) / 10.0).floor() * 10.0;
    let upper = ((maximum + padding) / 10.0).ceil() * 10.0;
    let required_span = (upper - required_lower).max(60.0);
    let axis_span = (required_span / 60.0).ceil() * 60.0;
    (upper - axis_span, upper)
}

fn frequency_x(plot: Rect, frequency_hz: f64, minimum_hz: f64, maximum_hz: f64) -> f32 {
    plot.left() + plot.width() * ((frequency_hz - minimum_hz) / (maximum_hz - minimum_hz)) as f32
}

fn magnitude_y(plot: Rect, value: f64, minimum: f64, maximum: f64) -> f32 {
    plot.bottom() - plot.height() * ((value - minimum) / (maximum - minimum)) as f32
}

fn phase_y(plot: Rect, phase_degrees: f64) -> f32 {
    plot.bottom() - plot.height() * ((phase_degrees + 180.0) / 360.0) as f32
}

fn format_frequency_mhz(frequency_hz: f64) -> String {
    let frequency_mhz = frequency_hz / 1_000_000.0;
    if frequency_mhz.abs() >= 1_000.0 {
        format!("{frequency_mhz:.1}")
    } else if frequency_mhz.abs() >= 10.0 {
        format!("{frequency_mhz:.2}")
    } else {
        format!("{frequency_mhz:.3}")
    }
}

const SMITH_HINT_PADDING: f32 = 6.0;
const SMITH_HINT_GAP: f32 = 10.0;
const SMITH_HINT_MARGIN: f32 = 6.0;

fn smith_hint_rect(plot: Rect, anchor: Pos2, content_size: Vec2) -> Rect {
    let size = content_size + Vec2::splat(2.0 * SMITH_HINT_PADDING);
    let inner = plot.shrink(SMITH_HINT_MARGIN);
    let preferred_x = if anchor.x + SMITH_HINT_GAP + size.x <= inner.right() {
        anchor.x + SMITH_HINT_GAP
    } else {
        anchor.x - SMITH_HINT_GAP - size.x
    };
    let preferred_y = if anchor.y - SMITH_HINT_GAP - size.y >= inner.top() {
        anchor.y - SMITH_HINT_GAP - size.y
    } else {
        anchor.y + SMITH_HINT_GAP
    };
    let max_x = (inner.right() - size.x).max(inner.left());
    let max_y = (inner.bottom() - size.y).max(inner.top());
    let min = Pos2::new(
        preferred_x.clamp(inner.left(), max_x),
        preferred_y.clamp(inner.top(), max_y),
    );
    Rect::from_min_size(min, size)
}

fn impedance_to_gamma(resistance: f64, reactance: f64) -> (f64, f64) {
    let denominator = (resistance + 1.0).powi(2) + reactance.powi(2);
    (
        (resistance.mul_add(resistance, reactance * reactance) - 1.0) / denominator,
        (2.0 * reactance) / denominator,
    )
}

fn chart_position(
    center: Pos2,
    radius: f32,
    gamma: (f64, f64),
    viewport: SmithChartViewport,
) -> Pos2 {
    let magnitude = gamma.0.hypot(gamma.1);
    let scale = if magnitude > 1.08 {
        1.08 / magnitude
    } else {
        1.0
    };
    let gamma = (gamma.0 * scale, gamma.1 * scale);
    let view_scale = f64::from(radius * viewport.zoom);
    Pos2::new(
        center.x + ((gamma.0 - viewport.center_gamma.0) * view_scale) as f32,
        center.y - ((gamma.1 - viewport.center_gamma.1) * view_scale) as f32,
    )
}

fn format_resistance(resistance: f64) -> String {
    if resistance == 0.0 {
        "0".to_owned()
    } else if resistance.fract() == 0.0 {
        format!("{resistance:.0}")
    } else {
        format!("{resistance:.1}")
    }
}

const VNA_REFERENCE_IMPEDANCE_OHMS: f64 = 50.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum ReactiveComponent {
    Inductor(f64),
    Capacitor(f64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LMatchTopology {
    ShuntOnReferenceSide,
    ShuntOnLoadSide,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LMatchSolution {
    topology: LMatchTopology,
    series_reactance_ohms: f64,
    shunt_susceptance_siemens: f64,
}

fn impedance_from_gamma(gamma: (f64, f64)) -> Option<(f64, f64)> {
    let denominator = (1.0 - gamma.0).powi(2) + gamma.1.powi(2);
    if denominator <= f64::EPSILON {
        return None;
    }
    let resistance = VNA_REFERENCE_IMPEDANCE_OHMS
        * (1.0 - gamma.0.mul_add(gamma.0, gamma.1 * gamma.1))
        / denominator;
    let reactance = VNA_REFERENCE_IMPEDANCE_OHMS * 2.0 * gamma.1 / denominator;
    (resistance.is_finite() && reactance.is_finite()).then_some((resistance, reactance))
}

fn component_for_series_reactance(
    reactance_ohms: f64,
    frequency_hz: f64,
) -> Option<ReactiveComponent> {
    let angular_frequency = std::f64::consts::TAU * frequency_hz;
    if !angular_frequency.is_finite() || angular_frequency <= 0.0 || reactance_ohms.abs() <= 1.0e-9
    {
        return None;
    }
    if reactance_ohms > 0.0 {
        Some(ReactiveComponent::Inductor(
            reactance_ohms / angular_frequency,
        ))
    } else {
        Some(ReactiveComponent::Capacitor(
            -1.0 / (angular_frequency * reactance_ohms),
        ))
    }
}

fn component_for_shunt_susceptance(
    susceptance_siemens: f64,
    frequency_hz: f64,
) -> Option<ReactiveComponent> {
    let angular_frequency = std::f64::consts::TAU * frequency_hz;
    if !angular_frequency.is_finite()
        || angular_frequency <= 0.0
        || susceptance_siemens.abs() <= 1.0e-12
    {
        return None;
    }
    if susceptance_siemens > 0.0 {
        Some(ReactiveComponent::Capacitor(
            susceptance_siemens / angular_frequency,
        ))
    } else {
        Some(ReactiveComponent::Inductor(
            -1.0 / (angular_frequency * susceptance_siemens),
        ))
    }
}

fn l_match_solution(resistance: f64, reactance: f64) -> Option<LMatchSolution> {
    if !resistance.is_finite()
        || !reactance.is_finite()
        || resistance <= 0.0
        || (resistance - VNA_REFERENCE_IMPEDANCE_OHMS).abs() <= 1.0e-9
    {
        return None;
    }
    let mut candidates = Vec::with_capacity(2);
    if resistance < VNA_REFERENCE_IMPEDANCE_OHMS {
        let transformed_reactance =
            (resistance * (VNA_REFERENCE_IMPEDANCE_OHMS - resistance)).sqrt();
        for sign in [-1.0_f64, 1.0] {
            let total_reactance = sign * transformed_reactance;
            candidates.push(LMatchSolution {
                topology: LMatchTopology::ShuntOnReferenceSide,
                series_reactance_ohms: total_reactance - reactance,
                shunt_susceptance_siemens: total_reactance
                    / (resistance * VNA_REFERENCE_IMPEDANCE_OHMS),
            });
        }
    } else {
        let load_denominator = resistance.mul_add(resistance, reactance * reactance);
        let conductance = resistance / load_denominator;
        let load_susceptance = -reactance / load_denominator;
        let transformed_susceptance = (conductance / VNA_REFERENCE_IMPEDANCE_OHMS
            - conductance * conductance)
            .max(0.0)
            .sqrt();
        for sign in [-1.0_f64, 1.0] {
            let total_susceptance = sign * transformed_susceptance;
            candidates.push(LMatchSolution {
                topology: LMatchTopology::ShuntOnLoadSide,
                series_reactance_ohms: VNA_REFERENCE_IMPEDANCE_OHMS * total_susceptance
                    / conductance,
                shunt_susceptance_siemens: total_susceptance - load_susceptance,
            });
        }
    }
    candidates
        .into_iter()
        .min_by(|left, right| l_match_complexity(*left).total_cmp(&l_match_complexity(*right)))
}

fn l_match_complexity(solution: LMatchSolution) -> f64 {
    solution.series_reactance_ohms.abs() / VNA_REFERENCE_IMPEDANCE_OHMS
        + solution.shunt_susceptance_siemens.abs() * VNA_REFERENCE_IMPEDANCE_OHMS
}

fn format_reactive_component(component: ReactiveComponent) -> String {
    match component {
        ReactiveComponent::Inductor(henries) => {
            let (value, unit) = if henries >= 1.0e-3 {
                (henries * 1.0e3, "mH")
            } else if henries >= 1.0e-6 {
                (henries * 1.0e6, "µH")
            } else if henries >= 1.0e-9 {
                (henries * 1.0e9, "nH")
            } else {
                (henries * 1.0e12, "pH")
            };
            format!("L {} {unit}", format_component_value(value))
        }
        ReactiveComponent::Capacitor(farads) => {
            let (value, unit) = if farads >= 1.0e-6 {
                (farads * 1.0e6, "µF")
            } else if farads >= 1.0e-9 {
                (farads * 1.0e9, "nF")
            } else if farads >= 1.0e-12 {
                (farads * 1.0e12, "pF")
            } else {
                (farads * 1.0e15, "fF")
            };
            format!("C {} {unit}", format_component_value(value))
        }
    }
}

fn format_component_value(value: f64) -> String {
    if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

fn matching_label(frequency_hz: f64, resistance: f64, reactance: f64) -> String {
    let mut lines = Vec::new();
    let nature = if reactance > 1.0e-6 {
        "inductive"
    } else if reactance < -1.0e-6 {
        "capacitive"
    } else {
        "resistive"
    };
    let sign = if reactance < 0.0 { "−" } else { "+" };
    lines.push(format!(
        "Z {resistance:.2} {sign} j{:.2} Ω · {nature}",
        reactance.abs()
    ));

    if let Some(component) = component_for_series_reactance(-reactance, frequency_hz) {
        let label = if (resistance - VNA_REFERENCE_IMPEDANCE_OHMS).abs() <= 0.5 {
            "Series match"
        } else {
            "Series resonance"
        };
        lines.push(format!(
            "{label}: {} → {resistance:.1} Ω",
            format_reactive_component(component)
        ));
    } else if (resistance - VNA_REFERENCE_IMPEDANCE_OHMS).abs() <= 0.5 {
        lines.push("Already matched to 50 Ω".to_owned());
    }

    if (resistance - VNA_REFERENCE_IMPEDANCE_OHMS).abs() > 0.5 {
        if let Some(solution) = l_match_solution(resistance, reactance) {
            let series =
                component_for_series_reactance(solution.series_reactance_ohms, frequency_hz)
                    .map(format_reactive_component)
                    .unwrap_or_else(|| "no series element".to_owned());
            let shunt =
                component_for_shunt_susceptance(solution.shunt_susceptance_siemens, frequency_hz)
                    .map(format_reactive_component)
                    .unwrap_or_else(|| "no shunt element".to_owned());
            lines.push("50 Ω L-match (ideal):".to_owned());
            match solution.topology {
                LMatchTopology::ShuntOnReferenceSide => {
                    lines.push(format!("  Shunt {shunt} on 50 Ω side"));
                    lines.push(format!("  Series {series}"));
                }
                LMatchTopology::ShuntOnLoadSide => {
                    lines.push(format!("  Series {series}"));
                    lines.push(format!("  Shunt {shunt} at antenna"));
                }
            }
        } else if resistance <= 0.0 {
            lines.push("Passive L-match unavailable for this Z".to_owned());
        }
    }
    lines.join("\n")
}

fn point_label(point: VnaPoint) -> String {
    let magnitude = point.s11_magnitude();
    let return_loss = if magnitude > 0.0 {
        -20.0 * magnitude.log10()
    } else {
        f64::INFINITY
    };
    let vswr = if magnitude < 1.0 {
        (1.0 + magnitude) / (1.0 - magnitude)
    } else {
        f64::INFINITY
    };
    let mut label = format!(
        "{:.6} MHz\nS11 {:+.4} {:+.4}j\nRL {:.2} dB · VSWR {:.2}",
        point.frequency_hz as f64 / 1_000_000.0,
        point.s11_real,
        point.s11_imaginary,
        return_loss,
        vswr,
    );
    if let Some((resistance, reactance)) =
        impedance_from_gamma((point.s11_real, point.s11_imaginary))
    {
        label.push('\n');
        label.push_str(&matching_label(
            point.frequency_hz as f64,
            resistance,
            reactance,
        ));
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::app;

    fn painted_text(
        context: &egui::Context,
        paint: impl FnMut(&egui::Context),
    ) -> Vec<(String, egui::Pos2)> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 900.0),
            )),
            ..egui::RawInput::default()
        };
        let output = context.run(input, paint);
        let mut texts = Vec::new();
        fn collect(shape: &egui::Shape, texts: &mut Vec<(String, egui::Pos2)>) {
            match shape {
                egui::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|shape| collect(shape, texts));
                }
                egui::Shape::Text(text) => texts.push((
                    text.galley.text().to_owned(),
                    text.visual_bounding_rect().center(),
                )),
                _ => {}
            }
        }
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut texts);
        }
        texts
    }

    #[test]
    fn impedance_transform_places_matched_load_at_chart_center() {
        let gamma = impedance_to_gamma(1.0, 0.0);
        assert!(gamma.0.abs() < 1.0e-12);
        assert!(gamma.1.abs() < 1.0e-12);
    }

    #[test]
    fn impedance_transform_places_open_at_right_edge() {
        let gamma = impedance_to_gamma(1.0e12, 0.0);
        assert!((gamma.0 - 1.0).abs() < 1.0e-9);
    }

    #[test]
    fn gamma_converts_back_to_resistance_and_reactance() {
        let gamma = impedance_to_gamma(0.7, -0.4);
        let impedance = impedance_from_gamma(gamma).unwrap();

        assert!((impedance.0 - 35.0).abs() < 1.0e-9);
        assert!((impedance.1 + 20.0).abs() < 1.0e-9);
    }

    #[test]
    fn capacitive_load_recommends_a_series_inductor_for_resonance() {
        let component = component_for_series_reactance(20.0, 100_000_000.0).unwrap();

        let ReactiveComponent::Inductor(henries) = component else {
            panic!("expected an inductor");
        };
        assert!((henries - 31.830_988_618e-9).abs() < 1.0e-15);
    }

    #[test]
    fn l_match_solutions_transform_low_and_high_resistance_loads_to_50_ohms() {
        for load in [(35.0, -20.0), (100.0, 30.0)] {
            let solution = l_match_solution(load.0, load.1).unwrap();
            let input = l_match_input_impedance(load, solution);

            assert!((input.0 - VNA_REFERENCE_IMPEDANCE_OHMS).abs() < 1.0e-9);
            assert!(input.1.abs() < 1.0e-9);
        }
    }

    #[test]
    fn smith_point_label_includes_impedance_and_matching_components() {
        let gamma = impedance_to_gamma(0.7, -0.4);
        let label = point_label(VnaPoint {
            frequency_hz: 100_000_000,
            s11_real: gamma.0,
            s11_imaginary: gamma.1,
            s21_real: 0.0,
            s21_imaginary: 0.0,
        });

        assert!(label.contains("Z 35.00 − j20.00 Ω · capacitive"));
        assert!(label.contains("Series resonance: L 31.8 nH → 35.0 Ω"));
        assert!(label.contains("50 Ω L-match (ideal):"));
        assert!(label.contains("Shunt L"));
        assert!(label.contains("Series C"));
    }

    fn l_match_input_impedance(load: (f64, f64), solution: LMatchSolution) -> (f64, f64) {
        match solution.topology {
            LMatchTopology::ShuntOnReferenceSide => {
                let branch_reactance = load.1 + solution.series_reactance_ohms;
                let branch_denominator =
                    load.0.mul_add(load.0, branch_reactance * branch_reactance);
                let conductance = load.0 / branch_denominator;
                let susceptance =
                    -branch_reactance / branch_denominator + solution.shunt_susceptance_siemens;
                let input_denominator = conductance.mul_add(conductance, susceptance * susceptance);
                (
                    conductance / input_denominator,
                    -susceptance / input_denominator,
                )
            }
            LMatchTopology::ShuntOnLoadSide => {
                let load_denominator = load.0.mul_add(load.0, load.1 * load.1);
                let conductance = load.0 / load_denominator;
                let susceptance = -load.1 / load_denominator + solution.shunt_susceptance_siemens;
                let parallel_denominator =
                    conductance.mul_add(conductance, susceptance * susceptance);
                (
                    conductance / parallel_denominator,
                    -susceptance / parallel_denominator + solution.series_reactance_ohms,
                )
            }
        }
    }

    #[test]
    fn log_magnitude_and_phase_follow_the_selected_s_parameter() {
        let point = VnaPoint {
            frequency_hz: 10_000_000,
            s11_real: 0.5,
            s11_imaginary: 0.0,
            s21_real: 0.0,
            s21_imaginary: 1.0,
        };

        let s11 = log_magnitude_phase_point(point, VnaChannel::S11);
        let s21 = log_magnitude_phase_point(point, VnaChannel::S21);

        assert!((s11.log_magnitude_db + 6.020_599_913).abs() < 1.0e-6);
        assert!(s11.phase_degrees.abs() < 1.0e-12);
        assert!(s21.log_magnitude_db.abs() < 1.0e-12);
        assert!((s21.phase_degrees - 90.0).abs() < 1.0e-12);
    }

    #[test]
    fn zero_magnitude_uses_a_finite_decibel_floor() {
        let point = VnaPoint {
            frequency_hz: 10_000_000,
            s11_real: 0.0,
            s11_imaginary: 0.0,
            s21_real: 0.0,
            s21_imaginary: 0.0,
        };

        let plotted = log_magnitude_phase_point(point, VnaChannel::S21);

        assert_eq!(plotted.log_magnitude_db, LOG_MAG_DB_FLOOR);
        assert!(plotted.phase_degrees.is_finite());
    }

    #[test]
    fn smith_hint_switches_sides_and_stays_inside_the_plot() {
        let plot = Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(500.0, 320.0));
        let content = Vec2::new(170.0, 55.0);
        for anchor in [
            plot.left_top(),
            plot.right_top(),
            plot.left_bottom(),
            plot.right_bottom(),
        ] {
            let hint = smith_hint_rect(plot, anchor, content);
            assert!(hint.left() >= plot.left());
            assert!(hint.right() <= plot.right());
            assert!(hint.top() >= plot.top());
            assert!(hint.bottom() <= plot.bottom());
        }
        let left_hint = smith_hint_rect(plot, plot.left_center(), content);
        let right_hint = smith_hint_rect(plot, plot.right_center(), content);
        assert!(left_hint.left() > plot.left_center().x);
        assert!(right_hint.right() < plot.right_center().x);
    }

    #[test]
    fn smith_zoom_keeps_the_gamma_under_the_pointer_fixed() {
        let center = Pos2::new(300.0, 220.0);
        let pointer = Pos2::new(390.0, 155.0);
        let radius = 180.0;
        let mut viewport = SmithChartViewport::default();
        let anchor = viewport.gamma_at(center, radius, pointer);

        viewport.zoom_about(center, radius, pointer, 2.0);

        let position = chart_position(center, radius, anchor, viewport);
        assert!(position.distance(pointer) < 1.0e-3);
    }

    #[test]
    fn smith_zoom_scales_real_and_imaginary_directions_equally() {
        let center = Pos2::new(300.0, 220.0);
        let radius = 180.0;
        let viewport = SmithChartViewport {
            zoom: 2.5,
            center_gamma: (0.0, 0.0),
        };
        let distances = [(0.5, 0.0), (-0.5, 0.0), (0.0, 0.5), (0.0, -0.5)]
            .map(|gamma| chart_position(center, radius, gamma, viewport).distance(center));

        for distance in distances.iter().skip(1) {
            assert!((distance - distances[0]).abs() < 1.0e-3);
        }
    }

    #[test]
    fn toolbar_uses_rf_style_single_and_run_controls_on_the_right() {
        let context = egui::Context::default();
        let mut app = app();
        let texts = painted_text(&context, |context| {
            app.paint_vna_toolbar(context, &mut FrameActions::default());
        });
        let position = |label: &str| {
            texts
                .iter()
                .find_map(|(text, position)| (text == label).then_some(*position))
                .unwrap()
        };
        assert!(position("▶ Single").x < position("▶ Run").x);
        assert!(position("↻ Refresh").x < position("📷").x);
        assert!(!texts
            .iter()
            .any(|(text, _)| text == "LiteVNA ready" || text == "No LiteVNA"));
    }

    #[test]
    fn vna_card_has_smith_and_log_magnitude_phase_tabs_with_a_channel_source() {
        let context = egui::Context::default();
        let mut app = app();
        let texts = painted_text(&context, |context| {
            app.paint_vna_central_panel(context, false);
        });
        assert!(texts.iter().any(|(text, _)| text == "SMITH CHART"));
        assert!(texts.iter().any(|(text, _)| text == "LOG MAG / PHASE"));

        app.vna_plot_tab = VnaPlotTab::LogMagnitudePhase;
        app.vna_plot_channel = VnaChannel::S21;
        let texts = painted_text(&context, |context| {
            app.paint_vna_central_panel(context, false);
        });
        assert!(texts.iter().any(|(text, _)| text == "Source"));
        assert!(texts.iter().any(|(text, _)| text == "S21"));
        assert!(texts
            .iter()
            .any(|(text, _)| text == "Run a sweep to display S21"));
    }

    #[test]
    fn setup_uses_a_litevna_settings_card_without_a_single_sweep_button() {
        let context = egui::Context::default();
        let mut app = app();
        let texts = painted_text(&context, |context| {
            app.paint_vna_setup_panel(context, &mut FrameActions::default());
        });
        assert!(texts.iter().any(|(text, _)| text == "LiteVNA 64"));
        assert!(texts.iter().any(|(text, _)| text == "▶ Open"));
        assert!(texts.iter().any(|(text, _)| text == "Start frequency"));
        assert_eq!(texts.iter().filter(|(text, _)| text == "MHz").count(), 2);
        assert!(texts.iter().any(|(text, _)| text == "Save to file"));
        assert!(texts.iter().any(|(text, _)| text == "Load from file"));
        assert!(texts
            .iter()
            .any(|(text, _)| text == "No active calibration"));
        assert!(texts.iter().any(|(text, _)| text == "💾 Save VNA settings"));
        assert!(!texts.iter().any(|(text, _)| text == "▶ Single Sweep"));
        assert!(!texts
            .iter()
            .any(|(text, _)| text == "Connect LiteVNA and press Refresh"));
    }

    #[test]
    fn vna_save_button_requires_valid_frequency_inputs() {
        let mut app = app();
        assert!(vna_frequency_inputs_are_valid(&app));

        app.vna_stop_frequency_input = "invalid".to_owned();
        assert!(!vna_frequency_inputs_are_valid(&app));

        app.vna_stop_frequency_input = "0.5".to_owned();
        assert!(!vna_frequency_inputs_are_valid(&app));
    }
}
