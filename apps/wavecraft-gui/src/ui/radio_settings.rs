//! Radio receiver settings panels: capture parameters, gains, and the analyzer
//! (demodulation, packet, and whitening) configuration.

use crate::{
    egui, nearest_domain_value, normalize_gain_value, paint_capture_settings_save_button,
    selected_radio, Color32, DiscoveredRadio, FontId, Pos2, RadioAnalyzerChannelSettings,
    RadioAnalyzerSettings, RadioDataMode, RadioDemodulation, RadioProvider, RadioProviderFailure,
    RadioSettings, RadioWhiteningPreset, RangeInclusive, RichText, SavedCaptureSettingsKind,
    SelectableHoverText, ValueDomain, WhiteningDirection, BACKGROUND, MAX_RADIO_ANALYZER_CHANNELS,
    RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS, RAW_AUTOMATIC_PREAMBLE_BITS, TEXT_MUTED,
};

pub(crate) fn selected_radio_text(radios: &[DiscoveredRadio], selected_id: Option<&str>) -> String {
    match selected_id {
        Some(id) => selected_radio(radios, Some(id))
            .map(|radio| radio.descriptor.name.clone())
            .unwrap_or_else(|| "Unavailable radio receiver".to_owned()),
        None => "No radio receiver available".to_owned(),
    }
}

pub(crate) fn displayed_radio_id(radio: &DiscoveredRadio) -> String {
    let id = radio.descriptor.id.as_str();
    if radio.provider != RadioProvider::HackRf {
        return id.to_owned();
    }
    let Some(serial) = id.strip_prefix("hackrf:") else {
        return id.to_owned();
    };
    let serial = serial.trim_start_matches('0');
    format!("hackrf:{}", if serial.is_empty() { "0" } else { serial })
}

pub(crate) fn format_radio_value(value_hz: u64) -> String {
    if value_hz >= 1_000_000_000 && value_hz % 1_000_000_000 == 0 {
        format!("{} GHz", value_hz / 1_000_000_000)
    } else if value_hz >= 1_000_000 && value_hz % 1_000_000 == 0 {
        format!("{} MHz", value_hz / 1_000_000)
    } else if value_hz >= 1_000 && value_hz % 1_000 == 0 {
        format!("{} kHz", value_hz / 1_000)
    } else {
        format!("{value_hz} Hz")
    }
}

pub(crate) fn radio_hz_from_mhz(domain: &ValueDomain, value_mhz: f64) -> u64 {
    if !value_mhz.is_finite() {
        return nearest_domain_value(domain, 0);
    }
    let requested_hz = (value_mhz * 1_000_000.0)
        .round()
        .clamp(0.0, u64::MAX as f64) as u64;
    nearest_domain_value(domain, requested_hz)
}

pub(crate) fn format_radio_mhz_number(value_hz: u64) -> String {
    let whole = value_hz / 1_000_000;
    let remainder = value_hz % 1_000_000;
    if remainder == 0 {
        return whole.to_string();
    }
    let fractional = format!("{remainder:06}");
    format!("{whole}.{}", fractional.trim_end_matches('0'))
}

pub(crate) const RADIO_VALUE_WIDTH: f32 = 80.0;

pub(crate) const RADIO_GAIN_HANDLE_ASPECT_RATIO: f32 = 1.8;

pub(crate) fn add_left_aligned_sized(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    widget: impl egui::Widget,
) -> egui::Response {
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::left_to_right(egui::Align::Center)
            .with_main_align(egui::Align::Min)
            .with_main_justify(true),
        |ui| ui.add(widget),
    )
    .inner
}

pub(crate) fn paint_radio_parameter_label(ui: &mut egui::Ui, label: &str, help: Option<&str>) {
    let response = ui.add(egui::Label::new(label).selectable(true));
    if let Some(help) = help {
        response.on_hover_selectable_text(help);
    }
}

pub(crate) fn paint_radio_gain_slider(
    ui: &mut egui::Ui,
    value: &mut i32,
    range: std::ops::RangeInclusive<i32>,
    step: u32,
) -> egui::Response {
    let minimum = *range.start();
    let maximum = *range.end();
    ui.spacing_mut().slider_width = RADIO_VALUE_WIDTH;
    let response = ui.add(
        egui::Slider::new(value, range)
            .step_by(f64::from(step.max(1)))
            .show_value(false)
            .handle_shape(egui::style::HandleShape::Rect {
                aspect_ratio: RADIO_GAIN_HANDLE_ASPECT_RATIO,
            }),
    );
    let fraction = if maximum > minimum {
        (*value - minimum) as f32 / (maximum - minimum) as f32
    } else {
        0.0
    }
    .clamp(0.0, 1.0);
    let handle_radius = response.rect.height() / 2.5;
    let handle_half_width = handle_radius * RADIO_GAIN_HANDLE_ASPECT_RATIO;
    let handle_x = egui::lerp(
        response.rect.left() + handle_half_width..=response.rect.right() - handle_half_width,
        fraction,
    );
    ui.painter().text(
        Pos2::new(handle_x, response.rect.center().y),
        egui::Align2::CENTER_CENTER,
        value.to_string(),
        FontId::proportional(9.0),
        ui.visuals().strong_text_color(),
    );
    response
}

pub(crate) fn paint_radio_mhz_value(ui: &mut egui::Ui, value_hz: &mut u64, domain: &ValueDomain) {
    let bounds = match domain {
        ValueDomain::Discrete(values) => values
            .iter()
            .copied()
            .min()
            .zip(values.iter().copied().max()),
        ValueDomain::Stepped { range, .. } => Some((*range.start(), *range.end())),
    };
    let mut value_mhz = *value_hz as f64 / 1_000_000.0;
    let mut editor = egui::DragValue::new(&mut value_mhz)
        .speed(0.001)
        .min_decimals(3)
        .max_decimals(6);
    if let Some((minimum_hz, maximum_hz)) = bounds {
        editor = editor.range(minimum_hz as f64 / 1_000_000.0..=maximum_hz as f64 / 1_000_000.0);
    }
    let editor_size = egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y);
    let changed = ui
        .horizontal(|ui| {
            let changed = add_left_aligned_sized(ui, editor_size, editor).changed();
            ui.label("MHz");
            changed
        })
        .inner;
    if changed {
        *value_hz = radio_hz_from_mhz(domain, value_mhz);
    }
}

pub(crate) fn paint_radio_bandwidth_value(
    ui: &mut egui::Ui,
    value: &mut u64,
    domain: &ValueDomain,
) {
    let editor_size = egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y);
    ui.horizontal(|ui| {
        match domain {
            ValueDomain::Discrete(values) => {
                egui::ComboBox::from_id_salt("radio_bandwidth")
                    .width(RADIO_VALUE_WIDTH)
                    .selected_text(format_radio_mhz_number(*value))
                    .show_ui(ui, |ui| {
                        for choice in values {
                            ui.selectable_value(value, *choice, format_radio_mhz_number(*choice));
                        }
                    });
            }
            ValueDomain::Stepped { range, step } => {
                let mut value_mhz = *value as f64 / 1_000_000.0;
                if add_left_aligned_sized(
                    ui,
                    editor_size,
                    egui::DragValue::new(&mut value_mhz)
                        .range(
                            *range.start() as f64 / 1_000_000.0..=*range.end() as f64 / 1_000_000.0,
                        )
                        .speed(*step as f64 / 1_000_000.0)
                        .min_decimals(3)
                        .max_decimals(6),
                )
                .changed()
                {
                    *value = radio_hz_from_mhz(domain, value_mhz);
                }
            }
        }
        ui.label("MHz");
    });
    *value = nearest_domain_value(domain, *value);
}

pub(crate) fn paint_radio_capture_settings(
    ui: &mut egui::Ui,
    radio: &DiscoveredRadio,
    settings: &mut RadioSettings,
    enabled: bool,
    runtime_enabled: bool,
    trigger_runtime_enabled: bool,
    show_save_button: bool,
) -> bool {
    ui.add_space(10.0);
    let card_width = ui.available_width();
    egui::Frame::none()
        .fill(BACKGROUND)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            let content_width = (card_width - 20.0).max(0.0);
            ui.set_width(content_width);
            ui.set_max_width(content_width);
            egui::CollapsingHeader::new(RichText::new("Receive configuration").strong())
                .id_salt("radio_receive_configuration_section")
                .default_open(true)
                .show(ui, |ui| {
                    paint_radio_receive_configuration(
                        ui,
                        radio,
                        settings,
                        enabled,
                        runtime_enabled,
                        trigger_runtime_enabled,
                    );
                });
            ui.separator();
            egui::CollapsingHeader::new(RichText::new("Signal analyzer").strong())
                .id_salt("radio_signal_analyzer_section")
                .default_open(false)
                .show(ui, |ui| {
                    paint_radio_analyzer_configuration(
                        ui,
                        &mut settings.analyzer,
                        settings.capture.sample_rate_hz,
                        enabled,
                    );
                });
            if settings.analyzer.is_enabled() {
                ui.separator();
                egui::CollapsingHeader::new(RichText::new("Analyzer channels").strong())
                    .id_salt("radio_analyzer_channels_section")
                    .default_open(true)
                    .show(ui, |ui| {
                        paint_radio_analyzer_channels(
                            ui,
                            &mut settings.analyzer,
                            settings.capture.sample_rate_hz,
                            enabled,
                        );
                    });
            }
            show_save_button
                && paint_capture_settings_save_button(
                    ui,
                    SavedCaptureSettingsKind::RadioFrequency,
                    enabled,
                )
        })
        .inner
}

pub(crate) fn paint_scaled_u64_value(
    ui: &mut egui::Ui,
    value: &mut u64,
    scale: f64,
    range: RangeInclusive<f64>,
    unit: &str,
) {
    let mut scaled = *value as f64 / scale;
    let changed = ui
        .horizontal(|ui| {
            let changed = add_left_aligned_sized(
                ui,
                egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                egui::DragValue::new(&mut scaled)
                    .range(range)
                    .clamp_existing_to_range(false)
                    .speed(0.001)
                    .min_decimals(3)
                    .max_decimals(6),
            )
            .changed();
            ui.label(unit);
            changed
        })
        .inner;
    if changed {
        *value = (scaled * scale).round().max(0.0) as u64;
    }
}

pub(crate) fn paint_scaled_i64_value(
    ui: &mut egui::Ui,
    value: &mut i64,
    scale: f64,
    range: RangeInclusive<f64>,
    unit: &str,
) {
    let mut scaled = *value as f64 / scale;
    let changed = ui
        .horizontal(|ui| {
            let changed = add_left_aligned_sized(
                ui,
                egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                egui::DragValue::new(&mut scaled)
                    .range(range)
                    .clamp_existing_to_range(false)
                    .speed(0.001)
                    .min_decimals(3)
                    .max_decimals(6),
            )
            .changed();
            ui.label(unit);
            changed
        })
        .inner;
    if changed {
        *value = (scaled * scale).round() as i64;
    }
}

pub(crate) fn paint_radio_analyzer_configuration(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    capture_sample_rate_hz: u64,
    enabled: bool,
) {
    ui.set_max_width(ui.available_width());
    egui::Grid::new("radio_signal_analyzer_configuration")
        .num_columns(2)
        .max_col_width(126.0)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            paint_radio_parameter_label(
                ui,
                "Modulation",
                Some("None disables signal analysis; each modulation shows only its applicable parameters"),
            );
            ui.add_enabled_ui(enabled, |ui| {
                egui::ComboBox::from_id_salt("radio_analyzer_modulation")
                    .width(RADIO_VALUE_WIDTH)
                    .selected_text(match settings.demodulation {
                        RadioDemodulation::None => "None",
                        RadioDemodulation::BinaryFsk => "2-FSK",
                        RadioDemodulation::BinaryGfsk => "GFSK",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut settings.demodulation,
                            RadioDemodulation::None,
                            "None",
                        );
                        ui.selectable_value(
                            &mut settings.demodulation,
                            RadioDemodulation::BinaryFsk,
                            "2-FSK",
                        );
                        ui.selectable_value(
                            &mut settings.demodulation,
                            RadioDemodulation::BinaryGfsk,
                            "GFSK",
                        );
                    });
            });
            ui.end_row();
        });

    if !settings.is_enabled() {
        return;
    }

    let controls_enabled = enabled;
    egui::Grid::new("radio_signal_analyzer_parameters")
        .num_columns(2)
        .max_col_width(126.0)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {

            paint_radio_parameter_label(ui, "Polarity", Some("Swaps mark and space decisions"));
            ui.add_enabled_ui(controls_enabled, |ui| {
                egui::ComboBox::from_id_salt("radio_analyzer_polarity")
                    .width(RADIO_VALUE_WIDTH)
                    .selected_text(if settings.inverted {
                        "Inverted"
                    } else {
                        "Normal"
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.inverted, false, "Normal");
                        ui.selectable_value(&mut settings.inverted, true, "Inverted");
                    });
            });
            ui.end_row();

            let capture_rate_mhz = capture_sample_rate_hz as f64 / 1_000_000.0;
            paint_radio_parameter_label(
                ui,
                "Channel bandwidth",
                Some("Full complex bandwidth retained around the selected channel"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                paint_scaled_u64_value(
                    ui,
                    &mut settings.channel_bandwidth_hz,
                    1_000_000.0,
                    0.000_001..=capture_rate_mhz.max(0.000_001),
                    "MHz",
                );
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Processing rate",
                Some("IQ sample rate after channel filtering and bounded downsampling"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                paint_scaled_u64_value(
                    ui,
                    &mut settings.output_sample_rate_hz,
                    1_000_000.0,
                    0.000_001..=capture_rate_mhz.max(0.000_001),
                    "MHz",
                );
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Channel filter taps",
                Some("Odd FIR length from 3 to 4095; more taps improve rejection but cost CPU"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                add_left_aligned_sized(
                    ui,
                    egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                    egui::DragValue::new(&mut settings.filter_tap_count)
                        .range(3..=4_095)
                        .speed(2),
                );
            });
            settings.filter_tap_count = settings.filter_tap_count.clamp(3, 4_095) | 1;
            ui.end_row();

            let processing_rate_khz = settings.output_sample_rate_hz as f64 / 1_000.0;
            paint_radio_parameter_label(
                ui,
                "Bit rate",
                Some("Raw bit rate for binary FSK/GFSK; one symbol carries one bit"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                paint_scaled_u64_value(
                    ui,
                    &mut settings.symbol_rate_hz,
                    1_000.0,
                    0.001..=(processing_rate_khz / 2.0).max(0.001),
                    "kbit/s",
                );
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Frequency deviation",
                Some("Expected mark or space displacement from the channel center"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                paint_scaled_u64_value(
                    ui,
                    &mut settings.deviation_hz,
                    1_000.0,
                    0.001..=(processing_rate_khz / 2.0).max(0.001),
                    "kHz",
                );
            });
            ui.end_row();

            if settings.demodulation == RadioDemodulation::BinaryGfsk {
                paint_radio_parameter_label(
                    ui,
                    "Gaussian BT",
                    Some("Expected transmitter Gaussian bandwidth-time product"),
                );
                ui.add_enabled_ui(controls_enabled, |ui| {
                    let mut bt = f64::from(settings.gfsk_bt_milli) / 1_000.0;
                    if add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut bt)
                            .range(0.01..=2.0)
                            .speed(0.01)
                            .min_decimals(2)
                            .max_decimals(3),
                    )
                    .changed()
                    {
                        settings.gfsk_bt_milli = (bt * 1_000.0).round() as u32;
                    }
                });
                ui.end_row();
            }

            paint_radio_parameter_label(
                ui,
                "Preamble value",
                Some("One hexadecimal byte repeated at the start of the FSK data"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                ui.horizontal(|ui| {
                    add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut settings.preamble_byte)
                            .range(0..=u8::MAX)
                            .speed(1)
                            .hexadecimal(2, false, true),
                    );
                    ui.label("hex");
                });
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Preamble length",
                Some("Minimum consecutive preamble bytes required; in raw mode zero starts at the first recovered bit and keeps the preamble as data, while sync or three repeated preamble bytes refine later packet boundaries"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                ui.horizontal(|ui| {
                    add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut settings.preamble_bytes)
                            .range(0..=256)
                            .speed(1),
                    );
                    ui.label("bytes");
                });
            });
            settings.preamble_bytes = settings.preamble_bytes.min(256);
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Sync word",
                Some(if settings.data_mode == RadioDataMode::Packets {
                    "Required 1-to-8-byte unwhitened word that establishes the packet and whitening boundary"
                } else {
                    "Optional 1-to-8-byte word after the repeated preamble; unlike 0xAA, it establishes an unambiguous byte boundary"
                }),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                ui.horizontal(|ui| {
                    add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut settings.sync_word)
                            .range(0..=u64::MAX)
                            .speed(1)
                            .hexadecimal(2, false, true),
                    );
                    ui.label("hex");
                });
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Sync word length",
                Some(if settings.data_mode == RadioDataMode::Packets {
                    "Required in packet mode; bytes are taken big-endian from the value above"
                } else {
                    "Zero disables the separate sync word; bytes are taken big-endian from the value above"
                }),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                ui.horizontal(|ui| {
                    add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut settings.sync_word_bytes)
                            .range(if settings.data_mode == RadioDataMode::Packets {
                                1..=8
                            } else {
                                0..=8
                            })
                            .speed(1),
                    );
                    ui.label("bytes");
                });
            });
            settings.sync_word_bytes = settings.sync_word_bytes.min(8);
            if settings.data_mode == RadioDataMode::Packets {
                settings.sync_word_bytes = settings.sync_word_bytes.max(1);
            }
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Adaptive timing",
                Some("GNU Radio-style Mueller-and-Muller symbol synchronization after quadrature demodulation"),
            );
            ui.add_enabled(
                controls_enabled,
                egui::Checkbox::without_text(&mut settings.adaptive_timing),
            );
            ui.end_row();

            if settings.adaptive_timing {
                paint_radio_parameter_label(
                    ui,
                    "Timing loop bandwidth",
                    Some("Higher values track faster but admit more timing jitter"),
                );
                ui.add_enabled_ui(controls_enabled, |ui| {
                    let mut bandwidth =
                        f64::from(settings.timing_loop_bandwidth_milli) / 1_000.0;
                    if add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut bandwidth)
                            .range(0.001..=1.0)
                            .speed(0.001)
                            .min_decimals(3)
                            .max_decimals(3),
                    )
                    .changed()
                    {
                        settings.timing_loop_bandwidth_milli =
                            (bandwidth * 1_000.0).round() as u32;
                    }
                });
                ui.end_row();

                paint_radio_parameter_label(
                    ui,
                    "Clock error limit",
                    Some("Maximum symbol-clock correction allowed by the timing loop"),
                );
                ui.add_enabled_ui(controls_enabled, |ui| {
                    ui.horizontal(|ui| {
                        add_left_aligned_sized(
                            ui,
                            egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                            egui::DragValue::new(&mut settings.max_clock_error_ppm)
                                .range(0..=250_000)
                                .speed(100),
                        );
                        ui.label("ppm");
                    });
                });
                ui.end_row();
            }

            paint_radio_parameter_label(
                ui,
                if settings.data_mode == RadioDataMode::Packets {
                    "Sync bit errors"
                } else {
                    "Preamble bit errors"
                },
                Some("Maximum hard-decision mismatches accepted in the sync word, or in the preamble when sync is disabled"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                ui.horizontal(|ui| {
                    add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut settings.maximum_preamble_bit_errors)
                            .range(0..=64)
                            .speed(1),
                    );
                    ui.label("bits");
                });
            });
            let acquisition_bits = if settings.sync_word_bytes > 0 {
                usize::from(settings.sync_word_bytes).saturating_mul(8)
            } else if settings.preamble_bytes == 0 {
                RAW_AUTOMATIC_PREAMBLE_BITS
            } else {
                settings.preamble_bytes.saturating_mul(8)
            };
            settings.maximum_preamble_bit_errors = settings
                .maximum_preamble_bit_errors
                .min(acquisition_bits.saturating_sub(1));
            ui.end_row();

            if settings.data_mode == RadioDataMode::Raw {
                paint_radio_parameter_label(
                    ui,
                    "Minimum bit confidence",
                    Some("Bits below this 0-to-1 demodulator confidence count as uncertain; zero disables confidence filtering"),
                );
                ui.add_enabled_ui(controls_enabled, |ui| {
                    let mut confidence =
                        f64::from(settings.minimum_bit_confidence_milli) / 1_000.0;
                    if add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut confidence)
                            .range(0.0..=1.0)
                            .speed(0.01)
                            .min_decimals(2)
                            .max_decimals(3),
                    )
                    .changed()
                    {
                        settings.minimum_bit_confidence_milli =
                            (confidence * 1_000.0).round() as u16;
                    }
                });
                ui.end_row();

                paint_radio_parameter_label(
                    ui,
                    "Maximum uncertain bits",
                    Some("Rejects a completed RAW message when more bits fall below the confidence threshold"),
                );
                ui.add_enabled_ui(controls_enabled, |ui| {
                    ui.horizontal(|ui| {
                        add_left_aligned_sized(
                            ui,
                            egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                            egui::DragValue::new(&mut settings.maximum_uncertain_bits)
                                .range(0..=1_000_000)
                                .speed(1),
                        );
                        ui.label("bits");
                    });
                });
                ui.end_row();
            }

            paint_radio_parameter_label(
                ui,
                "Data decoding",
                Some("RAW discovers byte alignment from a repeated preamble; Fixed requires the configured preamble and sync, then dewhitens post-sync data"),
            );
            ui.add_enabled_ui(controls_enabled, |ui| {
                egui::ComboBox::from_id_salt("radio_analyzer_data_mode")
                    .width(RADIO_VALUE_WIDTH)
                    .selected_text(match settings.data_mode {
                        RadioDataMode::Raw => "RAW",
                        RadioDataMode::Packets => "Fixed",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.data_mode, RadioDataMode::Raw, "RAW");
                        ui.selectable_value(
                            &mut settings.data_mode,
                            RadioDataMode::Packets,
                            "Fixed",
                        );
                    });
            });
            ui.end_row();

            if settings.data_mode == RadioDataMode::Packets {
                paint_radio_parameter_label(
                    ui,
                    "Data whitening",
                    Some("XOR packet bits after sync with a reset additive LFSR sequence"),
                );
                ui.add_enabled(
                    controls_enabled,
                    egui::Checkbox::without_text(&mut settings.whitening_enabled),
                );
                ui.end_row();

                if settings.whitening_enabled {
                    let whitening_enabled = controls_enabled && settings.whitening_enabled;
                    paint_radio_parameter_label(
                        ui,
                        "Whitening preset",
                        Some("Selects a predefined whitening sequence; Custom exposes all LFSR parameters"),
                    );
                    ui.add_enabled_ui(whitening_enabled, |ui| {
                        let combo = egui::ComboBox::from_id_salt("radio_whitening_preset")
                            .width(RADIO_VALUE_WIDTH)
                            .selected_text(settings.whitening_preset.label())
                            .show_ui(ui, |ui| {
                                for preset in [
                                    RadioWhiteningPreset::Custom,
                                    RadioWhiteningPreset::Pn9X9X5,
                                ] {
                                    let response = ui.selectable_value(
                                        &mut settings.whitening_preset,
                                        preset,
                                        preset.label(),
                                    );
                                    if let Some(description) = preset.description() {
                                        response.on_hover_selectable_text(description);
                                    }
                                }
                            });
                        if let Some(description) = settings.whitening_preset.description() {
                            combo.response.on_hover_selectable_text(description);
                        }
                    });
                    ui.end_row();

                    if settings.whitening_preset == RadioWhiteningPreset::Custom {
                        paint_radio_parameter_label(
                            ui,
                            "LFSR width",
                            Some("Whitening register width"),
                        );
                        ui.add_enabled_ui(whitening_enabled, |ui| {
                            ui.horizontal(|ui| {
                                add_left_aligned_sized(
                                    ui,
                                    egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                                    egui::DragValue::new(&mut settings.whitening_width)
                                        .range(1..=64)
                                        .speed(1),
                                );
                                ui.label("bits");
                            });
                        });
                        ui.end_row();

                        for (label, help, value) in [
                        (
                            "LFSR tap mask",
                            "Tap mask over the current LFSR state; tapped-bit parity becomes feedback",
                            &mut settings.whitening_polynomial,
                        ),
                        (
                            "LFSR seed",
                            "Nonzero register state loaded immediately after sync",
                            &mut settings.whitening_seed,
                        ),
                        ] {
                            paint_radio_parameter_label(ui, label, Some(help));
                            ui.add_enabled_ui(whitening_enabled, |ui| {
                                ui.horizontal(|ui| {
                                    add_left_aligned_sized(
                                        ui,
                                        egui::vec2(
                                            RADIO_VALUE_WIDTH,
                                            ui.spacing().interact_size.y,
                                        ),
                                        egui::DragValue::new(value)
                                            .range(0..=u64::MAX)
                                            .speed(1)
                                            .hexadecimal(2, false, true),
                                    );
                                    ui.label("hex");
                                });
                            });
                            ui.end_row();
                        }

                    paint_radio_parameter_label(
                        ui,
                        "LFSR direction",
                        Some("Selects the emitted end and shift direction of the register"),
                    );
                    ui.add_enabled_ui(whitening_enabled, |ui| {
                        egui::ComboBox::from_id_salt("radio_whitening_direction")
                            .width(RADIO_VALUE_WIDTH)
                            .selected_text(match settings.whitening_direction {
                                WhiteningDirection::ShiftRight => "Right · emit LSB",
                                WhiteningDirection::ShiftLeft => "Left · emit MSB",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut settings.whitening_direction,
                                    WhiteningDirection::ShiftRight,
                                    "Right · emit LSB",
                                );
                                ui.selectable_value(
                                    &mut settings.whitening_direction,
                                    WhiteningDirection::ShiftLeft,
                                    "Left · emit MSB",
                                );
                            });
                    });
                    ui.end_row();

                    paint_radio_parameter_label(
                        ui,
                        "Invert PN output",
                        Some("Inverts every generated whitening bit before XOR"),
                    );
                    ui.add_enabled(
                        whitening_enabled,
                        egui::Checkbox::without_text(&mut settings.whitening_invert_output),
                    );
                    ui.end_row();

                    paint_radio_parameter_label(
                        ui,
                        "Whitening start",
                        Some("First covered packet bit relative to the end of sync"),
                    );
                    ui.add_enabled_ui(whitening_enabled, |ui| {
                        ui.horizontal(|ui| {
                            add_left_aligned_sized(
                                ui,
                                egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                                egui::DragValue::new(&mut settings.whitening_start_bit)
                                    .speed(1),
                            );
                            ui.label("bit");
                        });
                    });
                    ui.end_row();

                    paint_radio_parameter_label(
                        ui,
                        "Whitening end",
                        Some("Optional exclusive data-bit end; disabled covers all following data"),
                    );
                    ui.add_enabled_ui(whitening_enabled, |ui| {
                        ui.horizontal(|ui| {
                            let mut bounded = settings.whitening_end_bit.is_some();
                            if ui.checkbox(&mut bounded, "Bounded").changed() {
                                settings.whitening_end_bit = bounded.then_some(
                                    settings.whitening_start_bit.saturating_add(1),
                                );
                            }
                            if let Some(end) = &mut settings.whitening_end_bit {
                                ui.add(
                                    egui::DragValue::new(end)
                                        .range(
                                            settings.whitening_start_bit.saturating_add(1)
                                                ..=usize::MAX,
                                        )
                                        .speed(1),
                                );
                                ui.label("bit");
                            }
                        });
                    });
                    ui.end_row();

                    paint_radio_parameter_label(
                        ui,
                        "Advance outside range",
                        Some("Advances the PN sequence over clear packet bits outside the covered range"),
                    );
                    ui.add_enabled(
                        whitening_enabled,
                        egui::Checkbox::without_text(
                            &mut settings.whitening_advance_excluded_bits,
                        ),
                    );
                    ui.end_row();
                }
            }
            }

        });

    if settings.is_enabled() {
        if let Err(error) = settings.signal_profiles() {
            ui.add(
                egui::Label::new(RichText::new(error).small().color(Color32::LIGHT_RED))
                    .selectable(true),
            );
        } else if let Err(error) = settings.decoder_config() {
            ui.add(
                egui::Label::new(RichText::new(error).small().color(Color32::LIGHT_RED))
                    .selectable(true),
            );
        }
    }
}

pub(crate) fn paint_radio_analyzer_channels(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    capture_sample_rate_hz: u64,
    enabled: bool,
) {
    let controls_enabled = enabled && settings.is_enabled();
    let half_span_hz = i64::try_from(capture_sample_rate_hz / 2).unwrap_or(i64::MAX);
    let half_span_mhz = capture_sample_rate_hz as f64 / 2_000_000.0;
    let can_remove = settings.channels.len() > 1;
    let mut remove_index = None;
    egui::Grid::new("radio_analyzer_channels")
        .num_columns(3)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            for (index, channel) in settings.channels.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(format!("{}.", index + 1));
                    ui.add_enabled_ui(controls_enabled, |ui| {
                        ui.add_sized(
                            [110.0 * 2.0 / 3.0, ui.spacing().interact_size.y],
                            egui::TextEdit::singleline(&mut channel.name)
                                .hint_text(format!("Channel {}", index + 1)),
                        );
                    });
                });
                ui.add_enabled_ui(controls_enabled, |ui| {
                    paint_scaled_i64_value(
                        ui,
                        &mut channel.frequency_offset_hz,
                        1_000_000.0,
                        -half_span_mhz..=half_span_mhz,
                        "MHz",
                    );
                });
                if ui
                    .add_enabled(
                        controls_enabled && can_remove,
                        egui::Button::new("×").small(),
                    )
                    .on_hover_selectable_text("Remove channel")
                    .clicked()
                {
                    remove_index = Some(index);
                }
                ui.end_row();
            }
        });
    if let Some(index) = remove_index {
        settings.channels.remove(index);
    }
    if ui
        .add_enabled(
            controls_enabled && settings.channels.len() < MAX_RADIO_ANALYZER_CHANNELS,
            egui::Button::new(format!(
                "+ Add channel ({}/{MAX_RADIO_ANALYZER_CHANNELS})",
                settings.channels.len()
            )),
        )
        .clicked()
    {
        let step_hz = i64::try_from(settings.channel_bandwidth_hz.max(1)).unwrap_or(i64::MAX);
        let preferred = (0..MAX_RADIO_ANALYZER_CHANNELS)
            .flat_map(|index| {
                let distance = i64::try_from(index)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(step_hz);
                [distance, distance.saturating_neg()]
            })
            .find(|candidate| {
                candidate.abs() <= half_span_hz
                    && !settings
                        .channels
                        .iter()
                        .any(|channel| channel.frequency_offset_hz == *candidate)
            });
        let candidate = preferred
            .or_else(|| {
                (0..MAX_RADIO_ANALYZER_CHANNELS)
                    .flat_map(|index| {
                        let distance = i64::try_from(index).unwrap_or(i64::MAX);
                        [distance, distance.saturating_neg()]
                    })
                    .find(|candidate| {
                        candidate.abs() <= half_span_hz
                            && !settings
                                .channels
                                .iter()
                                .any(|channel| channel.frequency_offset_hz == *candidate)
                    })
            })
            .unwrap_or(0);
        let name_index = (0..MAX_RADIO_ANALYZER_CHANNELS)
            .find(|index| {
                let name = format!("Channel {}", index + 1);
                !settings.channels.iter().any(|channel| channel.name == name)
            })
            .unwrap_or(settings.channels.len());
        settings
            .channels
            .push(RadioAnalyzerChannelSettings::numbered(
                name_index, candidate,
            ));
    }
    ui.label(
        RichText::new(
            "All modulation, framing, and whitening settings above are shared by every channel.",
        )
        .small()
        .color(TEXT_MUTED),
    );
}

pub(crate) fn paint_radio_receive_configuration(
    ui: &mut egui::Ui,
    radio: &DiscoveredRadio,
    settings: &mut RadioSettings,
    enabled: bool,
    runtime_enabled: bool,
    trigger_runtime_enabled: bool,
) {
    ui.set_max_width(ui.available_width());
    let tuning_enabled = enabled || runtime_enabled;
    let center_frequency_help = (radio.provider == RadioProvider::HackRf).then_some(
        "Tunes the frequency at the center of the RF spectrum and waterfall. HackRF One range: 1 MHz–6 GHz. This can be changed while capture is running.",
    );
    let sample_rate_help = (radio.provider == RadioProvider::HackRf).then_some(
        "Sets the I/Q sampling rate and the captured RF frequency span. Supported range: 2–20 MHz. Step: 1 Hz (0.000001 MHz). HackRF supports any whole-Hz value in this range; 8 MHz or higher is recommended.",
    );
    let bandwidth_help = (radio.provider == RadioProvider::HackRf).then_some(
        "Selects the HackRF analog baseband low-pass filter width. A narrower filter rejects more out-of-band noise and interference. Keep it at or below the sample rate to limit aliasing.",
    );
    egui::Grid::new("radio_receive_configuration")
        .num_columns(2)
        .max_col_width(126.0)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            paint_radio_parameter_label(ui, "Center frequency", center_frequency_help);
            ui.add_enabled_ui(tuning_enabled, |ui| {
                paint_radio_mhz_value(
                    ui,
                    &mut settings.capture.center_frequency_hz,
                    &radio.capabilities.frequency_hz,
                );
            });
            ui.end_row();

            if radio.provider == RadioProvider::HackRf {
                paint_radio_parameter_label(
                    ui,
                    "Frequency correction",
                    Some(
                        "Signed HackRF reference correction. Positive values compensate a fast clock. Changes apply to RF tuning and the sample clock during Run and reset the live DSP stream. Range: -200 to +200 ppm.",
                    ),
                );
                ui.add_enabled_ui(tuning_enabled, |ui| {
                    ui.horizontal(|ui| {
                        let mut correction_ppm =
                            f64::from(settings.frequency_correction_milli_ppm) / 1_000.0;
                        if add_left_aligned_sized(
                            ui,
                            egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                            egui::DragValue::new(&mut correction_ppm)
                                .range(-200.0..=200.0)
                                .speed(0.001)
                                .min_decimals(3)
                                .max_decimals(3),
                        )
                        .changed()
                        {
                            settings.frequency_correction_milli_ppm =
                                (correction_ppm * 1_000.0).round() as i32;
                        }
                        ui.label("ppm");
                    });
                });
                settings.frequency_correction_milli_ppm = settings
                    .frequency_correction_milli_ppm
                    .clamp(-200_000, 200_000);
                ui.end_row();
            }

            paint_radio_parameter_label(ui, "Sample rate", sample_rate_help);
            ui.add_enabled_ui(enabled, |ui| {
                paint_radio_mhz_value(
                    ui,
                    &mut settings.capture.sample_rate_hz,
                    &radio.capabilities.sample_rates_hz,
                );
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Single duration",
                Some(
                    "Capture time used by RF Single when Wait for trigger is disabled; a triggered Single uses Post-trigger instead",
                ),
            );
            ui.add_enabled_ui(enabled, |ui| {
                ui.horizontal(|ui| {
                    let mut duration_seconds = settings.single_duration_millis as f64 / 1_000.0;
                    if add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut duration_seconds)
                            .range(0.001..=3_600.0)
                            .speed(0.01)
                            .min_decimals(3)
                            .max_decimals(3),
                    )
                    .changed()
                    {
                        settings.single_duration_millis =
                            (duration_seconds * 1_000.0).round() as u64;
                    }
                    ui.label("s");
                });
            });
            settings.single_duration_millis =
                settings.single_duration_millis.clamp(1, 3_600_000);
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Wait for trigger",
                Some(
                    "In RF Single mode, wait until the spectrum peak reaches the trigger level, display the configured post-trigger interval, then freeze",
                ),
            );
            ui.add_enabled_ui(enabled, |ui| {
                ui.checkbox(&mut settings.trigger_enabled, "Enabled");
            });
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "Post-trigger",
                Some(
                    "Additional IQ display time after the triggering FFT window. Resolution: 0.001 ms. At 19.2 kbit/s, a 10-byte preamble alone lasts 4.167 ms, so allow enough time for the complete packet",
                ),
            );
            ui.add_enabled_ui(
                (enabled && settings.trigger_enabled) || trigger_runtime_enabled,
                |ui| {
                ui.horizontal(|ui| {
                    let mut milliseconds = settings.trigger_posttrigger_micros as f64 / 1_000.0;
                    if add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut milliseconds)
                            .range(
                                0.0..=RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS as f64 / 1_000.0,
                            )
                            .speed(0.001)
                            .min_decimals(3)
                            .max_decimals(3),
                    )
                    .changed()
                    {
                        settings.trigger_posttrigger_micros =
                            (milliseconds * 1_000.0).round() as u64;
                    }
                    ui.label("ms");
                });
                },
            );
            settings.trigger_posttrigger_micros = settings
                .trigger_posttrigger_micros
                .min(RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS);
            ui.end_row();

            paint_radio_parameter_label(
                ui,
                "RF trigger level",
                Some(
                    "Peak FFT-bin power threshold used by RF Single when Wait for trigger is enabled. After an FSK/GFSK preamble locks, the same level acts as squelch inside the configured analyzer channel and closes RAW data when the RF burst ends. Also represented by the draggable T marker",
                ),
            );
            ui.add_enabled_ui(enabled || runtime_enabled || trigger_runtime_enabled, |ui| {
                ui.horizontal(|ui| {
                    let mut level_dbfs = f64::from(settings.trigger_level_dbfs_tenths) / 10.0;
                    if add_left_aligned_sized(
                        ui,
                        egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut level_dbfs)
                            .range(-120.0..=0.0)
                            .speed(0.1)
                            .min_decimals(1)
                            .max_decimals(1),
                    )
                    .changed()
                    {
                        settings.trigger_level_dbfs_tenths = (level_dbfs * 10.0).round() as i16;
                    }
                    ui.label("dBFS");
                });
            });
            settings.trigger_level_dbfs_tenths =
                settings.trigger_level_dbfs_tenths.clamp(-1_200, 0);
            ui.end_row();

            paint_radio_parameter_label(ui, "Baseband bandwidth", bandwidth_help);
            ui.add_enabled_ui(tuning_enabled, |ui| {
                paint_radio_bandwidth_value(
                    ui,
                    &mut settings.capture.bandwidth_hz,
                    &radio.capabilities.bandwidths_hz,
                );
            });
            ui.end_row();

            if radio.capabilities.receive_channels > 1 {
                ui.label("Receive channel");
                ui.add_enabled_ui(enabled, |ui| {
                    egui::ComboBox::from_id_salt("radio_receive_channel")
                        .width(RADIO_VALUE_WIDTH)
                        .selected_text(format!("RX {}", settings.capture.receive_channel))
                        .show_ui(ui, |ui| {
                            for channel in 0..radio.capabilities.receive_channels {
                                ui.selectable_value(
                                    &mut settings.capture.receive_channel,
                                    channel,
                                    format!("RX {channel}"),
                                );
                            }
                        });
                });
                ui.end_row();
            }

            for control in &radio.capabilities.gain_controls {
                paint_radio_parameter_label(ui, &control.label, control.help.as_deref());
                let value = settings
                    .gains_db
                    .entry(control.id.clone())
                    .or_insert(control.default_db);
                let slider_response = ui
                    .add_enabled_ui(tuning_enabled, |ui| {
                        ui.horizontal(|ui| {
                            let response = ui
                                .scope(|ui| {
                                    paint_radio_gain_slider(
                                        ui,
                                        value,
                                        control.range_db.clone(),
                                        control.step_db,
                                    )
                                })
                                .inner;
                            ui.label("dB");
                            response
                        })
                        .inner
                    })
                    .inner;
                if let Some(help) = &control.help {
                    slider_response.on_hover_selectable_text(help);
                }
                *value = normalize_gain_value(
                    *value,
                    *control.range_db.start(),
                    *control.range_db.end(),
                    control.step_db,
                );
                ui.end_row();
            }

            if radio.provider == RadioProvider::HackRf {
                let iq_help = "Remove the measured I/Q DC offset before spectrum processing";
                paint_radio_parameter_label(ui, "IQ correction", Some(iq_help));
                ui.add_enabled(
                    enabled,
                    egui::Checkbox::without_text(&mut settings.iq_correction_enabled),
                )
                .on_hover_selectable_text(iq_help);
                ui.end_row();

                let amplifier_help = "Adds roughly 11 dB of HackRF front-end gain. Leave disabled around strong signals to reduce overload and distortion.";
                paint_radio_parameter_label(ui, "RF amplifier", Some(amplifier_help));
                ui.add_enabled(
                    tuning_enabled,
                    egui::Checkbox::without_text(&mut settings.amplifier_enabled),
                )
                .on_hover_selectable_text(amplifier_help);
                ui.end_row();

                let antenna_help =
                    "Only enable for an antenna or accessory designed for bias power";
                paint_radio_parameter_label(ui, "Antenna port power (3.3 V)", Some(antenna_help));
                ui.add_enabled(
                    enabled,
                    egui::Checkbox::without_text(&mut settings.antenna_power_enabled),
                )
                .on_hover_selectable_text(antenna_help);
                ui.end_row();
            }
        });
    if settings.capture.bandwidth_hz > settings.capture.sample_rate_hz {
        ui.label(
            RichText::new("Bandwidth above sample rate may cause aliasing")
                .small()
                .color(Color32::from_rgb(255, 190, 80)),
        );
    }
}

pub(crate) fn paint_radio_settings(
    ui: &mut egui::Ui,
    radios: &[DiscoveredRadio],
    selected_id: &mut Option<String>,
    failures: &[RadioProviderFailure],
    enabled: bool,
) -> bool {
    let changed = paint_radio_device_selector(ui, radios, selected_id, enabled);
    for radio in radios {
        ui.add_space(4.0);
        egui::Frame::none()
            .fill(BACKGROUND)
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(10.0))
            .show(ui, |ui| {
                ui.label(RichText::new(&radio.descriptor.name).strong());
                ui.small(RichText::new(displayed_radio_id(radio)).color(TEXT_MUTED));
                ui.small(
                    RichText::new(format!(
                        "{:?} · {:?} · {} RX channel(s)",
                        radio.provider,
                        radio.connection_backend,
                        radio.capabilities.receive_channels
                    ))
                    .color(TEXT_MUTED),
                );
            });
    }

    for failure in failures {
        ui.add_space(4.0);
        ui.small(
            RichText::new(format!(
                "{:?} discovery unavailable: {}",
                failure.provider, failure.error
            ))
            .color(Color32::LIGHT_RED),
        );
    }
    changed
}

pub(crate) fn paint_radio_device_selector(
    ui: &mut egui::Ui,
    radios: &[DiscoveredRadio],
    selected_id: &mut Option<String>,
    enabled: bool,
) -> bool {
    let before = selected_id.clone();
    let choices = radios
        .iter()
        .map(|radio| {
            let id = radio.descriptor.id.as_str().to_owned();
            let displayed_id = displayed_radio_id(radio);
            (id, format!("{}\n{displayed_id}", radio.descriptor.name))
        })
        .collect::<Vec<_>>();
    ui.add_enabled_ui(enabled && !choices.is_empty(), |ui| {
        egui::ComboBox::from_id_salt("radio_device_selector")
            .width(ui.available_width())
            .selected_text(selected_radio_text(radios, selected_id.as_deref()))
            .show_ui(ui, |ui| {
                for (id, text) in choices {
                    ui.selectable_value(selected_id, Some(id), text);
                }
            });
    });
    if let Some(id) = selected_id.as_deref() {
        let displayed_id = selected_radio(radios, Some(id))
            .map(displayed_radio_id)
            .unwrap_or_else(|| id.to_owned());
        ui.add(
            egui::Label::new(
                RichText::new(displayed_id)
                    .small()
                    .monospace()
                    .color(TEXT_MUTED),
            )
            .wrap(),
        );
    }
    before != *selected_id
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    #[test]
    fn analyzer_render_does_not_clamp_values_to_a_temporary_dependent_minimum() {
        let context = egui::Context::default();
        let mut settings = RadioAnalyzerSettings {
            demodulation: RadioDemodulation::BinaryGfsk,
            output_sample_rate_hz: 0,
            symbol_rate_hz: 19_200,
            deviation_hz: 35_000,
            preamble_bytes: 0,
            sync_word: 0xd391,
            sync_word_bytes: 2,
            ..RadioAnalyzerSettings::default()
        };

        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_radio_analyzer_configuration(ui, &mut settings, 0, true);
            });
        });

        assert_eq!(settings.symbol_rate_hz, 19_200);
        assert_eq!(settings.deviation_hz, 35_000);
        assert_eq!(settings.preamble_bytes, 0);
    }

    #[test]
    fn displayed_hackrf_id_omits_serial_leading_zeroes() {
        let radio = DiscoveredRadio {
            descriptor: DeviceDescriptor {
                id: DeviceId::new("hackrf:0000000000001234"),
                name: "HackRF One".to_owned(),
                kind: InstrumentKind::RadioReceiver,
            },
            provider: RadioProvider::HackRf,
            model: wavecraft_sdr::RadioModel::HackRfOne,
            connection_backend: wavecraft_sdr::ConnectionBackend::Usb,
            persistent_id: Some("0000000000001234".to_owned()),
            capabilities: wavecraft_hackrf::hackrf_one_capabilities(),
        };

        assert_eq!(displayed_radio_id(&radio), "hackrf:1234");
    }

    #[test]
    fn radio_values_convert_fractional_mhz_to_hz() {
        let domain = ValueDomain::Stepped {
            range: 1_000_000..=6_000_000_000,
            step: 1,
        };

        assert_eq!(radio_hz_from_mhz(&domain, 433.920_125), 433_920_125);
        assert_eq!(radio_hz_from_mhz(&domain, 0.5), 1_000_000);
        assert_eq!(radio_hz_from_mhz(&domain, 6_100.0), 6_000_000_000);

        let sample_rate_domain = ValueDomain::Stepped {
            range: 2_000_000..=20_000_000,
            step: 1,
        };
        assert_eq!(radio_hz_from_mhz(&sample_rate_domain, 12.5), 12_500_000);
    }

    #[test]
    fn radio_mhz_numbers_omit_the_unit_and_trailing_zeroes() {
        assert_eq!(format_radio_mhz_number(1_750_000), "1.75");
        assert_eq!(format_radio_mhz_number(5_500_000), "5.5");
        assert_eq!(format_radio_mhz_number(20_000_000), "20");
    }
}
