//! Independent System Trace workspace and decoded-event instruments.

use std::time::Duration;

use crate::system_trace::{
    choose_systemview_recording_path, choose_systemview_trace_to_open, SystemTraceAction,
    SystemTraceConnectionConfiguration, SystemTraceConnectionState,
};
use crate::system_trace::{SystemTraceDisplayState, TraceEventView};
use crate::waveform::chrome::half_transparent_plate;
use crate::{
    egui, format_duration, toolbar_action_button, ui::frame_actions::FrameActions, RichText,
    WaveCraftApp, ACCENT, BACKGROUND, GRID, ORANGE, PANEL, SCOPE_COLORS, TEXT_MUTED,
};
use wavecraft_debug_probe::{
    AttachConfiguration, DebugTargetDescriptor, ProbeDescriptor, ProbeProvider, ProbeSelector,
    RttConfiguration, RttControlBlockLocation, RttTerminalColor, RttTerminalLine, TargetSelector,
    WireProtocol,
};
use wavecraft_trace::{ExecutionContext, TraceEvent, TraceEventKind, TraceModel};

const TIMELINE_LABEL_WIDTH: f32 = 150.0;
const TIMELINE_HEADER_HEIGHT: f32 = 24.0;
const TIMELINE_ROW_HEIGHT: f32 = 25.0;
const TIMELINE_MINIMUM_WIDTH: f32 = 720.0;
const TIMELINE_ZOOM_STEP: f32 = 1.5;
const TIMELINE_SMART_TIMING_HINT_WIDTH: f32 = 300.0;
const TIMELINE_SMART_TIMING_HINT_HEIGHT: f32 = 42.0;
const EVENT_TABLE_MINIMUM_WIDTH: f32 = 760.0;
const EVENT_TABLE_ROW_HEIGHT: f32 = 22.0;
const TRACE_SPLITTER_HEIGHT: f32 = 10.0;
const SELECTED_EVENT_FOOTER_HEIGHT: f32 = 36.0;
const TERMINAL_TABLE_MINIMUM_WIDTH: f32 = 720.0;
const TRACE_SELECTION_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 214, 92);
const TRACE_SELECTION_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(32, 27, 12, 32);
const TRACE_EVENT_SELECTION_FILL: egui::Color32 = egui::Color32::from_rgb(62, 52, 18);
const TARGET_DROPDOWN_RESULT_LIMIT: usize = 200;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemTraceSettings {
    pub(crate) probe_connection_id: Option<String>,
    pub(crate) target_name: String,
    pub(crate) speed_khz: u32,
    pub(crate) reset_after_attach: bool,
    pub(crate) control_block_address: String,
    pub(crate) systemview_enabled: bool,
    pub(crate) up_channel: usize,
    pub(crate) down_channel: usize,
    pub(crate) terminal_enabled: bool,
    pub(crate) terminal_up_channel: usize,
}

impl Default for SystemTraceSettings {
    fn default() -> Self {
        Self {
            probe_connection_id: None,
            target_name: String::new(),
            speed_khz: 4_000,
            reset_after_attach: false,
            control_block_address: String::new(),
            systemview_enabled: true,
            up_channel: 1,
            down_channel: 1,
            terminal_enabled: true,
            terminal_up_channel: 0,
        }
    }
}

impl SystemTraceSettings {
    fn connection_configuration(&self) -> Result<SystemTraceConnectionConfiguration, String> {
        if !self.systemview_enabled && !self.terminal_enabled {
            return Err("Enable SystemView trace or the plain RTT terminal".to_owned());
        }
        let connection_id = self
            .probe_connection_id
            .clone()
            .ok_or_else(|| "Select a debug probe before connecting".to_owned())?;
        let target_name = self.target_name.trim();
        let target = if target_name.is_empty() {
            TargetSelector::Auto
        } else {
            TargetSelector::Name(target_name.to_owned())
        };
        let control_block = parse_rtt_control_block(&self.control_block_address)?;
        Ok(SystemTraceConnectionConfiguration {
            probe: ProbeSelector::connection_id(connection_id),
            attach: AttachConfiguration {
                target,
                wire_protocol: Some(WireProtocol::Swd),
                speed_khz: Some(self.speed_khz),
                core_index: 0,
            },
            reset_after_attach: self.reset_after_attach,
            rtt: RttConfiguration {
                control_block,
                timeout: std::time::Duration::from_secs(2),
            },
            systemview_enabled: self.systemview_enabled,
            up_channel: self.up_channel,
            down_channel: self.down_channel,
            terminal_up_channel: self.terminal_enabled.then_some(self.terminal_up_channel),
        })
    }
}

impl WaveCraftApp {
    pub(crate) fn paint_system_trace_toolbar(
        &mut self,
        context: &egui::Context,
        actions: &mut FrameActions,
    ) {
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
                    ui.separator();
                    actions.refresh_clicked = ui
                        .add_enabled(
                            !self.is_busy() && self.system_trace_connection.is_disconnected(),
                            toolbar_action_button("↻ Scan devices", SCOPE_COLORS[1]),
                        )
                        .on_hover_text("Scan for debug probes, DSLogic, DSCope, and radio devices")
                        .clicked();
                    ui.separator();
                    let can_record = matches!(
                        self.system_trace_connection,
                        SystemTraceConnectionState::Attached {
                            systemview_enabled: true,
                            ..
                        }
                    );
                    if ui
                        .add_enabled(
                            can_record,
                            toolbar_action_button(
                                "● Record…",
                                egui::Color32::from_rgb(245, 96, 96),
                            ),
                        )
                        .on_hover_text("Record the raw SystemView stream to an .SVDat file")
                        .clicked()
                    {
                        actions.system_trace_action = Some(SystemTraceAction::Record);
                    }
                    if ui
                        .add_enabled(
                            !self.is_busy() && self.system_trace_connection.is_disconnected(),
                            toolbar_action_button("Open .SVDat…", TEXT_MUTED),
                        )
                        .on_hover_text("Open a raw SEGGER SystemView event stream")
                        .clicked()
                    {
                        actions.system_trace_action = Some(SystemTraceAction::Open);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        paint_trace_run_control(ui, &self.system_trace_connection, actions);
                        if ui
                            .add_enabled(
                                !self.system_trace.events().is_empty()
                                    || !self.system_trace.terminal_lines().is_empty(),
                                toolbar_action_button("Clear", TEXT_MUTED),
                            )
                            .on_hover_text("Clear trace events and RTT terminal output")
                            .clicked()
                        {
                            self.system_trace.clear();
                        }
                    });
                });
            });
    }

    pub(crate) fn paint_system_trace_setup_panel(
        &mut self,
        context: &egui::Context,
        actions: &mut FrameActions,
    ) {
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
                        let selected_probe = self
                            .system_trace_settings
                            .probe_connection_id
                            .as_deref()
                            .and_then(|connection_id| {
                                self.debug_probes.iter().find(|probe| {
                                    probe.connection_id.as_str() == connection_id
                                })
                            })
                            .map(probe_label)
                            .unwrap_or_else(|| "No debug probe found".to_owned());
                        let mut probe_changed = false;
                        egui::ComboBox::from_id_salt("system_trace_probe")
                            .width(ui.available_width())
                            .selected_text(selected_probe)
                            .show_ui(ui, |ui| {
                                if self.debug_probes.is_empty() {
                                    ui.add_enabled(false, egui::Label::new("No probes found"));
                                }
                                for probe in &self.debug_probes {
                                    probe_changed |= ui
                                        .selectable_value(
                                            &mut self.system_trace_settings.probe_connection_id,
                                            Some(probe.connection_id.clone()),
                                            probe_label(probe),
                                        )
                                        .on_hover_text(&probe.connection_id)
                                        .changed();
                                }
                            });
                        if probe_changed {
                            self.system_trace_settings.target_name.clear();
                        }
                        if let Some(error) = &self.debug_probe_discovery_error {
                            ui.label(
                                RichText::new(error)
                                    .small()
                                    .color(egui::Color32::from_rgb(245, 96, 96)),
                            );
                        }

                        ui.add_space(10.0);
                        setup_label(ui, "Target MCU");
                        let selected_provider = self
                            .system_trace_settings
                            .probe_connection_id
                            .as_deref()
                            .and_then(|connection_id| {
                                self.debug_probes.iter().find(|probe| {
                                    probe.connection_id.as_str() == connection_id
                                })
                            })
                            .map(|probe| &probe.provider);
                        let targets = selected_provider
                            .and_then(|provider| {
                                self.debug_probe_target_groups
                                    .iter()
                                    .find(|group| &group.provider == provider)
                            })
                            .map(|group| group.targets.as_slice())
                            .unwrap_or_default();
                        paint_target_selector(
                            ui,
                            &mut self.system_trace_settings.target_name,
                            selected_provider,
                            targets,
                        );

                        ui.add_space(10.0);
                        setup_label(ui, "SWD speed (kHz)");
                        ui.add(
                            egui::DragValue::new(&mut self.system_trace_settings.speed_khz)
                                .range(100..=50_000)
                                .speed(100),
                        );

                        ui.add_space(10.0);
                        ui.checkbox(
                            &mut self.system_trace_settings.reset_after_attach,
                            "Reset target after attach",
                        )
                        .on_hover_text(
                            "Enable when a sleeping target must restart before RTT can be found",
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
                        ui.checkbox(
                            &mut self.system_trace_settings.systemview_enabled,
                            "SystemView binary trace",
                        );
                        ui.horizontal(|ui| {
                            setup_label(ui, "SystemView up");
                            ui.add_enabled(
                                self.system_trace_settings.systemview_enabled,
                                egui::DragValue::new(
                                    &mut self.system_trace_settings.up_channel,
                                )
                                .range(0..=255),
                            );
                            setup_label(ui, "Down");
                            ui.add_enabled(
                                self.system_trace_settings.systemview_enabled,
                                egui::DragValue::new(
                                    &mut self.system_trace_settings.down_channel,
                                )
                                .range(0..=255),
                            );
                        });

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut self.system_trace_settings.terminal_enabled,
                                "Plain RTT terminal",
                            );
                            ui.add_enabled(
                                self.system_trace_settings.terminal_enabled,
                                egui::DragValue::new(
                                    &mut self.system_trace_settings.terminal_up_channel,
                                )
                                .range(0..=255)
                                .prefix("Up "),
                            );
                        });

                        ui.add_space(16.0);
                        paint_connection_control(
                            ui,
                            &self.system_trace_connection,
                            self.system_trace_settings.probe_connection_id.is_some(),
                            actions,
                        );
                        if let SystemTraceConnectionState::Attached {
                            channels,
                            systemview_enabled,
                        } = &self.system_trace_connection
                        {
                            ui.label(
                                RichText::new(format!(
                                    "Attached with {} RTT channel(s); {}",
                                    channels.len(),
                                    if *systemview_enabled {
                                        "SystemView ready"
                                    } else {
                                        "terminal only"
                                    }
                                ))
                                .small()
                                .color(ACCENT),
                            );
                        }
                        if let SystemTraceConnectionState::Error(error) =
                            &self.system_trace_connection
                        {
                            ui.label(
                                RichText::new(error)
                                    .color(egui::Color32::from_rgb(245, 96, 96)),
                            );
                        }
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(
                                "Attach uses the isolated probe-rs helper and never flashes the target. Reset is applied only when selected above.",
                            )
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
                paint_trace_summary(ui, &self.system_trace);
                ui.add_space(10.0);
                let pane_spacing = ui.spacing().item_spacing.y * 2.0;
                let available_pane_height =
                    (ui.available_height() - TRACE_SPLITTER_HEIGHT - pane_spacing).max(0.0);
                self.system_trace
                    .fit_trace_instruments_to_height(available_pane_height);
                let pane_width = ui.available_width();
                ui.allocate_ui_with_layout(
                    egui::vec2(pane_width, self.system_trace.timeline_view_height()),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        paint_timeline_instrument(ui, &mut self.system_trace);
                    },
                );
                paint_trace_instrument_splitter(ui, &mut self.system_trace);
                ui.allocate_ui_with_layout(
                    egui::vec2(pane_width, self.system_trace.event_view_height()),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        paint_event_instrument(ui, &mut self.system_trace);
                    },
                );
            });
    }

    pub(crate) fn apply_system_trace_action(&mut self, action: Option<SystemTraceAction>) {
        match action {
            Some(SystemTraceAction::Open) => {
                if let Some(path) = choose_systemview_trace_to_open() {
                    self.begin_open_system_trace(path);
                }
            }
            Some(SystemTraceAction::Record) => {
                if let Some(path) = choose_systemview_recording_path() {
                    self.request_system_trace_recording(path);
                }
            }
            Some(SystemTraceAction::Connect) => {
                match self.system_trace_settings.connection_configuration() {
                    Ok(configuration) => self.begin_system_trace_connection(configuration),
                    Err(error) => {
                        self.system_trace_connection = SystemTraceConnectionState::Error(error)
                    }
                }
            }
            Some(SystemTraceAction::Start) => self.request_system_trace_start(),
            Some(SystemTraceAction::Stop) => self.request_system_trace_stop(),
            Some(SystemTraceAction::Disconnect) => self.request_system_trace_disconnect(),
            None => {}
        }
    }
}

fn setup_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(TEXT_MUTED));
}

fn paint_target_selector(
    ui: &mut egui::Ui,
    target_name: &mut String,
    provider: Option<&ProbeProvider>,
    targets: &[DebugTargetDescriptor],
) {
    let popup_id = ui.make_persistent_id("system_trace_target_popup");
    let controls = ui.horizontal(|ui| {
        let button_width = 24.0;
        let text_width =
            (ui.available_width() - button_width - ui.spacing().item_spacing.x).max(button_width);
        let text = ui.add_sized(
            [text_width, 20.0],
            egui::TextEdit::singleline(target_name).hint_text("Auto-detect or type a target"),
        );
        let dropdown = ui
            .add_sized([button_width, 20.0], egui::Button::new("▾"))
            .on_hover_text("Show supported targets for the selected probe");
        (text, dropdown)
    });
    let (text_response, dropdown_response) = controls.inner;
    let anchor_response = text_response.clone().union(dropdown_response.clone());
    if dropdown_response.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(popup_id));
    } else if text_response.clicked() || text_response.gained_focus() || text_response.changed() {
        ui.memory_mut(|memory| memory.open_popup(popup_id));
    }

    egui::popup::popup_below_widget(
        ui,
        popup_id,
        &anchor_response,
        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
        |popup_ui| {
            popup_ui.set_min_width(anchor_response.rect.width());
            popup_ui.set_max_width(anchor_response.rect.width());
            let provider_label = provider.map(probe_provider_label).unwrap_or("No probe");
            let query = target_name.trim().to_ascii_lowercase();
            let matching_targets = targets
                .iter()
                .filter(|target| target_matches_query(target, &query))
                .collect::<Vec<_>>();
            popup_ui.label(
                RichText::new(format!(
                    "{provider_label} · {} matching target(s)",
                    matching_targets.len()
                ))
                .small()
                .color(TEXT_MUTED),
            );
            popup_ui.separator();

            let mut selected_target = None;
            egui::ScrollArea::vertical()
                .id_salt("system_trace_target_results")
                .max_height(240.0)
                .auto_shrink([false, false])
                .show(popup_ui, |ui| {
                    if ui
                        .selectable_label(target_name.trim().is_empty(), "Auto-detect")
                        .on_hover_text("Let probe-rs identify the connected target")
                        .clicked()
                    {
                        selected_target = Some(String::new());
                    }
                    if provider.is_none() {
                        ui.add_enabled(false, egui::Label::new("Select a debug probe first"));
                    } else if targets.is_empty() {
                        ui.add_enabled(
                            false,
                            egui::Label::new("No target catalog is available for this probe"),
                        );
                    } else {
                        for target in matching_targets.iter().take(TARGET_DROPDOWN_RESULT_LIMIT) {
                            if ui
                                .selectable_label(
                                    target.name.eq_ignore_ascii_case(target_name.trim()),
                                    &target.name,
                                )
                                .on_hover_text(format!("Family: {}", target.family))
                                .clicked()
                            {
                                selected_target = Some(target.name.clone());
                            }
                        }
                        if matching_targets.len() > TARGET_DROPDOWN_RESULT_LIMIT {
                            ui.label(
                                RichText::new(format!(
                                    "Type more characters to narrow {} additional matches",
                                    matching_targets.len() - TARGET_DROPDOWN_RESULT_LIMIT
                                ))
                                .small()
                                .color(TEXT_MUTED),
                            );
                        }
                    }
                });
            if let Some(selected_target) = selected_target {
                *target_name = selected_target;
                popup_ui.memory_mut(egui::Memory::close_popup);
            }
        },
    );
}

fn target_matches_query(target: &DebugTargetDescriptor, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let searchable = format!("{} {}", target.name, target.family).to_ascii_lowercase();
    query
        .to_ascii_lowercase()
        .split_ascii_whitespace()
        .all(|term| searchable.contains(term))
}

fn paint_connection_control(
    ui: &mut egui::Ui,
    state: &SystemTraceConnectionState,
    probe_selected: bool,
    actions: &mut FrameActions,
) {
    match state {
        SystemTraceConnectionState::Disconnected | SystemTraceConnectionState::Error(_) => {
            if ui
                .add_enabled(probe_selected, egui::Button::new("Connect and attach"))
                .clicked()
            {
                actions.system_trace_action = Some(SystemTraceAction::Connect);
            }
        }
        SystemTraceConnectionState::Connecting => {
            ui.add_enabled(false, egui::Button::new("Connecting…"));
        }
        SystemTraceConnectionState::Attached { .. }
        | SystemTraceConnectionState::Starting { .. }
        | SystemTraceConnectionState::Tracing { .. }
        | SystemTraceConnectionState::Stopping { .. } => {
            if ui.button("Disconnect").clicked() {
                actions.system_trace_action = Some(SystemTraceAction::Disconnect);
            }
        }
        SystemTraceConnectionState::Disconnecting => {
            ui.add_enabled(false, egui::Button::new("Disconnecting…"));
        }
    }
}

fn paint_trace_run_control(
    ui: &mut egui::Ui,
    state: &SystemTraceConnectionState,
    actions: &mut FrameActions,
) {
    match state {
        SystemTraceConnectionState::Attached {
            systemview_enabled: true,
            ..
        } => {
            if ui.button("▶ Start").clicked() {
                actions.system_trace_action = Some(SystemTraceAction::Start);
            }
        }
        SystemTraceConnectionState::Attached {
            systemview_enabled: false,
            ..
        } => {
            ui.label(RichText::new("RTT terminal connected").color(ACCENT));
        }
        SystemTraceConnectionState::Starting { .. } => {
            ui.add_enabled(false, egui::Button::new("Starting…"));
        }
        SystemTraceConnectionState::Tracing { recording_path, .. } => {
            if ui.button("■ Stop").clicked() {
                actions.system_trace_action = Some(SystemTraceAction::Stop);
            }
            if recording_path.is_some() {
                ui.label(
                    RichText::new("● Recording .SVDat").color(egui::Color32::from_rgb(245, 96, 96)),
                );
            }
        }
        SystemTraceConnectionState::Stopping { .. } => {
            ui.add_enabled(false, egui::Button::new("Stopping…"));
        }
        SystemTraceConnectionState::Disconnected
        | SystemTraceConnectionState::Connecting
        | SystemTraceConnectionState::Disconnecting
        | SystemTraceConnectionState::Error(_) => {
            ui.add_enabled(false, egui::Button::new("▶ Start"));
        }
    }
}

fn parse_rtt_control_block(text: &str) -> Result<RttControlBlockLocation, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(RttControlBlockLocation::Ram);
    }
    let address =
        if let Some(hexadecimal) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            u64::from_str_radix(hexadecimal, 16)
        } else {
            text.parse::<u64>()
        }
        .map_err(|_| format!("RTT control-block address {text:?} is not valid"))?;
    Ok(RttControlBlockLocation::Exact(address))
}

fn probe_label(probe: &ProbeDescriptor) -> String {
    let provider = probe_provider_label(&probe.provider);
    let serial = probe
        .serial_number
        .as_deref()
        .map(|serial| format!(" · {serial}"))
        .unwrap_or_default();
    format!("{provider} · {}{serial}", probe.model)
}

fn probe_provider_label(provider: &ProbeProvider) -> &str {
    match provider {
        ProbeProvider::JLink => "J-Link",
        ProbeProvider::StLink => "ST-Link",
        ProbeProvider::BlackMagicProbe => "Black Magic Probe",
        ProbeProvider::CmsisDap => "CMSIS-DAP",
        ProbeProvider::Other(provider) => provider,
    }
}

fn instrument_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0_f32, GRID))
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(12.0))
}

fn paint_trace_instrument_splitter(ui: &mut egui::Ui, trace: &mut SystemTraceDisplayState) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TRACE_SPLITTER_HEIGHT),
        egui::Sense::drag(),
    );
    let response = response
        .on_hover_cursor(egui::CursorIcon::ResizeVertical)
        .on_hover_text("Drag to resize Timeline and Events");
    let active = response.hovered() || response.dragged();
    ui.painter().line_segment(
        [
            egui::pos2(rect.left() + 8.0, rect.center().y),
            egui::pos2(rect.right() - 8.0, rect.center().y),
        ],
        egui::Stroke::new(
            if active { 2.0_f32 } else { 1.0_f32 },
            if active { ACCENT } else { GRID },
        ),
    );
    if response.dragged() {
        trace.move_trace_instrument_splitter(ui.input(|input| input.pointer.delta().y));
    }
}

fn paint_trace_summary(ui: &mut egui::Ui, trace: &SystemTraceDisplayState) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("SYSTEM TRACE").strong().color(ACCENT));
        ui.separator();
        ui.label(format!("{} events", trace.events().len()));
        ui.label(format!("{} cores", trace.model().cores().len()));
        ui.label(format!("{} logs", trace.log_count()));
        if let Some((start, end)) = trace.time_bounds() {
            ui.label(
                RichText::new(format!("{} ticks", end.saturating_sub(start))).color(TEXT_MUTED),
            );
        }
        let dropped_events = trace.dropped_event_count();
        if dropped_events > 0 {
            ui.label(
                RichText::new(format!("⚠ {dropped_events} dropped"))
                    .strong()
                    .color(egui::Color32::from_rgb(245, 96, 96)),
            );
        } else {
            ui.label(RichText::new("No reported loss").color(TEXT_MUTED));
        }
    });
}

fn paint_timeline_instrument(ui: &mut egui::Ui, trace: &mut SystemTraceDisplayState) {
    instrument_frame().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("TIMELINE").strong().color(ACCENT));
            ui.label(
                RichText::new("Task, ISR, timer, scheduler, and idle execution").color(TEXT_MUTED),
            );
        });
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("−").on_hover_text("Zoom out").clicked() {
                trace.scale_timeline_zoom(1.0 / TIMELINE_ZOOM_STEP);
            }
            let mut zoom = trace.timeline_zoom();
            if ui
                .add_sized(
                    [110.0, 18.0],
                    egui::Slider::new(&mut zoom, 1.0..=64.0)
                        .logarithmic(true)
                        .show_value(false),
                )
                .on_hover_text("Drag to set timeline zoom")
                .changed()
            {
                trace.set_timeline_zoom(zoom);
            }
            ui.label(
                RichText::new(format!("{:.1}×", trace.timeline_zoom()))
                    .monospace()
                    .color(TEXT_MUTED),
            );
            if ui.small_button("+").on_hover_text("Zoom in").clicked() {
                trace.scale_timeline_zoom(TIMELINE_ZOOM_STEP);
            }
            if ui
                .small_button("Fit")
                .on_hover_text("Fit the entire trace")
                .clicked()
            {
                trace.set_timeline_zoom(1.0);
            }
            ui.checkbox(&mut trace.timeline_smart_timing, "Smart timing")
                .on_hover_text("Measure the hovered event from its exact start and end");
            ui.label(
                RichText::new("Zoom: pinch or modifier-scroll; pan: drag")
                    .small()
                    .color(TEXT_MUTED),
            );
        });
        ui.add_space(6.0);
        let timeline_height = ui.available_height().max(TIMELINE_HEADER_HEIGHT);

        let lanes = timeline_lanes(trace.model());
        let Some((start_ticks, end_ticks)) = trace.time_bounds() else {
            paint_trace_empty_state(
                ui,
                "No execution data",
                "Decoded task and interrupt intervals will appear here.",
                timeline_height,
            );
            return;
        };
        if lanes.is_empty() {
            paint_trace_empty_state(
                ui,
                "No execution intervals yet",
                "Trace metadata and non-execution events are available in the event stream below.",
                timeline_height,
            );
            return;
        }

        let timeline_focus = trace.take_timeline_focus();
        let timeline_focus_timestamp = timeline_focus.and_then(|sequence| {
            trace
                .events()
                .iter()
                .find(|event| event.sequence == sequence)
                .map(|event| event.timestamp_ticks)
        });
        let selected_timestamp = trace.selected_event().map(|event| event.timestamp_ticks);
        let viewport_width = ui.available_width();
        let mut timeline_scroll = egui::ScrollArea::both()
            .id_salt("system_trace_timeline")
            .max_height(timeline_height)
            .drag_to_scroll(true)
            .auto_shrink([false, false]);
        if let Some(timestamp) = timeline_focus_timestamp {
            timeline_scroll = timeline_scroll.horizontal_scroll_offset(timeline_focus_offset(
                viewport_width,
                trace.timeline_zoom(),
                start_ticks,
                end_ticks,
                timestamp,
            ));
        }
        let timeline = timeline_scroll.show(ui, |ui| {
            let canvas_width = timeline_canvas_width(ui.available_width(), trace.timeline_zoom());
            let canvas_height = TIMELINE_HEADER_HEIGHT + lanes.len() as f32 * TIMELINE_ROW_HEIGHT;
            let (canvas, _) = ui.allocate_exact_size(
                egui::vec2(canvas_width, canvas_height),
                egui::Sense::hover(),
            );
            let clicked_sequence = paint_timeline_canvas(
                ui,
                canvas,
                trace,
                &lanes,
                start_ticks,
                end_ticks,
                selected_timestamp,
            );
            (canvas.top(), clicked_sequence)
        });
        let pointer_over_timeline = ui.input(|input| {
            input
                .pointer
                .hover_pos()
                .is_some_and(|position| timeline.inner_rect.contains(position))
        });
        if pointer_over_timeline {
            let zoom_delta = ui.input(|input| {
                let native_zoom = input.zoom_delta();
                if (native_zoom - 1.0).abs() > f32::EPSILON {
                    native_zoom
                } else if input.modifiers.alt {
                    (input.raw_scroll_delta.y * 0.01).exp()
                } else {
                    1.0
                }
            });
            if (zoom_delta - 1.0).abs() > f32::EPSILON {
                trace.scale_timeline_zoom(zoom_delta);
            }
        }
        paint_sticky_timeline_labels(
            ui,
            timeline.inner_rect,
            timeline.inner.0,
            trace.model(),
            &lanes,
        );
        if let Some(sequence) = timeline.inner.1 {
            trace.select_from_timeline(sequence);
        }
    });
}

fn timeline_canvas_width(available_width: f32, zoom: f32) -> f32 {
    available_width.max(TIMELINE_MINIMUM_WIDTH) * zoom
}

fn timeline_focus_offset(
    viewport_width: f32,
    zoom: f32,
    start_ticks: u64,
    end_ticks: u64,
    timestamp: u64,
) -> f32 {
    let canvas_width = timeline_canvas_width(viewport_width, zoom);
    let plot_width = (canvas_width - TIMELINE_LABEL_WIDTH).max(1.0);
    let duration = end_ticks.saturating_sub(start_ticks).max(1);
    let target_x = TIMELINE_LABEL_WIDTH
        + timestamp.saturating_sub(start_ticks).min(duration) as f32 / duration as f32 * plot_width;
    let visible_plot_center =
        TIMELINE_LABEL_WIDTH + (viewport_width - TIMELINE_LABEL_WIDTH).max(1.0) * 0.5;
    (target_x - visible_plot_center).clamp(0.0, (canvas_width - viewport_width).max(0.0))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TimelineLane {
    core_id: u32,
    context: ExecutionContext,
}

#[derive(Clone, Debug, PartialEq)]
struct TimelineSmartTiming {
    lane: TimelineLane,
    start_ticks: u64,
    end_ticks: u64,
    pointer: egui::Pos2,
}

fn timeline_lanes(model: &TraceModel) -> Vec<TimelineLane> {
    let mut lanes = Vec::new();
    for (core_id, core) in model.cores() {
        for interval in core.intervals() {
            let lane = TimelineLane {
                core_id: *core_id,
                context: interval.context,
            };
            if !lanes.contains(&lane) {
                lanes.push(lane);
            }
        }
    }
    lanes
}

fn paint_timeline_canvas(
    ui: &egui::Ui,
    canvas: egui::Rect,
    trace: &SystemTraceDisplayState,
    lanes: &[TimelineLane],
    start_ticks: u64,
    end_ticks: u64,
    selected_timestamp: Option<u64>,
) -> Option<u64> {
    let model = trace.model();
    let painter = ui.painter().with_clip_rect(canvas);
    let plot_left = canvas.left() + TIMELINE_LABEL_WIDTH;
    let plot_right = canvas.right();
    let plot_width = (plot_right - plot_left).max(1.0);
    let duration = end_ticks.saturating_sub(start_ticks).max(1);

    painter.rect_filled(canvas, 3.0, BACKGROUND);
    painter.line_segment(
        [
            egui::pos2(plot_left, canvas.top()),
            egui::pos2(plot_left, canvas.bottom()),
        ],
        egui::Stroke::new(1.0_f32, GRID),
    );
    paint_timeline_time_grid(
        &painter,
        canvas,
        plot_left,
        plot_right,
        start_ticks,
        duration,
    );

    let mut clicked_sequence = None;
    let mut smart_timing = None;
    for (lane_index, lane) in lanes.iter().enumerate() {
        let row_top =
            canvas.top() + TIMELINE_HEADER_HEIGHT + lane_index as f32 * TIMELINE_ROW_HEIGHT;
        let row_bottom = row_top + TIMELINE_ROW_HEIGHT;
        painter.line_segment(
            [
                egui::pos2(canvas.left(), row_bottom),
                egui::pos2(canvas.right(), row_bottom),
            ],
            egui::Stroke::new(1.0_f32, GRID),
        );
        let Some(core) = model.core(lane.core_id) else {
            continue;
        };
        for (interval_index, interval) in core
            .intervals()
            .iter()
            .enumerate()
            .filter(|(_, interval)| interval.context == lane.context)
        {
            let interval_end = interval
                .end_ticks
                .unwrap_or(end_ticks)
                .max(interval.start_ticks);
            let x_start = tick_x(
                interval.start_ticks,
                start_ticks,
                duration,
                plot_left,
                plot_width,
            );
            let x_end = tick_x(interval_end, start_ticks, duration, plot_left, plot_width);
            let interval_rect = egui::Rect::from_min_max(
                egui::pos2(x_start, row_top + 4.0),
                egui::pos2((x_end.max(x_start + 2.0)).min(plot_right), row_bottom - 4.0),
            );
            let interval_sequence = interval_event_sequence(trace, *lane, interval.start_ticks);
            let interval_selected = interval_sequence == trace.selected_sequence;
            painter.rect_filled(interval_rect, 2.0, context_color(lane.context));
            if interval_selected {
                painter.rect_filled(interval_rect, 2.0, TRACE_SELECTION_FILL);
            }
            let mut response = ui.interact(
                interval_rect,
                ui.id().with(("trace_interval", lane_index, interval_index)),
                egui::Sense::click(),
            );
            if trace.timeline_smart_timing && interval.end_ticks.is_some() {
                if let Some(pointer) = response.hover_pos() {
                    smart_timing = Some(TimelineSmartTiming {
                        lane: *lane,
                        start_ticks: interval.start_ticks,
                        end_ticks: interval_end,
                        pointer,
                    });
                }
            } else {
                response = response.on_hover_text(format!(
                    "{} · {}–{} ticks",
                    context_label(model, *lane),
                    interval.start_ticks,
                    interval_end
                ));
            }
            if response.clicked() {
                clicked_sequence = interval_sequence;
            }
            if !interval.complete {
                painter.rect_stroke(
                    interval_rect,
                    2.0,
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(245, 96, 96)),
                );
            }
            if interval_selected {
                painter.rect_stroke(
                    interval_rect,
                    2.0,
                    egui::Stroke::new(3.0_f32, TRACE_SELECTION_COLOR),
                );
            } else if response.hovered() {
                painter.rect_stroke(
                    interval_rect,
                    2.0,
                    egui::Stroke::new(1.5_f32, half_transparent_plate(egui::Color32::WHITE)),
                );
            }
        }
    }
    if let Some(timestamp) = selected_timestamp {
        let x = tick_x(timestamp, start_ticks, duration, plot_left, plot_width);
        painter.line_segment(
            [egui::pos2(x, canvas.top()), egui::pos2(x, canvas.bottom())],
            egui::Stroke::new(2.0_f32, TRACE_SELECTION_COLOR),
        );
        painter.circle_filled(
            egui::pos2(x, canvas.top() + 5.0),
            4.0,
            TRACE_SELECTION_COLOR,
        );
    }
    if let Some(measurement) = smart_timing {
        paint_timeline_smart_timing_hint(
            &painter,
            canvas,
            start_ticks,
            end_ticks,
            trace,
            measurement,
        );
    }
    clicked_sequence
}

fn paint_timeline_smart_timing_hint(
    painter: &egui::Painter,
    canvas: egui::Rect,
    trace_start_ticks: u64,
    trace_end_ticks: u64,
    trace: &SystemTraceDisplayState,
    measurement: TimelineSmartTiming,
) {
    let plot_left = canvas.left() + TIMELINE_LABEL_WIDTH;
    let plot_right = canvas.right();
    let plot_width = (plot_right - plot_left).max(1.0);
    let trace_duration_ticks = trace_end_ticks.saturating_sub(trace_start_ticks).max(1);
    let start_x = tick_x(
        measurement.start_ticks,
        trace_start_ticks,
        trace_duration_ticks,
        plot_left,
        plot_width,
    );
    let end_x = tick_x(
        measurement.end_ticks,
        trace_start_ticks,
        trace_duration_ticks,
        plot_left,
        plot_width,
    );
    let plot = egui::Rect::from_min_max(
        egui::pos2(plot_left, canvas.top() + TIMELINE_HEADER_HEIGHT),
        egui::pos2(plot_right, canvas.bottom()),
    );
    let color = context_color(measurement.lane.context);
    let hover_color = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 128);
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(start_x.min(end_x), plot.top()),
            egui::pos2(start_x.max(end_x), plot.bottom()),
        ),
        0.0,
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24),
    );
    for x in [start_x, end_x] {
        painter.line_segment(
            [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
            egui::Stroke::new(1.5_f32, hover_color),
        );
        painter.circle_filled(egui::pos2(x, plot.top() + 3.0), 2.5, hover_color);
    }

    let hint_width = TIMELINE_SMART_TIMING_HINT_WIDTH.min((plot.width() - 8.0).max(1.0));
    let hint_height = TIMELINE_SMART_TIMING_HINT_HEIGHT.min((canvas.height() - 8.0).max(1.0));
    if hint_width < 120.0 || hint_height < 32.0 {
        return;
    }
    let preferred_left = if measurement.pointer.x + 12.0 + hint_width <= plot.right() {
        measurement.pointer.x + 12.0
    } else {
        measurement.pointer.x - hint_width - 12.0
    };
    let hint_left = preferred_left.clamp(plot.left() + 4.0, plot.right() - hint_width - 4.0);
    let preferred_top = measurement.pointer.y + 12.0;
    let hint_top = preferred_top.clamp(canvas.top() + 4.0, canvas.bottom() - hint_height - 4.0);
    let hint = egui::Rect::from_min_size(
        egui::pos2(hint_left, hint_top),
        egui::vec2(hint_width, hint_height),
    );
    painter.rect_filled(hint, 5.0, half_transparent_plate(PANEL));
    painter.rect_stroke(hint, 5.0, egui::Stroke::new(0.75_f32, hover_color));

    let duration_ticks = measurement
        .end_ticks
        .saturating_sub(measurement.start_ticks);
    let duration = trace_system_frequency_hz(trace).map(|frequency_hz| {
        format_duration(Duration::from_secs_f64(
            duration_ticks as f64 / frequency_hz as f64,
        ))
    });
    let timing = duration.map_or_else(
        || format!("Δ {duration_ticks} ticks"),
        |duration| format!("Δ {duration_ticks} ticks · {duration}"),
    );
    painter.text(
        hint.left_top() + egui::vec2(8.0, 5.0),
        egui::Align2::LEFT_TOP,
        format!(
            "SMART TIMING · {}",
            context_label(trace.model(), measurement.lane)
        ),
        egui::FontId::monospace(10.0),
        color,
    );
    painter.text(
        hint.left_top() + egui::vec2(8.0, 21.0),
        egui::Align2::LEFT_TOP,
        format!(
            "{} → {} ticks · {timing}",
            measurement.start_ticks, measurement.end_ticks
        ),
        egui::FontId::monospace(10.0),
        egui::Color32::WHITE,
    );
}

fn trace_system_frequency_hz(trace: &SystemTraceDisplayState) -> Option<u64> {
    trace.events().iter().rev().find_map(|event| {
        let TraceEventKind::TargetInfo {
            system_frequency_hz,
            ..
        } = &event.kind
        else {
            return None;
        };
        (*system_frequency_hz > 0).then_some(*system_frequency_hz)
    })
}

fn interval_event_sequence(
    trace: &SystemTraceDisplayState,
    lane: TimelineLane,
    timestamp_ticks: u64,
) -> Option<u64> {
    let events_at_timestamp = || {
        trace.events().iter().filter(|event| {
            event.core_id == lane.core_id && event.timestamp_ticks == timestamp_ticks
        })
    };
    events_at_timestamp()
        .find(|event| event_starts_context(&event.kind, lane.context))
        .or_else(|| events_at_timestamp().next())
        .map(|event| event.sequence)
}

fn event_starts_context(kind: &TraceEventKind, context: ExecutionContext) -> bool {
    match (context, kind) {
        (ExecutionContext::Task(expected), TraceEventKind::TaskStarted { task_id }) => {
            expected == *task_id
        }
        (ExecutionContext::Task(_), TraceEventKind::IsrExited | TraceEventKind::TimerExited) => {
            true
        }
        (ExecutionContext::Isr(expected), TraceEventKind::IsrEntered { isr_id }) => {
            expected == *isr_id
        }
        (ExecutionContext::Isr(_), TraceEventKind::IsrExited) => true,
        (ExecutionContext::Timer(expected), TraceEventKind::TimerEntered { timer_id }) => {
            expected == *timer_id
        }
        (ExecutionContext::Timer(_), TraceEventKind::IsrExited) => true,
        (
            ExecutionContext::Scheduler,
            TraceEventKind::TaskStopped
            | TraceEventKind::IsrExitedToScheduler
            | TraceEventKind::TaskTerminated { .. },
        ) => true,
        (ExecutionContext::Idle, TraceEventKind::Idle) => true,
        _ => false,
    }
}

fn paint_sticky_timeline_labels(
    ui: &egui::Ui,
    viewport: egui::Rect,
    canvas_top: f32,
    model: &TraceModel,
    lanes: &[TimelineLane],
) {
    let label_right = (viewport.left() + TIMELINE_LABEL_WIDTH).min(viewport.right());
    let label_area =
        egui::Rect::from_min_max(viewport.min, egui::pos2(label_right, viewport.bottom()));
    let painter = ui.painter().with_clip_rect(label_area);
    painter.rect_filled(label_area, 0.0, BACKGROUND);
    painter.line_segment(
        [
            egui::pos2(label_right, viewport.top()),
            egui::pos2(label_right, viewport.bottom()),
        ],
        egui::Stroke::new(1.0_f32, GRID),
    );
    painter.text(
        egui::pos2(viewport.left() + 6.0, viewport.top() + 2.0),
        egui::Align2::LEFT_TOP,
        "CONTEXT",
        egui::FontId::monospace(11.0),
        TEXT_MUTED,
    );

    let lane_area = egui::Rect::from_min_max(
        egui::pos2(viewport.left(), viewport.top() + TIMELINE_HEADER_HEIGHT),
        egui::pos2(label_right, viewport.bottom()),
    );
    let lane_painter = ui.painter().with_clip_rect(lane_area);
    for (lane_index, lane) in lanes.iter().enumerate() {
        let row_top = canvas_top + TIMELINE_HEADER_HEIGHT + lane_index as f32 * TIMELINE_ROW_HEIGHT;
        let row_bottom = row_top + TIMELINE_ROW_HEIGHT;
        lane_painter.line_segment(
            [
                egui::pos2(viewport.left(), row_bottom),
                egui::pos2(label_right, row_bottom),
            ],
            egui::Stroke::new(1.0_f32, GRID),
        );
        lane_painter.text(
            egui::pos2(viewport.left() + 6.0, row_top + TIMELINE_ROW_HEIGHT * 0.5),
            egui::Align2::LEFT_CENTER,
            context_label(model, *lane),
            egui::FontId::monospace(11.0),
            TEXT_MUTED,
        );
    }
}

fn paint_timeline_time_grid(
    painter: &egui::Painter,
    canvas: egui::Rect,
    plot_left: f32,
    plot_right: f32,
    start_ticks: u64,
    duration: u64,
) {
    let plot_width = (plot_right - plot_left).max(1.0);
    let divisions = ((plot_width / 120.0).floor() as usize).clamp(2, 512);
    for index in 0..=divisions {
        let fraction = index as f32 / divisions as f32;
        let x = plot_left + plot_width * fraction;
        let tick_offset = (u128::from(duration) * index as u128 / divisions as u128) as u64;
        painter.line_segment(
            [
                egui::pos2(x, canvas.top() + TIMELINE_HEADER_HEIGHT),
                egui::pos2(x, canvas.bottom()),
            ],
            egui::Stroke::new(0.5_f32, GRID),
        );
        painter.text(
            egui::pos2(x, canvas.top() + 2.0),
            if index == divisions {
                egui::Align2::RIGHT_TOP
            } else {
                egui::Align2::LEFT_TOP
            },
            format!("{}", start_ticks.saturating_add(tick_offset)),
            egui::FontId::monospace(11.0),
            TEXT_MUTED,
        );
    }
}

fn tick_x(timestamp: u64, start: u64, duration: u64, left: f32, width: f32) -> f32 {
    let elapsed = timestamp.saturating_sub(start).min(duration);
    left + elapsed as f32 / duration as f32 * width
}

fn context_label(model: &TraceModel, lane: TimelineLane) -> String {
    let label = match lane.context {
        ExecutionContext::Task(task_id) => model
            .core(lane.core_id)
            .and_then(|core| core.tasks().get(&task_id))
            .and_then(|task| task.name.as_deref())
            .map_or_else(|| format!("Task {task_id}"), ToOwned::to_owned),
        ExecutionContext::Isr(isr_id) => format!("ISR {isr_id}"),
        ExecutionContext::Timer(timer_id) => format!("Timer {timer_id}"),
        ExecutionContext::Scheduler => "Scheduler".to_owned(),
        ExecutionContext::Idle => "Idle".to_owned(),
    };
    format!("C{}  {label}", lane.core_id)
}

fn context_color(context: ExecutionContext) -> egui::Color32 {
    match context {
        ExecutionContext::Task(_) => ACCENT,
        ExecutionContext::Isr(_) => ORANGE,
        ExecutionContext::Timer(_) => egui::Color32::from_rgb(187, 134, 252),
        ExecutionContext::Scheduler => egui::Color32::from_rgb(105, 138, 180),
        ExecutionContext::Idle => egui::Color32::from_rgb(72, 160, 116),
    }
}

fn paint_event_instrument(ui: &mut egui::Ui, trace: &mut SystemTraceDisplayState) {
    instrument_frame().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut trace.event_view, TraceEventView::Events, "EVENTS");
            ui.selectable_value(&mut trace.event_view, TraceEventView::Logs, "LOGS");
            ui.selectable_value(
                &mut trace.event_view,
                TraceEventView::Terminal,
                "RTT TERMINAL",
            );
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut trace.event_filter)
                    .hint_text("Filter")
                    .desired_width(180.0),
            );
            if trace.event_view == TraceEventView::Terminal {
                ui.checkbox(&mut trace.terminal_wrap, "Wrap text");
            }
            ui.checkbox(&mut trace.follow_latest, "Follow latest");
        });
        ui.add_space(5.0);
        let footer_height = if matches!(
            trace.event_view,
            TraceEventView::Events | TraceEventView::Logs
        ) && trace.selected_event().is_some()
        {
            SELECTED_EVENT_FOOTER_HEIGHT
        } else {
            0.0
        };
        let event_height = (ui.available_height() - footer_height).max(EVENT_TABLE_ROW_HEIGHT * 2.0);
        match trace.event_view {
            TraceEventView::Terminal => {
                if trace.terminal_lines().is_empty() {
                    paint_trace_empty_state(
                        ui,
                        "No plain RTT terminal output yet",
                        "Text from the configured RTT up-channel will appear here after connecting.",
                        event_height,
                    );
                } else {
                    paint_terminal_table(ui, trace, event_height);
                }
            }
            TraceEventView::Events | TraceEventView::Logs => {
                if trace.events().is_empty() {
                    paint_trace_empty_state(
                        ui,
                        "Connect a debug probe to begin",
                        "Task, interrupt, timer, marker, log, and data events will appear here.",
                        event_height,
                    );
                } else {
                    paint_event_table(ui, trace, event_height);
                    paint_selected_event(ui, trace);
                }
            }
        }
    });
}

fn paint_terminal_table(ui: &mut egui::Ui, trace: &SystemTraceDisplayState, view_height: f32) {
    let filter = trace.event_filter.trim().to_lowercase();
    let visible_lines = trace
        .terminal_lines()
        .iter()
        .filter(|line| filter.is_empty() || line.text.to_lowercase().contains(filter.as_str()))
        .collect::<Vec<_>>();
    let line_count = visible_lines.len();

    egui::Frame::none().fill(BACKGROUND).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.set_min_height(view_height);
        if trace.terminal_wrap {
            egui::ScrollArea::vertical()
                .id_salt("system_trace_rtt_terminal")
                .max_height(view_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    paint_terminal_header(ui);
                    for (row, line) in visible_lines.iter().enumerate() {
                        let response = paint_terminal_line(ui, line, true);
                        if trace.follow_latest && row + 1 == line_count {
                            response.scroll_to_me(Some(egui::Align::BOTTOM));
                        }
                    }
                });
        } else {
            let table_row_count = line_count + 1;
            let table_width = ui.available_width().max(TERMINAL_TABLE_MINIMUM_WIDTH);
            egui::ScrollArea::both()
                .id_salt("system_trace_rtt_terminal")
                .max_height(view_height)
                .auto_shrink([false, false])
                .show_rows(ui, 22.0, table_row_count, |ui, row_range| {
                    ui.set_min_width(table_width);
                    for table_row in row_range {
                        if table_row == 0 {
                            paint_terminal_header(ui);
                            continue;
                        }
                        let row = table_row - 1;
                        let response = paint_terminal_line(ui, visible_lines[row], false);
                        if trace.follow_latest && row + 1 == line_count {
                            response.scroll_to_me(Some(egui::Align::BOTTOM));
                        }
                    }
                });
        }
    });
}

fn paint_terminal_header(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [70.0, 20.0],
            egui::Label::new(RichText::new("LINE").small().color(TEXT_MUTED)),
        );
        ui.label(RichText::new("TEXT").small().color(TEXT_MUTED));
    });
}

fn paint_terminal_line(ui: &mut egui::Ui, line: &RttTerminalLine, wrap: bool) -> egui::Response {
    ui.horizontal_top(|ui| {
        ui.add_sized(
            [70.0, 20.0],
            egui::Label::new(
                RichText::new(line.sequence.to_string())
                    .monospace()
                    .color(TEXT_MUTED),
            ),
        );
        let label = egui::Label::new(terminal_line_layout(ui, line));
        ui.add(if wrap { label.wrap() } else { label.extend() });
    })
    .response
}

fn terminal_line_layout(ui: &egui::Ui, line: &RttTerminalLine) -> egui::text::LayoutJob {
    let mut layout = egui::text::LayoutJob::default();
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let default_color = ui.visuals().text_color();
    let tagged_log_color = terminal_tagged_log_color(&line.text);
    for span in &line.spans {
        let mut color = span.style.foreground.map_or_else(
            || tagged_log_color.unwrap_or(default_color),
            |color| terminal_color(color, span.style.bold),
        );
        if span.style.dim {
            color = dim_terminal_color(color);
        }
        let background = span
            .style
            .background
            .map_or(egui::Color32::TRANSPARENT, |color| {
                terminal_color(color, false)
            });
        layout.append(
            &span.text,
            0.0,
            egui::TextFormat {
                font_id: font_id.clone(),
                color,
                background,
                ..egui::TextFormat::default()
            },
        );
    }
    if !line.complete {
        layout.append(
            " …",
            0.0,
            egui::TextFormat {
                font_id,
                color: TEXT_MUTED,
                ..egui::TextFormat::default()
            },
        );
    }
    layout
}

fn terminal_tagged_log_color(text: &str) -> Option<egui::Color32> {
    let text = text.trim_start().to_ascii_lowercase();
    if has_terminal_log_tag(&text, &["error", "err", "critical", "fatal"]) {
        Some(egui::Color32::from_rgb(245, 96, 96))
    } else if has_terminal_log_tag(&text, &["warning", "warn"]) {
        Some(ORANGE)
    } else if has_terminal_log_tag(&text, &["info", "information"]) {
        Some(ACCENT)
    } else if has_terminal_log_tag(&text, &["debug"]) {
        Some(egui::Color32::from_rgb(105, 180, 255))
    } else if has_terminal_log_tag(&text, &["trace", "verbose"]) {
        Some(egui::Color32::from_rgb(187, 134, 252))
    } else {
        None
    }
}

fn has_terminal_log_tag(text: &str, levels: &[&str]) -> bool {
    levels.iter().any(|level| {
        text.strip_prefix('<')
            .and_then(|text| text.strip_prefix(level))
            .is_some_and(|text| text.starts_with('>'))
            || text
                .strip_prefix('[')
                .and_then(|text| text.strip_prefix(level))
                .is_some_and(|text| text.starts_with(']'))
            || text
                .strip_prefix(level)
                .is_some_and(|text| text.starts_with(':') || text.starts_with(char::is_whitespace))
    })
}

fn terminal_color(color: RttTerminalColor, bold: bool) -> egui::Color32 {
    match color {
        RttTerminalColor::Basic { index, bright } => terminal_basic_color(index, bright || bold),
        RttTerminalColor::Indexed(index) if index < 16 => {
            terminal_basic_color(index % 8, index >= 8)
        }
        RttTerminalColor::Indexed(index) if index < 232 => {
            let cube_index = index - 16;
            let levels = [0, 95, 135, 175, 215, 255];
            let red = levels[usize::from(cube_index / 36)];
            let green = levels[usize::from((cube_index % 36) / 6)];
            let blue = levels[usize::from(cube_index % 6)];
            egui::Color32::from_rgb(red, green, blue)
        }
        RttTerminalColor::Indexed(index) => {
            let level = 8_u8.saturating_add((index - 232).saturating_mul(10));
            egui::Color32::from_gray(level)
        }
        RttTerminalColor::Rgb { red, green, blue } => egui::Color32::from_rgb(red, green, blue),
    }
}

fn terminal_basic_color(index: u8, bright: bool) -> egui::Color32 {
    const STANDARD: [[u8; 3]; 8] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
    ];
    const BRIGHT: [[u8; 3]; 8] = [
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    let palette = if bright { BRIGHT } else { STANDARD };
    let [red, green, blue] = palette
        .get(usize::from(index))
        .copied()
        .unwrap_or(STANDARD[7]);
    egui::Color32::from_rgb(red, green, blue)
}

fn dim_terminal_color(color: egui::Color32) -> egui::Color32 {
    egui::Color32::from_rgba_premultiplied(
        color.r().saturating_mul(2) / 3,
        color.g().saturating_mul(2) / 3,
        color.b().saturating_mul(2) / 3,
        color.a(),
    )
}

fn paint_event_table(ui: &mut egui::Ui, trace: &mut SystemTraceDisplayState, view_height: f32) {
    let filter = trace.event_filter.trim().to_lowercase();
    let visible_indices = trace
        .events()
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            let view_matches = trace.event_view == TraceEventView::Events
                || matches!(event.kind, TraceEventKind::FormattedPrint { .. });
            view_matches
                && (filter.is_empty()
                    || event_search_text(event)
                        .to_lowercase()
                        .contains(filter.as_str()))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut selected_sequence = trace.selected_sequence;
    let event_table_focus = trace.take_event_table_focus();
    let follow_latest = trace.follow_latest && event_table_focus.is_none();
    let event_count = visible_indices.len();
    let table_width = ui.available_width().max(EVENT_TABLE_MINIMUM_WIDTH);
    let focus_row = event_table_focus.and_then(|sequence| {
        visible_indices
            .iter()
            .position(|index| trace.events()[*index].sequence == sequence)
    });

    let header_width = ui.available_width();
    let (header_rect, _) = ui.allocate_exact_size(
        egui::vec2(header_width, EVENT_TABLE_ROW_HEIGHT),
        egui::Sense::hover(),
    );
    let body_height = (view_height - EVENT_TABLE_ROW_HEIGHT - ui.spacing().item_spacing.y)
        .max(EVENT_TABLE_ROW_HEIGHT);
    let event_row_stride = EVENT_TABLE_ROW_HEIGHT + ui.spacing().item_spacing.y;

    let mut event_scroll = egui::ScrollArea::both()
        .id_salt("system_trace_events")
        .max_height(body_height)
        .auto_shrink([false, false]);
    if let Some(row) = focus_row {
        event_scroll = event_scroll.vertical_scroll_offset(
            (row as f32 * event_row_stride + event_row_stride * 0.5 - body_height * 0.5).max(0.0),
        );
    }
    let mut clicked_sequence = None;
    let event_scroll =
        event_scroll.show_rows(ui, EVENT_TABLE_ROW_HEIGHT, event_count, |ui, row_range| {
            ui.set_min_width(table_width);
            for visible_row in row_range {
                let event = &trace.events()[visible_indices[visible_row]];
                let event_selected = selected_sequence == Some(event.sequence);
                let row = egui::Frame::none()
                    .fill(if event_selected {
                        TRACE_EVENT_SELECTION_FILL
                    } else {
                        egui::Color32::TRANSPARENT
                    })
                    .stroke(if event_selected {
                        egui::Stroke::new(1.0_f32, TRACE_SELECTION_COLOR)
                    } else {
                        egui::Stroke::NONE
                    })
                    .show(ui, |ui| {
                        ui.set_min_width(table_width);
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [90.0, 20.0],
                                egui::Label::new(
                                    RichText::new(event.timestamp_ticks.to_string())
                                        .monospace()
                                        .color(if event_selected {
                                            TRACE_SELECTION_COLOR
                                        } else {
                                            ui.visuals().text_color()
                                        }),
                                ),
                            );
                            ui.add_sized(
                                [45.0, 20.0],
                                egui::Label::new(RichText::new(event.core_id.to_string()).color(
                                    if event_selected {
                                        TRACE_SELECTION_COLOR
                                    } else {
                                        ui.visuals().text_color()
                                    },
                                )),
                            );
                            let clicked = ui
                                .add_sized(
                                    [170.0, 20.0],
                                    egui::SelectableLabel::new(
                                        event_selected,
                                        event_name(&event.kind),
                                    ),
                                )
                                .clicked();
                            ui.label(RichText::new(event_summary(&event.kind)).color(
                                if event_selected {
                                    TRACE_SELECTION_COLOR
                                } else {
                                    TEXT_MUTED
                                },
                            ));
                            clicked
                        })
                    });
                let row_clicked = ui
                    .interact(
                        row.response.rect,
                        ui.id().with(("trace_event_row", event.sequence)),
                        egui::Sense::click(),
                    )
                    .clicked();
                if row.inner.inner || row_clicked {
                    selected_sequence = Some(event.sequence);
                    clicked_sequence = Some(event.sequence);
                }
                if follow_latest && visible_row + 1 == event_count {
                    row.response.scroll_to_me(Some(egui::Align::BOTTOM));
                }
            }
        });
    paint_event_table_header(
        ui,
        header_rect,
        event_scroll.inner_rect.right(),
        event_scroll.state.offset.x,
    );
    if let Some(sequence) = clicked_sequence {
        trace.select_from_event_table(sequence);
    } else {
        trace.selected_sequence = selected_sequence;
    }
}

fn paint_event_table_header(
    ui: &mut egui::Ui,
    header_rect: egui::Rect,
    body_right: f32,
    horizontal_offset: f32,
) {
    let header_rect = egui::Rect::from_min_max(
        header_rect.min,
        egui::pos2(body_right, header_rect.bottom()),
    );
    let painter = ui.painter().with_clip_rect(header_rect);
    painter.rect_filled(header_rect, 0.0, BACKGROUND);
    painter.line_segment(
        [header_rect.left_bottom(), header_rect.right_bottom()],
        egui::Stroke::new(1.0_f32, GRID),
    );

    let content_rect = egui::Rect::from_min_size(
        egui::pos2(header_rect.left() - horizontal_offset, header_rect.top()),
        egui::vec2(
            EVENT_TABLE_MINIMUM_WIDTH.max(header_rect.width()),
            EVENT_TABLE_ROW_HEIGHT,
        ),
    );
    let mut header_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("system_trace_events_header")
            .max_rect(content_rect),
    );
    header_ui.set_clip_rect(header_rect);
    header_ui.horizontal(|ui| {
        ui.add_sized(
            [90.0, 20.0],
            egui::Label::new(RichText::new("TICKS").small().color(TEXT_MUTED)),
        );
        ui.add_sized(
            [45.0, 20.0],
            egui::Label::new(RichText::new("CORE").small().color(TEXT_MUTED)),
        );
        ui.add_sized(
            [170.0, 20.0],
            egui::Label::new(RichText::new("EVENT").small().color(TEXT_MUTED)),
        );
        ui.label(RichText::new("DETAILS").small().color(TEXT_MUTED));
    });
}

fn paint_selected_event(ui: &mut egui::Ui, trace: &SystemTraceDisplayState) {
    let Some(event) = trace.selected_event() else {
        return;
    };
    ui.separator();
    egui::ScrollArea::horizontal()
        .id_salt("system_trace_selected_event")
        .max_height(SELECTED_EVENT_FOOTER_HEIGHT)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SELECTED").strong().color(ACCENT));
                ui.label(format!("sequence {}", event.sequence));
                ui.label(format!("core {}", event.core_id));
                ui.label(format!("{} ticks", event.timestamp_ticks));
                ui.label(
                    RichText::new(format!("{:?}", event.kind))
                        .monospace()
                        .color(TEXT_MUTED),
                );
            });
        });
}

fn paint_trace_empty_state(ui: &mut egui::Ui, title: &str, description: &str, minimum_height: f32) {
    const EMPTY_STATE_VERTICAL_MARGIN: f32 = 24.0;
    egui::Frame::none()
        .fill(BACKGROUND)
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height((minimum_height - EMPTY_STATE_VERTICAL_MARGIN).max(0.0));
            ui.label(RichText::new(title).strong());
            ui.label(RichText::new(description).color(TEXT_MUTED));
        });
}

fn event_search_text(event: &TraceEvent) -> String {
    format!(
        "{} {} {} {}",
        event.timestamp_ticks,
        event.core_id,
        event_name(&event.kind),
        event_summary(&event.kind)
    )
}

fn event_name(kind: &TraceEventKind) -> &'static str {
    match kind {
        TraceEventKind::NoOp => "No-op",
        TraceEventKind::TraceStarted => "Trace started",
        TraceEventKind::TraceStopped => "Trace stopped",
        TraceEventKind::Overflow { .. } => "Overflow",
        TraceEventKind::TaskCreated { .. } => "Task created",
        TraceEventKind::TaskInfo { .. } => "Task info",
        TraceEventKind::TaskStarted { .. } => "Task started",
        TraceEventKind::TaskStopped => "Task stopped",
        TraceEventKind::TaskBecameReady { .. } => "Task ready",
        TraceEventKind::TaskStoppedReady { .. } => "Task blocked",
        TraceEventKind::TaskTerminated { .. } => "Task terminated",
        TraceEventKind::IsrEntered { .. } => "ISR entered",
        TraceEventKind::IsrExited => "ISR exited",
        TraceEventKind::IsrExitedToScheduler => "ISR to scheduler",
        TraceEventKind::TimerEntered { .. } => "Timer entered",
        TraceEventKind::TimerExited => "Timer exited",
        TraceEventKind::Idle => "Idle",
        TraceEventKind::MarkerStarted { .. } => "Marker started",
        TraceEventKind::MarkerStopped { .. } => "Marker stopped",
        TraceEventKind::SystemDescription(_) => "System description",
        TraceEventKind::ClockSyncCycles { .. } => "Clock sync cycles",
        TraceEventKind::ClockSyncMicros { .. } => "Clock sync time",
        TraceEventKind::TargetInfo { .. } => "Target info",
        TraceEventKind::StackInfo { .. } => "Stack info",
        TraceEventKind::ModuleDescription { .. } => "Module description",
        TraceEventKind::ModuleCount { .. } => "Module count",
        TraceEventKind::ResourceName { .. } => "Resource name",
        TraceEventKind::DataSample { .. } => "Data sample",
        TraceEventKind::FormattedPrint { .. } => "Log",
        TraceEventKind::ApiCallEnded { .. } => "API call ended",
        TraceEventKind::Unknown { .. } => "Unknown event",
    }
}

fn event_summary(kind: &TraceEventKind) -> String {
    match kind {
        TraceEventKind::Overflow { dropped_events } => format!("{dropped_events} events dropped"),
        TraceEventKind::TaskCreated { task_id }
        | TraceEventKind::TaskStarted { task_id }
        | TraceEventKind::TaskBecameReady { task_id }
        | TraceEventKind::TaskTerminated { task_id } => format!("task {task_id}"),
        TraceEventKind::TaskInfo {
            task_id,
            priority,
            name,
        } => format!("task {task_id}, priority {priority}, {name}"),
        TraceEventKind::TaskStoppedReady { task_id, cause } => {
            format!("task {task_id}, cause {cause}")
        }
        TraceEventKind::IsrEntered { isr_id } => format!("ISR {isr_id}"),
        TraceEventKind::TimerEntered { timer_id } => format!("timer {timer_id}"),
        TraceEventKind::MarkerStarted { marker_id }
        | TraceEventKind::MarkerStopped { marker_id } => format!("marker {marker_id}"),
        TraceEventKind::SystemDescription(description) => description.clone(),
        TraceEventKind::ClockSyncCycles { cycles } => format!("{cycles} cycles"),
        TraceEventKind::ClockSyncMicros { micros } => format!("{micros} µs"),
        TraceEventKind::TargetInfo {
            system_frequency_hz,
            cpu_frequency_hz,
            ram_base,
            id_shift,
        } => format!(
            "system {system_frequency_hz} Hz, CPU {cpu_frequency_hz} Hz, RAM 0x{ram_base:x}, ID shift {id_shift}"
        ),
        TraceEventKind::StackInfo {
            task_id,
            stack_base,
            stack_size,
            stack_usage,
        } => format!(
            "task {task_id}, base 0x{stack_base:x}, {stack_usage}/{stack_size} bytes"
        ),
        TraceEventKind::ModuleDescription {
            module_id,
            event_offset,
            name,
        } => format!("module {module_id}, offset {event_offset}, {name}"),
        TraceEventKind::ModuleCount { count } => format!("{count} modules"),
        TraceEventKind::ResourceName { resource_id, name } => {
            format!("resource {resource_id}, {name}")
        }
        TraceEventKind::DataSample { sample_id, value } => {
            format!("sample {sample_id}: {value}")
        }
        TraceEventKind::FormattedPrint {
            format,
            options,
            arguments,
        } => format!("{format}  args={arguments:?}, options={options}"),
        TraceEventKind::ApiCallEnded {
            event_id,
            return_value,
        } => format!("event {event_id}, return {return_value:?}"),
        TraceEventKind::Unknown { event_id, payload } => {
            format!("event {event_id}, {} raw bytes", payload.len())
        }
        TraceEventKind::NoOp
        | TraceEventKind::TraceStarted
        | TraceEventKind::TraceStopped
        | TraceEventKind::TaskStopped
        | TraceEventKind::IsrExited
        | TraceEventKind::IsrExitedToScheduler
        | TraceEventKind::TimerExited
        | TraceEventKind::Idle => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app::test_support::app;

    fn paint_toolbar(
        context: &egui::Context,
        app: &mut WaveCraftApp,
        events: Vec<egui::Event>,
    ) -> (FrameActions, Vec<(String, egui::Pos2)>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 300.0),
            )),
            events,
            ..egui::RawInput::default()
        };
        let mut actions = FrameActions::default();
        let output = context.run(input, |context| {
            app.paint_system_trace_toolbar(context, &mut actions);
        });
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
        (actions, texts)
    }

    fn pointer_button(position: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn paint_central_texts(context: &egui::Context, app: &mut WaveCraftApp) -> Vec<String> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..egui::RawInput::default()
        };
        let output = context.run(input, |context| {
            app.paint_system_trace_central_panel(context);
        });
        let mut texts = Vec::new();
        fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|shape| collect(shape, texts));
                }
                egui::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
                _ => {}
            }
        }
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut texts);
        }
        texts
    }

    fn shape_contains_fill(shape: &egui::Shape, expected: egui::Color32) -> bool {
        match shape {
            egui::Shape::Rect(rect) => rect.fill == expected,
            egui::Shape::Vec(shapes) => shapes
                .iter()
                .any(|shape| shape_contains_fill(shape, expected)),
            _ => false,
        }
    }

    fn shape_contains_text(shape: &egui::Shape, expected: &str) -> bool {
        match shape {
            egui::Shape::Text(text) => text.galley.text().contains(expected),
            egui::Shape::Vec(shapes) => shapes
                .iter()
                .any(|shape| shape_contains_text(shape, expected)),
            _ => false,
        }
    }

    fn collect_rects_with_stroke(
        shape: &egui::Shape,
        expected: egui::Color32,
        rects: &mut Vec<egui::Rect>,
    ) {
        match shape {
            egui::Shape::Rect(rect) if rect.stroke.color == expected => rects.push(rect.rect),
            egui::Shape::Vec(shapes) => shapes
                .iter()
                .for_each(|shape| collect_rects_with_stroke(shape, expected, rects)),
            _ => {}
        }
    }

    fn collect_rects_with_fill(
        shape: &egui::Shape,
        expected: egui::Color32,
        rects: &mut Vec<egui::Rect>,
    ) {
        match shape {
            egui::Shape::Rect(rect) if rect.fill == expected => rects.push(rect.rect),
            egui::Shape::Vec(shapes) => shapes
                .iter()
                .for_each(|shape| collect_rects_with_fill(shape, expected, rects)),
            _ => {}
        }
    }

    fn text_row_count(shape: &egui::Shape, expected: &str) -> Option<usize> {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == expected => {
                Some(text.galley.rows.len())
            }
            egui::Shape::Vec(shapes) => shapes
                .iter()
                .find_map(|shape| text_row_count(shape, expected)),
            _ => None,
        }
    }

    #[test]
    fn system_trace_toolbar_can_request_an_application_device_scan() {
        let context = egui::Context::default();
        let mut app = app();
        let (_, texts) = paint_toolbar(&context, &mut app, Vec::new());
        let scan_position = texts
            .iter()
            .find(|(text, _)| text == "↻ Scan devices")
            .map(|(_, position)| *position)
            .expect("scan button is painted");

        let _ = paint_toolbar(
            &context,
            &mut app,
            vec![
                egui::Event::PointerMoved(scan_position),
                pointer_button(scan_position, true),
            ],
        );
        let (actions, _) = paint_toolbar(
            &context,
            &mut app,
            vec![pointer_button(scan_position, false)],
        );

        assert!(actions.refresh_clicked);
    }

    #[test]
    fn system_trace_toolbar_exposes_systemview_file_import() {
        let context = egui::Context::default();
        let mut app = app();

        let (_, texts) = paint_toolbar(&context, &mut app, Vec::new());

        assert!(texts.iter().any(|(text, _)| text == "Open .SVDat…"));
    }

    #[test]
    fn system_trace_scan_and_open_follow_left_toolbar_order() {
        let context = egui::Context::default();
        let mut app = app();
        let (_, texts) = paint_toolbar(&context, &mut app, Vec::new());
        let position = |label: &str| {
            texts
                .iter()
                .find(|(text, _)| text == label)
                .map(|(_, position)| *position)
                .unwrap_or_else(|| panic!("{label} is painted"))
        };

        let title = position("System and RTOS trace");
        let scan = position("↻ Scan devices");
        let record = position("● Record…");
        let open = position("Open .SVDat…");

        assert!(title.x < scan.x);
        assert!(scan.x < record.x);
        assert!(record.x < open.x);
    }

    #[test]
    fn attached_system_trace_toolbar_exposes_raw_recording() {
        let context = egui::Context::default();
        let mut app = app();
        app.system_trace_connection = SystemTraceConnectionState::Attached {
            channels: Vec::new(),
            systemview_enabled: true,
        };

        let (_, texts) = paint_toolbar(&context, &mut app, Vec::new());

        assert!(texts.iter().any(|(text, _)| text == "● Record…"));
        assert!(texts.iter().any(|(text, _)| text == "▶ Start"));
    }

    #[test]
    fn system_trace_toolbar_clears_trace_and_terminal_data() {
        let context = egui::Context::default();
        let mut app = app();
        app.system_trace.ingest(TraceEvent {
            sequence: 1,
            core_id: 0,
            timestamp_ticks: 10,
            kind: TraceEventKind::TraceStarted,
        });
        app.system_trace
            .ingest_terminal_line(wavecraft_debug_probe::RttTerminalLine::plain(
                0, "booted", true,
            ));
        let (_, texts) = paint_toolbar(&context, &mut app, Vec::new());
        let clear_position = texts
            .iter()
            .find(|(text, _)| text == "Clear")
            .map(|(_, position)| *position)
            .expect("clear button is painted");

        let _ = paint_toolbar(
            &context,
            &mut app,
            vec![
                egui::Event::PointerMoved(clear_position),
                pointer_button(clear_position, true),
            ],
        );
        let _ = paint_toolbar(
            &context,
            &mut app,
            vec![pointer_button(clear_position, false)],
        );

        assert!(app.system_trace.events().is_empty());
        assert!(app.system_trace.terminal_lines().is_empty());
    }

    #[test]
    fn rtt_terminal_tab_paints_plain_text_lines() {
        let context = egui::Context::default();
        let mut app = app();
        app.system_trace.event_view = TraceEventView::Terminal;
        app.system_trace
            .ingest_terminal_line(wavecraft_debug_probe::RttTerminalLine::plain(
                0,
                "nRF52832 booted",
                true,
            ));

        let texts = paint_central_texts(&context, &mut app);

        assert!(texts.iter().any(|text| text == "RTT TERMINAL"));
        assert!(texts.iter().any(|text| text == "Wrap text"));
        assert!(texts.iter().any(|text| text == "nRF52832 booted"));
    }

    #[test]
    fn rtt_terminal_output_uses_the_log_background() {
        let context = egui::Context::default();
        let mut trace = SystemTraceDisplayState::default();
        trace.ingest_terminal_line(wavecraft_debug_probe::RttTerminalLine::plain(
            0,
            "nRF52832 booted",
            true,
        ));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 300.0),
            )),
            ..egui::RawInput::default()
        };

        let output = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_terminal_table(ui, &trace, 180.0);
            });
        });
        let mut background_rects = Vec::new();
        output.shapes.iter().for_each(|clipped| {
            collect_rects_with_fill(&clipped.shape, BACKGROUND, &mut background_rects);
        });

        assert!(background_rects
            .iter()
            .any(|rect| rect.width() > 700.0 && rect.height() >= 180.0));
    }

    #[test]
    fn rtt_terminal_wrap_setting_wraps_long_messages() {
        let context = egui::Context::default();
        let message = "long RTT message that should wrap across several visual rows when the terminal is narrow";
        let mut trace = SystemTraceDisplayState::default();
        trace.ingest_terminal_line(wavecraft_debug_probe::RttTerminalLine::plain(
            0, message, true,
        ));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 240.0),
            )),
            ..egui::RawInput::default()
        };

        let output = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_terminal_table(ui, &trace, 160.0);
            });
        });
        let wrapped_rows = output
            .shapes
            .iter()
            .find_map(|clipped| text_row_count(&clipped.shape, message))
            .expect("terminal message is painted");

        assert!(wrapped_rows > 1);

        trace.terminal_wrap = false;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 240.0),
            )),
            ..egui::RawInput::default()
        };
        let output = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_terminal_table(ui, &trace, 160.0);
            });
        });
        let unwrapped_rows = output
            .shapes
            .iter()
            .find_map(|clipped| text_row_count(&clipped.shape, message))
            .expect("terminal message is painted without wrapping");

        assert_eq!(unwrapped_rows, 1);
    }

    #[test]
    fn rtt_terminal_maps_ansi_palettes_to_display_colors() {
        assert_eq!(
            terminal_color(
                RttTerminalColor::Basic {
                    index: 1,
                    bright: false,
                },
                false,
            ),
            egui::Color32::from_rgb(205, 0, 0)
        );
        assert_eq!(
            terminal_color(RttTerminalColor::Indexed(196), false),
            egui::Color32::from_rgb(255, 0, 0)
        );
        assert_eq!(
            terminal_color(
                RttTerminalColor::Rgb {
                    red: 12,
                    green: 34,
                    blue: 56,
                },
                false,
            ),
            egui::Color32::from_rgb(12, 34, 56)
        );
    }

    #[test]
    fn timeline_zoom_expands_the_scrollable_time_canvas() {
        assert_eq!(timeline_canvas_width(600.0, 1.0), 720.0);
        assert_eq!(timeline_canvas_width(900.0, 1.0), 900.0);
        assert_eq!(timeline_canvas_width(900.0, 4.0), 3_600.0);
    }

    #[test]
    fn timeline_focus_centers_selected_timestamp_within_scroll_bounds() {
        assert_eq!(timeline_focus_offset(900.0, 1.0, 0, 1_000, 500), 0.0);
        let offset = timeline_focus_offset(900.0, 8.0, 0, 1_000, 500);
        assert!(offset > 2_000.0);
        assert!(offset < 4_000.0);
        assert_eq!(timeline_focus_offset(900.0, 8.0, 0, 1_000, 0), 0.0);
    }

    #[test]
    fn timeline_intervals_resolve_their_originating_events() {
        let mut trace = SystemTraceDisplayState::default();
        trace.ingest(TraceEvent {
            sequence: 41,
            core_id: 0,
            timestamp_ticks: 120,
            kind: TraceEventKind::TaskStarted { task_id: 7 },
        });

        assert_eq!(
            interval_event_sequence(
                &trace,
                TimelineLane {
                    core_id: 0,
                    context: ExecutionContext::Task(7),
                },
                120,
            ),
            Some(41)
        );
    }

    #[test]
    fn timeline_smart_timing_snaps_to_interval_edges_with_half_transparent_hover() {
        let context = egui::Context::default();
        let mut trace = SystemTraceDisplayState::default();
        for event in [
            TraceEvent {
                sequence: 0,
                core_id: 0,
                timestamp_ticks: 0,
                kind: TraceEventKind::TargetInfo {
                    system_frequency_hz: 1_000_000,
                    cpu_frequency_hz: 64_000_000,
                    ram_base: 0x2000_0000,
                    id_shift: 0,
                },
            },
            TraceEvent {
                sequence: 1,
                core_id: 0,
                timestamp_ticks: 0,
                kind: TraceEventKind::TaskInfo {
                    task_id: 7,
                    priority: 1,
                    name: "app_main".to_owned(),
                },
            },
            TraceEvent {
                sequence: 2,
                core_id: 0,
                timestamp_ticks: 100,
                kind: TraceEventKind::TaskStarted { task_id: 7 },
            },
            TraceEvent {
                sequence: 3,
                core_id: 0,
                timestamp_ticks: 160,
                kind: TraceEventKind::TaskStopped,
            },
        ] {
            trace.ingest(event);
        }
        let lanes = timeline_lanes(trace.model());
        let canvas = egui::Rect::from_min_size(
            egui::pos2(10.0, 10.0),
            egui::vec2(
                700.0,
                TIMELINE_HEADER_HEIGHT + lanes.len() as f32 * TIMELINE_ROW_HEIGHT,
            ),
        );
        let pointer = egui::pos2(600.0, canvas.top() + TIMELINE_HEADER_HEIGHT + 12.0);
        let warm_up = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(760.0, 180.0),
            )),
            ..egui::RawInput::default()
        };
        let _ = context.run(warm_up, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_timeline_canvas(ui, canvas, &trace, &lanes, 0, 160, None);
            });
        });
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(760.0, 180.0),
            )),
            events: vec![egui::Event::PointerMoved(pointer)],
            ..egui::RawInput::default()
        };

        let output = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_timeline_canvas(ui, canvas, &trace, &lanes, 0, 160, None);
            });
        });
        let hover_fill = half_transparent_plate(PANEL);

        assert_eq!(hover_fill.a(), 128);
        assert!(output
            .shapes
            .iter()
            .any(|clipped| shape_contains_fill(&clipped.shape, hover_fill)));
        assert!(output
            .shapes
            .iter()
            .any(|clipped| shape_contains_text(&clipped.shape, "SMART TIMING · C0  app_main")));
        assert!(output
            .shapes
            .iter()
            .any(|clipped| shape_contains_text(&clipped.shape, "100 → 160 ticks · Δ 60 ticks")));
    }

    #[test]
    fn timeline_selection_paints_event_table_highlight() {
        let context = egui::Context::default();
        let mut trace = SystemTraceDisplayState::default();
        for sequence in 0..100 {
            trace.ingest(TraceEvent {
                sequence,
                core_id: 0,
                timestamp_ticks: sequence * 10,
                kind: TraceEventKind::TaskStarted { task_id: 7 },
            });
        }
        trace.select_from_timeline(80);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 400.0),
            )),
            ..egui::RawInput::default()
        };

        let output = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_event_table(ui, &mut trace, 300.0);
            });
        });

        assert_eq!(trace.selected_sequence, Some(80));
        assert!(output
            .shapes
            .iter()
            .any(|clipped| shape_contains_fill(&clipped.shape, TRACE_EVENT_SELECTION_FILL)));
    }

    #[test]
    fn timeline_exposes_zoom_controls_without_trace_data() {
        let context = egui::Context::default();
        let mut app = app();

        let texts = paint_central_texts(&context, &mut app);

        assert!(texts.iter().any(|text| text == "1.0×"));
        assert!(texts.iter().any(|text| text == "Fit"));
        assert!(texts.iter().any(|text| text == "Smart timing"));
        assert!(texts.iter().any(|text| text == "+"));
        assert!(texts.iter().any(|text| text == "−"));
    }

    #[test]
    fn trace_panes_shrink_to_a_compact_main_window() {
        let context = egui::Context::default();
        let mut app = app();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 420.0),
            )),
            ..egui::RawInput::default()
        };

        let _ = context.run(input, |context| {
            app.paint_system_trace_central_panel(context);
        });

        let pane_height =
            app.system_trace.timeline_view_height() + app.system_trace.event_view_height();
        assert!(pane_height > 0.0);
        assert!(pane_height < 420.0);
    }

    #[test]
    fn empty_trace_panes_stay_within_compact_window_width() {
        let context = egui::Context::default();
        let mut app = app();
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 420.0));
        let input = egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        };

        let output = context.run(input, |context| {
            app.paint_system_trace_central_panel(context);
        });
        let mut bordered_rects = Vec::new();
        let mut background_rects = Vec::new();
        output.shapes.iter().for_each(|clipped| {
            collect_rects_with_stroke(&clipped.shape, GRID, &mut bordered_rects);
            collect_rects_with_fill(&clipped.shape, BACKGROUND, &mut background_rects);
        });

        assert!(!bordered_rects.is_empty());
        assert!(bordered_rects
            .iter()
            .all(|rect| rect.right() <= screen_rect.right() - 16.0));
        let expected_width = screen_rect.width() - 32.0;
        assert!(
            bordered_rects
                .iter()
                .filter(|rect| (rect.width() - expected_width).abs() < 1.0)
                .count()
                >= 2,
            "expected both trace panes to fill the available width: {bordered_rects:?}"
        );
        let expected_content_width = expected_width - 24.0;
        assert!(
            background_rects
                .iter()
                .filter(|rect| (rect.width() - expected_content_width).abs() < 1.0)
                .count()
                >= 2,
            "expected both empty surfaces to fill their pane width: {background_rects:?}"
        );
    }

    #[test]
    fn rtt_terminal_colors_plain_severity_tags() {
        assert_eq!(
            terminal_tagged_log_color("<debug> nrf_sdh: initialized"),
            Some(egui::Color32::from_rgb(105, 180, 255))
        );
        assert_eq!(
            terminal_tagged_log_color("  <INFO> app: started"),
            Some(ACCENT)
        );
        assert_eq!(
            terminal_tagged_log_color("[warn] low battery"),
            Some(ORANGE)
        );
        assert_eq!(
            terminal_tagged_log_color("ERROR: transport failed"),
            Some(egui::Color32::from_rgb(245, 96, 96))
        );
        assert_eq!(terminal_tagged_log_color("ordinary RTT output"), None);
    }

    #[test]
    fn connection_settings_apply_optional_reset_and_rtt_scan() {
        let settings = SystemTraceSettings {
            probe_connection_id: Some("1366:0105:1234".to_owned()),
            target_name: "STM32F407VGTx".to_owned(),
            speed_khz: 8_000,
            reset_after_attach: true,
            control_block_address: String::new(),
            systemview_enabled: true,
            up_channel: 1,
            down_channel: 1,
            terminal_enabled: true,
            terminal_up_channel: 0,
        };

        let configuration = settings
            .connection_configuration()
            .expect("settings are valid");

        assert_eq!(
            configuration.probe,
            ProbeSelector::connection_id("1366:0105:1234")
        );
        assert_eq!(
            configuration.attach.target,
            TargetSelector::Name("STM32F407VGTx".to_owned())
        );
        assert_eq!(configuration.attach.wire_protocol, Some(WireProtocol::Swd));
        assert_eq!(configuration.attach.speed_khz, Some(8_000));
        assert!(configuration.reset_after_attach);
        assert!(configuration.systemview_enabled);
        assert_eq!(configuration.terminal_up_channel, Some(0));
        assert_eq!(
            configuration.rtt.control_block,
            RttControlBlockLocation::Ram
        );
    }

    #[test]
    fn terminal_only_settings_do_not_require_systemview_channels() {
        let settings = SystemTraceSettings {
            probe_connection_id: Some("1366:0105:1234".to_owned()),
            systemview_enabled: false,
            terminal_enabled: true,
            ..SystemTraceSettings::default()
        };

        let configuration = settings
            .connection_configuration()
            .expect("terminal-only settings are valid");

        assert!(!configuration.systemview_enabled);
        assert_eq!(configuration.terminal_up_channel, Some(0));
    }

    #[test]
    fn target_filter_matches_names_and_family_tokens() {
        let target = DebugTargetDescriptor {
            name: "nRF52832_xxAA".to_owned(),
            family: "nRF52 Series".to_owned(),
        };

        assert!(target_matches_query(&target, ""));
        assert!(target_matches_query(&target, "52832"));
        assert!(target_matches_query(&target, "NRF52 series"));
        assert!(target_matches_query(&target, "nrf 32"));
        assert!(!target_matches_query(&target, "stm32"));
    }

    #[test]
    fn connection_requires_at_least_one_rtt_instrument() {
        let settings = SystemTraceSettings {
            probe_connection_id: Some("1366:0105:1234".to_owned()),
            systemview_enabled: false,
            terminal_enabled: false,
            ..SystemTraceSettings::default()
        };

        assert_eq!(
            settings.connection_configuration(),
            Err("Enable SystemView trace or the plain RTT terminal".to_owned())
        );
    }

    #[test]
    fn exact_rtt_control_block_accepts_hexadecimal_and_rejects_bad_text() {
        assert_eq!(
            parse_rtt_control_block("0x2000_0100"),
            Err("RTT control-block address \"0x2000_0100\" is not valid".to_owned())
        );
        assert_eq!(
            parse_rtt_control_block("0x20000100"),
            Ok(RttControlBlockLocation::Exact(0x2000_0100))
        );
    }
}
