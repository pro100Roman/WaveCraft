//! The top strip that switches between waveform and System Trace views.

use crate::{egui, workspace_views, RichText, WorkspaceView, PANEL, TEXT_MUTED};

use crate::WaveCraftApp;

impl WaveCraftApp {
    /// Paints the available waveform views and the independent System Trace view.
    pub(crate) fn paint_waveform_tabs(&mut self, context: &egui::Context) {
        let available_views = workspace_views(&self.available_waveform_tabs());
        let ordered_views = *self.workspace_view_layout.order();
        let active_view = self.active_workspace_view;
        let mut selected_view = None;
        egui::TopBottomPanel::top("waveform_tabs")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(14.0, 7.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("VIEW").strong().color(TEXT_MUTED));
                    ui.separator();
                    let (primary_down, pointer_position) = ui.input(|input| {
                        (input.pointer.primary_down(), input.pointer.interact_pos())
                    });
                    for view in ordered_views
                        .into_iter()
                        .filter(|view| available_views.contains(view))
                    {
                        let dragging = self.workspace_view_layout.dragged_view().is_some();
                        let response = ui
                            .add(
                                egui::Button::new(view.label())
                                    .selected(active_view == Some(view))
                                    .sense(egui::Sense::click_and_drag()),
                            )
                            .on_hover_cursor(if dragging {
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
                            && pointer_position
                                .is_some_and(|position| response.rect.contains(position))
                            && self.workspace_view_layout.dragged_view().is_some()
                        {
                            self.workspace_view_layout.drag_to(view);
                        }
                        if !dragging {
                            response.on_hover_text(
                                "Drag with the left mouse button to reorder this view",
                            );
                        }
                    }
                    if !primary_down {
                        self.workspace_view_layout.finish_drag();
                    }
                });
            });
        if let Some(view) = selected_view {
            match view {
                WorkspaceView::Waveform(tab) => self.select_waveform_tab(tab),
                WorkspaceView::Vna => self.select_vna_view(),
                WorkspaceView::SystemTrace => self.select_system_trace_view(),
            }
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
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
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
                WorkspaceView::Waveform(crate::WaveformTab::Lissajous),
                WorkspaceView::Waveform(crate::WaveformTab::RadioReceiver),
                WorkspaceView::Waveform(crate::WaveformTab::Combined),
                WorkspaceView::Vna,
            ]
        );
        assert_eq!(app.workspace_view_layout.dragged_view(), None);
    }
}
