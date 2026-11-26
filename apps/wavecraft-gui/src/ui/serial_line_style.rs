//! The same stroke renderer paints line-style choices and serial plot traces.

use crate::{egui, serial::SerialLineStyle};

pub(super) const LINE_STYLE_PICKER_WIDTH: f32 = 64.0;

impl SerialLineStyle {
    const ALL: [Self; 5] = [
        Self::Solid,
        Self::Dashed,
        Self::Dotted,
        Self::DashDot,
        Self::DashDotDot,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Dashed => "Dashed",
            Self::Dotted => "Dotted",
            Self::DashDot => "Dash-dot",
            Self::DashDotDot => "Dash-dot-dot",
        }
    }
}

pub(super) fn paint_serial_line(
    painter: &egui::Painter,
    points: &[egui::Pos2],
    style: SerialLineStyle,
    color: egui::Color32,
) {
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
    };
    painter.extend(shapes);
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
