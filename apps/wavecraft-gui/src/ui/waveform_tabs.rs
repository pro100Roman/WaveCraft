//! Workspace tab selection and drag-to-reorder interaction.

use super::view_shell::{workspace_tab, workspace_tab_bar};
use crate::{egui, SelectableHoverText, WorkspaceView};

use crate::WaveCraftApp;

impl WaveCraftApp {
    /// Paints the available waveform views and the independent System Trace view.
    pub(crate) fn paint_waveform_tabs(&mut self, context: &egui::Context) {
        let available_views = self.available_workspace_views();
        let ordered_views = *self.workspace_view_layout.order();
        let active_view = self.active_workspace_view;
        let selection_id = egui::Id::new("workspace_tab_last_selection");
        let selection_changed =
            context.data(|data| data.get_temp::<WorkspaceView>(selection_id)) != active_view;
        if let Some(view) = active_view {
            context.data_mut(|data| data.insert_temp(selection_id, view));
        }
        let mut selected_view = None;
        let mut closed_view = None;
        let mut added_view = None;
        workspace_tab_bar(
            context,
            |ui| {
                let (primary_down, pointer_position) =
                    ui.input(|input| (input.pointer.primary_down(), input.pointer.interact_pos()));
                for view in ordered_views
                    .into_iter()
                    .filter(|view| available_views.contains(view))
                {
                    let dragging = self.workspace_view_layout.dragged_view().is_some();
                    let tab = workspace_tab(
                        ui,
                        view.label(),
                        active_view == Some(view),
                        available_views.len() > 1,
                    );
                    if tab.close_clicked {
                        closed_view = Some(view);
                    }
                    if selection_changed && active_view == Some(view) {
                        tab.response.scroll_to_me(None);
                    }
                    let response = tab.response.on_hover_cursor(if dragging {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Grab
                    });
                    if response.clicked() {
                        selected_view = Some(view);
                    }
                    if primary_down
                        && response.is_pointer_button_down_on()
                        && self.workspace_view_layout.dragged_view().is_none()
                    {
                        self.workspace_view_layout.begin_drag(view);
                    }
                    if primary_down
                        && pointer_position.is_some_and(|position| response.rect.contains(position))
                        && self.workspace_view_layout.dragged_view().is_some()
                    {
                        self.workspace_view_layout.drag_to(view);
                    }
                    if !dragging {
                        response
                            .on_hover_text("Drag with the left mouse button to reorder this view");
                    }
                }
                if !primary_down {
                    self.workspace_view_layout.finish_drag();
                }
            },
            |ui| {
                for view in WorkspaceView::ALL {
                    if view.pd_power_index().is_some_and(|index| index != 0)
                        || view.battery_index().is_some_and(|index| index != 0)
                    {
                        continue;
                    }
                    let slots = match view {
                        WorkspaceView::PdPower => Some(WorkspaceView::PD_POWER.as_slice()),
                        WorkspaceView::Battery => Some(WorkspaceView::BATTERY.as_slice()),
                        _ => None,
                    };
                    let next_view = if let Some(slots) = slots {
                        slots
                            .iter()
                            .copied()
                            .find(|view| !available_views.contains(view))
                    } else {
                        (!available_views.contains(&view)).then_some(view)
                    };
                    let response =
                        ui.add_enabled(next_view.is_some(), egui::Button::new(view.label()));
                    if response.clicked() {
                        added_view = next_view;
                        ui.close_menu();
                    }
                    if next_view.is_none() {
                        let hint = match view {
                            WorkspaceView::PdPower => "Up to five PD Power views can be open",
                            WorkspaceView::Battery => "Up to ten Battery views can be open",
                            _ => "This view is already open",
                        };
                        response
                            .on_hover_selectable_text(hint)
                            .on_disabled_hover_selectable_text(hint);
                    }
                }
            },
        );
        if let Some(view) = closed_view {
            self.set_workspace_view_visible(view, false);
        } else if let Some(view) = added_view {
            self.open_workspace_view(view);
        } else if let Some(view) = selected_view {
            self.select_workspace_view(view);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::app;

    fn pointer_button(position: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn paint_tabs(
        context: &egui::Context,
        app: &mut WaveCraftApp,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Pos2)> {
        paint_tabs_with_width(context, app, events, 1200.0)
    }

    fn paint_tabs_with_width(
        context: &egui::Context,
        app: &mut WaveCraftApp,
        events: Vec<egui::Event>,
        width: f32,
    ) -> Vec<(String, egui::Pos2)> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 800.0),
            )),
            events,
            ..egui::RawInput::default()
        };
        let output = context.run(input, |context| app.paint_waveform_tabs(context));
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
        texts
    }

    #[test]
    fn plus_menu_adds_five_power_views_and_disables_the_sixth() {
        let context = egui::Context::default();
        let mut app = app();
        for expected in WorkspaceView::PD_POWER.into_iter().skip(1) {
            let texts = paint_tabs(&context, &mut app, vec![]);
            click_tabs(
                &context,
                &mut app,
                texts.iter().find(|(text, _)| text == "+").unwrap().1,
            );
            let texts = paint_tabs(&context, &mut app, vec![]);
            // The generic add-menu label is the last matching text (the
            // original primary tab can also be visible in the strip).
            let item = texts
                .iter()
                .rev()
                .find(|(text, _)| text == "PD Power")
                .unwrap()
                .1;
            click_tabs(&context, &mut app, item);
            assert_eq!(app.active_workspace_view, Some(expected));
        }
        assert_eq!(
            app.available_workspace_views()
                .iter()
                .filter(|view| view.pd_power_index().is_some())
                .count(),
            5
        );
        let texts = paint_tabs(&context, &mut app, vec![]);
        click_tabs(
            &context,
            &mut app,
            texts.iter().find(|(text, _)| text == "+").unwrap().1,
        );
        let texts = paint_tabs(&context, &mut app, vec![]);
        let item = texts
            .iter()
            .rev()
            .find(|(text, _)| text == "PD Power")
            .unwrap()
            .1;
        click_tabs(&context, &mut app, item);
        assert_eq!(app.active_workspace_view, Some(WorkspaceView::PdPower5));
        assert_eq!(
            app.available_workspace_views()
                .iter()
                .filter(|view| view.pd_power_index().is_some())
                .count(),
            5
        );
    }

    #[test]
    fn plus_menu_adds_ten_battery_views_limits_count_and_reuses_closed_slot() {
        let context = egui::Context::default();
        let mut app = app();
        let add_battery = |app: &mut WaveCraftApp| {
            let texts = paint_tabs(&context, app, vec![]);
            click_tabs(
                &context,
                app,
                texts.iter().find(|(text, _)| text == "+").unwrap().1,
            );
            let texts = paint_tabs(&context, app, vec![]);
            let item = texts
                .iter()
                .rev()
                .find(|(text, _)| text == "Battery")
                .unwrap()
                .1;
            assert!(!texts.iter().any(|(text, _)| text == "Battery 11"));
            click_tabs(&context, app, item);
        };
        assert_eq!(
            app.available_workspace_views()
                .iter()
                .filter(|v| v.battery_index().is_some())
                .count(),
            1
        );
        for expected in WorkspaceView::BATTERY.into_iter().skip(1) {
            add_battery(&mut app);
            assert_eq!(app.active_workspace_view, Some(expected));
        }
        add_battery(&mut app);
        assert_eq!(app.active_workspace_view, Some(WorkspaceView::Battery10));
        assert_eq!(
            app.available_workspace_views()
                .iter()
                .filter(|v| v.battery_index().is_some())
                .count(),
            10
        );
        paint_tabs(
            &context,
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        app.battery[3].settings.size = "Retained cell".into();
        app.set_workspace_view_visible(WorkspaceView::Battery4, false);
        add_battery(&mut app);
        assert_eq!(app.active_workspace_view, Some(WorkspaceView::Battery4));
        assert_eq!(app.battery[3].settings.size, "Retained cell");
        assert_eq!(
            app.available_workspace_views().last(),
            Some(&WorkspaceView::Battery4)
        );
    }

    #[test]
    fn hidden_views_are_not_painted_and_return_when_enabled() {
        let context = egui::Context::default();
        let mut app = app();
        let logic = WorkspaceView::Waveform(crate::WaveformTab::LogicAnalyzer);
        app.set_workspace_view_visible(logic, false);
        app.set_workspace_view_visible(WorkspaceView::SystemTrace, false);
        let texts = paint_tabs(&context, &mut app, Vec::new());
        assert!(!texts
            .iter()
            .any(|(text, _)| text == "Logic Analyzer" || text == "System Trace"));
        assert!(texts.iter().any(|(text, _)| text == "Oscilloscope"));
        app.set_workspace_view_visible(logic, true);
        let texts = paint_tabs(&context, &mut app, Vec::new());
        assert!(texts.iter().any(|(text, _)| text == "Logic Analyzer"));
    }

    fn click_tabs(context: &egui::Context, app: &mut WaveCraftApp, position: egui::Pos2) {
        paint_tabs(
            context,
            app,
            vec![
                egui::Event::PointerMoved(position),
                pointer_button(position, true),
            ],
        );
        paint_tabs(context, app, vec![pointer_button(position, false)]);
    }

    #[test]
    fn a_closed_view_can_be_added_and_selected_from_the_plus_menu() {
        let context = egui::Context::default();
        let mut app = app();
        app.select_system_trace_view();
        let texts = paint_tabs(&context, &mut app, Vec::new());
        let trace_position = texts
            .iter()
            .find(|(text, _)| text == "System Trace")
            .unwrap()
            .1;
        let close_position = texts
            .iter()
            .filter(|(text, position)| text == "×" && position.x > trace_position.x)
            .min_by(|(_, left), (_, right)| left.x.total_cmp(&right.x))
            .unwrap()
            .1;

        click_tabs(&context, &mut app, close_position);

        assert!(!app
            .available_workspace_views()
            .contains(&WorkspaceView::SystemTrace));
        assert_eq!(app.active_workspace_view, Some(WorkspaceView::Vna));
        let texts = paint_tabs(&context, &mut app, Vec::new());
        let add_position = texts.iter().find(|(text, _)| text == "+").unwrap().1;
        click_tabs(&context, &mut app, add_position);
        let texts = paint_tabs(&context, &mut app, Vec::new());
        let trace_position = texts
            .iter()
            .find(|(text, _)| text == "System Trace")
            .expect("the add menu offers the closed view")
            .1;

        click_tabs(&context, &mut app, trace_position);

        assert_eq!(app.active_workspace_view, Some(WorkspaceView::SystemTrace));
        assert_eq!(
            app.available_workspace_views().last(),
            Some(&WorkspaceView::SystemTrace)
        );
    }

    #[test]
    fn the_last_tab_stays_open_and_plus_remains_visible_in_a_narrow_strip() {
        let context = egui::Context::default();
        let mut app = app();
        for view in WorkspaceView::ALL.into_iter().skip(1) {
            app.set_workspace_view_visible(view, false);
        }
        let texts = paint_tabs(&context, &mut app, Vec::new());
        let close_position = texts.iter().find(|(text, _)| text == "×").unwrap().1;
        click_tabs(&context, &mut app, close_position);
        assert_eq!(app.available_workspace_views().len(), 1);

        for view in WorkspaceView::ALL {
            app.open_workspace_view(view);
        }
        let texts = paint_tabs_with_width(&context, &mut app, Vec::new(), 500.0);
        assert!(texts
            .iter()
            .any(|(text, position)| text == "+" && position.x < 500.0));
    }

    #[test]
    fn workspace_view_tabs_reorder_with_a_left_pointer_drag() {
        let context = egui::Context::default();
        let mut app = app();
        let texts = paint_tabs(&context, &mut app, Vec::new());
        let position = |label: &str| {
            texts
                .iter()
                .find(|(text, _)| text == label)
                .map(|(_, position)| *position)
                .unwrap_or_else(|| panic!("{label} tab is painted"))
        };
        let system_trace = position("System Trace");
        let oscilloscope = position("Oscilloscope");

        let _ = paint_tabs(
            &context,
            &mut app,
            vec![
                egui::Event::PointerMoved(system_trace),
                pointer_button(system_trace, true),
            ],
        );
        let _ = paint_tabs(
            &context,
            &mut app,
            vec![egui::Event::PointerMoved(oscilloscope)],
        );
        let _ = paint_tabs(
            &context,
            &mut app,
            vec![pointer_button(oscilloscope, false)],
        );

        assert_eq!(
            app.workspace_view_layout.order(),
            &[
                WorkspaceView::Waveform(crate::WaveformTab::LogicAnalyzer),
                WorkspaceView::SystemTrace,
                WorkspaceView::Waveform(crate::WaveformTab::Oscilloscope),
                WorkspaceView::Waveform(crate::WaveformTab::RadioReceiver),
                WorkspaceView::Waveform(crate::WaveformTab::Combined),
                WorkspaceView::Vna,
                WorkspaceView::Serial,
                WorkspaceView::Battery,
                WorkspaceView::Battery2,
                WorkspaceView::Battery3,
                WorkspaceView::Battery4,
                WorkspaceView::Battery5,
                WorkspaceView::Battery6,
                WorkspaceView::Battery7,
                WorkspaceView::Battery8,
                WorkspaceView::Battery9,
                WorkspaceView::Battery10,
                WorkspaceView::PdPower,
                WorkspaceView::PdPower2,
                WorkspaceView::PdPower3,
                WorkspaceView::PdPower4,
                WorkspaceView::PdPower5,
            ]
        );
        assert_eq!(app.workspace_view_layout.dragged_view(), None);
    }
}
