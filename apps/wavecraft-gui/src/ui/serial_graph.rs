//! Serial graph rendering and bounded cursor interactions.

use super::{
    plot_hover::{paint_plot_hover, plot_hover_pointer},
    serial_line_style::paint_serial_line,
    view_shell::view_card,
};
use crate::serial::{SerialGraphValueRange, SerialMode, SerialState};
use crate::{egui, RichText, BACKGROUND, GRID, TEXT_MUTED};

pub(super) fn paint_graph(ui: &mut egui::Ui, state: &mut SerialState) {
    view_card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("GRAPH").strong().color(TEXT_MUTED));
            if ui.checkbox(&mut state.follow, "Follow").changed() {
                state.graph_end = state.graph_latest_time();
            }
            ui.label(
                RichText::new(if state.x_axis.is_time() {
                    "X: Time"
                } else {
                    "X: Sample"
                })
                .small()
                .color(TEXT_MUTED),
            );
            time_window_editor(ui, state);
            if ui.small_button("Fit").clicked() {
                state.fit_graph();
            }
        });
        let (surface, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ui.available_height().max(0.0)),
            egui::Sense::click_and_drag(),
        );
        ui.painter().rect_filled(surface, 3.0, BACKGROUND);
        if surface.width() < 100.0 || surface.height() < 55.0 {
            return;
        }
        let plot = egui::Rect::from_min_max(
            surface.min + egui::vec2(66.0, 10.0),
            surface.max - egui::vec2(12.0, 24.0),
        );
        let mut values = state
            .graph_value_range
            .unwrap_or_else(|| auto_value_range(state));
        handle_graph_input(ui, &response, plot, state, &mut values);
        let (start, end) = state.graph_time_range();
        let SerialGraphValueRange {
            minimum: min,
            maximum: max,
            scale,
        } = values;
        let painter = ui.painter().with_clip_rect(surface);
        let (time_unit, time_multiplier) = if state.x_axis.is_time() {
            crate::time_scale_unit(end.abs().max(start.abs()))
        } else {
            ("", 1.0)
        };
        let time_precision = crate::time_scale_precision((end - start) * time_multiplier / 4.0);
        for step in 0..=4 {
            let fraction = step as f32 / 4.0;
            let y = egui::lerp(plot.y_range(), fraction);
            painter.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                egui::Stroke::new(1.0_f32, GRID),
            );
            let value = (max - f64::from(fraction) * (max - min)) * scale;
            let label = if value.abs() >= 1e5 || value != 0.0 && value.abs() < 0.001 {
                format!("{value:.1e}")
            } else {
                format_graph_number(value, 3)
            };
            painter.text(
                egui::pos2(plot.left() - 6.0, y),
                egui::Align2::RIGHT_CENTER,
                label,
                egui::FontId::monospace(10.0),
                TEXT_MUTED,
            );
            let x = egui::lerp(plot.x_range(), fraction);
            painter.line_segment(
                [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
                egui::Stroke::new(1.0_f32, GRID),
            );
            let value = (start + f64::from(fraction) * (end - start)) * time_multiplier;
            painter.text(
                egui::pos2(x, plot.bottom() + 5.0),
                if step == 0 {
                    egui::Align2::LEFT_TOP
                } else if step == 4 {
                    egui::Align2::RIGHT_TOP
                } else {
                    egui::Align2::CENTER_TOP
                },
                format!("{}{time_unit}", format_graph_number(value, time_precision)),
                egui::FontId::monospace(10.0),
                TEXT_MUTED,
            );
        }
        let painter = painter.with_clip_rect(plot);
        for series in &state.series {
            let color = egui::Color32::from_rgb(series.color[0], series.color[1], series.color[2]);
            for points in visible_graph_lines(&series.points, start, end, values, plot) {
                paint_serial_line(
                    &painter,
                    &points,
                    state.settings.line_style(&series.name),
                    color,
                );
            }
        }
        if state.series.iter().all(|series| series.points.is_empty()) {
            painter.text(
                plot.center(),
                egui::Align2::CENTER_CENTER,
                if state.imported_file.is_some() && state.series.is_empty() {
                    "Add text fields or load a schema, then press Plot to graph the opened file"
                } else if state.series.is_empty()
                    && (state.settings.mode == SerialMode::Text && state.settings.fields.is_empty()
                        || state.settings.mode == SerialMode::Binary
                            && state.settings.schema_json.is_empty())
                {
                    "Raw data only · add text fields or load a binary schema to plot"
                } else if state.series.is_empty() {
                    "Start receiving or open a raw capture to plot configured fields"
                } else {
                    "Waiting for matching values…"
                },
                egui::FontId::proportional(13.0),
                TEXT_MUTED,
            );
        }
        if let Some(pointer) = plot_hover_pointer(ui, &response, plot) {
            let time = start + f64::from((pointer.x - plot.left()) / plot.width()) * (end - start);
            painter.line_segment(
                [
                    egui::pos2(pointer.x, plot.top()),
                    egui::pos2(pointer.x, plot.bottom()),
                ],
                egui::Stroke::new(1.0_f32, TEXT_MUTED),
            );
            let coordinate = if state.x_axis.is_time() {
                format!("{} s", format_graph_number(time, 9))
            } else {
                format!("Sample {}", format_graph_number(time, 2))
            };
            let mut rows = vec![(coordinate, ui.visuals().text_color())];
            for series in &state.series {
                if let Some(point) = series
                    .points
                    .iter()
                    .min_by(|a, b| (a[0] - time).abs().total_cmp(&(b[0] - time).abs()))
                {
                    rows.push((
                        format!("{}: {}", series.name, format_graph_number(point[1], 6)),
                        egui::Color32::from_rgb(series.color[0], series.color[1], series.color[2]),
                    ));
                }
            }
            paint_plot_hover(ui, pointer, plot, &rows);
        }
    });
}

/// Retain segments crossing the viewport even when both samples are outside it.
/// Clip in f64 before screen conversion so large zooms do not produce enormous
/// offscreen paths, dash patterns or marker counts.
fn visible_time_points(
    points: &std::collections::VecDeque<[f64; 2]>,
    start: f64,
    end: f64,
) -> Vec<[f64; 2]> {
    let mut visible = Vec::new();
    let mut previous: Option<[f64; 2]> = None;
    for &point in points {
        if let Some(before) = previous {
            if before[0] > end {
                break;
            }
            if point[0] >= start && before[0] <= end {
                let interpolate = |time: f64| {
                    if time == before[0] {
                        return before;
                    }
                    if time == point[0] {
                        return point;
                    }
                    let fraction = (time - before[0]) / (point[0] - before[0]);
                    [time, before[1] * (1.0 - fraction) + point[1] * fraction]
                };
                let first = if before[0] >= start {
                    before
                } else {
                    interpolate(start)
                };
                let last = if point[0] <= end {
                    point
                } else {
                    interpolate(end)
                };
                if visible.last() != Some(&first) {
                    visible.push(first);
                }
                if visible.last() != Some(&last) {
                    visible.push(last);
                }
            }
        }
        previous = Some(point);
    }
    if points.len() == 1 && points[0][0] >= start && points[0][0] <= end {
        visible.push(points[0]);
    }
    visible
}

fn visible_graph_lines(
    points: &std::collections::VecDeque<[f64; 2]>,
    start: f64,
    end: f64,
    values: SerialGraphValueRange,
    plot: egui::Rect,
) -> Vec<Vec<egui::Pos2>> {
    let points = visible_time_points(points, start, end);
    let map = |point: [f64; 2]| {
        [
            (point[0] - start) / (end - start),
            (point[1] / values.scale - values.minimum) / (values.maximum - values.minimum),
        ]
    };
    let screen = |point: [f64; 2]| {
        egui::pos2(
            plot.left() + point[0].clamp(0.0, 1.0) as f32 * plot.width(),
            plot.bottom() - point[1].clamp(0.0, 1.0) as f32 * plot.height(),
        )
    };
    let mut lines: Vec<Vec<egui::Pos2>> = Vec::new();
    if points.len() == 1 {
        let point = map(points[0]);
        if (0.0..=1.0).contains(&point[1]) {
            lines.push(vec![screen(point)]);
        }
    }
    let mut continuous = false;
    for pair in points.windows(2) {
        let mut first = map(pair[0]);
        let mut last = map(pair[1]);
        let delta = last[1] - first[1];
        let (enter, leave) = if delta == 0.0 {
            if !(0.0..=1.0).contains(&first[1]) {
                continuous = false;
                continue;
            }
            (0.0, 1.0)
        } else {
            let a = -first[1] / delta;
            let b = (1.0 - first[1]) / delta;
            (a.min(b).max(0.0), a.max(b).min(1.0))
        };
        if enter > leave || !delta.is_finite() {
            continuous = false;
            continue;
        }
        let interpolate = |fraction: f64| {
            [
                first[0] * (1.0 - fraction) + last[0] * fraction,
                first[1] * (1.0 - fraction) + last[1] * fraction,
            ]
        };
        (first, last) = (interpolate(enter), interpolate(leave));
        let (first, last) = (screen(first), screen(last));
        if continuous && lines.last().and_then(|line| line.last()) == Some(&first) {
            if first != last {
                if let Some(line) = lines.last_mut() {
                    line.push(last);
                }
            }
        } else {
            lines.push(if first == last {
                vec![first]
            } else {
                vec![first, last]
            });
        }
        continuous = true;
    }
    lines
}

fn time_window_editor(ui: &mut egui::Ui, state: &mut SerialState) -> egui::Response {
    let mut draft = state
        .time_window_draft
        .take()
        .unwrap_or_else(|| format_time_window(state.time_window));
    let response = ui.add(
        egui::TextEdit::singleline(&mut draft)
            .id_source("serial_graph_time_window")
            .desired_width(110.0),
    );
    let parsed = draft
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= state.x_axis.minimum_span());
    if response.changed() {
        if let Some(value) = parsed {
            state.time_window = value;
        }
    }
    if response.has_focus() {
        state.time_window_draft = Some(draft);
    }
    ui.label(if state.x_axis.is_time() {
        "s"
    } else {
        "samples"
    });
    response.on_hover_text(if parsed.is_some() {
        "Visible X span: seconds for time, sample count for sample numbers. Type to edit."
    } else {
        "Enter a finite positive span (at least 1 sample or 0.000000001 seconds)."
    })
}

fn format_time_window(value: f64) -> String {
    let formatted = format_graph_number(value, 9);
    if formatted == "0" {
        return formatted;
    }
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

/// Normalize the rendered number, including negative values that round to zero.
fn format_graph_number(value: f64, precision: usize) -> String {
    let formatted = format!("{value:.precision$}");
    if formatted.parse::<f64>().is_ok_and(|rounded| rounded == 0.0) {
        "0".into()
    } else {
        formatted
    }
}

fn auto_value_range(state: &SerialState) -> SerialGraphValueRange {
    let (start, end) = state.graph_time_range();
    let mut minimum = f64::INFINITY;
    let mut maximum = f64::NEG_INFINITY;
    for point in state
        .series
        .iter()
        .flat_map(|series| visible_time_points(&series.points, start, end))
    {
        minimum = minimum.min(point[1]);
        maximum = maximum.max(point[1]);
    }
    if !minimum.is_finite() {
        minimum = 0.0;
        maximum = 1.0;
    }
    let scale = minimum.abs().max(maximum.abs()).max(1.0);
    minimum /= scale;
    maximum /= scale;
    let span = (maximum - minimum).max(minimum.abs() * 0.01).max(0.001);
    SerialGraphValueRange {
        minimum: minimum - span * 0.08,
        maximum: maximum + span * 0.08,
        scale,
    }
}

fn handle_graph_input(
    ui: &egui::Ui,
    response: &egui::Response,
    plot: egui::Rect,
    state: &mut SerialState,
    values: &mut SerialGraphValueRange,
) {
    if response.double_clicked() {
        state.fit_graph();
        *values = auto_value_range(state);
        return;
    }
    if let Some(pointer) = response
        .hover_pos()
        .filter(|pointer| plot.contains(*pointer))
    {
        let (scroll, both_axes) = ui.input(|input| {
            let modified = input.modifiers.ctrl || input.modifiers.command || input.modifiers.mac_cmd
                || input.events.iter().any(|event| matches!(event,
                    egui::Event::MouseWheel { modifiers, .. } if modifiers.ctrl || modifiers.command || modifiers.mac_cmd));
            (input.raw_scroll_delta.y, modified)
        });
        if scroll != 0.0 {
            zoom_graph(state, values, pointer, plot, scroll, both_axes);
        }
    }
    if response.dragged() {
        let (start, end) = state.graph_time_range();
        let shift =
            f64::from(ui.input(|input| input.pointer.delta().x) / plot.width()) * (end - start);
        state.graph_end = (end - shift).max(state.x_axis.minimum_x() + end - start);
        state.follow = false;
        state.graph_value_range = Some(*values);
    }
}

fn zoom_graph(
    state: &mut SerialState,
    values: &mut SerialGraphValueRange,
    pointer: egui::Pos2,
    plot: egui::Rect,
    scroll: f32,
    both_axes: bool,
) {
    let factor = (-f64::from(scroll) * 0.005).clamp(-4.0, 4.0).exp();
    let (start, end) = state.graph_time_range();
    let x = f64::from((pointer.x - plot.left()) / plot.width());
    let anchor = start + x * (end - start);
    let span = ((end - start) * factor).clamp(state.x_axis.minimum_span(), 1e12);
    let start = (anchor - x * span).max(state.x_axis.minimum_x());
    state.time_window = span;
    state.graph_end = start + span;
    state.follow = false;
    state.time_window_draft = None;
    if both_axes {
        let y = f64::from((pointer.y - plot.top()) / plot.height());
        let anchor = values.maximum - y * (values.maximum - values.minimum);
        let span = ((values.maximum - values.minimum) * factor).clamp(1e-12, 1e12);
        values.maximum = anchor + y * span;
        values.minimum = anchor - (1.0 - y) * span;
    }
    state.graph_value_range = Some(*values);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_labels_have_no_sign_or_fraction_while_real_negative_values_are_preserved() {
        for precision in 0..=9 {
            for value in [-0.0, 0.0, -1e-14, 1e-14] {
                assert_eq!(format_graph_number(value, precision), "0");
            }
        }
        assert_eq!(format_graph_number(-0.125, 3), "-0.125");
        assert_eq!(format_graph_number(1.25, 2), "1.25");
        assert_eq!(format_time_window(-0.0), "0");
        assert_eq!(format_time_window(1e-9), "0.000000001");
    }

    #[test]
    fn double_click_fits_both_axes_in_plot_and_axis_margins() {
        for fraction in [
            egui::vec2(0.5, 0.5),
            egui::vec2(0.01, 0.5),
            egui::vec2(0.5, 0.99),
        ] {
            let context = egui::Context::default();
            let mut state = SerialState::default();
            state.series.push(crate::serial::Series {
                name: "v".into(),
                color: [255, 0, 0],
                points: [[2.0, 1.0], [12.0, 4.0]].into(),
            });
            state.time_window = 1.0;
            state.graph_end = 8.0;
            state.follow = false;
            state.graph_value_range = Some(SerialGraphValueRange {
                minimum: 2.0,
                maximum: 3.0,
                scale: 1.0,
            });
            let mut time = 0.0;
            let mut render = |state: &mut SerialState, events| {
                time += 0.05;
                context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1000.0, 700.0),
                        )),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| paint_graph(ui, state));
                    },
                )
            };
            let output = render(&mut state, vec![]);
            let surface = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.fill == BACKGROUND
                            && rect.rect.width() > 100.0
                            && rect.rect.height() > 55.0 =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .expect("graph surface");
            let position = surface.min + surface.size() * fraction;
            for click in 0..2 {
                for pressed in [true, false] {
                    let _ = render(
                        &mut state,
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
                if click == 0 {
                    assert_eq!(state.time_window, 1.0);
                    assert!(state.graph_value_range.is_some());
                }
            }
            assert!((state.time_window - 10.0).abs() < 1e-10);
            assert!(state.follow);
            assert_eq!(state.graph_value_range, None);
            let (start, end) = state.graph_time_range();
            assert!(start <= 2.0 && end >= 12.0);
        }
    }

    #[test]
    fn repeated_zoom_keeps_segments_visible_between_samples_for_all_line_styles() {
        use crate::serial::SerialLineStyle;
        let points = [[0.0, 0.0], [10.0, 10.0]].into();
        let plot = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(900.0, 500.0));
        for both_axes in [false, true] {
            let mut state = SerialState::default();
            state.x_axis = crate::serial::SerialXAxis::SampleRate(1.0);
            state.follow = false;
            state.graph_end = 10.0;
            let mut values = SerialGraphValueRange {
                minimum: 0.0,
                maximum: 10.0,
                scale: 1.0,
            };
            for _ in 0..20 {
                zoom_graph(
                    &mut state,
                    &mut values,
                    plot.center(),
                    plot,
                    60.0,
                    both_axes,
                );
                let (start, end) = state.graph_time_range();
                let lines = visible_graph_lines(&points, start, end, values, plot);
                assert_eq!(lines.len(), 1);
                assert!(lines[0].len() >= 2);
                assert!((lines[0][0].x - plot.left()).abs() < 0.01);
                assert!((lines[0].last().unwrap().x - plot.right()).abs() < 0.01);
                assert!(lines[0].iter().all(|point| plot.contains(*point)));
                for style in [
                    SerialLineStyle::Solid,
                    SerialLineStyle::Dashed,
                    SerialLineStyle::Dotted,
                    SerialLineStyle::DashDot,
                    SerialLineStyle::DashDotDot,
                    SerialLineStyle::Star,
                    SerialLineStyle::Triangle,
                ] {
                    let context = egui::Context::default();
                    let output = context.run(egui::RawInput::default(), |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            paint_serial_line(ui.painter(), &lines[0], style, egui::Color32::RED);
                        });
                    });
                    assert!(
                        output.shapes.iter().any(|shape| match &shape.shape {
                            egui::Shape::Path(path) =>
                                path.stroke.color
                                    == egui::epaint::ColorMode::Solid(egui::Color32::RED),
                            egui::Shape::LineSegment { stroke, .. } =>
                                stroke.color == egui::epaint::ColorMode::Solid(egui::Color32::RED),
                            egui::Shape::Circle(circle) => circle.fill == egui::Color32::RED,
                            _ => false,
                        }),
                        "{style:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn clipped_traces_keep_vertical_segments_and_do_not_bridge_offscreen_excursions() {
        let plot = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0));
        let values = SerialGraphValueRange {
            minimum: 0.0,
            maximum: 1.0,
            scale: 1.0,
        };
        let vertical = [[0.5, -10.0], [0.5, 10.0]].into();
        let lines = visible_graph_lines(&vertical, 0.0, 1.0, values, plot);
        assert_eq!(
            lines,
            vec![vec![egui::pos2(50.0, 100.0), egui::pos2(50.0, 0.0)]]
        );
        let excursion = [[0.0, 0.5], [0.3, 2.0], [0.7, 2.0], [1.0, 0.5]].into();
        let lines = visible_graph_lines(&excursion, 0.0, 1.0, values, plot);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().flatten().all(|point| plot.contains(*point)));
        assert!(visible_graph_lines(&excursion, 2.0, 3.0, values, plot).is_empty());
    }

    #[test]
    fn automatic_value_range_uses_interpolated_values_between_sparse_samples() {
        let mut state = SerialState::default();
        state.follow = false;
        state.graph_end = 6.0;
        state.time_window = 2.0;
        state.series.push(crate::serial::Series {
            name: "v".into(),
            color: [255, 0, 0],
            points: [[0.0, 100.0], [10.0, 200.0]].into(),
        });
        let range = auto_value_range(&state);
        assert!(range.minimum * range.scale < 140.0);
        assert!(range.maximum * range.scale > 160.0);
        assert!(range.minimum * range.scale > 130.0);
        assert!(range.maximum * range.scale < 170.0);
    }

    #[test]
    fn wheel_preserves_vertical_scale_and_modified_wheel_anchors_both_axes() {
        let plot = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(900.0, 500.0));
        let pointer = egui::pos2(300.0, 200.0);
        for modifiers in [
            egui::Modifiers::NONE,
            egui::Modifiers::CTRL,
            egui::Modifiers::MAC_CMD,
        ] {
            let context = egui::Context::default();
            let mut state = SerialState::default();
            state.time_window = 10.0;
            state.graph_end = 20.0;
            state.follow = false;
            let original = SerialGraphValueRange {
                minimum: -10.0,
                maximum: 30.0,
                scale: 1.0,
            };
            let mut values = original;
            for wheel in [false, true] {
                let mut events = vec![egui::Event::PointerMoved(pointer)];
                if wheel {
                    events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, 60.0),
                        modifiers,
                    });
                }
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1000.0, 700.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            let response =
                                ui.interact(plot, ui.id().with("graph"), egui::Sense::drag());
                            handle_graph_input(ui, &response, plot, &mut state, &mut values);
                        });
                    },
                );
            }
            let (start, end) = state.graph_time_range();
            assert!(end - start < 10.0);
            assert!((start + 0.25 * (end - start) - 12.5).abs() < 1e-10);
            if modifiers == egui::Modifiers::NONE {
                assert_eq!(values, original);
            } else {
                assert!(values.maximum - values.minimum < 40.0);
                assert!(
                    (values.maximum - 0.25 * (values.maximum - values.minimum) - 20.0).abs()
                        < 1e-10
                );
            }
            assert_eq!(state.graph_value_range, Some(values));
            assert!(!state.follow);
            state.fit_graph();
            assert_eq!(state.graph_value_range, None);
        }
    }

    fn editor_frame(
        context: &egui::Context,
        state: &mut SerialState,
        events: Vec<egui::Event>,
    ) -> egui::Rect {
        let mut rect = egui::Rect::NOTHING;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(500.0, 300.0),
                )),
                events,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    rect = time_window_editor(ui, state).rect;
                });
            },
        );
        rect
    }

    #[test]
    fn time_editor_accepts_typed_values_but_dragging_does_not_change_time() {
        let context = egui::Context::default();
        let mut state = SerialState::default();
        state.x_axis = crate::serial::SerialXAxis::SampleRate(1000.0);
        let rect = editor_frame(&context, &mut state, vec![]);
        let position = rect.center();
        for (pressed, pos) in [
            (true, position),
            (true, position + egui::vec2(60.0, 0.0)),
            (false, position + egui::vec2(60.0, 0.0)),
        ] {
            let mut events = vec![egui::Event::PointerMoved(pos)];
            if pos == position || !pressed {
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            editor_frame(&context, &mut state, events);
        }
        assert_eq!(state.time_window, 10.0);
        // Partial and invalid edits do not change the valid numerical viewport.
        state.time_window_draft = Some(String::new());
        editor_frame(
            &context,
            &mut state,
            vec![egui::Event::Text("0.000123".into())],
        );
        assert_eq!(state.time_window, 0.000123);
        state.time_window_draft = Some(String::new());
        editor_frame(&context, &mut state, vec![egui::Event::Text("NaN".into())]);
        assert_eq!(state.time_window, 0.000123);
    }
}
