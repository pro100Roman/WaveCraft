//! Oscilloscope settings panels: input range, trigger, functions, markers, and
//! per-channel controls.

use crate::{
    channel_label, coupling_label, egui, format_input_scale, scope_channel_label,
    scope_marker_default_position, trigger_slope_label, trigger_source_label,
    CaptureSettingsAvailability, Color32, DsCopeCoupling, DsCopeTriggerSlope, DsCopeTriggerSource,
    RichText, ScopeCaptureSettings, ScopeChannelSettings, ScopeFftWindow, ScopeFunctionSettings,
    ScopeMarkerEditor, ScopeMarkerKey, ScopeMarkerKind, ScopeMarkerSettings, ScopeMathOperation,
    ScopeMeasurementSettings, ScopeTriggerSettings, SelectableHoverText, Sense, Stroke,
    TriggerHoldoffUnit, Vec2, WaveformViewport, CHANNEL_NAME_MAX_CHARS,
    DSCOPE_TRIGGER_HOLDOFF_MAX_NS, DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX, DSCOPE_VOLTS_PER_DIV_MV,
    FFT_LENGTHS, PANEL, SCOPE_COLORS, SCOPE_MARKER_LIMIT, SCOPE_MEASUREMENT_KINDS, TEXT_MUTED,
};

pub(crate) fn step_scope_input_range(current: u16, steps: i32) -> u16 {
    let current_index = DSCOPE_VOLTS_PER_DIV_MV
        .iter()
        .position(|candidate| *candidate == current)
        .unwrap_or(0) as i32;
    let maximum_index = DSCOPE_VOLTS_PER_DIV_MV.len() as i32 - 1;
    DSCOPE_VOLTS_PER_DIV_MV[(current_index + steps).clamp(0, maximum_index) as usize]
}

pub(crate) fn paint_scope_input_range_encoder(
    ui: &mut egui::Ui,
    channel_index: usize,
    volts_per_div_mv: &mut u16,
    channel_color: &mut Color32,
) -> bool {
    let mut steps = 0;
    ui.horizontal(|ui| {
        let encoder_id = ui.make_persistent_id(("scope_range_encoder", channel_index));
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(58.0), Sense::click_and_drag());
        let response = response
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_selectable_text(
                "Drag the knob vertically or scroll to rotate it · double-click to change color",
            );
        if response.clicked() {
            response.request_focus();
        }
        let popup_id = ui.make_persistent_id(("scope_channel_color_popup", channel_index));
        if response.double_clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }
        if response.hovered() {
            let scroll_delta = ui.input_mut(|input| {
                let delta = input.raw_scroll_delta.y;
                input.raw_scroll_delta.y = 0.0;
                delta
            });
            if scroll_delta.abs() > f32::EPSILON {
                steps += scroll_delta.signum() as i32;
            }
        }
        if response.dragged() {
            let pointer_delta = ui.input(|input| input.pointer.delta());
            let rotation_delta = pointer_delta.x - pointer_delta.y;
            let drag_steps = ui.data_mut(|data| {
                let accumulator = data.get_temp_mut_or_default::<f32>(encoder_id);
                *accumulator += rotation_delta;
                let whole_steps = (*accumulator / 12.0).trunc() as i32;
                *accumulator -= whole_steps as f32 * 12.0;
                whole_steps
            });
            steps += drag_steps;
        }
        if response.drag_stopped() {
            ui.data_mut(|data| data.remove_temp::<f32>(encoder_id));
        }
        if response.has_focus() {
            ui.input(|input| {
                if input.key_pressed(egui::Key::ArrowUp) || input.key_pressed(egui::Key::ArrowRight)
                {
                    steps += 1;
                }
                if input.key_pressed(egui::Key::ArrowDown)
                    || input.key_pressed(egui::Key::ArrowLeft)
                {
                    steps -= 1;
                }
            });
        }

        let current_index = DSCOPE_VOLTS_PER_DIV_MV
            .iter()
            .position(|candidate| *candidate == *volts_per_div_mv)
            .unwrap_or(0);
        let fraction = current_index as f32 / (DSCOPE_VOLTS_PER_DIV_MV.len() - 1) as f32;
        let center = rect.center();
        let radius = 22.0;
        let painter = ui.painter();
        painter.circle_filled(center, radius, channel_color.gamma_multiply(0.14));
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(1.4_f32, channel_color.gamma_multiply(0.7)),
        );
        for tick_index in 0..DSCOPE_VOLTS_PER_DIV_MV.len() {
            let tick_fraction = tick_index as f32 / (DSCOPE_VOLTS_PER_DIV_MV.len() - 1) as f32;
            let angle = std::f32::consts::PI * (0.75 + tick_fraction * 1.5);
            let direction = Vec2::new(angle.cos(), angle.sin());
            painter.line_segment(
                [center + direction * 25.0, center + direction * 28.0],
                Stroke::new(1.0_f32, channel_color.gamma_multiply(0.55)),
            );
        }
        let angle = std::f32::consts::PI * (0.75 + fraction * 1.5);
        let direction = Vec2::new(angle.cos(), angle.sin());
        painter.line_segment(
            [center, center + direction * 16.0],
            Stroke::new(2.8_f32, *channel_color),
        );
        painter.circle_filled(center, 2.5, *channel_color);

        ui.vertical(|ui| {
            ui.label(
                RichText::new(format_input_scale(*volts_per_div_mv))
                    .strong()
                    .color(*channel_color),
            );
            ui.small(RichText::new("drag / scroll").color(TEXT_MUTED));
        });

        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &response,
            egui::popup::PopupCloseBehavior::CloseOnClickOutside,
            |popup_ui| {
                popup_ui.set_min_width(280.0);
                popup_ui.label(RichText::new(format!("CH{channel_index} color")).strong());
                egui::color_picker::color_picker_color32(
                    popup_ui,
                    channel_color,
                    egui::color_picker::Alpha::Opaque,
                );
                if popup_ui.button("Reset color").clicked() {
                    *channel_color = SCOPE_COLORS[channel_index];
                }
            },
        );
    });

    let next = step_scope_input_range(*volts_per_div_mv, steps);
    let changed = next != *volts_per_div_mv;
    *volts_per_div_mv = next;
    changed
}

pub(crate) fn paint_scope_trigger_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeTriggerSettings,
    channels: [bool; 2],
    channel_names: &[String; 2],
    enabled: bool,
) {
    egui::CollapsingHeader::new(RichText::new("Trigger").strong())
        .default_open(false)
        .show(ui, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                egui::Grid::new("dscope_trigger_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Source");
                        egui::ComboBox::from_id_salt("scope_trigger_source")
                            .selected_text(trigger_source_label(settings.source, channel_names))
                            .show_ui(ui, |ui| {
                                for source in [
                                    DsCopeTriggerSource::Auto,
                                    DsCopeTriggerSource::ChannelZero,
                                    DsCopeTriggerSource::ChannelOne,
                                    DsCopeTriggerSource::ChannelZeroAndOne,
                                    DsCopeTriggerSource::ChannelZeroOrOne,
                                ] {
                                    let mut candidate = *settings;
                                    candidate.source = source;
                                    ui.add_enabled_ui(
                                        candidate.source_is_available(channels),
                                        |ui| {
                                            ui.selectable_value(
                                                &mut settings.source,
                                                source,
                                                trigger_source_label(source, channel_names),
                                            );
                                        },
                                    );
                                }
                            });
                        ui.end_row();

                        if settings.source == DsCopeTriggerSource::Auto {
                            ui.label("Auto channel");
                            egui::ComboBox::from_id_salt("scope_trigger_auto_channel")
                                .selected_text(scope_channel_label(
                                    usize::from(settings.auto_channel),
                                    channel_names,
                                ))
                                .show_ui(ui, |ui| {
                                    for (channel, channel_enabled) in
                                        channels.into_iter().enumerate()
                                    {
                                        ui.add_enabled_ui(channel_enabled, |ui| {
                                            ui.selectable_value(
                                                &mut settings.auto_channel,
                                                channel as u8,
                                                scope_channel_label(channel, channel_names),
                                            );
                                        });
                                    }
                                });
                            ui.end_row();
                        }

                        ui.label("Edge");
                        egui::ComboBox::from_id_salt("scope_trigger_slope")
                            .selected_text(trigger_slope_label(settings.slope))
                            .show_ui(ui, |ui| {
                                for slope in
                                    [DsCopeTriggerSlope::Rising, DsCopeTriggerSlope::Falling]
                                {
                                    ui.selectable_value(
                                        &mut settings.slope,
                                        slope,
                                        trigger_slope_label(slope),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Holdoff");
                        ui.horizontal(|ui| {
                            let multiplier = settings.holdoff_unit.nanoseconds();
                            let mut value = settings.holdoff_ns as f64 / multiplier;
                            let maximum = DSCOPE_TRIGGER_HOLDOFF_MAX_NS as f64 / multiplier;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut value)
                                        .range(0.0..=maximum)
                                        .speed(0.1)
                                        .max_decimals(3),
                                )
                                .changed()
                            {
                                settings.holdoff_ns = (value * multiplier).round() as u64;
                            }
                            egui::ComboBox::from_id_salt("scope_trigger_holdoff_unit")
                                .selected_text(settings.holdoff_unit.label())
                                .show_ui(ui, |ui| {
                                    for unit in [
                                        TriggerHoldoffUnit::Microseconds,
                                        TriggerHoldoffUnit::Milliseconds,
                                        TriggerHoldoffUnit::Seconds,
                                    ] {
                                        ui.selectable_value(
                                            &mut settings.holdoff_unit,
                                            unit,
                                            unit.label(),
                                        );
                                    }
                                });
                        });
                        ui.end_row();

                        ui.label("Noise sensitivity");
                        ui.add(egui::Slider::new(
                            &mut settings.noise_sensitivity,
                            0..=DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX,
                        ));
                        ui.end_row();
                    });
            });
        });
}

pub(crate) fn paint_scope_channel_picker(
    ui: &mut egui::Ui,
    id: &'static str,
    channel: &mut usize,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(scope_channel_label(*channel, channel_names))
        .show_ui(ui, |ui| {
            for (index, enabled) in channels.into_iter().enumerate() {
                ui.add_enabled_ui(enabled, |ui| {
                    ui.selectable_value(channel, index, scope_channel_label(index, channel_names));
                });
            }
        });
}

pub(crate) fn paint_scope_function_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeFunctionSettings,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::CollapsingHeader::new(RichText::new("Functions").strong())
        .default_open(false)
        .show(ui, |ui| {
            ui.checkbox(&mut settings.math.enabled, "Math");
            ui.add_enabled_ui(settings.math.enabled, |ui| {
                egui::Grid::new("dscope_math_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Operation");
                        egui::ComboBox::from_id_salt("scope_math_operation")
                            .selected_text(settings.math.operation.label())
                            .show_ui(ui, |ui| {
                                for operation in [
                                    ScopeMathOperation::Add,
                                    ScopeMathOperation::Subtract,
                                    ScopeMathOperation::Multiply,
                                    ScopeMathOperation::Divide,
                                ] {
                                    ui.selectable_value(
                                        &mut settings.math.operation,
                                        operation,
                                        operation.label(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("First source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_math_left_channel",
                            &mut settings.math.left_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();

                        ui.label("Second source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_math_right_channel",
                            &mut settings.math.right_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();
                    });
            });

            ui.add_space(6.0);
            ui.checkbox(&mut settings.fft.enabled, "FFT");
            ui.add_enabled_ui(settings.fft.enabled, |ui| {
                egui::Grid::new("dscope_fft_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_fft_channel",
                            &mut settings.fft.channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();

                        ui.label("Length");
                        egui::ComboBox::from_id_salt("scope_fft_length")
                            .selected_text(settings.fft.length.to_string())
                            .show_ui(ui, |ui| {
                                for length in FFT_LENGTHS {
                                    ui.selectable_value(
                                        &mut settings.fft.length,
                                        length,
                                        length.to_string(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Window");
                        egui::ComboBox::from_id_salt("scope_fft_window")
                            .selected_text(settings.fft.window.label())
                            .show_ui(ui, |ui| {
                                for window in [
                                    ScopeFftWindow::Rectangular,
                                    ScopeFftWindow::Hann,
                                    ScopeFftWindow::Hamming,
                                    ScopeFftWindow::Blackman,
                                    ScopeFftWindow::FlatTop,
                                ] {
                                    ui.selectable_value(
                                        &mut settings.fft.window,
                                        window,
                                        window.label(),
                                    );
                                }
                            });
                        ui.end_row();
                    });
                ui.small(RichText::new("Magnitude in dBV").color(TEXT_MUTED));
            });

            ui.add_space(6.0);
            let lissajous_available = channels.into_iter().filter(|enabled| *enabled).count() >= 2;
            ui.add_enabled(
                lissajous_available,
                egui::Checkbox::new(&mut settings.lissajous.enabled, "Lissajous tab (XY)"),
            );
            ui.add_enabled_ui(settings.lissajous.enabled && lissajous_available, |ui| {
                egui::Grid::new("dscope_lissajous_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("X source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_lissajous_x_channel",
                            &mut settings.lissajous.x_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();

                        ui.label("Y source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_lissajous_y_channel",
                            &mut settings.lissajous.y_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();
                    });
                ui.small(
                    RichText::new("Plots Y-channel voltage against X-channel voltage")
                        .color(TEXT_MUTED),
                );
            });
            if !lissajous_available {
                ui.small(RichText::new("Requires two enabled channels").color(TEXT_MUTED));
            }
        });
}

pub(crate) fn paint_scope_measurement_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeMeasurementSettings,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::CollapsingHeader::new(RichText::new("Measurements").strong())
        .default_open(false)
        .show(ui, |ui| {
            for (channel, channel_enabled) in channels.into_iter().enumerate() {
                let channel_settings = &mut settings.channels[channel];
                ui.add_enabled_ui(channel_enabled, |ui| {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut channel_settings.enabled,
                                RichText::new(format!(
                                    "{} measurements",
                                    scope_channel_label(channel, channel_names)
                                ))
                                .strong()
                                .color(SCOPE_COLORS[channel]),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Clear").clicked() {
                                        channel_settings.selected.fill(false);
                                    }
                                    if ui.small_button("All").clicked() {
                                        channel_settings.selected.fill(true);
                                    }
                                },
                            );
                        });
                        ui.add_enabled_ui(channel_settings.enabled, |ui| {
                            egui::Grid::new(("dscope_measurement_settings", channel))
                                .num_columns(2)
                                .spacing([12.0, 4.0])
                                .show(ui, |ui| {
                                    for (index, kind) in
                                        SCOPE_MEASUREMENT_KINDS.into_iter().enumerate()
                                    {
                                        ui.checkbox(
                                            &mut channel_settings.selected[kind.index()],
                                            kind.label(),
                                        );
                                        if index % 2 == 1 {
                                            ui.end_row();
                                        }
                                    }
                                });
                        });
                    });
                });
                if !channel_enabled {
                    ui.small(
                        RichText::new(format!(
                            "{} is disabled",
                            scope_channel_label(channel, channel_names)
                        ))
                        .color(TEXT_MUTED),
                    );
                }
                if channel + 1 < channels.len() {
                    ui.add_space(6.0);
                }
            }
            ui.small(RichText::new("Calculated over the visible window").color(TEXT_MUTED));
        });
}

pub(crate) fn paint_scope_marker_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeMarkerSettings,
    editor: &mut Option<ScopeMarkerEditor>,
    viewport: WaveformViewport,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::CollapsingHeader::new(RichText::new("Markers").strong())
        .default_open(false)
        .show(ui, |ui| {
            for (channel, channel_enabled) in channels.into_iter().enumerate() {
                ui.add_enabled_ui(channel_enabled, |ui| {
                    ui.group(|ui| {
                        let total = settings.channels[channel].total_count();
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} markers",
                                    scope_channel_label(channel, channel_names)
                                ))
                                    .strong()
                                    .color(SCOPE_COLORS[channel]),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            total < SCOPE_MARKER_LIMIT,
                                            egui::Button::new("+ Add marker"),
                                        )
                                        .clicked()
                                    {
                                        *editor = Some(ScopeMarkerEditor::for_new(
                                            *settings,
                                            viewport,
                                            channel,
                                            ScopeMarkerKind::Time,
                                        ));
                                    }
                                    ui.monospace(format!("{total} / {SCOPE_MARKER_LIMIT}"));
                                },
                            );
                        });

                        let mut remove = None;
                        for kind in [ScopeMarkerKind::Time, ScopeMarkerKind::Voltage] {
                            let count = settings.channels[channel].count(kind);
                            for index in 0..count {
                                let Some((_, comparison, color)) =
                                    settings.channels[channel].marker(kind, index)
                                else {
                                    continue;
                                };
                                ui.horizontal(|ui| {
                                    ui.colored_label(
                                        color,
                                        RichText::new(format!(
                                            "{}{}{}",
                                            kind.short_label(),
                                            channel,
                                            index + 1
                                        ))
                                        .strong(),
                                    );
                                    ui.small(kind.label());
                                    ui.small(match comparison {
                                        Some(other) => format!(
                                            "vs {}{}{}",
                                            kind.short_label(),
                                            channel,
                                            other + 1
                                        ),
                                        None => "Standalone".to_owned(),
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .small_button("×")
                                                .on_hover_selectable_text("Remove marker")
                                                .clicked()
                                            {
                                                remove = Some((kind, index));
                                            }
                                            if ui.small_button("Edit…").clicked() {
                                                *editor = ScopeMarkerEditor::for_existing(
                                                    *settings,
                                                    ScopeMarkerKey {
                                                        channel,
                                                        kind,
                                                        index,
                                                    },
                                                );
                                            }
                                        },
                                    );
                                });
                            }
                        }
                        if let Some((kind, index)) = remove {
                            settings.channels[channel].remove_marker(kind, index);
                        }
                        if total > 0 {
                            ui.small(
                                RichText::new(
                                    "Drag time markers left/right and amplitude markers up/down in the OSC plot",
                                )
                                .color(TEXT_MUTED),
                            );
                        }
                    });
                });
                if !channel_enabled {
                    ui.small(
                        RichText::new(format!(
                            "{} is disabled",
                            scope_channel_label(channel, channel_names)
                        ))
                        .color(TEXT_MUTED),
                    );
                }
                if channel + 1 < channels.len() {
                    ui.add_space(6.0);
                }
            }
        });
}

pub(crate) fn paint_scope_marker_editor(
    context: &egui::Context,
    editor_state: &mut Option<ScopeMarkerEditor>,
    settings: &mut ScopeMarkerSettings,
    viewport: WaveformViewport,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    let Some(mut editor) = *editor_state else {
        return;
    };
    let mut open = true;
    let mut cancel = false;
    let mut save = false;
    let screen_rect = context.input(|input| input.screen_rect());
    egui::Area::new(egui::Id::new("scope_marker_modal_backdrop"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen_rect.min)
        .show(context, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, Color32::from_black_alpha(96));
        });
    let title = if editor.original.is_some() {
        "Marker settings"
    } else {
        "Add marker"
    };
    egui::Window::new(title)
        .open(&mut open)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(360.0)
        .show(context, |ui| {
            ui.label(
                RichText::new(format!(
                    "{} marker",
                    scope_channel_label(editor.channel, channel_names)
                ))
                .strong()
                .color(SCOPE_COLORS[editor.channel]),
            );
            let kind_before = editor.kind;
            ui.horizontal(|ui| {
                ui.label("Type");
                egui::ComboBox::from_id_salt("scope_marker_kind")
                    .selected_text(editor.kind.label())
                    .show_ui(ui, |ui| {
                        for kind in [ScopeMarkerKind::Time, ScopeMarkerKind::Voltage] {
                            ui.selectable_value(&mut editor.kind, kind, kind.label());
                        }
                    });
            });
            if kind_before != editor.kind {
                editor.comparison = None;
                if editor.original.is_none() {
                    editor.position = scope_marker_default_position(editor.kind, viewport);
                }
            }

            ui.add_space(8.0);
            let position_label = match editor.kind {
                ScopeMarkerKind::Time => "Horizontal position",
                ScopeMarkerKind::Voltage => "Vertical position",
            };
            ui.label(position_label);
            let mut position_percent = editor.position * 100.0;
            if ui
                .add(
                    egui::Slider::new(&mut position_percent, 0.0..=100.0)
                        .suffix(" %")
                        .max_decimals(1),
                )
                .changed()
            {
                editor.position = position_percent / 100.0;
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Color");
                ui.color_edit_button_srgba(&mut editor.color);
            });

            ui.add_space(8.0);
            ui.label("Math");
            let marker_count = settings.channels[editor.channel].count(editor.kind);
            let original_index = editor
                .original
                .filter(|key| key.channel == editor.channel && key.kind == editor.kind)
                .map(|key| key.index);
            if editor
                .comparison
                .is_some_and(|index| index >= marker_count || Some(index) == original_index)
            {
                editor.comparison = None;
            }
            egui::ComboBox::from_id_salt("scope_marker_comparison")
                .selected_text(match editor.comparison {
                    Some(index) => format!(
                        "Compare with {}{}{}",
                        editor.kind.short_label(),
                        editor.channel,
                        index + 1
                    ),
                    None => "Standalone (no calculation)".to_owned(),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut editor.comparison,
                        None,
                        "Standalone (no calculation)",
                    );
                    for index in 0..marker_count {
                        if Some(index) == original_index {
                            continue;
                        }
                        ui.selectable_value(
                            &mut editor.comparison,
                            Some(index),
                            format!(
                                "{}{}{}",
                                editor.kind.short_label(),
                                editor.channel,
                                index + 1
                            ),
                        );
                    }
                });
            ui.small(
                RichText::new(match (editor.kind, editor.comparison) {
                    (ScopeMarkerKind::Time, Some(_)) => {
                        "The OSC plot displays Δt and the reciprocal frequency."
                    }
                    (ScopeMarkerKind::Voltage, Some(_)) => {
                        "The OSC plot displays ΔV using this channel's scale."
                    }
                    (_, None) => "Standalone markers do not display a delta calculation.",
                })
                .color(TEXT_MUTED),
            );

            let stays_on_channel = editor
                .original
                .is_some_and(|key| key.channel == editor.channel);
            let target_total = settings.channels[editor.channel].total_count();
            let can_save =
                channels[editor.channel] && (stays_on_channel || target_total < SCOPE_MARKER_LIMIT);
            if !can_save {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "This channel already has the maximum of {SCOPE_MARKER_LIMIT} markers."
                    ))
                    .color(Color32::LIGHT_RED),
                );
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui
                    .add_enabled(can_save, egui::Button::new("Save marker"))
                    .clicked()
                {
                    save = true;
                }
            });
        });

    if context.input(|input| input.key_pressed(egui::Key::Escape)) {
        cancel = true;
    }
    if save {
        editor.save(settings);
        *editor_state = None;
    } else if cancel || !open {
        *editor_state = None;
    } else {
        *editor_state = Some(editor);
    }
}

pub(crate) fn paint_scope_channel_settings(
    ui: &mut egui::Ui,
    channel_index: usize,
    settings: &mut ScopeChannelSettings,
    channel_color: &mut Color32,
    channel_name: &mut String,
    other_channel_enabled: bool,
    availability: CaptureSettingsAvailability,
) {
    egui::Frame::none()
        .fill(PANEL.gamma_multiply(0.72))
        .rounding(egui::Rounding::same(5.0))
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let can_toggle_enable = !settings.enabled || other_channel_enabled;
            let enable_label = format!(
                "{} enabled",
                channel_label("CH", channel_index, channel_name)
            );
            let enable_control = egui::Checkbox::new(&mut settings.enabled, enable_label);
            ui.add_enabled(
                availability.channel_enable && can_toggle_enable,
                enable_control,
            )
            .on_hover_selectable_text("At least one oscilloscope channel must remain enabled");
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add_enabled(
                    availability.capture,
                    egui::TextEdit::singleline(channel_name)
                        .char_limit(CHANNEL_NAME_MAX_CHARS)
                        .hint_text(format!("CH{channel_index}")),
                );
            });
            ui.add_enabled_ui(availability.input_range && settings.enabled, |ui| {
                ui.label("Input range");
                paint_scope_input_range_encoder(
                    ui,
                    channel_index,
                    &mut settings.volts_per_div_mv,
                    channel_color,
                );
            });
            ui.add_space(12.0);
            ui.add_enabled_ui(availability.capture && settings.enabled, |ui| {
                egui::Grid::new(("scope_channel_secondary_settings", channel_index))
                    .num_columns(2)
                    .spacing([8.0, 5.0])
                    .show(ui, |ui| {
                        ui.label("Coupling");
                        egui::ComboBox::from_id_salt(("scope_coupling", channel_index))
                            .selected_text(coupling_label(settings.coupling))
                            .show_ui(ui, |ui| {
                                for coupling in [DsCopeCoupling::Dc, DsCopeCoupling::Ac] {
                                    ui.selectable_value(
                                        &mut settings.coupling,
                                        coupling,
                                        coupling_label(coupling),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Probe");
                        egui::ComboBox::from_id_salt(("scope_probe", channel_index))
                            .selected_text(format!("{}×", settings.probe_attenuation))
                            .show_ui(ui, |ui| {
                                for attenuation in [1, 10, 100] {
                                    ui.selectable_value(
                                        &mut settings.probe_attenuation,
                                        attenuation,
                                        format!("{attenuation}×"),
                                    );
                                }
                            });
                        ui.end_row();
                    });
            });
        });
}

pub(crate) struct ScopeToolsOptions {
    pub(crate) viewport: WaveformViewport,
    pub(crate) trigger_controls_enabled: bool,
}

pub(crate) fn paint_scope_tools(
    ui: &mut egui::Ui,
    scope_settings: &mut ScopeCaptureSettings,
    function_settings: &mut ScopeFunctionSettings,
    marker_settings: &mut ScopeMarkerSettings,
    marker_editor: &mut Option<ScopeMarkerEditor>,
    channel_names: &[String; 2],
    options: ScopeToolsOptions,
) {
    let ScopeToolsOptions {
        viewport,
        trigger_controls_enabled,
    } = options;
    let channel_enabled = scope_settings.channels.map(|channel| channel.enabled);
    scope_settings.trigger.clamp_to_channels(channel_enabled);
    function_settings.clamp_to_channels(channel_enabled);

    paint_scope_trigger_settings(
        ui,
        &mut scope_settings.trigger,
        channel_enabled,
        channel_names,
        trigger_controls_enabled,
    );
    ui.add_space(8.0);
    paint_scope_function_settings(ui, function_settings, channel_enabled, channel_names);
    ui.add_space(8.0);
    paint_scope_measurement_settings(
        ui,
        &mut function_settings.measurements,
        channel_enabled,
        channel_names,
    );
    ui.add_space(8.0);
    paint_scope_marker_settings(
        ui,
        marker_settings,
        marker_editor,
        viewport,
        channel_enabled,
        channel_names,
    );
}
