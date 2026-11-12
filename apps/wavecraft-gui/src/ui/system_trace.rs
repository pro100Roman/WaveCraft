//! Independent System Trace workspace and decoded-event instruments.

use crate::system_trace::{SystemTraceDisplayState, TraceEventView};
use crate::{egui, RichText, WaveCraftApp, ACCENT, BACKGROUND, GRID, ORANGE, PANEL, TEXT_MUTED};
use wavecraft_trace::{ExecutionContext, TraceEvent, TraceEventKind, TraceModel};

const TIMELINE_LABEL_WIDTH: f32 = 150.0;
const TIMELINE_HEADER_HEIGHT: f32 = 24.0;
const TIMELINE_ROW_HEIGHT: f32 = 25.0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemTraceSettings {
    pub(crate) target_name: String,
    pub(crate) speed_khz: u32,
    pub(crate) control_block_address: String,
    pub(crate) up_channel: usize,
    pub(crate) down_channel: usize,
}

impl Default for SystemTraceSettings {
    fn default() -> Self {
        Self {
            target_name: String::new(),
            speed_khz: 4_000,
            control_block_address: String::new(),
            up_channel: 1,
            down_channel: 1,
        }
    }
}

impl WaveCraftApp {
    pub(crate) fn paint_system_trace_toolbar(&mut self, context: &egui::Context) {
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
                        ui.add_enabled(false, egui::Button::new("▶ Start"));
                        ui.add_enabled(false, egui::Button::new("↻ Scan probes"));
                    });
                });
            });
    }

    pub(crate) fn paint_system_trace_setup_panel(&mut self, context: &egui::Context) {
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
                        egui::ComboBox::from_id_salt("system_trace_probe")
                            .width(ui.available_width())
                            .selected_text("No probe backend in this build")
                            .show_ui(ui, |ui| {
                                ui.add_enabled(false, egui::Label::new("No probes found"));
                            });

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
                        setup_label(ui, "RTT control block");
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.system_trace_settings.control_block_address,
                            )
                            .hint_text("Auto scan or 0x20000000"),
                        );

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            setup_label(ui, "Up channel");
                            ui.add(
                                egui::DragValue::new(
                                    &mut self.system_trace_settings.up_channel,
                                )
                                .range(0..=255),
                            );
                            setup_label(ui, "Down");
                            ui.add(
                                egui::DragValue::new(
                                    &mut self.system_trace_settings.down_channel,
                                )
                                .range(0..=255),
                            );
                        });

                        ui.add_space(16.0);
                        ui.add_enabled_ui(false, |ui| {
                            let _ = ui.button("Connect and attach");
                        });
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(
                                "Probe discovery is isolated behind wavecraft-debug-probe. Enable a compatible backend to connect hardware.",
                            )
                            .small()
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
                paint_timeline_instrument(ui, &self.system_trace);
                ui.add_space(10.0);
                paint_event_instrument(ui, &mut self.system_trace);
            });
    }
}

fn setup_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(TEXT_MUTED));
}

fn instrument_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0_f32, GRID))
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(12.0))
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

fn paint_timeline_instrument(ui: &mut egui::Ui, trace: &SystemTraceDisplayState) {
    instrument_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("TIMELINE").strong().color(ACCENT));
            ui.label(
                RichText::new("Task, ISR, timer, scheduler, and idle execution").color(TEXT_MUTED),
            );
        });
        ui.add_space(6.0);

        let lanes = timeline_lanes(trace.model());
        let Some((start_ticks, end_ticks)) = trace.time_bounds() else {
            paint_trace_empty_state(
                ui,
                "No execution data",
                "Decoded task and interrupt intervals will appear here.",
            );
            return;
        };
        if lanes.is_empty() {
            paint_trace_empty_state(
                ui,
                "No execution intervals yet",
                "Trace metadata and non-execution events are available in the event stream below.",
            );
            return;
        }

        egui::ScrollArea::both()
            .id_salt("system_trace_timeline")
            .max_height(260.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let canvas_width = ui.available_width().max(720.0);
                let canvas_height =
                    TIMELINE_HEADER_HEIGHT + lanes.len() as f32 * TIMELINE_ROW_HEIGHT;
                let (canvas, _) = ui.allocate_exact_size(
                    egui::vec2(canvas_width, canvas_height),
                    egui::Sense::hover(),
                );
                paint_timeline_canvas(ui, canvas, trace.model(), &lanes, start_ticks, end_ticks);
            });
    });
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
    painter.text(
        egui::pos2(plot_left, canvas.top() + 2.0),
        egui::Align2::LEFT_TOP,
        format!("{start_ticks} ticks"),
        egui::FontId::monospace(11.0),
        TEXT_MUTED,
    );
    painter.text(
        egui::pos2(plot_right, canvas.top() + 2.0),
        egui::Align2::RIGHT_TOP,
        format!("{end_ticks} ticks"),
        egui::FontId::monospace(11.0),
        TEXT_MUTED,
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
        painter.text(
            egui::pos2(canvas.left() + 6.0, row_top + TIMELINE_ROW_HEIGHT * 0.5),
            egui::Align2::LEFT_CENTER,
            context_label(model, *lane),
            egui::FontId::monospace(11.0),
            TEXT_MUTED,
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
        let mut clear_requested = false;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut trace.event_view, TraceEventView::Events, "EVENTS");
            ui.selectable_value(&mut trace.event_view, TraceEventView::Logs, "LOGS");
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut trace.event_filter)
                    .hint_text("Filter")
                    .desired_width(180.0),
            );
            ui.checkbox(&mut trace.follow_latest, "Follow latest");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                clear_requested = ui
                    .add_enabled(!trace.events().is_empty(), egui::Button::new("Clear"))
                    .clicked();
            });
        });
        ui.add_space(5.0);
        if trace.events().is_empty() {
            paint_trace_empty_state(
                ui,
                "Connect a debug probe to begin",
                "Task, interrupt, timer, marker, log, and data events will appear here.",
            );
        } else {
            paint_event_table(ui, trace);
            paint_selected_event(ui, trace);
        }
        if clear_requested {
            trace.clear();
        }
    });
}

fn paint_event_table(ui: &mut egui::Ui, trace: &mut SystemTraceDisplayState) {
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
        .max_height(230.0)
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

fn paint_trace_empty_state(ui: &mut egui::Ui, title: &str, description: &str) {
    egui::Frame::none()
        .fill(BACKGROUND)
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_min_height(56.0);
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
