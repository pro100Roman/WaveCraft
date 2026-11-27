//! LiteVNA setup controls and Smith chart rendering.

use super::view_shell::{settings_card, view_card, view_toolbar, workspace_panel};

use crate::{
    egui, toolbar_action_button, CalibrationStandard, Color32, FontId, FrameActions, Pos2, Rect,
    RichText, SelectableHoverText, Sense, Stroke, Vec2, VnaMatchingNetwork, VnaPoint, WaveCraftApp,
    WorkspaceView, ACCENT, GRID, ORANGE, SCOPE_COLORS, TEXT_MUTED, VNA_MATCHING_Q_MAX,
    VNA_MATCHING_Q_MIN,
};

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
        let screen_rect = context.screen_rect();
        let backdrop = egui::Area::new(egui::Id::new("vna_calibration_notice_backdrop"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen_rect.min)
            .movable(false)
            .show(context, |ui| {
                let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
                ui.painter().rect_filled(
                    rect,
                    egui::Rounding::ZERO,
                    Color32::from_black_alpha(112),
                );
            });
        context.move_to_top(backdrop.response.layer_id);
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
        let notice = egui::Window::new("VNA calibration recommended")
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
                    let response = ui.add_sized(
                        [160.0, 30.0], toolbar_action_button("Got it", ORANGE),
                    );
                    // Keep keyboard navigation inside the modal as well as pointer input.
                    response.request_focus();
                    ui.memory_mut(|memory| memory.set_focus_lock_filter(response.id, egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    }));
                    acknowledged = response.clicked();
                });
            });
        if let Some(notice) = notice {
            context.move_to_top(notice.response.layer_id);
        }
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
        let toolbar = view_toolbar(
            context,
            "Vector network analyzer",
            self.pending_snapshot.is_none(),
            |ui| {
                actions.refresh_clicked = ui
                    .add_enabled(
                        !self.is_busy() && self.vna_session.is_none(),
                        toolbar_action_button("↻ Refresh", SCOPE_COLORS[1]),
                    )
                    .clicked();
                ui.separator();
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
            },
        );
        self.settings_open |= toolbar.settings_clicked;
        actions.snapshot_clicked |= toolbar.snapshot_clicked;
    }

    pub(crate) fn paint_vna_setup_panel(
        &mut self,
        context: &egui::Context,
        actions: &mut FrameActions,
    ) {
        self.show_setup_panel(context, WorkspaceView::Vna, actions, |app, ui, actions| {
            ui.label(RichText::new("Instrument").color(TEXT_MUTED));
            let selected_text = if app.vna_discovery_error.is_some() {
                "LiteVNA discovery failed".to_owned()
            } else {
                app.selected_vna()
                    .map(|device| device.display_name.clone())
                    .unwrap_or_else(|| "No LiteVNA detected".to_owned())
            };
            ui.add_enabled_ui(
                app.vna_session.is_none() && app.vna_connection_pending.is_none(),
                |ui| {
                    egui::ComboBox::from_id_salt("vna_device")
                        .width(ui.available_width())
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            for device in &app.vna_devices {
                                ui.selectable_value(
                                    &mut app.selected_vna_port,
                                    Some(device.port_name.clone()),
                                    &device.display_name,
                                );
                            }
                        });
                },
            );
            ui.add_space(6.0);
            let connection_pending = app.vna_connection_pending.is_some();
            let connection_open = app.vna_session.is_some();
            let connection_label = if connection_pending {
                if app.vna_connection_closing {
                    "Closing…"
                } else {
                    "Opening…"
                }
            } else if connection_open {
                "■ Close"
            } else {
                "▶ Open"
            };
            let connection_color = if connection_open || app.vna_connection_closing {
                Color32::LIGHT_RED
            } else {
                ACCENT
            };
            if ui
                .add_enabled(
                    !connection_pending
                        && !app.is_busy()
                        && (connection_open || app.selected_vna().is_some()),
                    toolbar_action_button(connection_label, connection_color),
                )
                .clicked()
            {
                actions.vna_connection_clicked = true;
            }

            ui.add_space(14.0);
            paint_vna_settings_card(ui, app, actions);
            !app.is_busy() && vna_frequency_inputs_are_valid(app)
        });
    }

    pub(crate) fn paint_vna_central_panel(&mut self, context: &egui::Context) {
        let mut plot_rect = None;
        workspace_panel(context, |ui| {
            view_card(ui, |ui| {
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
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                        });
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if self.vna_plot_tab == VnaPlotTab::SmithChart {
                        let subtitle = if self.vna_sweep_calibrated {
                            "SOL-calibrated S11 · Z₀ = 50 Ω"
                        } else {
                            "Raw S11 reflection coefficient · Z₀ = 50 Ω"
                        };
                        ui.label(RichText::new(subtitle).color(TEXT_MUTED));
                    }
                    if self.vna_plot_tab == VnaPlotTab::LogMagnitudePhase
                        || self.vna_sweep_progress.is_some()
                    {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(progress) = self.vna_sweep_progress {
                                ui.add(
                                    egui::ProgressBar::new(progress.fraction())
                                        .desired_width(260.0)
                                        .text(progress.label())
                                        .animate(true),
                                );
                            }
                            if self.vna_plot_tab == VnaPlotTab::LogMagnitudePhase {
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
                            }
                        });
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
                        (
                            self.vna_settings.matching_network,
                            self.vna_settings.matching_q,
                        ),
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
    }
}

fn paint_vna_settings_card(ui: &mut egui::Ui, app: &mut WaveCraftApp, actions: &mut FrameActions) {
    settings_card(ui, |ui| {
        ui.spacing_mut().indent = 10.0;
        ui.label(RichText::new("LiteVNA 64").strong());
        ui.add_space(10.0);
        let sweep_section = egui::CollapsingHeader::new(RichText::new("Sweep settings").strong())
            .id_salt("vna_sweep_settings_section")
            .default_open(true)
            .show(ui, |ui| paint_vna_sweep_settings(ui, app));
        sweep_section.header_response.on_hover_selectable_text(
                "Frequency range, point count, and averaging used by LiteVNA sweeps and calibration captures.",
            );

        ui.separator();
        let matching_section = egui::CollapsingHeader::new(RichText::new("Matching").strong())
            .id_salt("vna_matching_settings_section")
            .default_open(true)
            .show(ui, |ui| paint_vna_matching_settings(ui, app));
        matching_section.header_response.on_hover_selectable_text(
                "Select which ideal 50 Ω matching network is calculated for the hovered Smith-chart point. L-network Q is fixed by the impedance ratio; π and T networks use the configurable design Q.",
            );

        paint_calibration_controls(ui, app, actions);
        ui.add_space(12.0);
        ui.small(
                RichText::new(
                    "USB sweeps contain raw data. WaveCraft applies its own calibration and returns LiteVNA to local mode afterward.",
                )
                .color(TEXT_MUTED),
            );
    });
}

fn paint_vna_sweep_settings(ui: &mut egui::Ui, app: &mut WaveCraftApp) {
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
        let stop_changed =
            paint_frequency_input(ui, "vna_stop_frequency", &mut app.vna_stop_frequency_input);
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
                    ui.selectable_value(&mut app.vna_settings.points, points, points.to_string());
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
}

const MATCHING_NETWORK_HELP: &str = "Choose the ideal network shown in the Smith-chart point hint. L uses two reactive components and has Q = √(Rhigh / Rlow − 1), fixed by the impedance transformation. π uses shunt–series–shunt components. T uses series–shunt–series components. All shows every alternative.";
const MATCHING_Q_HELP: &str = "Dimensionless design Q used only for π and T synthesis. For a series branch Q = |X| / R; for a parallel branch Q = R / |X|. Approximate fractional bandwidth is Δf / f₀ ≈ 1 / Q: lower Q is broader and less sensitive, while higher Q is narrower and stores more reactive energy. The actual antenna bandwidth also depends on its impedance curve, component losses, and the accepted VSWR.";

fn paint_vna_matching_settings(ui: &mut egui::Ui, app: &mut WaveCraftApp) {
    ui.label(RichText::new("Network shown in hint").color(TEXT_MUTED))
        .on_hover_selectable_text(MATCHING_NETWORK_HELP);
    let combo = egui::ComboBox::from_id_salt("vna_matching_network")
        .width(ui.available_width())
        .selected_text(app.vna_settings.matching_network.label())
        .show_ui(ui, |ui| {
            for network in VnaMatchingNetwork::ALL {
                ui.selectable_value(
                    &mut app.vna_settings.matching_network,
                    network,
                    network.label(),
                )
                .on_hover_selectable_text(network.description());
            }
        });
    combo
        .response
        .on_hover_selectable_text(MATCHING_NETWORK_HELP);

    if app.vna_settings.matching_network.uses_configurable_q() {
        ui.add_space(7.0);
        ui.label(RichText::new("Design Q (π / T)").color(TEXT_MUTED))
            .on_hover_selectable_text(MATCHING_Q_HELP);
        ui.add(
            egui::DragValue::new(&mut app.vna_settings.matching_q)
                .range(VNA_MATCHING_Q_MIN..=VNA_MATCHING_Q_MAX)
                .speed(0.1)
                .min_decimals(1)
                .max_decimals(2),
        )
        .on_hover_selectable_text(MATCHING_Q_HELP);
    }

    ui.add_space(7.0);
    ui.small(
        RichText::new(
            "Results are calculated at the hovered point frequency and shown in its chart hint.",
        )
        .color(TEXT_MUTED),
    )
    .on_hover_selectable_text(MATCHING_Q_HELP);
}

fn vna_frequency_inputs_are_valid(app: &WaveCraftApp) -> bool {
    let Ok(start_mhz) = app.vna_start_frequency_input.trim().parse::<f64>() else {
        return false;
    };
    let Ok(stop_mhz) = app.vna_stop_frequency_input.trim().parse::<f64>() else {
        return false;
    };
    let settings = crate::VnaSettings {
        start_mhz,
        stop_mhz,
        points: app.vna_settings.points,
        averaging: app.vna_settings.averaging,
        matching_network: app.vna_settings.matching_network,
        matching_q: app.vna_settings.matching_q,
    };
    settings.sweep_config().is_ok() && settings.matching_q_is_valid()
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
    matching: (VnaMatchingNetwork, f64),
) {
    let (matching_network, matching_q) = matching;
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
        let text = point_label(point, matching_network, matching_q);
        let layout = smith_hint_layout(&text, vswr_color(point_vswr(point)));
        let galley = painter.layout_job(layout.clone());
        let hint_rect = smith_hint_rect(rect, position, galley.size());
        paint_selectable_smith_hint(ui.ctx(), hint_rect, layout);
    }
}

fn smith_hint_layout(text: &str, vswr_color: Color32) -> egui::text::LayoutJob {
    let mut layout = egui::text::LayoutJob::default();
    let append = |layout: &mut egui::text::LayoutJob, text: &str, color| {
        layout.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: FontId::monospace(11.0),
                color,
                ..egui::TextFormat::default()
            },
        );
    };
    for line in text.split_inclusive('\n') {
        let (content, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |content| (content, "\n"));
        if let Some(vswr_start) = content.find("VSWR ") {
            let prefix = &content[..vswr_start];
            if content.starts_with("RL ") {
                let separator_start = prefix.rfind(" · ").unwrap_or(prefix.len());
                append(&mut layout, &prefix[..separator_start], vswr_color);
                append(&mut layout, &prefix[separator_start..], Color32::WHITE);
            } else {
                append(&mut layout, prefix, Color32::WHITE);
            }
            append(&mut layout, &content[vswr_start..], vswr_color);
            append(&mut layout, newline, Color32::WHITE);
        } else {
            append(&mut layout, content, smith_hint_line_color(content));
            append(&mut layout, newline, Color32::WHITE);
        }
    }
    layout
}

fn smith_hint_line_color(line: &str) -> Color32 {
    const FREQUENCY: Color32 = Color32::from_rgb(178, 255, 102);
    const S_PARAMETER: Color32 = Color32::from_rgb(190, 120, 255);
    const IMPEDANCE: Color32 = Color32::from_rgb(99, 179, 255);
    const MATCH_TITLE: Color32 = Color32::from_rgb(255, 204, 74);
    const BAD_MATCH: Color32 = Color32::from_rgb(235, 72, 72);

    let trimmed = line.trim_start();
    if line.ends_with(" MHz") {
        FREQUENCY
    } else if line.starts_with("S11 ") {
        S_PARAMETER
    } else if line.starts_with("Z ") {
        IMPEDANCE
    } else if line.starts_with("50 Ω ") && line.contains("-match") {
        MATCH_TITLE
    } else if trimmed.starts_with("Series ") {
        ORANGE
    } else if trimmed.starts_with("Shunt ") || line.starts_with("Already matched") {
        ACCENT
    } else if line.starts_with("Passive L-match") {
        BAD_MATCH
    } else {
        Color32::WHITE
    }
}

fn paint_selectable_smith_hint(context: &egui::Context, rect: Rect, layout: egui::text::LayoutJob) {
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
                    ui.set_min_width((rect.width() - 2.0 * SMITH_HINT_PADDING).max(0.0));
                    ui.add(egui::Label::new(layout).selectable(true));
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

    let (frequency_min, frequency_max) = sweep.map_or(frequency_hint_hz, |sweep| {
        (sweep.requested.start_hz as f64, sweep.actual_stop_hz as f64)
    });
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
const SMITH_HINT_EXTRA_WIDTH: f32 = 32.0;
const SMITH_HINT_GAP: f32 = 10.0;
const SMITH_HINT_MARGIN: f32 = 6.0;

fn smith_hint_rect(plot: Rect, anchor: Pos2, content_size: Vec2) -> Rect {
    let size = content_size
        + Vec2::new(
            2.0 * SMITH_HINT_PADDING + SMITH_HINT_EXTRA_WIDTH,
            2.0 * SMITH_HINT_PADDING,
        );
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct PiMatchSolution {
    source_shunt_susceptance_siemens: f64,
    series_reactance_ohms: f64,
    load_shunt_susceptance_siemens: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TMatchSolution {
    source_series_reactance_ohms: f64,
    shunt_susceptance_siemens: f64,
    load_series_reactance_ohms: f64,
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

fn pi_match_solution(resistance: f64, reactance: f64, design_q: f64) -> Option<PiMatchSolution> {
    if !resistance.is_finite()
        || !reactance.is_finite()
        || !design_q.is_finite()
        || !(VNA_MATCHING_Q_MIN..=VNA_MATCHING_Q_MAX).contains(&design_q)
        || resistance <= 0.0
        || (resistance - VNA_REFERENCE_IMPEDANCE_OHMS).abs() <= 1.0e-9
    {
        return None;
    }
    let load_denominator = resistance.mul_add(resistance, reactance * reactance);
    let load_conductance = resistance / load_denominator;
    let load_susceptance = -reactance / load_denominator;
    if !load_conductance.is_finite() || load_conductance <= 0.0 {
        return None;
    }

    let maximum_intermediate_resistance = VNA_REFERENCE_IMPEDANCE_OHMS.min(1.0 / load_conductance);
    let intermediate_resistance = maximum_intermediate_resistance / design_q.mul_add(design_q, 1.0);
    let total_load_susceptance = (load_conductance / intermediate_resistance
        - load_conductance * load_conductance)
        .max(0.0)
        .sqrt();
    let total_series_reactance = (intermediate_resistance
        * (VNA_REFERENCE_IMPEDANCE_OHMS - intermediate_resistance))
        .max(0.0)
        .sqrt();

    let mut candidates = Vec::with_capacity(4);
    for load_sign in [-1.0_f64, 1.0] {
        let load_node_susceptance = load_sign * total_load_susceptance;
        let shunted_load_reactance =
            -load_node_susceptance * intermediate_resistance / load_conductance;
        for series_sign in [-1.0_f64, 1.0] {
            let branch_reactance = series_sign * total_series_reactance;
            let solution = PiMatchSolution {
                source_shunt_susceptance_siemens: branch_reactance
                    / (intermediate_resistance * VNA_REFERENCE_IMPEDANCE_OHMS),
                series_reactance_ohms: branch_reactance - shunted_load_reactance,
                load_shunt_susceptance_siemens: load_node_susceptance - load_susceptance,
            };
            if pi_match_complexity(solution).is_finite() {
                candidates.push(solution);
            }
        }
    }
    candidates
        .into_iter()
        .min_by(|left, right| pi_match_complexity(*left).total_cmp(&pi_match_complexity(*right)))
}

fn pi_match_complexity(solution: PiMatchSolution) -> f64 {
    (solution.source_shunt_susceptance_siemens.abs()
        + solution.load_shunt_susceptance_siemens.abs())
        * VNA_REFERENCE_IMPEDANCE_OHMS
        + solution.series_reactance_ohms.abs() / VNA_REFERENCE_IMPEDANCE_OHMS
}

fn t_match_solution(resistance: f64, reactance: f64, design_q: f64) -> Option<TMatchSolution> {
    if !resistance.is_finite()
        || !reactance.is_finite()
        || !design_q.is_finite()
        || !(VNA_MATCHING_Q_MIN..=VNA_MATCHING_Q_MAX).contains(&design_q)
        || resistance <= 0.0
        || (resistance - VNA_REFERENCE_IMPEDANCE_OHMS).abs() <= 1.0e-9
    {
        return None;
    }

    let maximum_intermediate_conductance =
        (1.0 / resistance).min(1.0 / VNA_REFERENCE_IMPEDANCE_OHMS);
    let intermediate_conductance =
        maximum_intermediate_conductance / design_q.mul_add(design_q, 1.0);
    let total_load_reactance = (resistance / intermediate_conductance - resistance * resistance)
        .max(0.0)
        .sqrt();
    let total_node_susceptance = (intermediate_conductance / VNA_REFERENCE_IMPEDANCE_OHMS
        - intermediate_conductance * intermediate_conductance)
        .max(0.0)
        .sqrt();

    let mut candidates = Vec::with_capacity(4);
    for load_sign in [-1.0_f64, 1.0] {
        let load_branch_reactance = load_sign * total_load_reactance;
        let load_branch_susceptance =
            -load_branch_reactance * intermediate_conductance / resistance;
        for shunt_sign in [-1.0_f64, 1.0] {
            let node_susceptance = shunt_sign * total_node_susceptance;
            let solution = TMatchSolution {
                source_series_reactance_ohms: node_susceptance * VNA_REFERENCE_IMPEDANCE_OHMS
                    / intermediate_conductance,
                shunt_susceptance_siemens: node_susceptance - load_branch_susceptance,
                load_series_reactance_ohms: load_branch_reactance - reactance,
            };
            if t_match_complexity(solution).is_finite() {
                candidates.push(solution);
            }
        }
    }
    candidates
        .into_iter()
        .min_by(|left, right| t_match_complexity(*left).total_cmp(&t_match_complexity(*right)))
}

fn t_match_complexity(solution: TMatchSolution) -> f64 {
    (solution.source_series_reactance_ohms.abs() + solution.load_series_reactance_ohms.abs())
        / VNA_REFERENCE_IMPEDANCE_OHMS
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

fn format_matching_q(value: f64) -> String {
    if value.fract().abs() <= 1.0e-9 {
        format!("{value:.0}")
    } else {
        let formatted = format!("{value:.2}");
        formatted.trim_end_matches('0').to_owned()
    }
}

fn matching_label(
    frequency_hz: f64,
    resistance: f64,
    reactance: f64,
    matching_network: VnaMatchingNetwork,
    matching_q: f64,
) -> String {
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
        if matching_network.shows_l() {
            if let Some(solution) = l_match_solution(resistance, reactance) {
                let series =
                    component_for_series_reactance(solution.series_reactance_ohms, frequency_hz)
                        .map(format_reactive_component)
                        .unwrap_or_else(|| "no series element".to_owned());
                let shunt = component_for_shunt_susceptance(
                    solution.shunt_susceptance_siemens,
                    frequency_hz,
                )
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

        if matching_network.shows_pi() {
            if let Some(solution) = pi_match_solution(resistance, reactance, matching_q) {
                let source_shunt = component_for_shunt_susceptance(
                    solution.source_shunt_susceptance_siemens,
                    frequency_hz,
                )
                .map(format_reactive_component)
                .unwrap_or_else(|| "no source shunt element".to_owned());
                let series =
                    component_for_series_reactance(solution.series_reactance_ohms, frequency_hz)
                        .map(format_reactive_component)
                        .unwrap_or_else(|| "no series element".to_owned());
                let load_shunt = component_for_shunt_susceptance(
                    solution.load_shunt_susceptance_siemens,
                    frequency_hz,
                )
                .map(format_reactive_component)
                .unwrap_or_else(|| "no load shunt element".to_owned());
                lines.push(format!(
                    "50 Ω π-match (ideal, Q≈{}):",
                    format_matching_q(matching_q)
                ));
                lines.push(format!("  Shunt {source_shunt} on 50 Ω side"));
                lines.push(format!("  Series {series}"));
                lines.push(format!("  Shunt {load_shunt} at antenna"));
            }
        }

        if matching_network.shows_t() {
            if let Some(solution) = t_match_solution(resistance, reactance, matching_q) {
                let source_series = component_for_series_reactance(
                    solution.source_series_reactance_ohms,
                    frequency_hz,
                )
                .map(format_reactive_component)
                .unwrap_or_else(|| "no source series element".to_owned());
                let shunt = component_for_shunt_susceptance(
                    solution.shunt_susceptance_siemens,
                    frequency_hz,
                )
                .map(format_reactive_component)
                .unwrap_or_else(|| "no shunt element".to_owned());
                let load_series = component_for_series_reactance(
                    solution.load_series_reactance_ohms,
                    frequency_hz,
                )
                .map(format_reactive_component)
                .unwrap_or_else(|| "no load series element".to_owned());
                lines.push(format!(
                    "50 Ω T-match (ideal, Q≈{}):",
                    format_matching_q(matching_q)
                ));
                lines.push(format!("  Series {source_series} on 50 Ω side"));
                lines.push(format!("  Shunt {shunt}"));
                lines.push(format!("  Series {load_series} at antenna"));
            }
        }
    }
    lines.join("\n")
}

fn point_label(point: VnaPoint, matching_network: VnaMatchingNetwork, matching_q: f64) -> String {
    let magnitude = point.s11_magnitude();
    let return_loss = if magnitude > 0.0 {
        -20.0 * magnitude.log10()
    } else {
        f64::INFINITY
    };
    let vswr = point_vswr(point);
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
            matching_network,
            matching_q,
        ));
    }
    label
}

fn point_vswr(point: VnaPoint) -> f64 {
    let magnitude = point.s11_magnitude();
    if magnitude < 1.0 {
        (1.0 + magnitude) / (1.0 - magnitude)
    } else {
        f64::INFINITY
    }
}

fn vswr_color(vswr: f64) -> Color32 {
    const GOOD: Color32 = ACCENT;
    const WARNING: Color32 = Color32::from_rgb(255, 204, 74);
    const BAD: Color32 = Color32::from_rgb(235, 72, 72);

    let level = if vswr.is_finite() {
        ((vswr - 1.0) / 2.0).clamp(0.0, 1.0) as f32
    } else {
        1.0
    };
    if level <= 0.5 {
        mix_color(GOOD, WARNING, level * 2.0)
    } else {
        mix_color(WARNING, BAD, (level - 0.5) * 2.0)
    }
}

fn mix_color(from: Color32, to: Color32, amount: f32) -> Color32 {
    let channel = |from: u8, to: u8| {
        (f32::from(from) + (f32::from(to) - f32::from(from)) * amount).round() as u8
    };
    Color32::from_rgb(
        channel(from.r(), to.r()),
        channel(from.g(), to.g()),
        channel(from.b(), to.b()),
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
    fn calibration_notice_blocks_workspace_controls_until_acknowledged() {
        let context = egui::Context::default();
        let mut app = app();
        app.select_vna_view();
        let _ = painted_text(&context, |context| app.run_frame(context));
        let texts = painted_text(&context, |context| app.run_frame(context));
        let position = |label: &str| {
            texts
                .iter()
                .find_map(|(text, position)| (text == label).then_some(*position))
                .unwrap_or_else(|| panic!("{label} is visible"))
        };
        let logic = position("Logic Analyzer");
        let app_menu = position("App");
        let got_it = position("Got it");
        let click = |app: &mut WaveCraftApp, position| {
            for pressed in [true, false] {
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            Pos2::ZERO,
                            Vec2::new(1200.0, 900.0),
                        )),
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
                    |context| app.run_frame(context),
                );
            }
        };
        click(&mut app, logic);
        assert!(app.vna_view_is_active());
        assert!(app.vna_calibration_notice_open);
        click(&mut app, app_menu);
        let texts = painted_text(&context, |context| app.run_frame(context));
        assert!(!texts.iter().any(|(text, _)| text == "Settings…"));
        let focused = context.memory(|memory| memory.focused());
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 900.0))),
                events: vec![egui::Event::Key {
                    key: egui::Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |context| app.run_frame(context),
        );
        assert_eq!(context.memory(|memory| memory.focused()), focused);
        click(&mut app, got_it);
        assert!(!app.vna_calibration_notice_open);
        let _ = painted_text(&context, |context| app.run_frame(context));
        click(&mut app, logic);
        assert_eq!(
            app.active_waveform_view(),
            Some(crate::WaveformTab::LogicAnalyzer)
        );
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
    fn pi_and_t_match_solutions_transform_complex_loads_to_50_ohms() {
        for load in [(35.0, -20.0), (100.0, 30.0), (12.0, 80.0)] {
            let pi_input =
                pi_match_input_impedance(load, pi_match_solution(load.0, load.1, 1.0).unwrap());
            assert!((pi_input.0 - VNA_REFERENCE_IMPEDANCE_OHMS).abs() < 1.0e-8);
            assert!(pi_input.1.abs() < 1.0e-8);

            let t_input =
                t_match_input_impedance(load, t_match_solution(load.0, load.1, 1.0).unwrap());
            assert!((t_input.0 - VNA_REFERENCE_IMPEDANCE_OHMS).abs() < 1.0e-8);
            assert!(t_input.1.abs() < 1.0e-8);
        }
    }

    #[test]
    fn smith_point_label_includes_impedance_and_matching_components() {
        let gamma = impedance_to_gamma(0.7, -0.4);
        let label = point_label(
            VnaPoint {
                frequency_hz: 100_000_000,
                s11_real: gamma.0,
                s11_imaginary: gamma.1,
                s21_real: 0.0,
                s21_imaginary: 0.0,
            },
            VnaMatchingNetwork::All,
            2.0,
        );

        assert!(label.contains("Z 35.00 − j20.00 Ω · capacitive"));
        assert!(label.contains("Series resonance: L 31.8 nH → 35.0 Ω"));
        assert!(label.contains("50 Ω L-match (ideal):"));
        assert!(label.contains("Shunt L"));
        assert!(label.contains("Series C"));
        assert!(label.contains("50 Ω π-match (ideal, Q≈2):"));
        assert!(label.contains("50 Ω T-match (ideal, Q≈2):"));
    }

    #[test]
    fn matching_network_selection_and_q_control_the_point_hint() {
        let gamma = impedance_to_gamma(0.7, -0.4);
        let point = VnaPoint {
            frequency_hz: 100_000_000,
            s11_real: gamma.0,
            s11_imaginary: gamma.1,
            s21_real: 0.0,
            s21_imaginary: 0.0,
        };

        let pi_only = point_label(point, VnaMatchingNetwork::Pi, 1.5);
        assert!(pi_only.contains("50 Ω π-match (ideal, Q≈1.5):"));
        assert!(!pi_only.contains("L-match"));
        assert!(!pi_only.contains("T-match"));

        let all = point_label(point, VnaMatchingNetwork::All, 1.0);
        assert!(all.contains("50 Ω L-match (ideal):"));
        assert!(all.contains("50 Ω π-match (ideal, Q≈1):"));
        assert!(all.contains("50 Ω T-match (ideal, Q≈1):"));
    }

    #[test]
    fn vswr_label_changes_from_green_through_yellow_to_red() {
        let green = ACCENT;
        let yellow = Color32::from_rgb(255, 204, 74);
        let red = Color32::from_rgb(235, 72, 72);

        assert_eq!(vswr_color(1.0), green);
        assert_eq!(vswr_color(2.0), yellow);
        assert_eq!(vswr_color(3.0), red);
        assert_eq!(vswr_color(12.0), red);
        assert_eq!(vswr_color(f64::INFINITY), red);

        let layout = smith_hint_layout(
            "100.000000 MHz\nS11 +0.1000 -0.2000j\nRL 9.54 dB · VSWR 2.00\nZ 50.00 + j0.00 Ω · resistive\nSeries resonance: C 10 pF\n50 Ω L-match (ideal):\n  Shunt L 12 nH\n  Series C 8 pF",
            yellow,
        );
        let span_color = |prefix: &str| {
            layout
                .sections
                .iter()
                .find(|section| layout.text[section.byte_range.clone()].starts_with(prefix))
                .map(|section| section.format.color)
        };
        assert_eq!(
            span_color("100.000000 MHz"),
            Some(Color32::from_rgb(178, 255, 102))
        );
        assert_eq!(span_color("S11 "), Some(Color32::from_rgb(190, 120, 255)));
        assert_eq!(span_color("RL "), Some(yellow));
        assert_eq!(span_color("VSWR "), Some(yellow));
        assert_eq!(span_color("Z "), Some(Color32::from_rgb(99, 179, 255)));
        assert_eq!(span_color("Series resonance"), Some(ORANGE));
        assert_eq!(
            span_color("50 Ω L-match"),
            Some(Color32::from_rgb(255, 204, 74))
        );
        assert_eq!(span_color("  Shunt"), Some(ACCENT));
        assert_eq!(span_color("  Series"), Some(ORANGE));
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

    fn reciprocal(value: (f64, f64)) -> (f64, f64) {
        let denominator = value.0.mul_add(value.0, value.1 * value.1);
        (value.0 / denominator, -value.1 / denominator)
    }

    fn pi_match_input_impedance(load: (f64, f64), solution: PiMatchSolution) -> (f64, f64) {
        let load_admittance = reciprocal(load);
        let shunted_load = reciprocal((
            load_admittance.0,
            load_admittance.1 + solution.load_shunt_susceptance_siemens,
        ));
        let branch_admittance = reciprocal((
            shunted_load.0,
            shunted_load.1 + solution.series_reactance_ohms,
        ));
        reciprocal((
            branch_admittance.0,
            branch_admittance.1 + solution.source_shunt_susceptance_siemens,
        ))
    }

    fn t_match_input_impedance(load: (f64, f64), solution: TMatchSolution) -> (f64, f64) {
        let load_branch_admittance =
            reciprocal((load.0, load.1 + solution.load_series_reactance_ohms));
        let node_impedance = reciprocal((
            load_branch_admittance.0,
            load_branch_admittance.1 + solution.shunt_susceptance_siemens,
        ));
        (
            node_impedance.0,
            node_impedance.1 + solution.source_series_reactance_ohms,
        )
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
        assert_eq!(left_hint.width(), content.x + 44.0);
        assert_eq!(left_hint.height(), content.y + 12.0);
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
        assert!(position("📷 Snapshot").x < position("↻ Refresh").x);
        assert!(!texts
            .iter()
            .any(|(text, _)| text == "LiteVNA ready" || text == "No LiteVNA"));
    }

    #[test]
    fn vna_card_has_smith_and_log_magnitude_phase_tabs_with_a_channel_source() {
        let context = egui::Context::default();
        let mut app = app();
        let texts = painted_text(&context, |context| {
            app.paint_vna_central_panel(context);
        });
        assert!(texts.iter().any(|(text, _)| text == "SMITH CHART"));
        assert!(texts.iter().any(|(text, _)| text == "LOG MAG / PHASE"));

        app.vna_plot_tab = VnaPlotTab::LogMagnitudePhase;
        app.vna_plot_channel = VnaChannel::S21;
        let texts = painted_text(&context, |context| {
            app.paint_vna_central_panel(context);
        });
        assert!(texts.iter().any(|(text, _)| text == "Source"));
        assert!(texts.iter().any(|(text, _)| text == "S21"));
        assert!(texts
            .iter()
            .any(|(text, _)| text == "Run a sweep to display S21"));
    }

    #[test]
    fn vna_card_shows_sweep_progress_below_the_device_version_area() {
        let context = egui::Context::default();
        let mut app = app();
        app.vna_sweep_progress = Some(crate::VnaSweepProgress {
            purpose: crate::VnaSweepPurpose::Calibration(CalibrationStandard::Open),
            completed_points: 64,
            total_points: 1024,
        });

        let texts = painted_text(&context, |context| {
            app.paint_vna_central_panel(context);
        });

        assert!(texts
            .iter()
            .any(|(text, _)| text == "OPEN calibration · 64/1024 points"));
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
        assert!(texts.iter().any(|(text, _)| text == "Sweep settings"));
        assert!(texts.iter().any(|(text, _)| text == "Matching"));
        assert!(texts
            .iter()
            .any(|(text, _)| text == "Network shown in hint"));
        assert!(texts.iter().any(|(text, _)| text == "Design Q (π / T)"));
        assert!(texts.iter().any(|(text, _)| text == "All (L / π / T)"));
        assert!(texts.iter().any(|(text, _)| text == "Start frequency"));
        assert_eq!(texts.iter().filter(|(text, _)| text == "MHz").count(), 2);
        assert!(texts.iter().any(|(text, _)| text == "Save to file"));
        assert!(texts.iter().any(|(text, _)| text == "Load from file"));
        assert!(texts
            .iter()
            .any(|(text, _)| text == "No active calibration"));
        assert!(texts.iter().any(|(text, _)| text == "💾 Save settings"));
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

        app.vna_stop_frequency_input = "100".to_owned();
        app.vna_settings.matching_q = 0.0;
        assert!(!vna_frequency_inputs_are_valid(&app));
    }
}
