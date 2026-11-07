//! The decoded-data side panel: the protocol messages decoded for each analyzer.

use crate::{
    analyzer_name, egui, format_duration, logic_marker_color, toolbar_action_button, Color32,
    Duration, ProtocolAnalyzerInstance, ProtocolDataFormat, ProtocolDecodeOutcome,
    ProtocolDecodedEvent, ProtocolDecodedKind, RichText, SelectableHoverText, Sense,
    WaveformViewport, ACCENT, BACKGROUND, DECODED_PANEL_ACTION_BUTTON_SIZE, TEXT_MUTED,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedPanelEntry {
    pub(crate) start_sample: u64,
    pub(crate) end_sample: u64,
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) warning: bool,
}

pub(crate) struct I2cPanelTransaction {
    pub(crate) start_sample: u64,
    pub(crate) end_sample: u64,
    pub(crate) address: u8,
    pub(crate) read: bool,
    pub(crate) data: Vec<u8>,
    pub(crate) acknowledgements: Vec<bool>,
}

impl I2cPanelTransaction {
    pub(crate) fn entry(self) -> DecodedPanelEntry {
        let operation = if self.read { "READ" } else { "WRITE" };
        let response = |index| {
            self.acknowledgements
                .get(index)
                .map_or(
                    "no response",
                    |acknowledged| {
                        if *acknowledged {
                            "ACK"
                        } else {
                            "NACK"
                        }
                    },
                )
        };
        let mut details = vec![format!(
            "Address 0x{:02X} ({}) → {}",
            self.address,
            if self.read { "R" } else { "W" },
            response(0)
        )];
        if self.data.is_empty() {
            details.push("No data bytes".to_owned());
        } else {
            details.extend(
                self.data
                    .iter()
                    .enumerate()
                    .map(|(index, byte)| format!("Data 0x{byte:02X} → {}", response(index + 1))),
            );
        }
        DecodedPanelEntry {
            start_sample: self.start_sample,
            end_sample: self.end_sample,
            title: format!("I²C {operation} · address 0x{:02X}", self.address),
            detail: details.join("\n"),
            warning: self
                .acknowledgements
                .iter()
                .any(|acknowledged| !acknowledged),
        }
    }
}

pub(crate) fn decoded_panel_entries(events: &[ProtocolDecodedEvent]) -> Vec<DecodedPanelEntry> {
    let mut entries = Vec::new();
    let mut i2c_transaction: Option<I2cPanelTransaction> = None;
    for event in events {
        match event.kind {
            ProtocolDecodedKind::I2cAddress { address, read } => {
                if let Some(transaction) = i2c_transaction.take() {
                    entries.push(transaction.entry());
                }
                i2c_transaction = Some(I2cPanelTransaction {
                    start_sample: event.start_sample,
                    end_sample: event.end_sample,
                    address,
                    read,
                    data: Vec::new(),
                    acknowledgements: Vec::new(),
                });
            }
            ProtocolDecodedKind::Data { value } => {
                if let Some(transaction) = &mut i2c_transaction {
                    transaction.end_sample = event.end_sample;
                    transaction.data.push(value);
                } else {
                    entries.push(DecodedPanelEntry {
                        start_sample: event.start_sample,
                        end_sample: event.end_sample,
                        title: event.text.clone(),
                        detail: event.detail.clone(),
                        warning: event.warning,
                    });
                }
            }
            ProtocolDecodedKind::Acknowledge { acknowledged } => {
                if let Some(transaction) = &mut i2c_transaction {
                    transaction.end_sample = event.end_sample;
                    transaction.acknowledgements.push(acknowledged);
                }
            }
            ProtocolDecodedKind::Spi { .. }
            | ProtocolDecodedKind::Uart { .. }
            | ProtocolDecodedKind::Custom => {
                if let Some(transaction) = i2c_transaction.take() {
                    entries.push(transaction.entry());
                }
                entries.push(DecodedPanelEntry {
                    start_sample: event.start_sample,
                    end_sample: event.end_sample,
                    title: event.text.clone(),
                    detail: event.detail.clone(),
                    warning: event.warning,
                });
            }
        }
    }
    if let Some(transaction) = i2c_transaction {
        entries.push(transaction.entry());
    }
    entries.reverse();
    entries
}

pub(crate) fn uart_ascii_text(events: &[ProtocolDecodedEvent]) -> Option<String> {
    let bytes = events
        .iter()
        .filter_map(|event| match event.kind {
            ProtocolDecodedKind::Uart { value } => Some(value),
            ProtocolDecodedKind::I2cAddress { .. }
            | ProtocolDecodedKind::Data { .. }
            | ProtocolDecodedKind::Acknowledge { .. }
            | ProtocolDecodedKind::Spi { .. }
            | ProtocolDecodedKind::Custom => None,
        })
        .collect::<Vec<_>>();
    (!bytes.is_empty()).then(|| {
        bytes
            .into_iter()
            .map(|byte| match byte {
                b'\n' => "\n".to_owned(),
                b'\r' => "\r".to_owned(),
                b'\t' => "\t".to_owned(),
                byte if byte.is_ascii_graphic() || byte == b' ' => char::from(byte).to_string(),
                byte => format!("\\x{byte:02X}"),
            })
            .collect::<String>()
    })
}

pub(crate) fn focus_logic_sample_range(
    viewport: &mut WaveformViewport,
    sample_offset: u64,
    sample_count: usize,
    start_sample: u64,
    end_sample: u64,
) {
    if sample_count == 0 {
        return;
    }
    let local_start = start_sample.saturating_sub(sample_offset) as f64;
    let local_end = end_sample.saturating_sub(sample_offset) as f64;
    let center = ((local_start + local_end) * 0.5 / sample_count as f64).clamp(0.0, 1.0);
    let event_span = ((local_end - local_start).max(1.0) / sample_count as f64) as f32;
    viewport.span = (event_span * 4.0).clamp(1.0 / sample_count as f32, 1.0);
    viewport.start = viewport.clamp_start(center as f32 - viewport.span * 0.5);
    viewport.anchor_locked = true;
}

pub(crate) fn protocol_event_elapsed(start_sample: u64, sample_rate_hz: u64) -> Option<Duration> {
    (sample_rate_hz != 0)
        .then(|| Duration::from_secs_f64(start_sample as f64 / sample_rate_hz as f64))
}

pub(crate) fn decoded_panel_entry_id(
    ui: &egui::Ui,
    analyzer: &ProtocolAnalyzerInstance,
    entry: &DecodedPanelEntry,
) -> egui::Id {
    ui.make_persistent_id((
        "decoded_panel_entry",
        analyzer.instance_id,
        entry.start_sample,
        entry.end_sample,
        entry.title.as_str(),
    ))
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DecodedPanelScrollAnchor {
    pub(crate) newest_start_sample: Option<u64>,
    pub(crate) newest_content_y: f32,
}

#[derive(Default)]
pub(crate) struct DecodedMessagesPaintOutput {
    pub(crate) selected_range: Option<(u64, u64)>,
    pub(crate) newest_content_y: Option<f32>,
    pub(crate) previous_newest_content_y: Option<f32>,
}

pub(crate) fn retained_decoded_scroll_offset(
    current_offset: f32,
    added_height_above: f32,
    maximum_offset: f32,
) -> f32 {
    if current_offset <= 1.0 {
        current_offset
    } else {
        (current_offset + added_height_above.max(0.0)).clamp(0.0, maximum_offset.max(0.0))
    }
}

pub(crate) fn paint_decoded_analyzer_messages(
    ui: &mut egui::Ui,
    analyzer: &ProtocolAnalyzerInstance,
    events: &[ProtocolDecodedEvent],
    entries: &[DecodedPanelEntry],
    open_all: Option<bool>,
    sample_rate_hz: u64,
    previous_newest_start_sample: Option<u64>,
) -> DecodedMessagesPaintOutput {
    let content_origin_y = ui.cursor().top();
    let mut output = DecodedMessagesPaintOutput::default();
    if analyzer.data_format == ProtocolDataFormat::Ascii {
        if let Some(text) = uart_ascii_text(events) {
            ui.add_space(5.0);
            ui.label(RichText::new("TEXT").small().strong().color(TEXT_MUTED));
            egui::Frame::none()
                .fill(BACKGROUND)
                .rounding(egui::Rounding::same(5.0))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(text).monospace()).wrap());
                });
        }
    }
    if entries.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new("No decoded operations").color(TEXT_MUTED));
        return output;
    }
    ui.add_space(6.0);
    for (entry_index, entry) in entries.iter().enumerate() {
        let content_y = ui.cursor().top() - content_origin_y;
        if entry_index == 0 {
            output.newest_content_y = Some(content_y);
        }
        if previous_newest_start_sample == Some(entry.start_sample) {
            output.previous_newest_content_y = Some(content_y);
        }
        let card_width = ui.available_width();
        let entry_id = decoded_panel_entry_id(ui, analyzer, entry);
        let mut open = open_all
            .unwrap_or_else(|| ui.data(|data| data.get_temp::<bool>(entry_id).unwrap_or(false)));
        if open_all.is_some() {
            ui.data_mut(|data| data.insert_temp(entry_id, open));
        }
        let time = protocol_event_elapsed(entry.start_sample, sample_rate_hz)
            .map(format_duration)
            .unwrap_or_default();
        let color = if entry.warning {
            Color32::LIGHT_RED
        } else {
            Color32::WHITE
        };
        let card = egui::Frame::none()
            .fill(BACKGROUND)
            .rounding(egui::Rounding::same(5.0))
            .inner_margin(egui::Margin::same(8.0))
            .show(ui, |ui| {
                let content_width = (card_width - 16.0).max(0.0);
                ui.set_width(content_width);
                ui.set_max_width(content_width);
                let header = ui.horizontal(|ui| {
                    ui.label(RichText::new(if open { "▼" } else { "▶" }).color(TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        ui.label(RichText::new(&time).small().monospace().color(TEXT_MUTED));
                        ui.add(
                            egui::Label::new(RichText::new(&entry.title).strong().color(color))
                                .wrap()
                                .halign(egui::Align::Min),
                        );
                    });
                });
                let header_response = ui
                    .interact(
                        header.response.rect,
                        entry_id.with("header"),
                        Sense::click(),
                    )
                    .on_hover_selectable_text("Expand or collapse message data");
                if header_response.clicked() {
                    open = !open;
                    ui.data_mut(|data| data.insert_temp(entry_id, open));
                }
                if open {
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(RichText::new(&entry.detail).small().monospace())
                            .wrap()
                            .sense(Sense::click()),
                    )
                    .on_hover_selectable_text("Click to focus this message in the waveform")
                    .clicked()
                } else {
                    false
                }
            });
        if card.inner {
            output.selected_range = Some((entry.start_sample, entry.end_sample));
        }
        ui.add_space(5.0);
    }
    output
}

pub(crate) fn paint_decoded_data_panel(
    ui: &mut egui::Ui,
    analyzers: &[ProtocolAnalyzerInstance],
    outcomes: &[ProtocolDecodeOutcome],
    sample_rate_hz: u64,
) -> Option<(u64, u64)> {
    let mut selected_range = None;
    if analyzers.is_empty() {
        ui.label(RichText::new("No protocol analyzer configured").color(TEXT_MUTED));
        ui.add_space(4.0);
        ui.small("Add an analyzer in Capture Setup to decode transactions.");
        return None;
    }
    for (analyzer_index, (analyzer, outcome)) in analyzers.iter().zip(outcomes).enumerate() {
        if analyzer_index != 0 {
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);
        }
        let name = analyzer_name(analyzer);
        match outcome {
            ProtocolDecodeOutcome::Unavailable(message) => {
                ui.label(
                    RichText::new(name)
                        .strong()
                        .color(logic_marker_color(analyzer_index)),
                );
                ui.add_space(4.0);
                ui.label(RichText::new(message).color(TEXT_MUTED));
            }
            ProtocolDecodeOutcome::Events(events) => {
                let entries = decoded_panel_entries(events);
                let all_open = !entries.is_empty()
                    && entries.iter().all(|entry| {
                        let entry_id = decoded_panel_entry_id(ui, analyzer, entry);
                        ui.data(|data| data.get_temp::<bool>(entry_id).unwrap_or(false))
                    });
                let mut open_all = None;
                let remaining_analyzers = analyzers.len() - analyzer_index;
                let remaining_separator_height =
                    17.0 * remaining_analyzers.saturating_sub(1) as f32;
                let analyzer_height = ((ui.available_height() - remaining_separator_height)
                    / remaining_analyzers as f32)
                    .max(DECODED_PANEL_ACTION_BUTTON_SIZE[1] + 32.0);
                let header = ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(name)
                            .strong()
                            .color(logic_marker_color(analyzer_index)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let response = ui
                            .add_enabled_ui(!entries.is_empty(), |ui| {
                                ui.add_sized(
                                    DECODED_PANEL_ACTION_BUTTON_SIZE,
                                    toolbar_action_button(if all_open { "⊟" } else { "⊞" }, ACCENT),
                                )
                            })
                            .inner
                            .on_hover_selectable_text(if all_open {
                                "Collapse all messages for this analyzer"
                            } else {
                                "Expand all messages for this analyzer"
                            });
                        if response.clicked() {
                            open_all = Some(!all_open);
                        }
                    });
                });
                let message_area_height =
                    (analyzer_height - header.response.rect.height()).max(24.0);
                let scroll_salt = ("decoded_analyzer_messages", analyzer.instance_id);
                let scroll_id = ui.make_persistent_id(scroll_salt);
                let anchor_id = scroll_id.with("newest_anchor");
                let previous_anchor = ui.data(|data| {
                    data.get_temp::<DecodedPanelScrollAnchor>(anchor_id)
                        .unwrap_or_default()
                });
                let mut scroll_output = egui::ScrollArea::vertical()
                    .id_salt(scroll_salt)
                    .max_height(message_area_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        paint_decoded_analyzer_messages(
                            ui,
                            analyzer,
                            events,
                            &entries,
                            open_all,
                            sample_rate_hz,
                            previous_anchor.newest_start_sample,
                        )
                    });
                let current_newest_start_sample = entries.first().map(|entry| entry.start_sample);
                if previous_anchor.newest_start_sample.is_some()
                    && previous_anchor.newest_start_sample != current_newest_start_sample
                {
                    if let Some(previous_newest_y) = scroll_output.inner.previous_newest_content_y {
                        let added_height = previous_newest_y - previous_anchor.newest_content_y;
                        let maximum_offset = (scroll_output.content_size.y
                            - scroll_output.inner_rect.height())
                        .max(0.0);
                        let retained_offset = retained_decoded_scroll_offset(
                            scroll_output.state.offset.y,
                            added_height,
                            maximum_offset,
                        );
                        if retained_offset != scroll_output.state.offset.y {
                            scroll_output.state.offset.y = retained_offset;
                            scroll_output.state.store(ui.ctx(), scroll_output.id);
                            ui.ctx().request_discard(
                                "retain decoded-message position after prepending data",
                            );
                        }
                    }
                }
                ui.data_mut(|data| {
                    data.insert_temp(
                        anchor_id,
                        DecodedPanelScrollAnchor {
                            newest_start_sample: current_newest_start_sample,
                            newest_content_y: scroll_output
                                .inner
                                .newest_content_y
                                .unwrap_or_default(),
                        },
                    );
                });
                let clicked_range = scroll_output.inner.selected_range;
                if clicked_range.is_some() {
                    selected_range = clicked_range;
                }
            }
        }
    }
    selected_range
}
