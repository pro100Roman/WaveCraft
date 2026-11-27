//! One bottom message area for every workspace: operation status, connection
//! details, and errors. Process metrics always occupy the right end of the row.

use std::borrow::Cow;

use crate::system_trace::SystemTraceConnectionState;
use crate::{
    egui, Color32, RichText, SelectableHoverText, WaveCraftApp, ACCENT, BUILD_VERSION_LABEL, PANEL,
    TEXT_MUTED,
};

pub(crate) const STATUS_MESSAGE_FONT_SIZE: f32 = 12.0;

/// View-independent presentation model. Operational state stays with its owner;
/// views only adapt it to this model and never paint messages over their plots.
struct ViewStatus<'a> {
    view: &'a str,
    message: &'a str,
    detail: Option<Cow<'a, str>>,
    active: bool,
    errors: Vec<&'a str>,
}

impl<'a> ViewStatus<'a> {
    fn add_error(&mut self, error: Option<&'a str>) {
        if let Some(error) = error.filter(|error| !error.is_empty()) {
            if !self.errors.contains(&error) {
                self.errors.push(error);
            }
        }
    }
}

impl WaveCraftApp {
    fn active_view_status(&self) -> ViewStatus<'_> {
        let mut status = ViewStatus {
            view: self
                .active_workspace_view
                .map_or("WaveCraft", |view| view.label()),
            message: &self.status,
            detail: None,
            active: self.is_busy(),
            errors: Vec::new(),
        };
        status.add_error(self.last_error.as_deref());
        if self.pd_power_view_is_active() {
            status.message = &self.pd_power.status;
            status.detail = self.snapshot_message.as_deref().map(Cow::Borrowed);
            status.active = self.pd_power.connected;
            status.add_error(self.pd_power.error.as_deref());
        } else if self.battery_view_is_active() {
            status.message = &self.battery.status;
            status.detail = self.snapshot_message.as_deref().map(Cow::Borrowed);
            status.active = false;
            status.add_error(self.battery.error.as_deref());
        } else if self.serial_view_is_active() {
            status.message = &self.serial.status;
            status.detail = self.snapshot_message.as_deref().map(Cow::Borrowed);
            status.active = self.serial.session.is_some();
            status.add_error(self.serial.error.as_deref());
        } else if self.system_trace_view_is_active() {
            use SystemTraceConnectionState as Connection;
            status.detail = Some(match &self.system_trace_connection {
                Connection::Disconnected => "Probe disconnected".into(),
                Connection::Connecting => "Connecting to probe…".into(),
                Connection::Attached {
                    channels,
                    systemview_enabled,
                } => format!(
                    "Attached with {} RTT channel(s); {}",
                    channels.len(),
                    if *systemview_enabled {
                        "SystemView ready"
                    } else {
                        "terminal only"
                    },
                )
                .into(),
                Connection::Starting { .. } => "Starting trace…".into(),
                Connection::Tracing { recording_path, .. } => if recording_path.is_some() {
                    "Tracing and recording"
                } else {
                    "Tracing"
                }
                .into(),
                Connection::Stopping { .. } => "Stopping trace…".into(),
                Connection::Disconnecting => "Disconnecting probe…".into(),
                Connection::Error(error) => {
                    status.add_error(Some(error));
                    "Probe connection failed".into()
                }
            });
            status.active |= matches!(
                self.system_trace_connection,
                Connection::Connecting
                    | Connection::Starting { .. }
                    | Connection::Tracing { .. }
                    | Connection::Stopping { .. }
                    | Connection::Disconnecting
            );
        } else if self.vna_view_is_active() {
            status.add_error(self.vna_discovery_error.as_deref());
        }
        status
    }

    pub(crate) fn paint_status_bar(&self, context: &egui::Context) {
        paint_view_status(context, &self.active_view_status(), |ui| {
            ui.label(
                RichText::new(BUILD_VERSION_LABEL)
                    .small()
                    .monospace()
                    .color(TEXT_MUTED),
            )
            .on_hover_selectable_text("Build version");
            ui.separator();
            ui.label(
                RichText::new(self.memory_usage.label())
                    .small()
                    .monospace()
                    .color(TEXT_MUTED),
            )
            .on_hover_selectable_text("Current resident memory used by the WaveCraft process");
            ui.separator();
            ui.label(
                RichText::new(self.cpu_load.label()).small().monospace().color(TEXT_MUTED),
            ).on_hover_selectable_text(
                "WaveCraft process CPU usage; 100% equals one fully utilized logical CPU, so multi-threaded processing can exceed 100%",
            );
        });
    }
}

fn paint_view_status(
    context: &egui::Context,
    status: &ViewStatus<'_>,
    metrics: impl FnOnce(&mut egui::Ui),
) {
    // Reserve the error row before painting. A bottom panel cannot grow down
    // from its previous single-row height when an error first arrives.
    let error_height = if status.errors.is_empty() {
        0.0
    } else {
        let width = (context.available_rect().width() - 32.0).max(1.0);
        let spacing = context.style().spacing.item_spacing.y;
        context
            .fonts(|fonts| {
                status
                    .errors
                    .iter()
                    .map(|error| {
                        fonts
                            .layout(
                                (*error).to_owned(),
                                egui::FontId::proportional(STATUS_MESSAGE_FONT_SIZE),
                                Color32::LIGHT_RED,
                                width,
                            )
                            .size()
                            .y
                            + spacing
                    })
                    .sum::<f32>()
            })
            .clamp(20.0, 80.0)
            + 12.0
    };
    egui::TopBottomPanel::bottom("status")
        .exact_height(40.0 + error_height)
        .frame(egui::Frame::none().fill(PANEL).inner_margin(9.0))
        .show(context, |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    metrics(ui);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let color = if !status.errors.is_empty() {
                            Color32::LIGHT_RED
                        } else if status.active {
                            ACCENT
                        } else {
                            TEXT_MUTED
                        };
                        ui.label(RichText::new("●").color(color));
                        ui.label(RichText::new(status.view).strong().color(TEXT_MUTED));
                        ui.separator();
                        // Truncated labels provide their full selectable text in a tooltip.
                        ui.add(
                            egui::Label::new(
                                RichText::new(status.message).size(STATUS_MESSAGE_FONT_SIZE),
                            )
                            .selectable(true)
                            .truncate(),
                        );
                        if let Some(detail) = &status.detail {
                            ui.separator();
                            ui.add(
                                egui::Label::new(
                                    RichText::new(detail.as_ref())
                                        .size(STATUS_MESSAGE_FONT_SIZE)
                                        .color(TEXT_MUTED),
                                )
                                .selectable(true)
                                .truncate(),
                            );
                        }
                    });
                });
            });
            if !status.errors.is_empty() {
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("view_status_errors")
                    .max_height(80.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width());
                        for error in &status.errors {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(*error)
                                        .size(STATUS_MESSAGE_FONT_SIZE)
                                        .color(Color32::LIGHT_RED),
                                )
                                .selectable(true)
                                .wrap(),
                            );
                        }
                    });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::test_support::app, ui::view_shell::workspace_panel, WorkspaceView};

    #[test]
    fn serial_snapshot_feedback_preserves_the_opened_file_status() {
        let mut app = app();
        app.active_workspace_view = Some(WorkspaceView::Serial);
        app.serial.status = "Imported · readings.csv".into();
        app.report_snapshot_status("Snapshot saved to /tmp/wavecraft-snapshot.png".into());
        let status = app.active_view_status();
        assert!(status.message.contains("readings.csv"));
        assert!(status
            .detail
            .as_deref()
            .unwrap()
            .contains("wavecraft-snapshot.png"));
    }

    fn paint(
        app: &WaveCraftApp,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |context| {
                app.paint_status_bar(context);
                workspace_panel(context, |ui| {
                    ui.label("Plot content");
                });
            },
        )
    }

    fn text_rect(output: &egui::FullOutput, expected: &str) -> egui::Rect {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == expected => {
                    Some(text.visual_bounding_rect())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{expected} is painted"))
    }

    #[test]
    fn every_view_keeps_errors_in_the_bottom_area_outside_the_plot() {
        let mut app = app();
        let error = "The selected instrument disconnected. Reconnect it and try again.";
        app.last_error = Some(error.to_owned());
        for view in WorkspaceView::ALL {
            app.active_workspace_view = Some(view);
            let context = egui::Context::default();
            crate::configure_style(&context);
            let output = paint(&app, &context, Vec::new());
            let area = egui::containers::panel::PanelState::load(&context, egui::Id::new("status"))
                .expect("status panel exists")
                .rect;
            assert_eq!(area.bottom(), 700.0, "{view:?}");
            assert!(area.contains_rect(text_rect(&output, error)), "{view:?}");
            assert!(
                text_rect(&output, "Plot content").bottom() < area.top(),
                "{view:?}"
            );
            assert!(!output.shapes.iter().any(|shape| matches!(
                &shape.shape, egui::Shape::Text(text) if text.galley.text() == "Copy errors"
            )));
        }
    }

    #[test]
    fn collapsed_settings_do_not_hide_local_errors_and_duplicate_errors_appear_once() {
        let mut app = app();
        app.workspace_view_layout
            .set_setup_panel_open(WorkspaceView::Serial, false);
        app.workspace_view_layout
            .set_setup_panel_open(WorkspaceView::SystemTrace, false);
        app.serial.error = Some("Serial port unavailable".into());
        app.system_trace_connection = SystemTraceConnectionState::Error("Probe unavailable".into());
        app.vna_discovery_error = Some("VNA discovery failed".into());
        for (view, error) in [
            (WorkspaceView::Serial, "Serial port unavailable"),
            (WorkspaceView::SystemTrace, "Probe unavailable"),
            (WorkspaceView::Vna, "VNA discovery failed"),
        ] {
            app.active_workspace_view = Some(view);
            app.last_error = Some(error.into());
            let context = egui::Context::default();
            let output = paint(&app, &context, Vec::new());
            text_rect(&output, error);
            assert_eq!(app.active_view_status().errors, vec![error]);
        }
    }

    #[test]
    fn status_errors_support_selection_and_native_keyboard_copy() {
        let mut app = app();
        let error = "Reconnect the instrument before starting another capture.";
        app.last_error = Some(error.into());
        let context = egui::Context::default();
        crate::configure_style(&context);
        let output = paint(&app, &context, Vec::new());
        let rect = text_rect(&output, error);
        let position = rect.center();
        // Triple click selects the entire message, as in the native application.
        for _ in 0..3 {
            for pressed in [true, false] {
                let _ = paint(
                    &app,
                    &context,
                    vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        }
        let output = paint(&app, &context, vec![egui::Event::Copy]);
        assert_eq!(output.platform_output.copied_text.trim(), error);
    }
}
