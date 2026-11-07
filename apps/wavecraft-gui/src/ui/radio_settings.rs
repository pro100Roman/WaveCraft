//! Radio receiver settings panels: capture parameters, gains, and the analyzer
//! (demodulation, packet, and whitening) configuration.

use crate::{
    automatic_filter_tap_count, egui, nearest_domain_value, normalize_gain_value,
    paint_capture_settings_save_button, selected_radio, Color32, DiscoveredRadio, FontId, Pos2,
    RadioAnalyzerChannelSettings, RadioAnalyzerSettings, RadioDataMode, RadioDemodulation,
    RadioProvider, RadioProviderFailure, RadioSettings, RadioWhiteningPreset, RangeInclusive,
    RichText, SavedCaptureSettingsKind, SelectableHoverText, ValueDomain, WhiteningDirection,
    AUTOMATIC_CHANNEL_BANDWIDTH_HZ, AUTOMATIC_FILTER_TAP_COUNT, BACKGROUND,
    MAX_RADIO_ANALYZER_CHANNELS, RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS, RAW_AUTOMATIC_PREAMBLE_BITS,
    TEXT_MUTED,
};
use std::collections::BTreeMap;
use wavecraft_sdr::GainControlDescriptor;

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
    paint_radio_analyzer_modulation(ui, settings, enabled);

    if !settings.is_enabled() {
        return;
    }

    egui::Grid::new("radio_signal_analyzer_parameters")
        .num_columns(2)
        .max_col_width(126.0)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            paint_radio_analyzer_filter_rows(ui, settings, capture_sample_rate_hz, enabled);
            paint_radio_analyzer_symbol_rows(ui, settings, enabled);
            paint_radio_analyzer_preamble_rows(ui, settings, enabled);
            paint_radio_analyzer_sync_word_rows(ui, settings, enabled);
            paint_radio_analyzer_timing_rows(ui, settings, enabled);
            paint_radio_analyzer_acquisition_rows(ui, settings, enabled);
            paint_radio_analyzer_data_mode_row(ui, settings, enabled);
            paint_radio_analyzer_whitening_rows(ui, settings, enabled);
        });

    paint_radio_analyzer_configuration_error(ui, settings);
}

fn paint_radio_analyzer_modulation(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    enabled: bool,
) {
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
}

fn paint_radio_analyzer_filter_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    capture_sample_rate_hz: u64,
    enabled: bool,
) {
    paint_radio_parameter_label(ui, "Polarity", Some("Swaps mark and space decisions"));
    ui.add_enabled_ui(enabled, |ui| {
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
        "Auto channel bandwidth",
        Some(
            "Choose the channel bandwidth from the bit rate and deviation: Carson's rule \
             (2 x deviation + bit rate) plus 15 kHz each side for carrier frequency error, \
             rounded up to 5 kHz. A narrower channel passes less noise, but a carrier more \
             than about 20 kHz off the channel centre is lost; switch this off and set a \
             wider channel then.",
        ),
    );
    let mut automatic_bandwidth = settings.channel_bandwidth_hz == AUTOMATIC_CHANNEL_BANDWIDTH_HZ;
    if ui
        .add_enabled(
            enabled,
            egui::Checkbox::without_text(&mut automatic_bandwidth),
        )
        .changed()
    {
        // Switching Auto off keeps the bandwidth that was in use.
        settings.channel_bandwidth_hz = if automatic_bandwidth {
            AUTOMATIC_CHANNEL_BANDWIDTH_HZ
        } else {
            settings.effective_channel_bandwidth_hz()
        };
    }
    ui.end_row();

    paint_radio_parameter_label(
        ui,
        "Channel bandwidth",
        Some("Full complex bandwidth retained around the selected channel"),
    );
    ui.add_enabled_ui(enabled, |ui| {
        paint_channel_bandwidth_value(ui, settings, capture_rate_mhz);
    });
    ui.end_row();

    paint_radio_parameter_label(
        ui,
        "Processing rate",
        Some("IQ sample rate after channel filtering and bounded downsampling"),
    );
    ui.add_enabled_ui(enabled, |ui| {
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
        "Auto filter taps",
        Some(
            "Size the channel filter from the capture rate so its transition band stays a \
             quarter of the channel bandwidth with 60 dB of rejection. A fixed length \
             loses selectivity as the capture rate grows.",
        ),
    );
    let mut automatic_taps = settings.filter_tap_count == AUTOMATIC_FILTER_TAP_COUNT;
    if ui
        .add_enabled(enabled, egui::Checkbox::without_text(&mut automatic_taps))
        .changed()
    {
        settings.filter_tap_count = if automatic_taps {
            AUTOMATIC_FILTER_TAP_COUNT
        } else {
            automatic_filter_tap_count(
                capture_sample_rate_hz,
                settings.effective_channel_bandwidth_hz(),
            )
        };
    }
    ui.end_row();

    paint_radio_parameter_label(
        ui,
        "Channel filter taps",
        Some("Odd FIR length from 3 to 4095; more taps improve rejection but cost CPU"),
    );
    ui.add_enabled_ui(enabled, |ui| {
        paint_filter_tap_count_value(ui, settings, capture_sample_rate_hz);
    });
    if settings.filter_tap_count != AUTOMATIC_FILTER_TAP_COUNT {
        settings.filter_tap_count = settings.filter_tap_count.clamp(3, 4_095) | 1;
    }
    ui.end_row();
}

/// Channel bandwidth editor. While Auto is on it shows the bandwidth the
/// analyzer will use and cannot be edited.
fn paint_channel_bandwidth_value(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    capture_rate_mhz: f64,
) {
    let range = 0.000_001..=capture_rate_mhz.max(0.000_001);
    if settings.channel_bandwidth_hz == AUTOMATIC_CHANNEL_BANDWIDTH_HZ {
        let mut shown_hz = settings.effective_channel_bandwidth_hz();
        ui.add_enabled_ui(false, |ui| {
            paint_scaled_u64_value(ui, &mut shown_hz, 1_000_000.0, range, "MHz");
        });
    } else {
        paint_scaled_u64_value(
            ui,
            &mut settings.channel_bandwidth_hz,
            1_000_000.0,
            range,
            "MHz",
        );
    }
}

/// Filter length editor. While Auto is on it shows the length the channelizer
/// will use and cannot be edited.
fn paint_filter_tap_count_value(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    capture_sample_rate_hz: u64,
) {
    let size = egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y);
    if settings.filter_tap_count == AUTOMATIC_FILTER_TAP_COUNT {
        let mut shown = automatic_filter_tap_count(
            capture_sample_rate_hz,
            settings.effective_channel_bandwidth_hz(),
        );
        ui.add_enabled_ui(false, |ui| {
            add_left_aligned_sized(ui, size, egui::DragValue::new(&mut shown).range(3..=4_095));
        });
    } else {
        add_left_aligned_sized(
            ui,
            size,
            egui::DragValue::new(&mut settings.filter_tap_count)
                .range(3..=4_095)
                .speed(2),
        );
    }
}

fn paint_radio_analyzer_symbol_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    enabled: bool,
) {
    let processing_rate_khz = settings.output_sample_rate_hz as f64 / 1_000.0;
    paint_radio_parameter_label(
        ui,
        "Bit rate",
        Some("Raw bit rate for binary FSK/GFSK; one symbol carries one bit"),
    );
    ui.add_enabled_ui(enabled, |ui| {
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
    ui.add_enabled_ui(enabled, |ui| {
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
        ui.add_enabled_ui(enabled, |ui| {
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
}

fn paint_radio_analyzer_preamble_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    enabled: bool,
) {
    paint_radio_parameter_label(
        ui,
        "Preamble value",
        Some("One hexadecimal byte repeated at the start of the FSK data"),
    );
    ui.add_enabled_ui(enabled, |ui| {
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
    ui.add_enabled_ui(enabled, |ui| {
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
}

fn paint_radio_analyzer_sync_word_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    enabled: bool,
) {
    paint_radio_parameter_label(
        ui,
        "Sync word",
        Some(if settings.data_mode == RadioDataMode::Packets {
            "Required 1-to-8-byte unwhitened word that establishes the packet and whitening boundary"
        } else {
            "Optional 1-to-8-byte word after the repeated preamble; unlike 0xAA, it establishes an unambiguous byte boundary"
        }),
    );
    ui.add_enabled_ui(enabled, |ui| {
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
    ui.add_enabled_ui(enabled, |ui| {
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
}

fn paint_radio_analyzer_timing_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    enabled: bool,
) {
    paint_radio_parameter_label(
                ui,
                "Adaptive timing",
                Some("GNU Radio-style Mueller-and-Muller symbol synchronization after quadrature demodulation"),
            );
    ui.add_enabled(
        enabled,
        egui::Checkbox::without_text(&mut settings.adaptive_timing),
    );
    ui.end_row();

    if settings.adaptive_timing {
        paint_radio_parameter_label(
            ui,
            "Timing loop bandwidth",
            Some("Higher values track faster but admit more timing jitter"),
        );
        ui.add_enabled_ui(enabled, |ui| {
            let mut bandwidth = f64::from(settings.timing_loop_bandwidth_milli) / 1_000.0;
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
                settings.timing_loop_bandwidth_milli = (bandwidth * 1_000.0).round() as u32;
            }
        });
        ui.end_row();

        paint_radio_parameter_label(
            ui,
            "Clock error limit",
            Some("Maximum symbol-clock correction allowed by the timing loop"),
        );
        ui.add_enabled_ui(enabled, |ui| {
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
}

fn paint_radio_analyzer_acquisition_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    enabled: bool,
) {
    paint_radio_parameter_label(
                ui,
                if settings.data_mode == RadioDataMode::Packets {
                    "Sync bit errors"
                } else {
                    "Preamble bit errors"
                },
                Some("Maximum hard-decision mismatches accepted in the sync word, or in the preamble when sync is disabled"),
            );
    ui.add_enabled_ui(enabled, |ui| {
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
        ui.add_enabled_ui(enabled, |ui| {
            let mut confidence = f64::from(settings.minimum_bit_confidence_milli) / 1_000.0;
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
                settings.minimum_bit_confidence_milli = (confidence * 1_000.0).round() as u16;
            }
        });
        ui.end_row();

        paint_radio_parameter_label(
                    ui,
                    "Maximum uncertain bits",
                    Some("Rejects a completed RAW message when more bits fall below the confidence threshold"),
                );
        ui.add_enabled_ui(enabled, |ui| {
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
}

fn paint_radio_analyzer_data_mode_row(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    enabled: bool,
) {
    paint_radio_parameter_label(
                ui,
                "Data decoding",
                Some("RAW discovers byte alignment from a repeated preamble; Fixed requires the configured preamble and sync, then dewhitens post-sync data"),
            );
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt("radio_analyzer_data_mode")
            .width(RADIO_VALUE_WIDTH)
            .selected_text(match settings.data_mode {
                RadioDataMode::Raw => "RAW",
                RadioDataMode::Packets => "Fixed",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut settings.data_mode, RadioDataMode::Raw, "RAW");
                ui.selectable_value(&mut settings.data_mode, RadioDataMode::Packets, "Fixed");
            });
    });
    ui.end_row();
}

fn paint_radio_analyzer_whitening_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    enabled: bool,
) {
    if settings.data_mode == RadioDataMode::Packets {
        paint_radio_parameter_label(
            ui,
            "Data whitening",
            Some("XOR packet bits after sync with a reset additive LFSR sequence"),
        );
        ui.add_enabled(
            enabled,
            egui::Checkbox::without_text(&mut settings.whitening_enabled),
        );
        ui.end_row();

        if settings.whitening_enabled {
            let whitening_enabled = enabled && settings.whitening_enabled;
            paint_radio_whitening_preset_row(ui, settings, whitening_enabled);
            if settings.whitening_preset == RadioWhiteningPreset::Custom {
                paint_radio_whitening_register_rows(ui, settings, whitening_enabled);
                paint_radio_whitening_output_rows(ui, settings, whitening_enabled);
                paint_radio_whitening_range_rows(ui, settings, whitening_enabled);
            }
        }
    }
}

fn paint_radio_whitening_preset_row(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    whitening_enabled: bool,
) {
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
                for preset in [RadioWhiteningPreset::Custom, RadioWhiteningPreset::Pn9X9X5] {
                    let response =
                        ui.selectable_value(&mut settings.whitening_preset, preset, preset.label());
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
}

fn paint_radio_whitening_register_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    whitening_enabled: bool,
) {
    paint_radio_parameter_label(ui, "LFSR width", Some("Whitening register width"));
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
                    egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
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
}

fn paint_radio_whitening_output_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    whitening_enabled: bool,
) {
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
}

fn paint_radio_whitening_range_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioAnalyzerSettings,
    whitening_enabled: bool,
) {
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
                egui::DragValue::new(&mut settings.whitening_start_bit).speed(1),
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
                settings.whitening_end_bit =
                    bounded.then_some(settings.whitening_start_bit.saturating_add(1));
            }
            if let Some(end) = &mut settings.whitening_end_bit {
                ui.add(
                    egui::DragValue::new(end)
                        .range(settings.whitening_start_bit.saturating_add(1)..=usize::MAX)
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
        egui::Checkbox::without_text(&mut settings.whitening_advance_excluded_bits),
    );
    ui.end_row();
}

fn paint_radio_analyzer_configuration_error(ui: &mut egui::Ui, settings: &RadioAnalyzerSettings) {
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
            egui::Button::new("Add"),
        )
        .clicked()
    {
        let step_hz =
            i64::try_from(settings.effective_channel_bandwidth_hz().max(1)).unwrap_or(i64::MAX);
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

/// The receive parameters of one radio as a two-column grid: tuning, capture
/// length, trigger, bandwidth, gains and the HackRF front-end switches. Rows
/// that a driver cannot honour are left out.
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
    egui::Grid::new("radio_receive_configuration")
        .num_columns(2)
        .max_col_width(126.0)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            paint_radio_center_frequency_row(ui, radio, settings, tuning_enabled);
            if radio.provider == RadioProvider::HackRf {
                paint_radio_frequency_correction_row(ui, settings, tuning_enabled);
            }
            paint_radio_sample_rate_row(ui, radio, settings, enabled);
            paint_radio_single_duration_row(ui, settings, enabled);
            paint_radio_wait_for_trigger_row(ui, settings, enabled);
            paint_radio_post_trigger_row(
                ui,
                settings,
                (enabled && settings.trigger_enabled) || trigger_runtime_enabled,
            );
            paint_radio_trigger_level_row(
                ui,
                settings,
                enabled || runtime_enabled || trigger_runtime_enabled,
            );
            paint_radio_bandwidth_row(ui, radio, settings, tuning_enabled);
            if radio.capabilities.receive_channels > 1 {
                paint_radio_receive_channel_row(ui, radio, settings, enabled);
            }
            for control in &radio.capabilities.gain_controls {
                paint_radio_gain_row(ui, control, &mut settings.gains_db, tuning_enabled);
            }
            if radio.provider == RadioProvider::HackRf {
                paint_radio_hackrf_option_rows(ui, settings, enabled, tuning_enabled);
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

fn paint_radio_center_frequency_row(
    ui: &mut egui::Ui,
    radio: &DiscoveredRadio,
    settings: &mut RadioSettings,
    tuning_enabled: bool,
) {
    let center_frequency_help = (radio.provider == RadioProvider::HackRf).then_some(
        "Tunes the frequency at the center of the RF spectrum and waterfall. HackRF One range: 1 MHz–6 GHz. This can be changed while capture is running.",
    );
    paint_radio_parameter_label(ui, "Center frequency", center_frequency_help);
    ui.add_enabled_ui(tuning_enabled, |ui| {
        paint_radio_mhz_value(
            ui,
            &mut settings.capture.center_frequency_hz,
            &radio.capabilities.frequency_hz,
        );
    });
    ui.end_row();
}

/// The signed reference correction in ppm, kept within ±200 ppm.
fn paint_radio_frequency_correction_row(
    ui: &mut egui::Ui,
    settings: &mut RadioSettings,
    tuning_enabled: bool,
) {
    paint_radio_parameter_label(
        ui,
        "Frequency correction",
        Some(
            "Signed HackRF reference correction. Positive values compensate a fast clock. Changes apply to RF tuning and the sample clock during Run and reset the live DSP stream. Range: -200 to +200 ppm.",
        ),
    );
    ui.add_enabled_ui(tuning_enabled, |ui| {
        ui.horizontal(|ui| {
            let mut correction_ppm = f64::from(settings.frequency_correction_milli_ppm) / 1_000.0;
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
                settings.frequency_correction_milli_ppm = (correction_ppm * 1_000.0).round() as i32;
            }
            ui.label("ppm");
        });
    });
    settings.frequency_correction_milli_ppm = settings
        .frequency_correction_milli_ppm
        .clamp(-200_000, 200_000);
    ui.end_row();
}

fn paint_radio_sample_rate_row(
    ui: &mut egui::Ui,
    radio: &DiscoveredRadio,
    settings: &mut RadioSettings,
    enabled: bool,
) {
    let sample_rate_help = (radio.provider == RadioProvider::HackRf).then_some(
        "Sets the I/Q sampling rate and the captured RF frequency span. Supported range: 2–20 MHz. Step: 1 Hz (0.000001 MHz). HackRF supports any whole-Hz value in this range; 8 MHz or higher is recommended.",
    );
    paint_radio_parameter_label(ui, "Sample rate", sample_rate_help);
    ui.add_enabled_ui(enabled, |ui| {
        paint_radio_mhz_value(
            ui,
            &mut settings.capture.sample_rate_hz,
            &radio.capabilities.sample_rates_hz,
        );
    });
    ui.end_row();
}

/// How long RF Single captures when it is not waiting for a trigger, in
/// seconds with millisecond resolution, between 1 ms and one hour.
fn paint_radio_single_duration_row(ui: &mut egui::Ui, settings: &mut RadioSettings, enabled: bool) {
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
                settings.single_duration_millis = (duration_seconds * 1_000.0).round() as u64;
            }
            ui.label("s");
        });
    });
    settings.single_duration_millis = settings.single_duration_millis.clamp(1, 3_600_000);
    ui.end_row();
}

fn paint_radio_wait_for_trigger_row(
    ui: &mut egui::Ui,
    settings: &mut RadioSettings,
    enabled: bool,
) {
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
}

/// The interval shown after the trigger, in milliseconds with microsecond
/// resolution, up to [`RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS`].
fn paint_radio_post_trigger_row(ui: &mut egui::Ui, settings: &mut RadioSettings, enabled: bool) {
    paint_radio_parameter_label(
        ui,
        "Post-trigger",
        Some(
            "Additional IQ display time after the triggering FFT window. Resolution: 0.001 ms. At 19.2 kbit/s, a 10-byte preamble alone lasts 4.167 ms, so allow enough time for the complete packet",
        ),
    );
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            let mut milliseconds = settings.trigger_posttrigger_micros as f64 / 1_000.0;
            if add_left_aligned_sized(
                ui,
                egui::vec2(RADIO_VALUE_WIDTH, ui.spacing().interact_size.y),
                egui::DragValue::new(&mut milliseconds)
                    .range(0.0..=RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS as f64 / 1_000.0)
                    .speed(0.001)
                    .min_decimals(3)
                    .max_decimals(3),
            )
            .changed()
            {
                settings.trigger_posttrigger_micros = (milliseconds * 1_000.0).round() as u64;
            }
            ui.label("ms");
        });
    });
    settings.trigger_posttrigger_micros = settings
        .trigger_posttrigger_micros
        .min(RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS);
    ui.end_row();
}

/// The RF trigger threshold in dBFS with 0.1 dB resolution, between -120 and 0.
fn paint_radio_trigger_level_row(ui: &mut egui::Ui, settings: &mut RadioSettings, enabled: bool) {
    paint_radio_parameter_label(
        ui,
        "RF trigger level",
        Some(
            "Peak FFT-bin power threshold used by RF Single when Wait for trigger is enabled. After an FSK/GFSK preamble locks, the same level acts as squelch inside the configured analyzer channel and closes RAW data when the RF burst ends. Also represented by the draggable T marker",
        ),
    );
    ui.add_enabled_ui(enabled, |ui| {
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
    settings.trigger_level_dbfs_tenths = settings.trigger_level_dbfs_tenths.clamp(-1_200, 0);
    ui.end_row();
}

fn paint_radio_bandwidth_row(
    ui: &mut egui::Ui,
    radio: &DiscoveredRadio,
    settings: &mut RadioSettings,
    tuning_enabled: bool,
) {
    let bandwidth_help = (radio.provider == RadioProvider::HackRf).then_some(
        "Selects the HackRF analog baseband low-pass filter width. A narrower filter rejects more out-of-band noise and interference. Keep it at or below the sample rate to limit aliasing.",
    );
    paint_radio_parameter_label(ui, "Baseband bandwidth", bandwidth_help);
    ui.add_enabled_ui(tuning_enabled, |ui| {
        paint_radio_bandwidth_value(
            ui,
            &mut settings.capture.bandwidth_hz,
            &radio.capabilities.bandwidths_hz,
        );
    });
    ui.end_row();
}

/// Which of the radio's receive inputs to capture; only shown when it has
/// more than one.
fn paint_radio_receive_channel_row(
    ui: &mut egui::Ui,
    radio: &DiscoveredRadio,
    settings: &mut RadioSettings,
    enabled: bool,
) {
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

/// One gain stage: a slider whose value is snapped to the stage's range and
/// step. A stage that has no value yet starts at its default.
fn paint_radio_gain_row(
    ui: &mut egui::Ui,
    control: &GainControlDescriptor,
    gains_db: &mut BTreeMap<String, i32>,
    tuning_enabled: bool,
) {
    paint_radio_parameter_label(ui, &control.label, control.help.as_deref());
    let value = gains_db
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

/// The HackRF-only switches: I/Q DC correction, the RF amplifier and the
/// antenna port bias power.
fn paint_radio_hackrf_option_rows(
    ui: &mut egui::Ui,
    settings: &mut RadioSettings,
    enabled: bool,
    tuning_enabled: bool,
) {
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

    let antenna_help = "Only enable for an antenna or accessory designed for bias power";
    paint_radio_parameter_label(ui, "Antenna port power (3.3 V)", Some(antenna_help));
    ui.add_enabled(
        enabled,
        egui::Checkbox::without_text(&mut settings.antenna_power_enabled),
    )
    .on_hover_selectable_text(antenna_help);
    ui.end_row();
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

    /// Paints the analyzer configuration for one frame and returns the settings
    /// it leaves behind, so each test sees the normalisation the panel applies.
    fn render_analyzer(settings: &mut RadioAnalyzerSettings) {
        let context = egui::Context::default();
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_radio_analyzer_configuration(ui, settings, 20_000_000, true);
            });
        });
    }

    fn enabled_analyzer() -> RadioAnalyzerSettings {
        RadioAnalyzerSettings {
            demodulation: RadioDemodulation::BinaryFsk,
            ..RadioAnalyzerSettings::default()
        }
    }

    #[test]
    fn an_even_filter_tap_count_is_rounded_up_to_an_odd_one() {
        let mut settings = RadioAnalyzerSettings {
            filter_tap_count: 4,
            ..enabled_analyzer()
        };
        render_analyzer(&mut settings);
        assert_eq!(settings.filter_tap_count, 5);

        settings.filter_tap_count = 1;
        render_analyzer(&mut settings);
        assert_eq!(settings.filter_tap_count, 3);

        settings.filter_tap_count = 100_000;
        render_analyzer(&mut settings);
        assert_eq!(settings.filter_tap_count, 4_095);
    }

    #[test]
    fn an_automatic_filter_tap_count_and_bandwidth_are_left_alone() {
        let mut settings = enabled_analyzer();
        assert_eq!(settings.filter_tap_count, AUTOMATIC_FILTER_TAP_COUNT);
        assert_eq!(
            settings.channel_bandwidth_hz,
            AUTOMATIC_CHANNEL_BANDWIDTH_HZ
        );
        render_analyzer(&mut settings);
        assert_eq!(settings.filter_tap_count, AUTOMATIC_FILTER_TAP_COUNT);
        assert_eq!(
            settings.channel_bandwidth_hz,
            AUTOMATIC_CHANNEL_BANDWIDTH_HZ
        );
    }

    #[test]
    fn a_disabled_modulation_shows_and_changes_nothing_else() {
        let mut settings = RadioAnalyzerSettings {
            demodulation: RadioDemodulation::None,
            filter_tap_count: 4,
            maximum_preamble_bit_errors: 500,
            ..RadioAnalyzerSettings::default()
        };
        let before = settings.clone();
        render_analyzer(&mut settings);
        assert_eq!(settings, before);
    }

    #[test]
    fn preamble_bit_errors_stay_below_the_synchronisation_length() {
        // A sync word is what the receiver locks on, so it sets the limit.
        let mut settings = RadioAnalyzerSettings {
            sync_word_bytes: 2,
            preamble_bytes: 4,
            maximum_preamble_bit_errors: 64,
            ..enabled_analyzer()
        };
        render_analyzer(&mut settings);
        assert_eq!(settings.maximum_preamble_bit_errors, 15);

        // Without a sync word the repeated preamble is matched instead.
        settings.sync_word_bytes = 0;
        settings.maximum_preamble_bit_errors = 64;
        render_analyzer(&mut settings);
        assert_eq!(settings.maximum_preamble_bit_errors, 31);

        // Raw mode with neither searches for the automatic preamble.
        settings.preamble_bytes = 0;
        settings.maximum_preamble_bit_errors = 1_000;
        render_analyzer(&mut settings);
        assert_eq!(
            settings.maximum_preamble_bit_errors,
            RAW_AUTOMATIC_PREAMBLE_BITS - 1
        );
    }

    #[test]
    fn a_preamble_longer_than_the_limit_is_shortened() {
        let mut settings = RadioAnalyzerSettings {
            preamble_bytes: 1_000,
            ..enabled_analyzer()
        };
        render_analyzer(&mut settings);
        assert_eq!(settings.preamble_bytes, 256);
    }

    #[test]
    fn fixed_packets_always_have_a_sync_word_of_at_least_one_byte() {
        let mut settings = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Packets,
            sync_word_bytes: 0,
            ..enabled_analyzer()
        };
        render_analyzer(&mut settings);
        assert_eq!(settings.sync_word_bytes, 1);

        settings.sync_word_bytes = 200;
        render_analyzer(&mut settings);
        assert_eq!(settings.sync_word_bytes, 8);

        // Raw decoding may run without one.
        settings.data_mode = RadioDataMode::Raw;
        settings.sync_word_bytes = 0;
        render_analyzer(&mut settings);
        assert_eq!(settings.sync_word_bytes, 0);
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

    fn receive_radio(provider: RadioProvider) -> DiscoveredRadio {
        DiscoveredRadio {
            descriptor: DeviceDescriptor {
                id: DeviceId::new("test:radio"),
                name: "Test radio".to_owned(),
                kind: InstrumentKind::RadioReceiver,
            },
            provider,
            model: wavecraft_sdr::RadioModel::HackRfOne,
            connection_backend: wavecraft_sdr::ConnectionBackend::Usb,
            persistent_id: None,
            capabilities: wavecraft_hackrf::hackrf_one_capabilities(),
        }
    }

    /// Settings in which every edit box shows a different text.
    fn receive_settings() -> RadioSettings {
        let mut settings = RadioSettings::default();
        settings.capture.center_frequency_hz = 433_920_000;
        settings.capture.sample_rate_hz = 8_000_000;
        settings.capture.bandwidth_hz = 3_500_000;
        settings.single_duration_millis = 5_000;
        settings.trigger_enabled = true;
        settings.trigger_posttrigger_micros = 1_500;
        settings.trigger_level_dbfs_tenths = -500;
        settings.frequency_correction_milli_ppm = 1_234;
        settings.gains_db.insert("lna".to_owned(), 24);
        settings.gains_db.insert("vga".to_owned(), 20);
        settings
    }

    /// What one frame of the receive panel painted.
    struct Painted {
        texts: Vec<(String, Pos2)>,
        squares: Vec<Pos2>,
    }

    impl Painted {
        fn new(shapes: &[egui::epaint::ClippedShape]) -> Self {
            fn collect(shape: &egui::Shape, painted: &mut Painted) {
                match shape {
                    egui::Shape::Vec(shapes) => {
                        shapes.iter().for_each(|shape| collect(shape, painted));
                    }
                    egui::Shape::Text(text) => painted.texts.push((
                        text.galley.text().replace('\u{2212}', "-"),
                        text.visual_bounding_rect().center(),
                    )),
                    egui::Shape::Rect(rect) => {
                        let size = rect.rect.size();
                        if (size.x - size.y).abs() < 1.0 && (10.0..24.0).contains(&size.x) {
                            painted.squares.push(rect.rect.center());
                        }
                    }
                    _ => {}
                }
            }
            let mut painted = Self {
                texts: Vec::new(),
                squares: Vec::new(),
            };
            for clipped in shapes {
                collect(&clipped.shape, &mut painted);
            }
            painted
        }

        fn has(&self, text: &str) -> bool {
            self.texts.iter().any(|(painted, _)| painted == text)
        }

        fn starts_with(&self, prefix: &str) -> bool {
            self.texts
                .iter()
                .any(|(painted, _)| painted.starts_with(prefix))
        }

        fn center_of(&self, text: &str) -> Pos2 {
            let matches = self
                .texts
                .iter()
                .filter(|(painted, _)| painted == text)
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1, "expected exactly one {text:?}");
            matches[0].1
        }

        /// The check box in the row of `label`.
        fn checkbox_beside(&self, label: &str) -> Pos2 {
            let row = self.center_of(label);
            let matches = self
                .squares
                .iter()
                .filter(|square| (square.y - row.y).abs() < 6.0 && square.x > row.x)
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1, "expected one check box beside {label:?}");
            *matches[0]
        }
    }

    /// The radio receive panel, painted frame after frame like the real one.
    struct ReceivePanel {
        context: egui::Context,
        radio: DiscoveredRadio,
        settings: RadioSettings,
        enabled: bool,
        runtime_enabled: bool,
        trigger_runtime_enabled: bool,
        /// Paint the signal analyzer section instead of the receive one.
        analyzer_section: bool,
        frames: u32,
    }

    impl ReceivePanel {
        fn new(radio: DiscoveredRadio, settings: RadioSettings) -> Self {
            let mut panel = Self {
                context: egui::Context::default(),
                radio,
                settings,
                enabled: true,
                runtime_enabled: false,
                trigger_runtime_enabled: false,
                analyzer_section: false,
                frames: 0,
            };
            // The grid only knows its column widths after its first frame.
            panel.frame(Vec::new());
            panel
        }

        fn stopped(mut self) -> Self {
            self.enabled = false;
            self
        }

        fn analyzer_section(mut self) -> Self {
            self.analyzer_section = true;
            self.frame(Vec::new());
            self
        }

        fn running(mut self) -> Self {
            self.enabled = false;
            self.runtime_enabled = true;
            self
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Painted {
            self.frames += 1;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 900.0))),
                time: Some(f64::from(self.frames) / 60.0),
                events,
                ..egui::RawInput::default()
            };
            let output = self.context.run(input, |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    if self.analyzer_section {
                        paint_radio_analyzer_configuration(
                            ui,
                            &mut self.settings.analyzer,
                            self.settings.capture.sample_rate_hz,
                            self.enabled,
                        );
                    } else {
                        paint_radio_receive_configuration(
                            ui,
                            &self.radio,
                            &mut self.settings,
                            self.enabled,
                            self.runtime_enabled,
                            self.trigger_runtime_enabled,
                        );
                    }
                });
            });
            Painted::new(&output.shapes)
        }

        fn painted(&mut self) -> Painted {
            self.frame(Vec::new())
        }

        fn press(position: Pos2, pressed: bool) -> egui::Event {
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            }
        }

        fn click(&mut self, position: Pos2) {
            self.frame(vec![
                egui::Event::PointerMoved(position),
                Self::press(position, true),
            ]);
            self.frame(vec![Self::press(position, false)]);
            self.frame(Vec::new());
        }

        /// Clicks the edit box that currently shows `shown`, types `typed` and
        /// confirms it with Enter.
        fn type_into(&mut self, shown: &str, typed: &str) {
            let position = self.painted().center_of(shown);
            self.click(position);
            self.frame(vec![egui::Event::Text(typed.to_owned())]);
            self.frame(vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }]);
            self.frame(Vec::new());
        }

        /// Drags the control whose value is painted as `shown` to the right.
        fn drag_right(&mut self, shown: &str) {
            let start = self.painted().center_of(shown);
            let end = start + Vec2::new(60.0, 0.0);
            self.frame(vec![
                egui::Event::PointerMoved(start),
                Self::press(start, true),
            ]);
            self.frame(vec![egui::Event::PointerMoved(end)]);
            self.frame(vec![Self::press(end, false)]);
            self.frame(Vec::new());
        }

        /// Rests the pointer on `position` until its tooltip is shown.
        fn hover(&mut self, position: Pos2) -> Painted {
            self.frame(vec![egui::Event::PointerMoved(position)]);
            for _ in 0..60 {
                self.frame(Vec::new());
            }
            let painted = self.painted();
            self.frame(vec![egui::Event::PointerGone]);
            painted
        }
    }

    #[test]
    fn a_hackrf_shows_every_receive_row_but_the_channel_choice() {
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings());

        let painted = panel.painted();

        for row in [
            "Center frequency",
            "Frequency correction",
            "Sample rate",
            "Single duration",
            "Wait for trigger",
            "Post-trigger",
            "RF trigger level",
            "Baseband bandwidth",
            "IF (LNA) gain",
            "Baseband (VGA) gain",
            "IQ correction",
            "RF amplifier",
            "Antenna port power (3.3 V)",
        ] {
            assert!(painted.has(row), "missing row {row:?}");
        }
        assert!(!painted.has("Receive channel"));
        for unit in ["MHz", "ppm", "s", "ms", "dBFS", "dB"] {
            assert!(painted.has(unit), "missing unit {unit:?}");
        }
    }

    #[test]
    fn the_receive_rows_follow_each_other_in_a_fixed_order() {
        let mut radio = receive_radio(RadioProvider::HackRf);
        radio.capabilities.receive_channels = 2;
        let mut panel = ReceivePanel::new(radio, receive_settings());
        let painted = panel.painted();

        let rows = [
            "Center frequency",
            "Frequency correction",
            "Sample rate",
            "Single duration",
            "Wait for trigger",
            "Post-trigger",
            "RF trigger level",
            "Baseband bandwidth",
            "Receive channel",
            "IF (LNA) gain",
            "Baseband (VGA) gain",
            "IQ correction",
            "RF amplifier",
            "Antenna port power (3.3 V)",
        ];
        let heights = rows.map(|row| painted.center_of(row).y);
        assert!(
            heights.windows(2).all(|pair| pair[0] < pair[1]),
            "the rows are out of order: {heights:?}"
        );
    }

    #[test]
    fn other_radios_leave_out_the_hackrf_only_rows() {
        let mut settings = receive_settings();
        settings.frequency_correction_milli_ppm = 300_000;
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::Simulator), settings);

        let painted = panel.painted();

        for row in [
            "Frequency correction",
            "IQ correction",
            "RF amplifier",
            "Antenna port power (3.3 V)",
        ] {
            assert!(!painted.has(row), "unexpected row {row:?}");
        }
        assert_eq!(
            panel.settings.frequency_correction_milli_ppm, 300_000,
            "a row that is not shown does not limit its value"
        );
    }

    #[test]
    fn the_receive_channel_choice_appears_only_for_a_radio_with_several_channels() {
        let mut radio = receive_radio(RadioProvider::LimeSuiteNg);
        radio.capabilities.receive_channels = 2;
        let mut panel = ReceivePanel::new(radio, receive_settings());

        assert!(panel.painted().has("Receive channel"));

        panel.radio.capabilities.receive_channels = 1;
        assert!(!panel.painted().has("Receive channel"));
    }

    #[test]
    fn the_receive_channel_can_be_chosen_only_while_stopped() {
        let mut radio = receive_radio(RadioProvider::LimeSuiteNg);
        radio.capabilities.receive_channels = 2;
        let mut panel = ReceivePanel::new(radio, receive_settings());

        let position = panel.painted().center_of("RX 0");
        panel.click(position);
        let second = panel.painted().center_of("RX 1");
        panel.click(second);
        assert_eq!(panel.settings.capture.receive_channel, 1);

        let mut radio = receive_radio(RadioProvider::LimeSuiteNg);
        radio.capabilities.receive_channels = 2;
        let mut panel = ReceivePanel::new(radio, receive_settings()).running();
        let position = panel.painted().center_of("RX 0");
        panel.click(position);
        assert!(
            !panel.painted().has("RX 1"),
            "the choice does not open while capturing"
        );
    }

    #[test]
    fn a_bandwidth_above_the_sample_rate_is_flagged() {
        const WARNING: &str = "Bandwidth above sample rate may cause aliasing";
        let mut settings = receive_settings();
        settings.capture.sample_rate_hz = 8_000_000;
        settings.capture.bandwidth_hz = 8_000_000;
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::HackRf), settings);
        assert!(!panel.painted().has(WARNING));

        panel.settings.capture.bandwidth_hz = 15_000_000;
        assert!(panel.painted().has(WARNING));
    }

    #[test]
    fn a_gain_without_a_value_starts_at_its_default() {
        let mut settings = receive_settings();
        settings.gains_db.clear();
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::HackRf), settings);

        panel.painted();

        for control in &panel.radio.capabilities.gain_controls {
            assert_eq!(panel.settings.gains_db[&control.id], control.default_db);
        }
    }

    #[test]
    fn gains_are_limited_to_their_range_and_snapped_to_their_step() {
        let mut settings = receive_settings();
        settings.gains_db.insert("lna".to_owned(), 19);
        settings.gains_db.insert("vga".to_owned(), 500);
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::HackRf), settings);

        panel.painted();

        assert_eq!(
            panel.settings.gains_db["lna"], 16,
            "19 dB is nearer 16 than 24 in 8 dB steps"
        );
        assert_eq!(panel.settings.gains_db["vga"], 62);
    }

    #[test]
    fn a_gain_can_be_moved_while_stopped_or_capturing_but_not_otherwise() {
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings());
        panel.drag_right("24");
        assert!(panel.settings.gains_db["lna"] > 24);

        let mut panel =
            ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings()).running();
        panel.drag_right("24");
        assert!(panel.settings.gains_db["lna"] > 24);

        let mut panel =
            ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings()).stopped();
        panel.drag_right("24");
        assert_eq!(panel.settings.gains_db["lna"], 24);
    }

    #[test]
    fn typed_values_are_converted_to_the_units_of_the_settings() {
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings());

        panel.type_into("1.234", "-2.5");
        assert_eq!(panel.settings.frequency_correction_milli_ppm, -2_500);
        panel.type_into("5.000", "2.5");
        assert_eq!(panel.settings.single_duration_millis, 2_500);
        panel.type_into("1.500", "2.25");
        assert_eq!(panel.settings.trigger_posttrigger_micros, 2_250);
        panel.type_into("-50.0", "-70.3");
        assert_eq!(panel.settings.trigger_level_dbfs_tenths, -703);
    }

    #[test]
    fn typed_values_stop_at_the_limits_of_their_fields() {
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings());

        panel.type_into("1.234", "500");
        assert_eq!(panel.settings.frequency_correction_milli_ppm, 200_000);
        panel.type_into("5.000", "99999");
        assert_eq!(panel.settings.single_duration_millis, 3_600_000);
        panel.type_into("3600.000", "0");
        assert_eq!(panel.settings.single_duration_millis, 1);
        panel.type_into("1.500", "99999");
        assert_eq!(
            panel.settings.trigger_posttrigger_micros,
            RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS
        );
        panel.type_into("-50.0", "10");
        assert_eq!(panel.settings.trigger_level_dbfs_tenths, 0);
        panel.type_into("0.0", "-500");
        assert_eq!(panel.settings.trigger_level_dbfs_tenths, -1_200);
    }

    #[test]
    fn a_stopped_panel_with_nothing_running_edits_nothing() {
        let mut panel =
            ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings()).stopped();
        panel.radio.capabilities.bandwidths_hz = wavecraft_sdr::ValueDomain::Stepped {
            range: 1_000_000..=28_000_000,
            step: 1_000,
        };
        let before = panel.settings.clone();

        panel.type_into("433.920", "100");
        panel.type_into("8.000", "10");
        panel.type_into("3.500", "5");
        panel.type_into("5.000", "2");
        panel.type_into("-50.0", "-60");
        panel.type_into("1.234", "2");
        for label in [
            "Wait for trigger",
            "IQ correction",
            "RF amplifier",
            "Antenna port power (3.3 V)",
        ] {
            let checkbox = panel.painted().checkbox_beside(label);
            panel.click(checkbox);
        }

        assert_eq!(panel.settings, before);
    }

    #[test]
    fn a_running_capture_can_only_retune_and_move_the_trigger_level() {
        let mut panel =
            ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings()).running();
        panel.radio.capabilities.bandwidths_hz = wavecraft_sdr::ValueDomain::Stepped {
            range: 1_000_000..=28_000_000,
            step: 1_000,
        };

        panel.type_into("433.920", "100");
        panel.type_into("1.234", "2");
        panel.type_into("3.500", "6");
        panel.type_into("-50.0", "-60");
        panel.type_into("8.000", "10");
        panel.type_into("5.000", "2");
        for label in [
            "Wait for trigger",
            "IQ correction",
            "Antenna port power (3.3 V)",
        ] {
            let checkbox = panel.painted().checkbox_beside(label);
            panel.click(checkbox);
        }
        let amplifier = panel.painted().checkbox_beside("RF amplifier");
        panel.click(amplifier);

        assert_eq!(panel.settings.capture.center_frequency_hz, 100_000_000);
        assert_eq!(panel.settings.frequency_correction_milli_ppm, 2_000);
        assert_eq!(panel.settings.capture.bandwidth_hz, 6_000_000);
        assert_eq!(panel.settings.trigger_level_dbfs_tenths, -600);
        assert!(panel.settings.amplifier_enabled);
        assert_eq!(panel.settings.capture.sample_rate_hz, 8_000_000);
        assert_eq!(panel.settings.single_duration_millis, 5_000);
        assert!(panel.settings.trigger_enabled);
        assert!(
            panel.settings.iq_correction_enabled,
            "the I/Q correction stays on"
        );
        assert!(!panel.settings.antenna_power_enabled);
    }

    #[test]
    fn a_stopped_panel_toggles_its_switches() {
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings());

        for label in [
            "Wait for trigger",
            "IQ correction",
            "RF amplifier",
            "Antenna port power (3.3 V)",
        ] {
            let checkbox = panel.painted().checkbox_beside(label);
            panel.click(checkbox);
        }

        assert!(!panel.settings.trigger_enabled);
        assert!(
            !panel.settings.iq_correction_enabled,
            "the I/Q correction starts on"
        );
        assert!(panel.settings.amplifier_enabled);
        assert!(panel.settings.antenna_power_enabled);
    }

    fn analyzer_settings() -> RadioSettings {
        let mut settings = receive_settings();
        settings.analyzer.demodulation = RadioDemodulation::BinaryGfsk;
        settings
    }

    #[test]
    fn the_automatic_switches_have_their_own_rows_and_start_on() {
        let mut panel =
            ReceivePanel::new(receive_radio(RadioProvider::HackRf), analyzer_settings())
                .analyzer_section();
        let painted = panel.painted();

        for label in [
            "Auto channel bandwidth",
            "Channel bandwidth",
            "Auto filter taps",
            "Channel filter taps",
        ] {
            assert!(painted.has(label), "missing the {label:?} row");
        }
        // The switches sit on rows of their own, above the value they control.
        let row = |label: &str| painted.center_of(label).y;
        assert!(row("Auto channel bandwidth") < row("Channel bandwidth"));
        assert!(row("Auto filter taps") < row("Channel filter taps"));
        painted.checkbox_beside("Auto channel bandwidth");
        painted.checkbox_beside("Auto filter taps");
        assert_eq!(
            panel.settings.analyzer.channel_bandwidth_hz,
            AUTOMATIC_CHANNEL_BANDWIDTH_HZ
        );
        assert_eq!(
            panel.settings.analyzer.filter_tap_count,
            AUTOMATIC_FILTER_TAP_COUNT
        );
    }

    #[test]
    fn switching_the_automatic_bandwidth_off_keeps_the_value_in_use() {
        let mut panel =
            ReceivePanel::new(receive_radio(RadioProvider::HackRf), analyzer_settings())
                .analyzer_section();
        let automatic = panel.settings.analyzer.effective_channel_bandwidth_hz();
        assert_eq!(automatic, 120_000);

        let checkbox = panel.painted().checkbox_beside("Auto channel bandwidth");
        panel.click(checkbox);
        assert_eq!(panel.settings.analyzer.channel_bandwidth_hz, automatic);

        // A manual value no longer follows the bit rate and deviation;
        // switching Auto back on does.
        panel.settings.analyzer.symbol_rate_hz = 100_000;
        panel.settings.analyzer.deviation_hz = 50_000;
        assert_eq!(
            panel.settings.analyzer.effective_channel_bandwidth_hz(),
            automatic
        );
        let checkbox = panel.painted().checkbox_beside("Auto channel bandwidth");
        panel.click(checkbox);
        assert_eq!(
            panel.settings.analyzer.channel_bandwidth_hz,
            AUTOMATIC_CHANNEL_BANDWIDTH_HZ
        );
        assert_eq!(
            panel.settings.analyzer.effective_channel_bandwidth_hz(),
            230_000
        );
    }

    #[test]
    fn switching_the_automatic_filter_taps_off_keeps_the_length_in_use() {
        let mut panel =
            ReceivePanel::new(receive_radio(RadioProvider::HackRf), analyzer_settings())
                .analyzer_section();
        let capture_rate = panel.settings.capture.sample_rate_hz;
        let automatic = automatic_filter_tap_count(
            capture_rate,
            panel.settings.analyzer.effective_channel_bandwidth_hz(),
        );

        let checkbox = panel.painted().checkbox_beside("Auto filter taps");
        panel.click(checkbox);
        assert_eq!(panel.settings.analyzer.filter_tap_count, automatic);
        assert_eq!(automatic % 2, 1);

        let checkbox = panel.painted().checkbox_beside("Auto filter taps");
        panel.click(checkbox);
        assert_eq!(
            panel.settings.analyzer.filter_tap_count,
            AUTOMATIC_FILTER_TAP_COUNT
        );
    }

    #[test]
    fn a_stopped_panel_leaves_the_automatic_switches_alone() {
        let mut panel =
            ReceivePanel::new(receive_radio(RadioProvider::HackRf), analyzer_settings())
                .stopped()
                .analyzer_section();
        let before = panel.settings.clone();
        for label in ["Auto channel bandwidth", "Auto filter taps"] {
            let checkbox = panel.painted().checkbox_beside(label);
            panel.click(checkbox);
        }
        assert_eq!(panel.settings, before);
    }

    #[test]
    fn the_post_trigger_interval_is_editable_with_a_trigger_or_a_running_trigger_only() {
        let mut settings = receive_settings();
        settings.trigger_enabled = false;
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::HackRf), settings);

        panel.type_into("1.500", "2");
        assert_eq!(
            panel.settings.trigger_posttrigger_micros, 1_500,
            "without a trigger the interval is not editable"
        );

        panel.trigger_runtime_enabled = true;
        panel.type_into("1.500", "2");
        assert_eq!(panel.settings.trigger_posttrigger_micros, 2_000);
    }

    #[test]
    fn the_trigger_level_follows_a_running_trigger_alone() {
        let mut panel =
            ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings()).stopped();
        panel.trigger_runtime_enabled = true;

        panel.type_into("-50.0", "-60");

        assert_eq!(panel.settings.trigger_level_dbfs_tenths, -600);
    }

    #[test]
    fn only_a_hackrf_explains_its_tuning_rows() {
        for (provider, explained) in [
            (RadioProvider::HackRf, true),
            (RadioProvider::Simulator, false),
        ] {
            for (row, help) in [
                ("Center frequency", "Tunes the frequency"),
                ("Sample rate", "Sets the I/Q sampling rate"),
                ("Baseband bandwidth", "Selects the HackRF analog baseband"),
            ] {
                let mut panel = ReceivePanel::new(receive_radio(provider), receive_settings());
                let position = panel.painted().center_of(row);

                let painted = panel.hover(position);

                assert_eq!(
                    painted.starts_with(help),
                    explained,
                    "{provider:?} {row:?} help"
                );
            }
        }
    }

    #[test]
    fn a_gain_slider_shows_the_help_of_its_control() {
        let mut panel = ReceivePanel::new(receive_radio(RadioProvider::HackRf), receive_settings());
        let slider = panel.painted().center_of("24");

        let painted = panel.hover(slider);

        assert!(painted.starts_with("Controls IF-stage gain"));
    }
}
