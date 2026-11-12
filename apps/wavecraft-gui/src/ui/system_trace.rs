//! Independent System Trace workspace and decoded-event instruments.

use crate::system_trace::{
    choose_systemview_recording_path, choose_systemview_trace_to_open, SystemTraceAction,
    SystemTraceConnectionConfiguration, SystemTraceConnectionState,
};
use crate::system_trace::{SystemTraceDisplayState, TraceEventView};
use crate::{
    egui, toolbar_action_button, ui::frame_actions::FrameActions, RichText, WaveCraftApp, ACCENT,
    BACKGROUND, GRID, ORANGE, PANEL, SCOPE_COLORS, TEXT_MUTED,
};
use wavecraft_debug_probe::{
    AttachConfiguration, ProbeDescriptor, ProbeProvider, ProbeSelector, RttConfiguration,
    RttControlBlockLocation, RttTerminalColor, RttTerminalLine, TargetSelector, WireProtocol,
};
use wavecraft_trace::{ExecutionContext, TraceEvent, TraceEventKind, TraceModel};

const TIMELINE_LABEL_WIDTH: f32 = 150.0;
const TIMELINE_HEADER_HEIGHT: f32 = 24.0;
const TIMELINE_ROW_HEIGHT: f32 = 25.0;
const TIMELINE_MINIMUM_WIDTH: f32 = 720.0;
const TIMELINE_ZOOM_STEP: f32 = 1.5;

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
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        paint_trace_run_control(ui, &self.system_trace_connection, actions);
                        actions.refresh_clicked = ui
                            .add_enabled(
                                !self.is_busy() && self.system_trace_connection.is_disconnected(),
                                toolbar_action_button("↻ Scan devices", SCOPE_COLORS[1]),
                            )
                            .on_hover_text(
                                "Scan for debug probes, DSLogic, DSCope, and radio devices",
                            )
                            .clicked();
                        if ui
                            .add_enabled(
                                !self.is_busy() && self.system_trace_connection.is_disconnected(),
                                egui::Button::new("Open .SVDat…"),
                            )
                            .on_hover_text("Open a raw SEGGER SystemView event stream")
                            .clicked()
                        {
                            actions.system_trace_action = Some(SystemTraceAction::Open);
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
                        egui::ComboBox::from_id_salt("system_trace_probe")
                            .width(ui.available_width())
                            .selected_text(selected_probe)
                            .show_ui(ui, |ui| {
                                if self.debug_probes.is_empty() {
                                    ui.add_enabled(false, egui::Label::new("No probes found"));
                                }
                                for probe in &self.debug_probes {
                                    ui.selectable_value(
                                        &mut self.system_trace_settings.probe_connection_id,
                                        Some(probe.connection_id.clone()),
                                        probe_label(probe),
                                    )
                                    .on_hover_text(&probe.connection_id);
                                }
                            });
                        if let Some(error) = &self.debug_probe_discovery_error {
                            ui.label(
                                RichText::new(error)
                                    .small()
                                    .color(egui::Color32::from_rgb(245, 96, 96)),
                            );
                        }

                        ui.add_space(10.0);
                        setup_label(ui, "Target MCU");
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.system_trace_settings.target_name,
                            )
                            .hint_text("e.g. STM32F407VGTx"),
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
                egui::ScrollArea::vertical()
                    .id_salt("system_trace_workspace")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        paint_trace_summary(ui, &self.system_trace);
                        ui.add_space(10.0);
                        paint_timeline_instrument(ui, &mut self.system_trace);
                        ui.add_space(10.0);
                        paint_event_instrument(ui, &mut self.system_trace);
                    });
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
            if ui
                .add(toolbar_action_button(
                    "● Record…",
                    egui::Color32::from_rgb(245, 96, 96),
                ))
                .on_hover_text("Record the raw SystemView stream to an .SVDat file")
                .clicked()
            {
                actions.system_trace_action = Some(SystemTraceAction::Record);
            }
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
    let provider = match &probe.provider {
        ProbeProvider::JLink => "J-Link",
        ProbeProvider::StLink => "ST-Link",
        ProbeProvider::BlackMagicProbe => "Black Magic Probe",
        ProbeProvider::CmsisDap => "CMSIS-DAP",
        ProbeProvider::Other(provider) => provider,
    };
    let serial = probe
        .serial_number
        .as_deref()
        .map(|serial| format!(" · {serial}"))
        .unwrap_or_default();
    format!("{provider} · {}{serial}", probe.model)
}

fn instrument_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0_f32, GRID))
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(12.0))
}

fn paint_vertical_resize_handle(
    ui: &mut egui::Ui,
    id_salt: &str,
    current_height: f32,
) -> Option<f32> {
    ui.push_id(id_salt, |ui| {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 10.0), egui::Sense::drag());
        let response = response
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_text("Drag vertically to resize");
        let grip_color = if response.hovered() || response.dragged() {
            ACCENT
        } else {
            GRID
        };
        ui.painter().line_segment(
            [
                egui::pos2(rect.center().x - 32.0, rect.center().y),
                egui::pos2(rect.center().x + 32.0, rect.center().y),
            ],
            egui::Stroke::new(2.0_f32, grip_color),
        );
        response.dragged().then(|| {
            let pointer_delta = ui.input(|input| input.pointer.delta().y);
            current_height + pointer_delta
        })
    })
    .inner
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
        let timeline_height = trace.timeline_view_height();
        ui.horizontal(|ui| {
            ui.label(RichText::new("TIMELINE").strong().color(ACCENT));
            ui.label(
                RichText::new("Task, ISR, timer, scheduler, and idle execution").color(TEXT_MUTED),
            );
        });
        ui.horizontal(|ui| {
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
            ui.label(
                RichText::new("Pinch, ⌘-scroll, or ⌥-scroll to zoom · drag to pan")
                    .small()
                    .color(TEXT_MUTED),
            );
        });
        ui.add_space(6.0);

        let lanes = timeline_lanes(trace.model());
        let Some((start_ticks, end_ticks)) = trace.time_bounds() else {
            paint_trace_empty_state(
                ui,
                "No execution data",
                "Decoded task and interrupt intervals will appear here.",
                timeline_height,
            );
            if let Some(height) =
                paint_vertical_resize_handle(ui, "timeline_height", timeline_height)
            {
                trace.set_timeline_view_height(height);
            }
            return;
        };
        if lanes.is_empty() {
            paint_trace_empty_state(
                ui,
                "No execution intervals yet",
                "Trace metadata and non-execution events are available in the event stream below.",
                timeline_height,
            );
            if let Some(height) =
                paint_vertical_resize_handle(ui, "timeline_height", timeline_height)
            {
                trace.set_timeline_view_height(height);
            }
            return;
        }

        let timeline = egui::ScrollArea::both()
            .id_salt("system_trace_timeline")
            .max_height(timeline_height)
            .drag_to_scroll(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let canvas_width =
                    timeline_canvas_width(ui.available_width(), trace.timeline_zoom());
                let canvas_height =
                    TIMELINE_HEADER_HEIGHT + lanes.len() as f32 * TIMELINE_ROW_HEIGHT;
                let (canvas, _) = ui.allocate_exact_size(
                    egui::vec2(canvas_width, canvas_height),
                    egui::Sense::hover(),
                );
                paint_timeline_canvas(ui, canvas, trace.model(), &lanes, start_ticks, end_ticks);
                canvas.top()
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
            timeline.inner,
            trace.model(),
            &lanes,
        );
        if let Some(height) = paint_vertical_resize_handle(ui, "timeline_height", timeline_height) {
            trace.set_timeline_view_height(height);
        }
    });
}

fn timeline_canvas_width(available_width: f32, zoom: f32) -> f32 {
    available_width.max(TIMELINE_MINIMUM_WIDTH) * zoom
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TimelineLane {
    core_id: u32,
    context: ExecutionContext,
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
    model: &TraceModel,
    lanes: &[TimelineLane],
    start_ticks: u64,
    end_ticks: u64,
) {
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
        for interval in core
            .intervals()
            .iter()
            .filter(|interval| interval.context == lane.context)
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
            painter.rect_filled(interval_rect, 2.0, context_color(lane.context));
            if !interval.complete {
                painter.rect_stroke(
                    interval_rect,
                    2.0,
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(245, 96, 96)),
                );
            }
        }
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
        let event_height = trace.event_view_height();
        let mut clear_requested = false;
        ui.horizontal(|ui| {
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
            ui.checkbox(&mut trace.follow_latest, "Follow latest");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                clear_requested = ui
                    .add_enabled(
                        !trace.events().is_empty() || !trace.terminal_lines().is_empty(),
                        egui::Button::new("Clear"),
                    )
                    .clicked();
            });
        });
        ui.add_space(5.0);
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
        if let Some(height) = paint_vertical_resize_handle(ui, "event_height", event_height) {
            trace.set_event_view_height(height);
        }
        if clear_requested {
            trace.clear();
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

    ui.horizontal(|ui| {
        ui.add_sized(
            [70.0, 18.0],
            egui::Label::new(RichText::new("LINE").small().color(TEXT_MUTED)),
        );
        ui.label(RichText::new("TEXT").small().color(TEXT_MUTED));
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("system_trace_rtt_terminal")
        .max_height(view_height)
        .auto_shrink([false, false])
        .show_rows(ui, 22.0, line_count, |ui, row_range| {
            for row in row_range {
                let line = visible_lines[row];
                let response = ui.horizontal(|ui| {
                    ui.add_sized(
                        [70.0, 20.0],
                        egui::Label::new(
                            RichText::new(line.sequence.to_string())
                                .monospace()
                                .color(TEXT_MUTED),
                        ),
                    );
                    ui.label(terminal_line_layout(ui, line));
                });
                if trace.follow_latest && row + 1 == line_count {
                    response.response.scroll_to_me(Some(egui::Align::BOTTOM));
                }
            }
        });
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
    let follow_latest = trace.follow_latest;
    let event_count = visible_indices.len();

    ui.horizontal(|ui| {
        ui.add_sized(
            [90.0, 18.0],
            egui::Label::new(RichText::new("TICKS").small().color(TEXT_MUTED)),
        );
        ui.add_sized(
            [45.0, 18.0],
            egui::Label::new(RichText::new("CORE").small().color(TEXT_MUTED)),
        );
        ui.add_sized(
            [170.0, 18.0],
            egui::Label::new(RichText::new("EVENT").small().color(TEXT_MUTED)),
        );
        ui.label(RichText::new("DETAILS").small().color(TEXT_MUTED));
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("system_trace_events")
        .max_height(view_height)
        .auto_shrink([false, false])
        .show_rows(ui, 22.0, event_count, |ui, row_range| {
            for visible_row in row_range {
                let event = &trace.events()[visible_indices[visible_row]];
                let response = ui.horizontal(|ui| {
                    ui.add_sized(
                        [90.0, 20.0],
                        egui::Label::new(
                            RichText::new(event.timestamp_ticks.to_string()).monospace(),
                        ),
                    );
                    ui.add_sized([45.0, 20.0], egui::Label::new(event.core_id.to_string()));
                    let clicked = ui
                        .add_sized(
                            [170.0, 20.0],
                            egui::SelectableLabel::new(
                                selected_sequence == Some(event.sequence),
                                event_name(&event.kind),
                            ),
                        )
                        .clicked();
                    ui.label(RichText::new(event_summary(&event.kind)).color(TEXT_MUTED));
                    clicked
                });
                if response.inner {
                    selected_sequence = Some(event.sequence);
                }
                if follow_latest && visible_row + 1 == event_count {
                    response.response.scroll_to_me(Some(egui::Align::BOTTOM));
                }
            }
        });
    trace.selected_sequence = selected_sequence;
}

fn paint_selected_event(ui: &mut egui::Ui, trace: &SystemTraceDisplayState) {
    let Some(event) = trace.selected_event() else {
        return;
    };
    ui.separator();
    ui.horizontal_wrapped(|ui| {
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
}

fn paint_trace_empty_state(ui: &mut egui::Ui, title: &str, description: &str, minimum_height: f32) {
    egui::Frame::none()
        .fill(BACKGROUND)
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_min_height(minimum_height.max(56.0));
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
        assert!(texts.iter().any(|text| text == "nRF52832 booted"));
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
    fn timeline_exposes_zoom_controls_without_trace_data() {
        let context = egui::Context::default();
        let mut app = app();

        let texts = paint_central_texts(&context, &mut app);

        assert!(texts.iter().any(|text| text == "1.0×"));
        assert!(texts.iter().any(|text| text == "Fit"));
        assert!(texts.iter().any(|text| text == "+"));
        assert!(texts.iter().any(|text| text == "−"));
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
