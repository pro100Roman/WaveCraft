//! Application adapter for the shared left-panel template. Each view supplies
//! only its settings editor; visibility, persistence, and save requests are common.

use super::{frame_actions::FrameActions, view_shell::SetupPanel};
use crate::{egui, WaveCraftApp, WaveformTab, WorkspaceView};

impl WaveCraftApp {
    pub(crate) fn show_setup_panel(
        &mut self,
        context: &egui::Context,
        view: WorkspaceView,
        actions: &mut FrameActions,
        contents: impl FnOnce(&mut Self, &mut egui::Ui, &mut FrameActions) -> bool,
    ) {
        let (id, title) = match view {
            WorkspaceView::Waveform(WaveformTab::LogicAnalyzer) => ("logic_setup", "CAPTURE SETUP"),
            WorkspaceView::Waveform(WaveformTab::Oscilloscope) => ("scope_setup", "CAPTURE SETUP"),
            WorkspaceView::Waveform(WaveformTab::RadioReceiver) => ("radio_setup", "CAPTURE SETUP"),
            WorkspaceView::Waveform(WaveformTab::Combined) => ("combined_setup", "CAPTURE SETUP"),
            WorkspaceView::Vna => ("vna_setup", "VNA SETUP"),
            WorkspaceView::SystemTrace => ("system_trace_setup", "SYSTEM TRACE SETUP"),
            WorkspaceView::Serial => ("serial_setup", "SERIAL / UART SETUP"),
            WorkspaceView::Battery => ("battery_setup", "BATTERY SETUP"),
            WorkspaceView::PdPower => ("pd_power_setup", "PD POWER SETUP"),
            WorkspaceView::PdPower2 => ("pd_power_setup_2", "PD POWER 2 SETUP"),
            WorkspaceView::PdPower3 => ("pd_power_setup_3", "PD POWER 3 SETUP"),
            WorkspaceView::PdPower4 => ("pd_power_setup_4", "PD POWER 4 SETUP"),
            WorkspaceView::PdPower5 => ("pd_power_setup_5", "PD POWER 5 SETUP"),
        };
        let mut open = self.workspace_view_layout.setup_panel_open(view);
        if SetupPanel::new(id, title).show(context, &mut open, |ui| contents(self, ui, actions)) {
            actions.save_setup_settings_view = Some(view);
        }
        if self.workspace_view_layout.set_setup_panel_open(view, open) {
            self.save_global_settings();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::app;

    fn paint(
        app: &mut WaveCraftApp,
        context: &egui::Context,
        view: WorkspaceView,
        events: Vec<egui::Event>,
    ) -> (FrameActions, Vec<(String, egui::Rect)>) {
        app.active_workspace_view = Some(view);
        if let WorkspaceView::Waveform(tab) = view {
            app.active_waveform_tab = Some(tab);
        }
        let frame = app.frame_state();
        let mut actions = FrameActions::default();
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |context| match view {
                WorkspaceView::Waveform(_) => {
                    app.paint_capture_setup_panel(context, &frame, &mut actions)
                }
                WorkspaceView::Vna => app.paint_vna_setup_panel(context, &mut actions),
                WorkspaceView::SystemTrace => {
                    app.paint_system_trace_setup_panel(context, &mut actions)
                }
                WorkspaceView::Serial => app.paint_serial_setup(context, &mut actions),
                WorkspaceView::Battery => app.paint_battery_setup(context, &mut actions),
                WorkspaceView::PdPower
                | WorkspaceView::PdPower2
                | WorkspaceView::PdPower3
                | WorkspaceView::PdPower4
                | WorkspaceView::PdPower5 => app.paint_pd_power_setup(context, &mut actions),
            },
        );
        let texts = output
            .shapes
            .into_iter()
            .filter_map(|shape| match shape.shape {
                egui::Shape::Text(text) => {
                    Some((text.galley.text().to_owned(), text.visual_bounding_rect()))
                }
                _ => None,
            })
            .collect();
        (actions, texts)
    }

    fn position(texts: &[(String, egui::Rect)], expected: &str) -> egui::Pos2 {
        texts
            .iter()
            .find(|(text, _)| text == expected)
            .map(|(_, rect)| rect.center())
            .unwrap_or_else(|| panic!("{expected} is visible"))
    }

    fn click(
        app: &mut WaveCraftApp,
        context: &egui::Context,
        view: WorkspaceView,
        pos: egui::Pos2,
    ) -> FrameActions {
        let _ = paint(
            app,
            context,
            view,
            vec![egui::Event::PointerMoved(pos), pointer(pos, true)],
        );
        paint(app, context, view, vec![pointer(pos, false)]).0
    }

    fn pointer(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn every_workspace_has_the_same_pinned_save_and_collapse_controls() {
        for view in WorkspaceView::ALL {
            let mut app = app();
            let context = egui::Context::default();
            crate::configure_style(&context);
            let _ = paint(&mut app, &context, view, vec![]);
            let (_, texts) = paint(&mut app, &context, view, vec![]);
            let save = position(&texts, "💾 Save settings");
            assert!(save.y > 750.0 && save.y < 800.0, "{view:?}: {save:?}");
            assert_eq!(
                texts
                    .iter()
                    .filter(|(text, _)| text == "💾 Save settings")
                    .count(),
                1
            );
            let actions = click(&mut app, &context, view, save);
            assert_eq!(actions.save_setup_settings_view, Some(view), "{view:?}");

            let collapse = position(&texts, "◀");
            let _ = click(&mut app, &context, view, collapse);
            assert!(
                !app.workspace_view_layout.setup_panel_open(view),
                "{view:?}"
            );
            let (_, texts) = paint(&mut app, &context, view, vec![]);
            assert!(!texts.iter().any(|(text, _)| text == "💾 Save settings"));
            let expand = position(&texts, "▶");
            let _ = click(&mut app, &context, view, expand);
            assert!(app.workspace_view_layout.setup_panel_open(view), "{view:?}");
        }
    }

    #[test]
    fn invalid_vna_settings_keep_the_shared_save_control_disabled() {
        let mut app = app();
        app.vna_start_frequency_input = "invalid".into();
        let context = egui::Context::default();
        let (_, texts) = paint(&mut app, &context, WorkspaceView::Vna, vec![]);
        let save = position(&texts, "💾 Save settings");
        let actions = click(&mut app, &context, WorkspaceView::Vna, save);
        assert_eq!(actions.save_setup_settings_view, None);
    }
}
