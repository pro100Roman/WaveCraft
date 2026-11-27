//! Battery lifetime workspace using the shared instrument chrome.

use super::{
    frame_actions::FrameActions,
    manual_value::ManualValue,
    plot_hover::{paint_plot_hover, plot_hover_pointer},
    view_shell::{
        primary_toolbar_button, remaining_plot_rect, settings_card, view_card, view_toolbar,
        workspace_panel, ToolbarSection,
    },
};
use crate::{
    battery::{
        units::DisplayUnit, BatterySettings, BatteryState, Chemistry, CurveKind, LoadKind,
        HOURS_PER_YEAR, MAX_PATTERNS, MAX_PATTERN_NAME_CHARS,
    },
    egui, toolbar_action_button, RichText, SelectableHoverText, WaveCraftApp, WorkspaceView,
    ACCENT, BACKGROUND, GRID, SCOPE_COLORS, TEXT_MUTED,
};

const VALUE_FIELD_WIDTH: f32 = 82.0;
const UNIT_COLUMN_WIDTH: f32 = 46.0;

impl WaveCraftApp {
    pub(crate) fn battery_view_is_active(&self) -> bool {
        self.active_workspace_view == Some(WorkspaceView::Battery)
    }

    pub(crate) fn select_battery_view(&mut self) {
        if let Some(active) = self.active_waveform_tab {
            self.save_waveform_view_settings(active);
        }
        self.active_workspace_view = Some(WorkspaceView::Battery);
        self.snapshot_plot_rect = None;
        self.save_global_settings();
    }

    pub(crate) fn paint_battery_toolbar(
        &mut self,
        context: &egui::Context,
        actions: &mut FrameActions,
    ) {
        let toolbar = view_toolbar(
            context,
            "Battery lifetime",
            self.pending_snapshot.is_none(),
            |ui, section| match section {
                ToolbarSection::Files => {
                    actions.battery_load |= ui
                        .add(toolbar_action_button("📂 Load…", TEXT_MUTED))
                        .clicked();
                    actions.battery_save |= ui
                        .add_enabled(
                            self.battery.settings.validate().is_ok(),
                            toolbar_action_button("Save file…", TEXT_MUTED),
                        )
                        .clicked();
                }
                ToolbarSection::Controls => {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        actions.battery_calculate |=
                            ui.add(primary_toolbar_button(ui, "Calc", ACCENT)).clicked();
                    });
                }
            },
        );
        self.settings_open |= toolbar.settings_clicked;
        actions.snapshot_clicked |= toolbar.snapshot_clicked;
    }

    pub(crate) fn paint_battery_setup(
        &mut self,
        context: &egui::Context,
        actions: &mut FrameActions,
    ) {
        self.show_setup_panel(
            context,
            WorkspaceView::Battery,
            actions,
            |app, ui, _actions| {
                settings_card(ui, |ui| battery_editor(ui, &mut app.battery.settings));
                app.battery.settings.validate().is_ok()
            },
        );
    }

    pub(crate) fn paint_battery_central(&mut self, context: &egui::Context) {
        workspace_panel(context, |ui| {
            view_card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("BATTERY DISCHARGE")
                            .strong()
                            .color(TEXT_MUTED),
                    );
                    if ui.small_button("Fit").clicked() {
                        self.battery.bounds = None;
                    }
                    ui.label(format!(
                        "Time (h) / Voltage ({})",
                        self.battery.settings.voltage_unit.symbol()
                    ));
                });
                if let Some(result) = &self.battery.result {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!(
                            "Lifetime: {:.3} h · {:.3} days · {:.3} years",
                            result.lifetime_hours,
                            result.lifetime_hours / 24.0,
                            result.lifetime_hours / HOURS_PER_YEAR
                        ));
                        ui.label(format!(
                            "Energy: {:.3} mWh · Average: {:.4} mW",
                            result.energy_mwh, result.average_power_mw
                        ));
                    });
                    if !result.cutoff_reached {
                        ui.label("Rated capacity exhausted before the voltage cutoff.");
                    }
                    if self.battery.is_stale() {
                        ui.label(
                            RichText::new("Parameters changed — press Calc to update the curve.")
                                .color(egui::Color32::YELLOW),
                        );
                    }
                } else {
                    ui.label("Set battery parameters and a repeating load cycle, then press Calc.");
                }
                let rect = remaining_plot_rect(ui);
                self.snapshot_plot_rect = Some(rect);
                paint_graph(ui, rect, &mut self.battery);
            });
        });
    }

    /// Apply file and calculation requests after all views finish editing.
    pub(crate) fn apply_battery_actions(&mut self, actions: &FrameActions) {
        if actions.battery_load {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Load battery analysis")
                .add_filter("WaveCraft battery analysis", &["json"])
                .pick_file()
            {
                match crate::battery::load(&path) {
                    Ok(settings) => {
                        self.battery.settings = settings;
                        self.battery.result = None;
                        self.battery.calculated_settings = None;
                        self.battery.bounds = None;
                        self.battery.error = None;
                        self.battery.status = format!("Loaded {} — press Calc", path.display());
                    }
                    Err(error) => {
                        self.battery.error = Some(format!("Could not load battery file: {error}"))
                    }
                }
            }
        }
        if actions.battery_save {
            match crate::battery::encode(&self.battery.settings) {
                Ok(json) => {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Save battery analysis")
                        .set_file_name("battery-analysis.json")
                        .add_filter("WaveCraft battery analysis", &["json"])
                        .save_file()
                    {
                        match std::fs::write(&path, json) {
                            Ok(()) => {
                                self.battery.error = None;
                                self.battery.status = format!("Saved {}", path.display());
                            }
                            Err(error) => {
                                self.battery.error =
                                    Some(format!("Could not save battery file: {error}"))
                            }
                        }
                    }
                }
                Err(error) => self.battery.error = Some(error),
            }
        }
        if actions.battery_calculate {
            self.battery.calculate();
        }
    }
}

fn battery_editor(ui: &mut egui::Ui, settings: &mut BatterySettings) {
    ui.label(RichText::new("BATTERY").strong().color(TEXT_MUTED));
    ui.label("Chemistry");
    let previous = settings.chemistry;
    egui::ComboBox::from_id_salt("battery_chemistry")
        .width(ui.available_width())
        .selected_text(settings.chemistry.label())
        .show_ui(ui, |ui| {
            for chemistry in Chemistry::ALL {
                ui.selectable_value(&mut settings.chemistry, chemistry, chemistry.label());
            }
        });
    if previous != settings.chemistry {
        settings.select_chemistry(settings.chemistry);
    }
    ui.label("Type / size");
    egui::ComboBox::from_id_salt("battery_size")
        .width(ui.available_width())
        .selected_text(&settings.size)
        .show_ui(ui, |ui| {
            for size in settings.chemistry.sizes() {
                ui.selectable_value(&mut settings.size, (*size).into(), *size);
            }
        });
    ui.add(
        egui::TextEdit::singleline(&mut settings.size)
            .desired_width(ui.available_width())
            .char_limit(128)
            .hint_text("Custom size / battery model"),
    );
    number_with_unit(
        ui,
        "Initial / full voltage",
        &mut settings.initial_voltage,
        &mut settings.voltage_unit,
        0.01,
    );
    number_with_unit(
        ui,
        "Rated capacity",
        &mut settings.capacity_mah,
        &mut settings.capacity_unit,
        1.0,
    );
    number(
        ui,
        "Capacity loss / year",
        &mut settings.aging_percent_per_year,
        " %",
        0.1,
    ).on_hover_selectable_text("Calendar aging: 0 disables it. 10%/year retains 90% after one year, 81% after two. Range: 0 to <100%.");
    number(
        ui,
        "Self-discharge / year",
        &mut settings.self_discharge_percent_per_year,
        " %",
        0.1,
    )
    .on_hover_selectable_text(
        "Loss of remaining charge, including sleep. 0 disables it; range: 0 to <100%/year.",
    );
    number_with_unit(
        ui,
        "Discharge cutoff",
        &mut settings.cutoff_voltage,
        &mut settings.voltage_unit,
        0.01,
    );
    ui.add_space(8.0);
    ui.label(
        RichText::new("DISCHARGE FUNCTION")
            .strong()
            .color(TEXT_MUTED),
    );
    let curve_response = value_row(ui, "Model", "", |ui| {
        egui::ComboBox::from_id_salt("battery_curve")
            .width(VALUE_FIELD_WIDTH)
            .wrap_mode(egui::TextWrapMode::Truncate)
            .selected_text(match settings.curve {
                CurveKind::Chemistry => "Chemistry",
                CurveKind::Polynomial => "Polynomial",
                CurveKind::Table => "Table",
            })
            .show_ui(ui, |ui| {
                for kind in [
                    CurveKind::Chemistry,
                    CurveKind::Polynomial,
                    CurveKind::Table,
                ] {
                    ui.selectable_value(&mut settings.curve, kind, kind.label());
                }
            })
            .response
    });
    let mut curve_hint =
        "x = discharged capacity / rated capacity (0…1). V(x) = V₀ × f(x).".to_owned();
    if settings.curve == CurveKind::Chemistry {
        curve_hint.push_str("\n\nGeneric exponential drop + slope + end knee. Illustrative chemistry profile; size is a label, not a measured cell model.");
    }
    curve_response.on_hover_selectable_text(curve_hint);
    match settings.curve {
        CurveKind::Chemistry => {}
        CurveKind::Polynomial => {
            value_row(ui, "Degree", "", |ui| {
                ui.add_sized(
                    [VALUE_FIELD_WIDTH, ui.spacing().interact_size.y],
                    ManualValue::new(&mut settings.polynomial_degree).range(1..=5),
                )
            });
            ui.label(RichText::new("f(x) = c₀ + c₁x + … + cₙxⁿ. Start with degree 5; verify monotonicity against measured data.")
                .small().color(TEXT_MUTED));
            for i in 0..=usize::from(settings.polynomial_degree.min(5)) {
                number(
                    ui,
                    &format!("c{i}"),
                    &mut settings.coefficients[i],
                    "",
                    0.001,
                );
            }
        }
        CurveKind::Table => {
            ui.label(RichText::new("Measured points: x and V/V₀. Linear interpolation; x increases from 0 to 1, voltage never increases.")
                .small().color(TEXT_MUTED));
            let mut remove = None;
            for (i, point) in settings.table.iter_mut().enumerate() {
                ui.push_id(i, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let field_size =
                            egui::vec2(VALUE_FIELD_WIDTH, ui.spacing().interact_size.y);
                        ui.label("x");
                        ui.add_sized(
                            field_size,
                            ManualValue::new(&mut point[0]).speed(0.001).max_decimals(5),
                        );
                        ui.label("V/V₀");
                        ui.add_sized(
                            field_size,
                            ManualValue::new(&mut point[1]).speed(0.001).max_decimals(5),
                        );
                        if ui.small_button("×").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }
            if let Some(i) = remove {
                settings.table.remove(i);
            }
            if ui
                .add_enabled(settings.table.len() < 256, egui::Button::new("Add point"))
                .clicked()
            {
                if let Some(last) = settings.table.last().copied() {
                    let index = settings.table.len() - 1;
                    let before = if index > 0 {
                        settings.table[index - 1]
                    } else {
                        [0.0, 1.0]
                    };
                    settings.table.insert(
                        index,
                        [(before[0] + last[0]) * 0.5, (before[1] + last[1]) * 0.5],
                    );
                } else {
                    settings.table = vec![[0.0, 1.0], [1.0, 0.7]];
                }
            }
        }
    }
    if ui.small_button("Reset curve for chemistry").clicked() {
        settings.reset_curve();
    }
    ui.label(
        RichText::new("Ideal battery-side power. No temperature, converter loss or voltage sag.")
            .small()
            .color(TEXT_MUTED),
    );
    ui.add_space(8.0);
    ui.label(
        RichText::new(format!(
            "CONSUMPTION CYCLE · {}/50",
            settings.patterns.len()
        ))
        .strong()
        .color(TEXT_MUTED),
    )
    .on_hover_selectable_text(
        "Patterns run in order and repeat until cutoff or capacity exhaustion.",
    );
    let mut remove = None;
    for (i, pattern) in settings.patterns.iter_mut().enumerate() {
        ui.push_id(("load", i), |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("{}.", i + 1));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut pattern.name)
                            .desired_width(ui.available_width())
                            .char_limit(MAX_PATTERN_NAME_CHARS)
                            .hint_text(format!("Pattern {} name", i + 1)),
                    );
                });
            });
            let mut kind = pattern.load_kind;
            value_row(ui, "Load", "", |ui| {
                egui::ComboBox::from_id_salt("load_kind").width(VALUE_FIELD_WIDTH)
                    .selected_text(match kind { LoadKind::Power => "Power", LoadKind::Current => "Current" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut kind, LoadKind::Power, "Constant power");
                        ui.selectable_value(&mut kind, LoadKind::Current, "Constant current");
                    }).response
            }).on_hover_selectable_text("Power mode holds power constant as voltage changes. Current mode holds current constant. Switching modes matches consumption at the battery's initial voltage.");
            pattern.set_kind(kind, settings.initial_voltage);
            match pattern.load_kind {
                LoadKind::Power => { number_with_unit(ui, "Power", &mut pattern.power_mw, &mut pattern.power_unit, 0.1); }
                LoadKind::Current => { number_with_unit(ui, "Current", &mut pattern.current_ma, &mut pattern.current_unit, 0.1); }
            }
            number_with_unit(ui, "Duration", &mut pattern.duration_ms, &mut pattern.duration_unit, 0.1);
        });
        ui.separator();
    }
    if let Some(i) = remove {
        settings.patterns.remove(i);
    }
    if ui
        .add_enabled(
            settings.patterns.len() < MAX_PATTERNS,
            egui::Button::new("+ Add pattern"),
        )
        .clicked()
    {
        settings.add_pattern();
    }
}

/// Keep settings controls and their external unit labels in aligned columns.
fn value_row(
    ui: &mut egui::Ui,
    label: &str,
    unit: &str,
    editor: impl FnOnce(&mut egui::Ui) -> egui::Response,
) -> egui::Response {
    value_row_with_unit(ui, label, |ui| ui.label(unit.trim()), editor)
}

fn value_row_with_unit(
    ui: &mut egui::Ui,
    label: &str,
    unit: impl FnOnce(&mut egui::Ui) -> egui::Response,
    editor: impl FnOnce(&mut egui::Ui) -> egui::Response,
) -> egui::Response {
    ui.push_id(label, |ui| {
        ui.horizontal(|ui| {
            let label = ui.label(label);
            let controls = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let height = ui.spacing().interact_size.y;
                let unit = ui
                    .allocate_ui_with_layout(
                        egui::vec2(UNIT_COLUMN_WIDTH, height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_width(UNIT_COLUMN_WIDTH);
                            unit(ui)
                        },
                    )
                    .inner;
                let editor = ui
                    .allocate_ui_with_layout(
                        egui::vec2(VALUE_FIELD_WIDTH, height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_width(VALUE_FIELD_WIDTH);
                            ui.spacing_mut().interact_size.x = VALUE_FIELD_WIDTH;
                            editor(ui)
                        },
                    )
                    .inner;
                editor.union(unit)
            });
            label.union(controls.inner)
        })
        .inner
    })
    .inner
}

fn number(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    unit: &str,
    speed: f64,
) -> egui::Response {
    value_row(ui, label, unit, |ui| {
        ui.add_sized(
            [VALUE_FIELD_WIDTH, ui.spacing().interact_size.y],
            ManualValue::new(value).speed(speed).max_decimals(6),
        )
    })
}

fn unit_picker<U: DisplayUnit>(ui: &mut egui::Ui, unit: &mut U) -> egui::Response {
    let previous = *unit;
    let mut response = ui
        .scope(|ui| {
            ui.spacing_mut().icon_width = 6.0;
            ui.spacing_mut().icon_spacing = 2.0;
            ui.spacing_mut().button_padding.x = 2.0;
            egui::ComboBox::from_id_salt("unit")
                .width(UNIT_COLUMN_WIDTH)
                .truncate()
                .selected_text(unit.symbol())
                .show_ui(ui, |ui| {
                    for candidate in U::ALL {
                        ui.selectable_value(unit, *candidate, candidate.symbol());
                    }
                })
                .response
        })
        .inner;
    if previous != *unit {
        response.mark_changed();
    }
    response
}

fn number_with_unit<U: DisplayUnit>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    unit: &mut U,
    speed: f64,
) -> egui::Response {
    let previous_unit = *unit;
    let mut displayed = previous_unit.to_display(*value);
    let mut edited = false;
    let response = value_row_with_unit(
        ui,
        label,
        |ui| unit_picker(ui, unit),
        |ui| {
            let response = ui.add_sized(
                [VALUE_FIELD_WIDTH, ui.spacing().interact_size.y],
                ManualValue::new(&mut displayed)
                    .speed(speed / previous_unit.scale())
                    .max_decimals(6),
            );
            edited = response.changed();
            response
        },
    );
    // A unit-only change never rescales canonical data or accumulates rounding.
    if edited {
        *value = previous_unit.to_canonical(displayed);
    }
    response
}

fn fitted_bounds(result: &crate::battery::BatteryResult) -> [f64; 4] {
    let min = result.points.last().map_or(0.0, |p| p[1]);
    let max = result.points.first().map_or(1.0, |p| p[1]);
    let pad = ((max - min) * 0.08).max(max * 0.01);
    [
        0.0,
        result.lifetime_hours * 1.02,
        (min - pad).max(0.0),
        max + pad,
    ]
}

fn zoom_bounds(bounds: &mut [f64; 4], anchor: [f64; 2], factor: f64, both: bool) {
    let axes = if both { 2 } else { 1 };
    for (axis, fraction) in anchor.into_iter().enumerate().take(axes) {
        let i = axis * 2;
        let span = bounds[i + 1] - bounds[i];
        let fraction = fraction.clamp(0.0, 1.0);
        let value = bounds[i] + fraction * span;
        let new_span = (span * factor).clamp(1e-12, 1e18);
        let min = (value - fraction * new_span).clamp(-1e18, 1e18);
        bounds[i] = min;
        bounds[i + 1] = min + new_span;
    }
}

fn paint_graph(ui: &mut egui::Ui, rect: egui::Rect, state: &mut BatteryState) {
    let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
    ui.painter().rect_filled(rect, 3.0, BACKGROUND);
    let Some(result) = &state.result else {
        return;
    };
    if rect.width() < 140.0 || rect.height() < 90.0 {
        return;
    }
    let plot = egui::Rect::from_min_max(
        rect.min + egui::vec2(68.0, 14.0),
        rect.max - egui::vec2(15.0, 35.0),
    );
    if response.double_clicked() {
        state.bounds = None;
    }
    let mut bounds = state.bounds.unwrap_or_else(|| fitted_bounds(result));
    if let Some(pointer) = response.hover_pos().filter(|p| plot.contains(*p)) {
        let (scroll, both) = ui.input(|input| {
            let both = input.modifiers.ctrl
                || input.modifiers.command
                || input.modifiers.mac_cmd
                || input.events.iter().any(|event| {
                    matches!(event, egui::Event::MouseWheel { modifiers, .. }
                    if modifiers.ctrl || modifiers.command || modifiers.mac_cmd)
                });
            (input.raw_scroll_delta.y, both)
        });
        if scroll != 0.0 {
            let anchor = [
                f64::from((pointer.x - plot.left()) / plot.width()),
                f64::from((plot.bottom() - pointer.y) / plot.height()),
            ];
            zoom_bounds(
                &mut bounds,
                anchor,
                (-f64::from(scroll) * 0.005).clamp(-4.0, 4.0).exp(),
                both,
            );
        }
    }
    if response.dragged() {
        let delta = ui.input(|input| input.pointer.delta());
        let dx = f64::from(delta.x / plot.width()) * (bounds[1] - bounds[0]);
        let dy = f64::from(delta.y / plot.height()) * (bounds[3] - bounds[2]);
        bounds = [
            bounds[0] - dx,
            bounds[1] - dx,
            bounds[2] + dy,
            bounds[3] + dy,
        ];
    }
    state.bounds = Some(bounds);
    let [left, right, bottom, top] = bounds;
    for i in 0..=5 {
        let fraction = i as f32 / 5.0;
        let x = egui::lerp(plot.x_range(), fraction);
        let y = egui::lerp(plot.y_range(), fraction);
        ui.painter().line_segment(
            [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
            egui::Stroke::new(1.0_f32, GRID),
        );
        ui.painter().line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            egui::Stroke::new(1.0_f32, GRID),
        );
        // Keep axis widgets inside the allocated plot surface. ui.put expands
        // its parent's minimum rectangle, so an overflowing tick widens the card.
        let time_label = format!("{:.3} h", left + f64::from(fraction) * (right - left));
        let label_left = (x - 37.5).clamp(rect.left(), rect.right() - 75.0);
        ui.put(
            egui::Rect::from_min_size(
                egui::pos2(label_left, plot.bottom() + 6.0),
                egui::vec2(75.0, 20.0),
            ),
            egui::Label::new(
                RichText::new(&time_label)
                    .monospace()
                    .size(10.0)
                    .color(TEXT_MUTED),
            )
            .truncate()
            .selectable(true),
        )
        .on_hover_selectable_text(time_label);
        let voltage_precision = if state.settings.voltage_unit.scale() < 1.0 {
            0
        } else {
            3
        };
        let voltage_value = state
            .settings
            .voltage_unit
            .to_display(top - f64::from(fraction) * (top - bottom));
        let voltage_label = format!(
            "{voltage_value:.voltage_precision$} {}",
            state.settings.voltage_unit.symbol()
        );
        ui.put(
            egui::Rect::from_min_size(
                egui::pos2(rect.left() + 2.0, y - 8.0),
                egui::vec2(62.0, 18.0),
            ),
            egui::Label::new(
                RichText::new(&voltage_label)
                    .monospace()
                    .size(10.0)
                    .color(TEXT_MUTED),
            )
            .truncate()
            .selectable(true),
        )
        .on_hover_selectable_text(voltage_label);
    }
    let map = |p: [f64; 2]| {
        egui::pos2(
            plot.left() + ((p[0] - left) / (right - left)).clamp(-1e4, 1e4) as f32 * plot.width(),
            plot.bottom()
                - ((p[1] - bottom) / (top - bottom)).clamp(-1e4, 1e4) as f32 * plot.height(),
        )
    };
    let points: Vec<_> = result.points.iter().copied().map(map).collect();
    ui.painter().with_clip_rect(plot).add(egui::Shape::line(
        points,
        egui::Stroke::new(2.0_f32, ACCENT),
    ));
    if let Some(pointer) = plot_hover_pointer(ui, &response, plot) {
        let hours = left + f64::from((pointer.x - plot.left()) / plot.width()) * (right - left);
        if let Some((index, point)) = result
            .points
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (a[0] - hours).abs().total_cmp(&(b[0] - hours).abs()))
        {
            let voltage_precision = if state.settings.voltage_unit.scale() < 1.0 {
                0
            } else {
                2
            };
            let mut rows = vec![
                ("Time:".into(), ACCENT),
                (format!("    {:.6} h", point[0]), ACCENT),
                (format!("    {:.6} days", point[0] / 24.0), ACCENT),
                (
                    format!("    {:.6} years", point[0] / HOURS_PER_YEAR),
                    ACCENT,
                ),
                (
                    format!(
                        "Voltage: {:.voltage_precision$} {}",
                        state.settings.voltage_unit.to_display(point[1]),
                        state.settings.voltage_unit.symbol()
                    ),
                    SCOPE_COLORS[1],
                ),
            ];
            if let Some(charge) = result.charge_remaining_percent.get(index) {
                rows.push((
                    format!("Charge remaining: {charge:.1}% (SOC)"),
                    SCOPE_COLORS[0],
                ));
            }
            paint_plot_hover(ui, pointer, plot, &rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::app;

    fn frame(
        app: &mut WaveCraftApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<(String, egui::Rect)>) {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| app.run_frame(ctx),
        );
        fn collect(shape: &egui::Shape, labels: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(text) => {
                    labels.push((text.galley.text().into(), text.visual_bounding_rect()))
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, labels);
                    }
                }
                _ => {}
            }
        }
        let mut labels = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut labels);
        }
        (output, labels)
    }

    fn label(labels: &[(String, egui::Rect)], text: &str) -> egui::Pos2 {
        labels
            .iter()
            .find(|(s, _)| s == text)
            .unwrap_or_else(|| panic!("{text} is visible"))
            .1
            .center()
    }

    fn click(app: &mut WaveCraftApp, ctx: &egui::Context, pos: egui::Pos2) {
        for pressed in [true, false] {
            frame(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    #[test]
    fn full_frame_uses_shared_chrome_calculates_and_keeps_errors_visible_when_collapsed() {
        let mut app = app();
        app.select_battery_view();
        let ctx = egui::Context::default();
        crate::configure_style(&ctx);
        frame(&mut app, &ctx, vec![]);
        let (_, labels) = frame(&mut app, &ctx, vec![]);
        assert_eq!(labels.iter().filter(|(s, _)| s == "📷 Snapshot").count(), 1);
        assert!(label(&labels, "Calc").x > 1250.0);
        let _ = label(&labels, "Capacity loss / year");
        let _ = label(&labels, "Self-discharge / year");
        assert!(label(&labels, "💾 Save settings").y > 840.0);
        assert_eq!(labels.iter().filter(|(s, _)| s == "+").count(), 1); // Workspace add only.
        click(&mut app, &ctx, label(&labels, "Calc"));
        assert!(app.battery.result.is_some());
        let rect = app.snapshot_plot_rect.unwrap();
        assert!(rect.left() >= 300.0 && rect.bottom() < 880.0);
        app.battery.settings.capacity_mah = -1.0;
        app.battery.calculate();
        app.workspace_view_layout
            .set_setup_panel_open(WorkspaceView::Battery, false);
        let (_, labels) = frame(&mut app, &ctx, vec![]);
        let error = label(&labels, "Capacity must be finite and greater than 0 mAh");
        assert!(error.y > 800.0);
        assert!(app.battery.is_stale());
    }

    #[test]
    fn unit_menus_preserve_values_and_hover_shows_days_years_and_charge() {
        use crate::battery::units::{CapacityUnit, VoltageUnit};
        let mut app = app();
        app.select_battery_view();
        app.battery.calculate();
        let before = app.battery.result.as_ref().unwrap().points.clone();
        let ctx = egui::Context::default();
        crate::configure_style(&ctx);
        frame(&mut app, &ctx, vec![]);
        let (_, labels) = frame(&mut app, &ctx, vec![]);
        click(&mut app, &ctx, label(&labels, "V"));
        let (_, labels) = frame(&mut app, &ctx, vec![]);
        assert!(
            labels.iter().any(|(text, _)| text == "mV"),
            "unit menu labels: {labels:?}"
        );
        click(&mut app, &ctx, label(&labels, "mV"));
        assert_eq!(app.battery.settings.voltage_unit, VoltageUnit::Millivolts);
        assert_eq!(app.battery.settings.initial_voltage, 4.2);
        assert_eq!(app.battery.settings.cutoff_voltage, 3.0);
        assert!(!app.battery.is_stale());
        let (_, labels) = frame(&mut app, &ctx, vec![]);
        click(&mut app, &ctx, label(&labels, "mAh"));
        let (_, labels) = frame(&mut app, &ctx, vec![]);
        click(&mut app, &ctx, label(&labels, "Ah"));
        assert_eq!(app.battery.settings.capacity_unit, CapacityUnit::AmpHours);
        assert_eq!(app.battery.settings.capacity_mah, 2600.0);
        assert!(!app.battery.is_stale());
        assert_eq!(app.battery.result.as_ref().unwrap().points, before);
        let pointer = app.snapshot_plot_rect.unwrap().center();
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(pointer)]);
        let (_, labels) = frame(&mut app, &ctx, vec![]);
        assert!(labels.iter().any(|(text, _)| text.starts_with("Time:")
            && text.contains("days")
            && text.contains("years")));
        assert!(labels
            .iter()
            .any(|(text, _)| text.contains("Charge remaining:") && text.contains("% (SOC)")));
        assert!(labels
            .iter()
            .any(|(text, _)| text.contains("Voltage:") && text.contains("mV")));
    }

    #[test]
    fn numeric_parameters_ignore_pointer_drags_and_accept_typed_values() {
        fn drag(app: &mut WaveCraftApp, ctx: &egui::Context, start: egui::Pos2, offset: f32) {
            let end = start + egui::vec2(offset, 0.0);
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(
                app,
                ctx,
                vec![egui::Event::PointerMoved(start), button(start, true)],
            );
            frame(app, ctx, vec![egui::Event::PointerMoved(end)]);
            frame(app, ctx, vec![button(end, false)]);
            frame(app, ctx, vec![]);
        }
        fn type_value(app: &mut WaveCraftApp, ctx: &egui::Context, pos: egui::Pos2, value: &str) {
            click(app, ctx, pos);
            frame(app, ctx, vec![egui::Event::Text(value.into())]);
            frame(
                app,
                ctx,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        let mut app = app();
        app.select_battery_view();
        let ctx = egui::Context::default();
        crate::configure_style(&ctx);
        frame(&mut app, &ctx, vec![]);
        let (_, labels) = frame(&mut app, &ctx, vec![]);
        let capacity = label(&labels, "2600");
        let before = app.battery.settings.clone();
        for offset in [80.0, -80.0] {
            drag(&mut app, &ctx, capacity, offset);
        }
        assert_eq!(app.battery.settings, before);
        type_value(&mut app, &ctx, capacity, "2750.5");
        assert_eq!(app.battery.settings.capacity_mah, 2750.5);

        app.battery.settings.curve = CurveKind::Polynomial;
        frame(&mut app, &ctx, vec![]);
        let (_, labels) = frame(&mut app, &ctx, vec![]);
        let degree = label(&labels, "5");
        for offset in [80.0, -80.0] {
            drag(&mut app, &ctx, degree, offset);
        }
        assert_eq!(app.battery.settings.polynomial_degree, 5);
        type_value(&mut app, &ctx, degree, "3");
        assert_eq!(app.battery.settings.polynomial_degree, 3);
    }

    #[test]
    fn discharge_card_keeps_workspace_margins_after_calculation_and_zoom() {
        fn cards(shape: &egui::Shape, rects: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Rect(shape)
                    if shape.fill == crate::PANEL
                        && shape.stroke.color == GRID
                        && shape.rect.height() > 300.0
                        && shape.rect.left() >= 300.0 =>
                {
                    rects.push(shape.rect)
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        cards(shape, rects);
                    }
                }
                _ => {}
            }
        }
        for width in [800.0, 1400.0] {
            for calculated in [false, true] {
                let mut app = app();
                app.select_battery_view();
                if calculated {
                    app.battery.calculate();
                }
                let ctx = egui::Context::default();
                crate::configure_style(&ctx);
                for zoomed in [false, true] {
                    if zoomed && calculated {
                        app.battery.bounds = Some([0.0, 1e20, 2.0, 5.0]);
                    }
                    for _ in 0..2 {
                        let output = ctx.run(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(width, 900.0),
                                )),
                                ..Default::default()
                            },
                            |ctx| app.run_frame(ctx),
                        );
                        let mut rects = Vec::new();
                        for shape in output.shapes {
                            cards(&shape.shape, &mut rects);
                        }
                        assert_eq!(rects.len(), 1);
                        let card = rects[0];
                        assert!(
                            card.right() <= width - 15.0,
                            "width={width}, calculated={calculated}, zoomed={zoomed}: {card:?}"
                        );
                        assert!(card.left() >= 315.0);
                    }
                }
            }
        }
    }

    #[test]
    fn wheel_zoom_is_horizontal_unless_modified_and_double_click_fits() {
        let mut app = app();
        app.select_battery_view();
        app.battery.calculate();
        let ctx = egui::Context::default();
        crate::configure_style(&ctx);
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![]);
        let pointer = app.snapshot_plot_rect.unwrap().center();
        let original = app.battery.bounds.unwrap();
        let scroll = |modifiers| {
            vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, 80.0),
                    modifiers,
                },
            ]
        };
        frame(&mut app, &ctx, scroll(egui::Modifiers::NONE));
        let horizontal = app.battery.bounds.unwrap();
        assert!(horizontal[1] - horizontal[0] < original[1] - original[0]);
        assert_eq!(horizontal[2..], original[2..]);
        frame(&mut app, &ctx, scroll(egui::Modifiers::CTRL));
        let both = app.battery.bounds.unwrap();
        assert!(both[3] - both[2] < horizontal[3] - horizontal[2]);
        click(&mut app, &ctx, pointer);
        click(&mut app, &ctx, pointer);
        assert_eq!(app.battery.bounds.unwrap(), original);
        app.battery.bounds = Some(both);
        let (_, labels) = frame(&mut app, &ctx, vec![]);
        click(&mut app, &ctx, label(&labels, "Fit"));
        assert_eq!(app.battery.bounds.unwrap(), original);
    }

    #[test]
    fn settings_and_battery_only_workspace_restore_and_legacy_order_migrates() {
        let directory = std::env::temp_dir().join(format!(
            "wavecraft-battery-persistence-{}",
            std::process::id()
        ));
        let repository = crate::CaptureSettingsRepository::at(directory.join("settings.json"));
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            crate::CaptureSettingsDocument::default(),
        );
        app.select_battery_view();
        app.battery.settings.size = "Prototype cell".into();
        app.battery.settings.patterns[0].power_mw = 0.125;
        app.battery.settings.patterns[0].name = "Передавання даних".into();
        app.battery.settings.aging_percent_per_year = 2.75;
        app.battery.settings.self_discharge_percent_per_year = 1.25;
        app.save_setup_settings(WorkspaceView::Battery);
        for view in WorkspaceView::ALL {
            if view != WorkspaceView::Battery {
                app.set_workspace_view_visible(view, false);
            }
        }
        app.workspace_view_layout
            .set_setup_panel_open(WorkspaceView::Battery, false);
        app.save_global_settings();
        let mut restored = WaveCraftApp::with_settings(None, repository.load().unwrap());
        restored.reconcile_waveform_tab();
        assert!(restored.battery_view_is_active());
        assert!(restored.active_waveform_view().is_none());
        assert_eq!(
            restored.available_workspace_views(),
            vec![WorkspaceView::Battery]
        );
        assert_eq!(restored.battery.settings, app.battery.settings);
        assert!(!restored
            .workspace_view_layout
            .setup_panel_open(WorkspaceView::Battery));

        std::fs::remove_dir_all(&directory).unwrap();

        let mut saved = crate::CaptureSettingsDocument::default();
        let legacy: Vec<_> = WorkspaceView::ALL[..7].iter().rev().copied().collect();
        saved.global.workspace_view_order = Some(
            legacy
                .iter()
                .copied()
                .map(crate::SavedWorkspaceView::from)
                .collect(),
        );
        let restored = WaveCraftApp::with_settings(None, saved);
        assert_eq!(
            &restored.workspace_view_layout.order()[..7],
            legacy.as_slice()
        );
        assert_eq!(
            restored.workspace_view_layout.order()[7],
            WorkspaceView::Battery
        );
    }
}
