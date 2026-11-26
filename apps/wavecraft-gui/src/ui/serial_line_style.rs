//! The same stroke renderer paints line-style choices and serial plot traces.

use crate::{egui, serial::SerialLineStyle};

pub(super) const LINE_STYLE_PICKER_WIDTH: f32 = 64.0;

impl SerialLineStyle {
    const ALL: [Self; 7] = [
        Self::Solid,
        Self::Dashed,
        Self::Dotted,
        Self::DashDot,
        Self::DashDotDot,
        Self::Star,
        Self::Triangle,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Dashed => "Dashed",
            Self::Dotted => "Dotted",
            Self::DashDot => "Dash-dot",
            Self::DashDotDot => "Dash-dot-dot",
            Self::Star => "Line with star markers",
            Self::Triangle => "Line with triangle markers",
        }
    }
}

pub(super) fn paint_serial_line(
    painter: &egui::Painter,
    points: &[egui::Pos2],
    style: SerialLineStyle,
    color: egui::Color32,
) {
    if points.len() == 1 {
        if matches!(style, SerialLineStyle::Star | SerialLineStyle::Triangle) {
            paint_marker(painter, points[0], style, color);
        } else {
            painter.circle_filled(points[0], 2.5, color);
        }
        return;
    }
    let stroke = egui::Stroke::new(1.5_f32, color);
    let shapes = match style {
        SerialLineStyle::Solid => vec![egui::Shape::line(points.to_vec(), stroke)],
        SerialLineStyle::Dashed => egui::Shape::dashed_line(points, stroke, 8.0, 5.0),
        SerialLineStyle::Dotted => egui::Shape::dotted_line(points, color, 5.0, 1.5),
        SerialLineStyle::DashDot => {
            egui::Shape::dashed_line_with_offset(points, stroke, &[8.0, 1.5], &[4.0, 4.0], 0.0)
        }
        SerialLineStyle::DashDotDot => egui::Shape::dashed_line_with_offset(
            points,
            stroke,
            &[8.0, 1.5, 1.5],
            &[4.0, 4.0, 4.0],
            0.0,
        ),
        SerialLineStyle::Star | SerialLineStyle::Triangle => {
            for center in marker_positions(points, 24.0) {
                paint_marker(painter, center, style, color);
            }
            vec![egui::Shape::line(points.to_vec(), stroke)]
        }
    };
    painter.extend(shapes);
}

/// Screen-space spacing keeps markers legible even with dense samples. The
/// phase carries across vertices, including repeated samples at one position.
fn marker_positions(points: &[egui::Pos2], spacing: f32) -> Vec<egui::Pos2> {
    let mut positions = Vec::new();
    let mut distance = spacing / 2.0;
    for segment in points.windows(2) {
        let vector = segment[1] - segment[0];
        let length = vector.length();
        if length <= f32::EPSILON || !length.is_finite() {
            continue;
        }
        while distance <= length {
            positions.push(segment[0] + vector * (distance / length));
            distance += spacing;
        }
        distance -= length;
    }
    positions
}

fn paint_marker(
    painter: &egui::Painter,
    center: egui::Pos2,
    style: SerialLineStyle,
    color: egui::Color32,
) {
    let count = if style == SerialLineStyle::Star {
        10
    } else {
        3
    };
    let vertices: Vec<_> = (0..count)
        .map(|index| {
            let angle =
                std::f32::consts::TAU * index as f32 / count as f32 - std::f32::consts::FRAC_PI_2;
            let radius = if style == SerialLineStyle::Star && index % 2 == 1 {
                1.8
            } else {
                4.5
            };
            center + egui::vec2(angle.cos(), angle.sin()) * radius
        })
        .collect();
    let stroke = egui::Stroke::new(1.2_f32, color);
    if style == SerialLineStyle::Star {
        // The star is concave. A center fan fills it without treating it as a
        // convex polygon, which would fill the gaps between its tips.
        let mut mesh = egui::epaint::Mesh::default();
        mesh.colored_vertex(center, color);
        for vertex in &vertices {
            mesh.colored_vertex(*vertex, color);
        }
        for index in 0..vertices.len() {
            mesh.add_triangle(
                0,
                index as u32 + 1,
                ((index + 1) % vertices.len()) as u32 + 1,
            );
        }
        painter.add(egui::Shape::mesh(mesh));
        painter.add(egui::Shape::closed_line(vertices, stroke));
    } else {
        painter.add(egui::Shape::convex_polygon(vertices, color, stroke));
    }
}

fn sample(ui: &egui::Ui, rect: egui::Rect, style: SerialLineStyle, color: egui::Color32) {
    let rect = rect.shrink2(egui::vec2(6.0, 3.0));
    paint_serial_line(
        ui.painter(),
        &[
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        ],
        style,
        color,
    );
}

/// Only the stroke sample is visible; names remain available as hover hints.
pub(super) fn line_style_picker(ui: &mut egui::Ui, style: &mut SerialLineStyle, color: [u8; 3]) {
    let color = egui::Color32::from_rgb(color[0], color[1], color[2]);
    let response = egui::ComboBox::from_id_salt("line_style")
        .width(LINE_STYLE_PICKER_WIDTH)
        .selected_text("")
        .show_ui(ui, |ui| {
            for option in SerialLineStyle::ALL {
                let response = ui.add_sized(
                    [100.0, 22.0],
                    egui::SelectableLabel::new(*style == option, ""),
                );
                sample(ui, response.rect, option, color);
                if response.on_hover_text(option.label()).clicked() {
                    *style = option;
                    ui.close_menu();
                }
            }
        })
        .response;
    let rect =
        egui::Rect::from_min_max(response.rect.min, response.rect.max - egui::vec2(22.0, 0.0));
    sample(ui, rect, *style, color);
    response.on_hover_text(format!("Line style: {}", style.label()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_spacing_survives_dense_and_repeated_samples() {
        let coarse = [egui::pos2(0.0, 0.0), egui::pos2(96.0, 0.0)];
        let dense: Vec<_> = (0..=96)
            .flat_map(|x| [egui::pos2(x as f32, 0.0); 2])
            .collect();
        let expected: Vec<_> = [12.0, 36.0, 60.0, 84.0].map(|x| egui::pos2(x, 0.0)).into();
        assert_eq!(marker_positions(&coarse, 24.0), expected);
        assert_eq!(marker_positions(&dense, 24.0), expected);
        assert!(marker_positions(&[], 24.0).is_empty());
    }

    #[test]
    fn star_and_triangle_styles_draw_their_markers_for_a_single_sample() {
        let context = egui::Context::default();
        for (style, vertex_count) in [(SerialLineStyle::Star, 10), (SerialLineStyle::Triangle, 3)] {
            let output = context.run(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    paint_serial_line(
                        ui.painter(),
                        &[egui::pos2(50.0, 50.0)],
                        style,
                        egui::Color32::RED,
                    );
                });
            });
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|shape| matches!(&shape.shape, egui::Shape::Path(path)
                if path.closed && path.points.len() == vertex_count
                    && path.stroke.color == egui::epaint::ColorMode::Solid(egui::Color32::RED))),
                "{style:?}"
            );
            assert!(
                output.shapes.iter().any(|shape| match &shape.shape {
                    egui::Shape::Mesh(mesh) =>
                        !mesh.indices.is_empty()
                            && mesh
                                .vertices
                                .iter()
                                .all(|vertex| vertex.color == egui::Color32::RED),
                    egui::Shape::Path(path) => path.closed && path.fill == egui::Color32::RED,
                    _ => false,
                }),
                "{style:?} has the same fill color as its outline"
            );
        }
    }

    #[test]
    fn every_line_style_renders_its_pattern_across_dense_samples() {
        let context = egui::Context::default();
        let mut counts = Vec::new();
        for style in SerialLineStyle::ALL {
            let output = context.run(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    // Dense samples must not restart the pattern at each sample.
                    let points: Vec<_> = (0..=200)
                        .map(|x| egui::pos2(x as f32 + 20.0, 50.0))
                        .collect();
                    paint_serial_line(ui.painter(), &points, style, egui::Color32::RED);
                });
            });
            let strokes: Vec<_> = output
                .shapes
                .iter()
                .filter(|shape| match &shape.shape {
                    egui::Shape::Path(path) => {
                        path.stroke.color == egui::epaint::ColorMode::Solid(egui::Color32::RED)
                    }
                    egui::Shape::LineSegment { stroke, .. } => {
                        stroke.color == egui::epaint::ColorMode::Solid(egui::Color32::RED)
                    }
                    egui::Shape::Circle(circle) => circle.fill == egui::Color32::RED,
                    _ => false,
                })
                .collect();
            assert!(!strokes.is_empty(), "{style:?}");
            counts.push(strokes.len());
        }
        assert_eq!(counts[0], 1);
        assert!(counts[1..].iter().all(|count| *count > 1));
        assert_ne!(counts[1], counts[3]);
        assert_ne!(counts[3], counts[4]);
    }
}
