//! The capture-settings tab: logic and scope capture controls, trigger
//! conditions, marker settings, and the save-as-default button.

use crate::{
    egui, format_duration, format_sample_count, format_sample_rate, logic_capture_duration_label,
    logic_capture_duration_options, logic_marker_color, logic_trigger_condition_label,
    marker_text_color, paint_protocol_analyzer_settings, paint_scope_channel_settings,
    set_logic_operation_mode_preserving_duration, set_logic_sample_rate_preserving_duration,
    toolbar_action_button, AcquisitionMode, CaptureSettingsTab, Color32, DsCopeBandwidthLimit,
    DsCopeOperationMode, DsLogicInputFilter, DsLogicOperationMode, DsLogicStopOption,
    DsLogicTriggerCondition, Duration, LogicCaptureSettings, LogicMarkerSettings, LogicTriggerMode,
    ProtocolAnalyzerSettings, RichText, SavedCaptureSettingsKind, ScopeCalibrationAction,
    ScopeCaptureSettings, SelectableHoverText, Stroke, UsbSpeedClass, Vec2, ACCENT, BACKGROUND,
    DSCOPE_SAMPLE_COUNTS, DSCOPE_SAMPLE_RATES_HZ, DSLOGIC_SAMPLE_RATES_HZ,
    DSLOGIC_TRIGGER_STAGE_COUNT, GRID, LOGIC_CHANNEL_COUNT, LOGIC_MARKER_LIMIT,
    LOGIC_MARKER_NAME_MAX_CHARS, SAMPLE_RATE_HZ, TEXT_MUTED,
};

#[derive(Clone, Copy)]
pub(crate) struct CaptureSettingsAvailability {
    pub(crate) capture: bool,
    pub(crate) sample_rate: bool,
    pub(crate) channel_enable: bool,
    pub(crate) input_range: bool,
    pub(crate) manual_calibration: bool,
}

pub(crate) struct ScopeCaptureSettingsEditor<'a> {
    pub(crate) settings: &'a mut ScopeCaptureSettings,
    pub(crate) auto_sample_rate: &'a mut bool,
    pub(crate) smart_timing_enabled: &'a mut bool,
}

pub(crate) struct LogicCaptureSettingsEditor<'a> {
    pub(crate) settings: &'a mut LogicCaptureSettings,
    pub(crate) speed: UsbSpeedClass,
    pub(crate) acquisition_mode: &'a mut AcquisitionMode,
    pub(crate) repetitive_interval_seconds: &'a mut f32,
    pub(crate) smart_timing_enabled: &'a mut bool,
    pub(crate) marker_settings: &'a mut LogicMarkerSettings,
    pub(crate) protocol_analyzer: &'a mut ProtocolAnalyzerSettings,
    pub(crate) displayed_sample_count: usize,
    pub(crate) displayed_sample_rate_hz: u64,
}

pub(crate) struct ScopeVisualizationSettingsEditor<'a> {
    pub(crate) channel_colors: &'a mut [Color32; 2],
    pub(crate) scope_channel_names: &'a mut [String; 2],
}

pub(crate) fn paint_logic_trigger_conditions(
    ui: &mut egui::Ui,
    enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
    conditions: &mut [Option<DsLogicTriggerCondition>; LOGIC_CHANNEL_COUNT],
    id_source: impl std::hash::Hash,
) {
    ui.vertical(|ui| {
        ui.small(RichText::new("All selected conditions must match together").color(TEXT_MUTED));
        for channel in 0..LOGIC_CHANNEL_COUNT {
            if !enabled_channels[channel] {
                continue;
            }
            ui.horizontal(|ui| {
                ui.monospace(format!("CH {channel}"));
                egui::ComboBox::from_id_salt((&id_source, channel))
                    .width(92.0)
                    .selected_text(
                        conditions[channel]
                            .map(logic_trigger_condition_label)
                            .unwrap_or("Don't care"),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut conditions[channel], None, "Don't care");
                        for condition in [
                            DsLogicTriggerCondition::RisingEdge,
                            DsLogicTriggerCondition::FallingEdge,
                            DsLogicTriggerCondition::EitherEdge,
                            DsLogicTriggerCondition::High,
                            DsLogicTriggerCondition::Low,
                        ] {
                            ui.selectable_value(
                                &mut conditions[channel],
                                Some(condition),
                                logic_trigger_condition_label(condition),
                            );
                        }
                    });
            });
        }
        if !conditions.iter().any(Option::is_some) {
            ui.add(
                egui::Label::new(
                    RichText::new("Choose at least one condition")
                        .small()
                        .color(Color32::LIGHT_RED),
                )
                .selectable(true),
            );
        }
    });
}

pub(crate) fn paint_capture_settings(
    ui: &mut egui::Ui,
    tab: CaptureSettingsTab,
    logic_editor: LogicCaptureSettingsEditor<'_>,
    scope_editor: ScopeCaptureSettingsEditor<'_>,
    visualization_editor: ScopeVisualizationSettingsEditor<'_>,
    availability: CaptureSettingsAvailability,
    calibration_action: &mut Option<ScopeCalibrationAction>,
) -> bool {
    let LogicCaptureSettingsEditor {
        settings: logic_settings,
        speed: logic_speed,
        acquisition_mode: logic_acquisition_mode,
        repetitive_interval_seconds,
        smart_timing_enabled: logic_smart_timing_enabled,
        marker_settings: logic_marker_settings,
        protocol_analyzer,
        displayed_sample_count: displayed_logic_sample_count,
        displayed_sample_rate_hz: displayed_logic_sample_rate_hz,
    } = logic_editor;
    let ScopeCaptureSettingsEditor {
        settings: scope_settings,
        auto_sample_rate: scope_auto_sample_rate,
        smart_timing_enabled: scope_smart_timing_enabled,
    } = scope_editor;
    let ScopeVisualizationSettingsEditor {
        channel_colors: scope_channel_colors,
        scope_channel_names,
    } = visualization_editor;
    let card_width = ui.available_width();
    let save_clicked = egui::Frame::none()
        .fill(BACKGROUND)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            let content_width = (card_width - 20.0).max(0.0);
            ui.set_width(content_width);
            ui.set_max_width(content_width);
            ui.label(
                RichText::new(match tab {
                    CaptureSettingsTab::DsLogic => "DSLogic 16-channel analyzer",
                    CaptureSettingsTab::DsCope => "DSCope U3P100",
                })
                .strong(),
            );
            ui.add_space(6.0);
            match tab {
                CaptureSettingsTab::DsLogic => {
                    if logic_settings.operation_mode == DsLogicOperationMode::InternalTest {
                        logic_settings.enabled_channels = [true; LOGIC_CHANNEL_COUNT];
                        logic_settings.trigger_enabled = false;
                        set_logic_sample_rate_preserving_duration(
                            logic_settings,
                            100_000_000,
                        );
                    }
                    if *logic_acquisition_mode == AcquisitionMode::Loop {
                        logic_settings.trigger_enabled = false;
                    }
                    if logic_settings.operation_mode != DsLogicOperationMode::Buffer
                        && logic_settings.trigger_mode == LogicTriggerMode::Smart
                    {
                        logic_settings.trigger_mode = LogicTriggerMode::Simple;
                    }
                    for channel in 0..LOGIC_CHANNEL_COUNT {
                        if !logic_settings.enabled_channels[channel] {
                            logic_settings.trigger_conditions[channel] = None;
                            for stage in &mut logic_settings.smart_trigger_conditions {
                                stage[channel] = None;
                            }
                        }
                    }
                    if logic_settings.trigger_enabled
                        && logic_settings.trigger_mode == LogicTriggerMode::Simple
                        && !logic_settings.trigger_conditions.iter().any(Option::is_some)
                    {
                        if let Some(channel) = logic_settings
                            .enabled_channels
                            .iter()
                            .position(|enabled| *enabled)
                        {
                            logic_settings.trigger_conditions[channel] =
                                Some(DsLogicTriggerCondition::RisingEdge);
                        }
                    }
                    if logic_settings.trigger_enabled
                        && logic_settings.trigger_mode == LogicTriggerMode::Smart
                    {
                        let default_channel = logic_settings
                            .enabled_channels
                            .iter()
                            .position(|enabled| *enabled);
                        for stage_index in
                            0..usize::from(logic_settings.smart_trigger_stage_count)
                        {
                            if !logic_settings.smart_trigger_conditions[stage_index]
                                .iter()
                                .any(Option::is_some)
                            {
                                if let Some(channel) = default_channel {
                                    logic_settings.smart_trigger_conditions[stage_index][channel] =
                                        Some(if stage_index % 2 == 0 {
                                            DsLogicTriggerCondition::RisingEdge
                                        } else {
                                            DsLogicTriggerCondition::FallingEdge
                                        });
                                }
                            }
                        }
                    }
                    let maximum_sample_rate_hz =
                        logic_settings.maximum_sample_rate_hz(logic_speed);
                    if logic_settings.sample_rate_hz > maximum_sample_rate_hz {
                        let sample_rate_hz = DSLOGIC_SAMPLE_RATES_HZ
                            .iter()
                            .rev()
                            .copied()
                            .find(|rate| *rate <= maximum_sample_rate_hz)
                            .unwrap_or(SAMPLE_RATE_HZ);
                        set_logic_sample_rate_preserving_duration(
                            logic_settings,
                            sample_rate_hz,
                        );
                    }
                    let enabled_channel_count = logic_settings
                        .enabled_channels
                        .iter()
                        .filter(|enabled| **enabled)
                        .count();
                    egui::Grid::new("dslogic_capture_settings")
                        .num_columns(2)
                        .min_col_width(90.0)
                        .max_col_width(((content_width - 12.0) * 0.5).max(90.0))
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Samples");
                            ui.monospace(format_sample_count(logic_settings.sample_count));
                            ui.end_row();
                            ui.label("Maximum");
                            ui.monospace(format_sample_rate(maximum_sample_rate_hz));
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    ui.checkbox(
                        logic_smart_timing_enabled,
                        "Smart timing on waveform hover",
                    )
                    .on_hover_selectable_text(
                        "Show edge width, rising-edge period, frequency, and duty cycle for the digital waveform under the mouse",
                    );
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(RichText::new("Options").strong())
                        .default_open(false)
                        .show(ui, |ui| {
                            const CONTROL_WIDTH: f32 = 100.0;
                            ui.set_max_width(ui.available_width());
                            ui.add_enabled_ui(availability.capture, |ui| {
                                egui::Grid::new("dslogic_options")
                                    .num_columns(2)
                                    .max_col_width(126.0)
                                    .spacing([8.0, 6.0])
                                    .show(ui, |ui| {
                                        ui.label("Operation mode");
                                        let previous_operation_mode =
                                            logic_settings.operation_mode;
                                        egui::ComboBox::from_id_salt("logic_operation_mode")
                                            .width(CONTROL_WIDTH)
                                            .selected_text(match logic_settings.operation_mode {
                                                DsLogicOperationMode::Buffer => "Buffer mode",
                                                DsLogicOperationMode::Stream => "Stream mode",
                                                DsLogicOperationMode::InternalTest => {
                                                    "Internal test"
                                                }
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut logic_settings.operation_mode,
                                                    DsLogicOperationMode::Buffer,
                                                    "Buffer mode",
                                                );
                                                ui.selectable_value(
                                                    &mut logic_settings.operation_mode,
                                                    DsLogicOperationMode::Stream,
                                                    "Stream mode",
                                                );
                                                ui.selectable_value(
                                                    &mut logic_settings.operation_mode,
                                                    DsLogicOperationMode::InternalTest,
                                                    "Internal test",
                                                );
                                            });
                                        if logic_settings.operation_mode
                                            != previous_operation_mode
                                        {
                                            let selected_operation_mode =
                                                logic_settings.operation_mode;
                                            logic_settings.operation_mode =
                                                previous_operation_mode;
                                            set_logic_operation_mode_preserving_duration(
                                                logic_settings,
                                                selected_operation_mode,
                                            );
                                        }
                                        ui.end_row();

                                        ui.label("Stop options");
                                        ui.add_enabled_ui(
                                            logic_settings.operation_mode
                                                != DsLogicOperationMode::Stream,
                                            |ui| {
                                                egui::ComboBox::from_id_salt("logic_stop_option")
                                                    .width(CONTROL_WIDTH)
                                                    .selected_text(match logic_settings.stop_option {
                                                        DsLogicStopOption::StopImmediately => {
                                                            "Stop immediately"
                                                        }
                                                        DsLogicStopOption::UploadCapturedData => {
                                                            "Upload data"
                                                        }
                                                    })
                                                    .show_ui(ui, |ui| {
                                                        ui.selectable_value(
                                                            &mut logic_settings.stop_option,
                                                            DsLogicStopOption::StopImmediately,
                                                            "Stop immediately",
                                                        );
                                                        ui.selectable_value(
                                                            &mut logic_settings.stop_option,
                                                            DsLogicStopOption::UploadCapturedData,
                                                            "Upload data",
                                                        );
                                                    });
                                            },
                                        );
                                        ui.end_row();

                                        ui.label("Threshold level");
                                        ui.add(
                                            egui::DragValue::new(
                                                &mut logic_settings.threshold_voltage,
                                            )
                                            .range(0.0..=5.0)
                                            .speed(0.1)
                                            .fixed_decimals(1)
                                            .suffix(" V"),
                                        );
                                        ui.end_row();

                                        ui.label("Filter targets");
                                        egui::ComboBox::from_id_salt("logic_input_filter")
                                            .width(CONTROL_WIDTH)
                                            .selected_text(match logic_settings.input_filter {
                                                DsLogicInputFilter::None => "None",
                                                DsLogicInputFilter::OneSampleClock => {
                                                    "1 sample clock"
                                                }
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut logic_settings.input_filter,
                                                    DsLogicInputFilter::None,
                                                    "None",
                                                );
                                                ui.selectable_value(
                                                    &mut logic_settings.input_filter,
                                                    DsLogicInputFilter::OneSampleClock,
                                                    "1 sample clock",
                                                );
                                            });
                                        ui.end_row();

                                        ui.label("Enable RLE compress");
                                        ui.add_enabled(
                                            logic_settings.operation_mode
                                                != DsLogicOperationMode::Stream,
                                            egui::Checkbox::without_text(
                                                &mut logic_settings.rle_enabled,
                                            ),
                                        );
                                        ui.end_row();

                                        ui.label("Using external clock");
                                        ui.checkbox(&mut logic_settings.external_clock, "");
                                        ui.end_row();

                                        ui.label("Using clock negedge");
                                        ui.checkbox(&mut logic_settings.falling_clock_edge, "");
                                        ui.end_row();
                                    });
                            });
                        });
                    ui.add_space(8.0);
                    let capture_summary = format!(
                        "Capture · {} @ {}",
                        logic_capture_duration_label(*logic_settings),
                        format_sample_rate(logic_settings.sample_rate_hz)
                    );
                    egui::CollapsingHeader::new(RichText::new(capture_summary).strong())
                        .id_salt("dslogic_capture_section")
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width());
                            egui::Grid::new("dslogic_capture_menu")
                                .num_columns(2)
                                .max_col_width(126.0)
                                .spacing([8.0, 6.0])
                                .show(ui, |ui| {
                                    ui.label("Mode");
                                    ui.add_enabled_ui(availability.capture, |ui| {
                                        let loop_available = logic_settings.operation_mode
                                            == DsLogicOperationMode::Stream;
                                        egui::ComboBox::from_id_salt("logic_acquisition_mode")
                                            .width(100.0)
                                            .selected_text(logic_acquisition_mode.logic_label())
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    logic_acquisition_mode,
                                                    AcquisitionMode::Single,
                                                    "Single",
                                                );
                                                ui.selectable_value(
                                                    logic_acquisition_mode,
                                                    AcquisitionMode::Repetitive,
                                                    "Repetitive",
                                                );
                                                ui.add_enabled_ui(loop_available, |ui| {
                                                    ui.selectable_value(
                                                        logic_acquisition_mode,
                                                        AcquisitionMode::Loop,
                                                        "Loop",
                                                    )
                                                    .on_disabled_hover_selectable_text(
                                                        "Loop requires DSLogic Stream mode",
                                                    );
                                                });
                                            });
                                    });
                                    ui.end_row();

                                    ui.label("Duration");
                                    ui.add_enabled_ui(availability.capture, |ui| {
                                        egui::ComboBox::from_id_salt("logic_capture_duration")
                                            .width(100.0)
                                            .selected_text(logic_capture_duration_label(
                                                *logic_settings,
                                            ))
                                            .show_ui(ui, |ui| {
                                                for (duration_seconds, sample_count) in
                                                    logic_capture_duration_options(*logic_settings)
                                                {
                                                    ui.selectable_value(
                                                        &mut logic_settings.sample_count,
                                                        sample_count,
                                                        format_duration(Duration::from_secs_f64(
                                                            duration_seconds,
                                                        )),
                                                    );
                                                }
                                            });
                                    });
                                    ui.end_row();

                                    ui.label("Sample rate");
                                    ui.add_enabled_ui(
                                        availability.capture
                                            && logic_settings.operation_mode
                                                != DsLogicOperationMode::InternalTest,
                                        |ui| {
                                            let previous_rate = logic_settings.sample_rate_hz;
                                            egui::ComboBox::from_id_salt("logic_sample_rate")
                                                .width(100.0)
                                                .selected_text(format_sample_rate(
                                                    logic_settings.sample_rate_hz,
                                                ))
                                                .show_ui(ui, |ui| {
                                                    for rate in DSLOGIC_SAMPLE_RATES_HZ
                                                        .iter()
                                                        .copied()
                                                        .filter(|rate| {
                                                            *rate <= maximum_sample_rate_hz
                                                        })
                                                    {
                                                        ui.selectable_value(
                                                            &mut logic_settings.sample_rate_hz,
                                                            rate,
                                                            format_sample_rate(rate),
                                                        );
                                                    }
                                                });
                                            if logic_settings.sample_rate_hz != previous_rate {
                                                let selected_rate = logic_settings.sample_rate_hz;
                                                logic_settings.sample_rate_hz = previous_rate;
                                                set_logic_sample_rate_preserving_duration(
                                                    logic_settings,
                                                    selected_rate,
                                                );
                                            }
                                        },
                                    );
                                    ui.end_row();

                                    ui.label("Hardware trigger");
                                    let trigger_response = ui.add_enabled(
                                        availability.capture
                                            && logic_settings.operation_mode
                                                != DsLogicOperationMode::InternalTest
                                            && *logic_acquisition_mode != AcquisitionMode::Loop,
                                        egui::Checkbox::without_text(
                                            &mut logic_settings.trigger_enabled,
                                        ),
                                    )
                                    .on_hover_selectable_text(
                                        "Arm the analyzer and wait for the configured hardware trigger",
                                    );
                                    if trigger_response.changed()
                                        && logic_settings.trigger_enabled
                                        && logic_settings.trigger_mode == LogicTriggerMode::Simple
                                        && !logic_settings
                                            .trigger_conditions
                                            .iter()
                                            .any(Option::is_some)
                                    {
                                        if let Some(channel) = logic_settings
                                            .enabled_channels
                                            .iter()
                                            .position(|enabled| *enabled)
                                        {
                                            logic_settings.trigger_conditions[channel] =
                                                Some(DsLogicTriggerCondition::RisingEdge);
                                        }
                                    }
                                    ui.end_row();

                                    if logic_settings.trigger_enabled {
                                        ui.label("Trigger mode");
                                        ui.add_enabled_ui(availability.capture, |ui| {
                                            egui::ComboBox::from_id_salt("logic_trigger_mode")
                                                .width(92.0)
                                                .selected_text(match logic_settings.trigger_mode {
                                                    LogicTriggerMode::Simple => "Simple",
                                                    LogicTriggerMode::Smart => "Smart",
                                                })
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut logic_settings.trigger_mode,
                                                        LogicTriggerMode::Simple,
                                                        "Simple",
                                                    )
                                                    .on_hover_selectable_text(
                                                        "All channel conditions match at one sample",
                                                    );
                                                    let smart_available =
                                                        logic_settings.operation_mode
                                                            == DsLogicOperationMode::Buffer;
                                                    ui.add_enabled_ui(smart_available, |ui| {
                                                        ui.selectable_value(
                                                            &mut logic_settings.trigger_mode,
                                                            LogicTriggerMode::Smart,
                                                            if smart_available {
                                                                "Smart"
                                                            } else {
                                                                "Smart (Buffer mode required)"
                                                            },
                                                        )
                                                        .on_hover_selectable_text(
                                                            "Match two or more channel-condition stages in order",
                                                        )
                                                        .on_disabled_hover_selectable_text(
                                                            "Smart trigger requires Buffer mode. Select Buffer under Capture mode to enable it.",
                                                        );
                                                    });
                                                });
                                        });
                                        ui.end_row();

                                        match logic_settings.trigger_mode {
                                            LogicTriggerMode::Simple => {
                                                ui.label("Conditions");
                                                ui.add_enabled_ui(availability.capture, |ui| {
                                                    paint_logic_trigger_conditions(
                                                        ui,
                                                        logic_settings.enabled_channels,
                                                        &mut logic_settings.trigger_conditions,
                                                        "logic_simple_trigger",
                                                    );
                                                });
                                                ui.end_row();
                                            }
                                            LogicTriggerMode::Smart => {
                                                ui.label("Smart stages");
                                                ui.add_enabled_ui(availability.capture, |ui| {
                                                    ui.vertical(|ui| {
                                                        ui.horizontal(|ui| {
                                                            ui.label("Stages");
                                                            ui.add(
                                                                egui::DragValue::new(
                                                                    &mut logic_settings
                                                                        .smart_trigger_stage_count,
                                                                )
                                                                .range(
                                                                    2..=DSLOGIC_TRIGGER_STAGE_COUNT
                                                                        as u8,
                                                                ),
                                                            );
                                                        });
                                                        ui.small(
                                                            RichText::new(
                                                                "Each stage matches its channels together; stages match in order",
                                                            )
                                                            .color(TEXT_MUTED),
                                                        );
                                                        for stage_index in 0..usize::from(
                                                            logic_settings.smart_trigger_stage_count,
                                                        ) {
                                                            egui::CollapsingHeader::new(format!(
                                                                "Stage {}",
                                                                stage_index + 1
                                                            ))
                                                            .default_open(stage_index < 2)
                                                            .show(ui, |ui| {
                                                                paint_logic_trigger_conditions(
                                                                    ui,
                                                                    logic_settings.enabled_channels,
                                                                    &mut logic_settings
                                                                        .smart_trigger_conditions
                                                                        [stage_index],
                                                                    (
                                                                        "logic_smart_trigger",
                                                                        stage_index,
                                                                    ),
                                                                );
                                                            });
                                                        }
                                                    });
                                                });
                                                ui.end_row();
                                            }
                                        }
                                    }

                                    if *logic_acquisition_mode == AcquisitionMode::Repetitive {
                                        ui.label("Repeat interval");
                                        if logic_settings.operation_mode
                                            == DsLogicOperationMode::Stream
                                        {
                                            ui.label(RichText::new("0.1 s").color(TEXT_MUTED))
                                                .on_hover_selectable_text(
                                                    "Stream-mode Repetitive uses the original fixed 0.1 s interval",
                                                );
                                        } else {
                                            ui.add_enabled(
                                                availability.capture,
                                                egui::DragValue::new(
                                                    repetitive_interval_seconds,
                                                )
                                                .range(0.1..=10.0)
                                                .speed(0.1)
                                                .fixed_decimals(1)
                                                .suffix(" s"),
                                            )
                                            .on_hover_selectable_text("Delay between complete captures");
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(
                        RichText::new(format!(
                            "Channels ({enabled_channel_count}/{LOGIC_CHANNEL_COUNT} enabled)"
                        ))
                        .strong(),
                    )
                    .id_salt("dslogic_channels_section")
                    .default_open(true)
                    .show(ui, |ui| {
                            let channel_controls_enabled = availability.capture
                                && logic_settings.operation_mode
                                    != DsLogicOperationMode::InternalTest;
                            ui.add_enabled_ui(channel_controls_enabled, |ui| {
                                egui::Grid::new("dslogic_channels")
                                    .num_columns(8)
                                    .min_col_width(20.0)
                                    .max_col_width(24.0)
                                    .spacing([5.0, 4.0])
                                    .show(ui, |ui| {
                                        for (channel, enabled) in logic_settings
                                            .enabled_channels
                                            .iter_mut()
                                            .enumerate()
                                        {
                                            ui.vertical_centered(|ui| {
                                                ui.small(channel.to_string());
                                                ui.add(egui::Checkbox::without_text(enabled));
                                            });
                                            if channel % 8 == 7 {
                                                ui.end_row();
                                            }
                                        }
                                    });
                                ui.horizontal(|ui| {
                                    if ui.small_button("Enable all").clicked() {
                                        logic_settings.enabled_channels =
                                            [true; LOGIC_CHANNEL_COUNT];
                                    }
                                    if ui.small_button("Disable all").clicked() {
                                        logic_settings.enabled_channels =
                                            [false; LOGIC_CHANNEL_COUNT];
                                    }
                                });
                            });
                            if !logic_settings
                                .enabled_channels
                                .iter()
                                .any(|enabled| *enabled)
                            {
                                ui.small(
                                    RichText::new("Enable at least one channel")
                                        .color(Color32::LIGHT_RED),
                                );
                            }
                        });
                    ui.add_space(8.0);
                    paint_logic_marker_settings(
                        ui,
                        logic_marker_settings,
                        displayed_logic_sample_count,
                        displayed_logic_sample_rate_hz,
                    );
                    ui.add_space(8.0);
                    paint_protocol_analyzer_settings(
                        ui,
                        protocol_analyzer,
                        &logic_settings.enabled_channels,
                    );
                }
                CaptureSettingsTab::DsCope => {
                    let maximum_sample_rate_hz = scope_settings.maximum_sample_rate_hz();
                    let maximum_sample_count = scope_settings.maximum_sample_count();
                    if scope_settings.sample_rate_hz > maximum_sample_rate_hz {
                        scope_settings.sample_rate_hz = maximum_sample_rate_hz;
                    }
                    if scope_settings.sample_count > maximum_sample_count {
                        scope_settings.sample_count = maximum_sample_count;
                    }
                    egui::Grid::new("dscope_common_capture_settings")
                        .num_columns(2)
                        .spacing([8.0, 5.0])
                        .show(ui, |ui| {
                            ui.label("Sample rate");
                            ui.horizontal(|ui| {
                                ui.add_enabled_ui(
                                    availability.sample_rate && !*scope_auto_sample_rate,
                                    |ui| {
                                        egui::ComboBox::from_id_salt("scope_sample_rate")
                                            .width(100.0)
                                            .selected_text(format_sample_rate(
                                                scope_settings.sample_rate_hz,
                                            ))
                                            .show_ui(ui, |ui| {
                                                for rate in DSCOPE_SAMPLE_RATES_HZ
                                                    .into_iter()
                                                    .filter(|rate| *rate <= maximum_sample_rate_hz)
                                                {
                                                    ui.selectable_value(
                                                        &mut scope_settings.sample_rate_hz,
                                                        rate,
                                                        format_sample_rate(rate),
                                                    );
                                                }
                                            });
                                    },
                                );
                                ui.add_enabled(
                                    availability.sample_rate,
                                    egui::Checkbox::new(scope_auto_sample_rate, "Auto"),
                                );
                            });
                            ui.end_row();

                            ui.label("Buffer");
                            ui.add_enabled_ui(availability.capture, |ui| {
                                egui::ComboBox::from_id_salt("scope_sample_count")
                                    .selected_text(format_sample_count(scope_settings.sample_count))
                                    .show_ui(ui, |ui| {
                                        for count in DSCOPE_SAMPLE_COUNTS
                                            .into_iter()
                                            .filter(|count| *count <= maximum_sample_count)
                                        {
                                            ui.selectable_value(
                                                &mut scope_settings.sample_count,
                                                count,
                                                format_sample_count(count),
                                            );
                                        }
                                    });
                            });
                            ui.end_row();

                            ui.label("Maximum");
                            ui.monospace(format_sample_rate(maximum_sample_rate_hz));
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    ui.checkbox(
                        scope_smart_timing_enabled,
                        "Smart timing on waveform hover",
                    )
                    .on_hover_selectable_text(
                        "Show elapsed time from the trigger crossing and the voltage at the waveform point under the mouse",
                    );
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(RichText::new("Options").strong())
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.add_enabled_ui(availability.capture, |ui| {
                                egui::Grid::new("dscope_options")
                                    .num_columns(2)
                                    .spacing([8.0, 6.0])
                                    .show(ui, |ui| {
                                        ui.label("Operating mode");
                                        egui::ComboBox::from_id_salt("scope_operation_mode")
                                            .selected_text(match scope_settings.operation_mode {
                                                DsCopeOperationMode::Normal => "Normal",
                                                DsCopeOperationMode::InternalTest => {
                                                    "Internal test"
                                                }
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut scope_settings.operation_mode,
                                                    DsCopeOperationMode::Normal,
                                                    "Normal",
                                                );
                                                ui.selectable_value(
                                                    &mut scope_settings.operation_mode,
                                                    DsCopeOperationMode::InternalTest,
                                                    "Internal test",
                                                );
                                            });
                                        ui.end_row();

                                        ui.label("Bandwidth");
                                        egui::ComboBox::from_id_salt("scope_bandwidth_limit")
                                            .selected_text(match scope_settings.bandwidth_limit {
                                                DsCopeBandwidthLimit::Full => "Full",
                                                DsCopeBandwidthLimit::Mhz20 => "20 MHz",
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut scope_settings.bandwidth_limit,
                                                    DsCopeBandwidthLimit::Full,
                                                    "Full",
                                                );
                                                ui.selectable_value(
                                                    &mut scope_settings.bandwidth_limit,
                                                    DsCopeBandwidthLimit::Mhz20,
                                                    "20 MHz",
                                                );
                                            });
                                        ui.end_row();
                                    });
                            });
                            ui.add_space(6.0);
                            ui.label(RichText::new("Calibration").color(TEXT_MUTED));
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(availability.capture, egui::Button::new("Auto…"))
                                    .clicked()
                                {
                                    *calibration_action = Some(ScopeCalibrationAction::Auto);
                                }
                                if ui
                                    .add_enabled(
                                        availability.manual_calibration,
                                        egui::Button::new("Manual…"),
                                    )
                                    .clicked()
                                {
                                    *calibration_action = Some(ScopeCalibrationAction::Manual);
                                }
                            });
                        });
                    ui.add_space(8.0);
                    let channel_count = scope_settings.channels.len();
                    let channel_enabled = scope_settings.channels.map(|channel| channel.enabled);
                    for (index, channel) in scope_settings.channels.iter_mut().enumerate() {
                        paint_scope_channel_settings(
                            ui,
                            index,
                            channel,
                            &mut scope_channel_colors[index],
                            &mut scope_channel_names[index],
                            channel_enabled[1 - index],
                            availability,
                        );
                        if index + 1 < channel_count {
                            ui.add_space(6.0);
                        }
                    }
                }
            }
            paint_capture_settings_save_button(
                ui,
                match tab {
                    CaptureSettingsTab::DsLogic => SavedCaptureSettingsKind::LogicAnalyzer,
                    CaptureSettingsTab::DsCope => SavedCaptureSettingsKind::Oscilloscope,
                },
                availability.capture,
            )
        })
        .inner;
    scope_settings.clamp_to_channel_limits();
    save_clicked
}

pub(crate) fn paint_capture_settings_save_button(
    ui: &mut egui::Ui,
    kind: SavedCaptureSettingsKind,
    enabled: bool,
) -> bool {
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(8.0);
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), 20.0),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.add_space(8.0);
            ui.add_enabled(
                enabled,
                toolbar_action_button(&format!("💾 Save {} settings", kind.label()), ACCENT)
                    .small()
                    .min_size(Vec2::new(136.0, 20.0)),
            )
            .on_hover_selectable_text("Replace the saved defaults for this instrument type")
            .clicked()
        },
    )
    .inner
}

pub(crate) fn paint_logic_marker_settings(
    ui: &mut egui::Ui,
    settings: &mut LogicMarkerSettings,
    logic_sample_count: usize,
    logic_sample_rate_hz: u64,
) {
    let has_waveform = logic_sample_count > 1 && logic_sample_rate_hz != 0;
    egui::CollapsingHeader::new(RichText::new("Markers").strong())
        .id_salt("logic_markers_section")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if settings.adding {
                    if ui.button("Cancel").clicked() {
                        settings.cancel();
                    }
                } else if ui
                    .add_enabled(
                        has_waveform && settings.markers.len() < LOGIC_MARKER_LIMIT,
                        egui::Button::new("+ Add"),
                    )
                    .on_disabled_hover_selectable_text(if has_waveform {
                        "The maximum of 20 markers has been reached"
                    } else {
                        "Capture or open logic samples before adding a marker"
                    })
                    .clicked()
                {
                    settings.arm();
                }
                ui.label(
                    RichText::new(format!("{}/{}", settings.markers.len(), LOGIC_MARKER_LIMIT))
                        .small()
                        .color(TEXT_MUTED),
                );
                if !settings.markers.is_empty()
                    && ui
                        .small_button("Clear")
                        .on_hover_selectable_text("Remove all markers")
                        .clicked()
                {
                    settings.clear();
                }
            });

            if settings.adding {
                ui.add_space(4.0);
                ui.small(
                    RichText::new(
                        "Drag with the left mouse button. Endpoints snap to nearby edges on the channel under the pointer.",
                    )
                    .color(ACCENT),
                );
            } else if settings.markers.is_empty() {
                ui.add_space(4.0);
                ui.small(RichText::new("No markers").color(TEXT_MUTED));
            }

            let mut remove = None;
            for (index, marker) in settings.markers.iter_mut().enumerate() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    paint_logic_marker_color_picker(ui, index, &mut marker.color);
                    if marker.name.trim().is_empty() {
                        ui.colored_label(marker.color, format!("M{}", index + 1));
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut marker.name)
                            .char_limit(LOGIC_MARKER_NAME_MAX_CHARS)
                            .desired_width(105.0)
                            .hint_text("Name"),
                    )
                    .on_hover_selectable_text("Optional marker name");
                });
                ui.horizontal(|ui| {
                    ui.add_space(26.0);
                    ui.monospace(format_duration(marker.duration()))
                        .on_hover_selectable_text(format!(
                            "Samples {}–{} at {}",
                            marker.start_sample,
                            marker.end_sample,
                            format_sample_rate(marker.sample_rate_hz)
                        ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("×")
                            .on_hover_selectable_text("Remove marker")
                            .clicked()
                        {
                            remove = Some(index);
                        }
                    });
                });
            }
            if let Some(index) = remove {
                settings.markers.remove(index);
            }
        });
}

pub(crate) fn paint_logic_marker_color_picker(
    ui: &mut egui::Ui,
    index: usize,
    color: &mut Color32,
) {
    let popup_id = ui.make_persistent_id(("logic_marker_color_popup", index));
    let button = ui
        .add(
            egui::Button::new(RichText::new("◆").color(marker_text_color(*color)))
                .fill(*color)
                .min_size(Vec2::splat(18.0)),
        )
        .on_hover_selectable_text("Change marker color");
    if button.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(popup_id));
    }
    egui::popup::popup_below_widget(
        ui,
        popup_id,
        &button,
        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
        |popup_ui| {
            popup_ui.set_min_width(150.0);
            popup_ui.set_max_width(150.0);
            popup_ui.label(RichText::new("Marker color").strong());
            egui::Grid::new(("logic_marker_color_palette", index))
                .num_columns(5)
                .spacing([4.0, 4.0])
                .show(popup_ui, |ui| {
                    for palette_index in 0..LOGIC_MARKER_LIMIT {
                        let palette_color = logic_marker_color(palette_index);
                        let selected = *color == palette_color;
                        let response = ui
                            .add(
                                egui::Button::new("")
                                    .fill(palette_color)
                                    .stroke(Stroke::new(
                                        if selected { 2.0_f32 } else { 1.0_f32 },
                                        if selected { Color32::WHITE } else { GRID },
                                    ))
                                    .min_size(Vec2::splat(20.0)),
                            )
                            .on_hover_selectable_text(format!("Color {}", palette_index + 1));
                        if response.clicked() {
                            *color = palette_color;
                            ui.memory_mut(egui::Memory::close_popup);
                        }
                        if palette_index % 5 == 4 {
                            ui.end_row();
                        }
                    }
                });
            popup_ui.separator();
            let [mut red, mut green, mut blue, _] = color.to_array();
            let mut changed = false;
            egui::Grid::new(("logic_marker_color_rgb", index))
                .num_columns(2)
                .spacing([6.0, 3.0])
                .show(popup_ui, |ui| {
                    for (label, component) in [("R", &mut red), ("G", &mut green), ("B", &mut blue)]
                    {
                        ui.label(label);
                        changed |= ui
                            .add(egui::DragValue::new(component).range(0..=255).speed(1))
                            .changed();
                        ui.end_row();
                    }
                });
            if changed {
                *color = Color32::from_rgb(red, green, blue);
            }
        },
    );
}
