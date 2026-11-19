//! LiteVNA setup controls and Smith chart rendering.

use crate::{
    apply_setup_panel_typography, card_frame, egui, paint_capture_settings_save_button,
    toolbar_action_button, CalibrationStandard, Color32, FontId, FrameActions, PendingSnapshot,
    Pos2, Rect, RichText, SavedCaptureSettingsKind, Sense, Stroke, Vec2, VnaPoint, WaveCraftApp,
    ACCENT, BACKGROUND, GRID, PANEL, SCOPE_COLORS, TEXT_MUTED,
};

const VNA_PANEL_WIDTH: f32 = 300.0;
const SMITH_GRID: Color32 = Color32::from_rgb(54, 68, 81);
const SMITH_TRACE: Color32 = Color32::from_rgb(78, 205, 196);
const POINT_COUNTS: [u16; 6] = [51, 101, 201, 401, 801, 1024];
const AVERAGING_VALUES: [u8; 7] = [1, 2, 4, 8, 16, 32, 64];

impl WaveCraftApp {
    pub(crate) fn paint_vna_calibration_notice(&mut self, context: &egui::Context) {
        if !self.vna_calibration_notice_open {
            return;
        }
        let calibration_state = self.vna_calibration.as_ref().map(|calibration| {
            self.vna_settings
                .sweep_config()
                .is_ok_and(|config| calibration.matches_config(config))
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
            .id(egui::Id::new("vna_calibration_notice"))
            .open(&mut open)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(560.0)
            .show(context, |ui| {
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
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(toolbar_action_button("Got it", ACCENT))
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
                        let selected_text = self
                            .selected_vna()
                            .map(|device| device.display_name.clone())
                            .unwrap_or_else(|| "No LiteVNA detected".to_owned());
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
                        if connection_open {
                            ui.small(
                                RichText::new("Port open · persistent USB session").color(ACCENT),
                            );
                        }
                        if let Some(error) = &self.vna_discovery_error {
                            ui.small(RichText::new(error).color(Color32::LIGHT_RED));
                        } else if let Some(device) = self.selected_vna() {
                            ui.small(
                                RichText::new(device.port_name.clone())
                                    .monospace()
                                    .color(TEXT_MUTED),
                            );
                        } else {
                            ui.small(
                                RichText::new("Connect LiteVNA and press Refresh")
                                    .color(TEXT_MUTED),
                            );
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
                        ui.vertical(|ui| {
                            ui.label(RichText::new("SMITH CHART").strong().color(ACCENT));
                            let subtitle = if self.vna_sweep_calibrated {
                                "SOL-calibrated S11 · Z₀ = 50 Ω"
                            } else {
                                "Raw S11 reflection coefficient · Z₀ = 50 Ω"
                            };
                            ui.label(RichText::new(subtitle).color(TEXT_MUTED));
                        });
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
                    ui.add_space(12.0);
                    let available = ui.available_size();
                    let plot_size = Vec2::new(available.x.max(120.0), available.y.max(120.0));
                    let (rect, response) = ui.allocate_exact_size(plot_size, Sense::hover());
                    plot_rect = Some(rect);
                    paint_smith_chart(ui, rect, self.vna_sweep.as_ref(), &response);
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
    if let Some(calibration) = &app.vna_calibration {
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
            "Active"
        } else {
            "Saved; sweep differs"
        };
        ui.small(
            RichText::new(format!("{state} · {range}")).color(if matches {
                ACCENT
            } else {
                TEXT_MUTED
            }),
        );
        if ui
            .add_enabled(
                !app.is_busy(),
                toolbar_action_button("Clear calibration", Color32::LIGHT_RED)
                    .min_size(Vec2::new(ui.available_width(), 28.0)),
            )
            .clicked()
        {
            actions.vna_clear_calibration = true;
        }
    } else {
        ui.small(RichText::new("No saved calibration").color(TEXT_MUTED));
    }
    ui.small(
        RichText::new("Coefficients are stored in WaveCraft, not in LiteVNA.").color(TEXT_MUTED),
    );
}

fn paint_smith_chart(
    ui: &egui::Ui,
    rect: Rect,
    sweep: Option<&wavecraft_vna::VnaSweep>,
    response: &egui::Response,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        egui::Rounding::same(8.0),
        Color32::from_rgb(14, 19, 25),
    );
    let radius = rect.width().min(rect.height()) * 0.43;
    let center = rect.center();
    painter.circle_stroke(center, radius, Stroke::new(1.5_f32, GRID));
    painter.line_segment(
        [
            Pos2::new(center.x - radius, center.y),
            Pos2::new(center.x + radius, center.y),
        ],
        Stroke::new(1.0_f32, SMITH_GRID),
    );

    for resistance in [0.2_f64, 0.5, 1.0, 2.0, 5.0] {
        let points = (-160..=160)
            .map(|index| f64::from(index) / 8.0)
            .map(|reactance| impedance_to_gamma(resistance, reactance))
            .map(|gamma| chart_position(center, radius, gamma))
            .collect::<Vec<_>>();
        painter.add(egui::Shape::line(points, Stroke::new(0.75_f32, SMITH_GRID)));
    }
    for reactance in [0.2_f64, 0.5, 1.0, 2.0, 5.0] {
        for sign in [-1.0_f64, 1.0] {
            let points = (0..=240)
                .map(|index| 30.0 * f64::from(index) / 240.0)
                .map(|resistance| impedance_to_gamma(resistance, sign * reactance))
                .map(|gamma| chart_position(center, radius, gamma))
                .collect::<Vec<_>>();
            painter.add(egui::Shape::line(points, Stroke::new(0.75_f32, SMITH_GRID)));
        }
    }

    for resistance in [0.0_f64, 0.2, 0.5, 1.0, 2.0, 5.0] {
        let gamma = impedance_to_gamma(resistance, 0.0);
        let position = chart_position(center, radius, gamma);
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
        .map(|point| chart_position(center, radius, (point.s11_real, point.s11_imaginary)))
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

    if let Some(pointer) = response.hover_pos() {
        if let Some((index, distance)) = trace
            .iter()
            .enumerate()
            .map(|(index, position)| (index, position.distance(pointer)))
            .min_by(|left, right| left.1.total_cmp(&right.1))
        {
            if distance <= 16.0 {
                let point = sweep.points[index];
                let position = trace[index];
                painter.circle_stroke(position, 5.5, Stroke::new(1.5_f32, Color32::WHITE));
                let text = point_label(point);
                let galley = painter.layout_no_wrap(text, FontId::monospace(11.0), Color32::WHITE);
                let hint_rect = smith_hint_rect(rect, position, galley.size());
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

fn chart_position(center: Pos2, radius: f32, gamma: (f64, f64)) -> Pos2 {
    let magnitude = gamma.0.hypot(gamma.1);
    let scale = if magnitude > 1.08 {
        1.08 / magnitude
    } else {
        1.0
    };
    Pos2::new(
        center.x + (gamma.0 * scale) as f32 * radius,
        center.y - (gamma.1 * scale) as f32 * radius,
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
    format!(
        "{:.6} MHz\nS11 {:+.4} {:+.4}j\nRL {:.2} dB · VSWR {:.2}",
        point.frequency_hz as f64 / 1_000_000.0,
        point.s11_real,
        point.s11_imaginary,
        return_loss,
        vswr
    )
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
        assert!(texts.iter().any(|(text, _)| text == "💾 Save VNA settings"));
        assert!(!texts.iter().any(|(text, _)| text == "▶ Single Sweep"));
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
