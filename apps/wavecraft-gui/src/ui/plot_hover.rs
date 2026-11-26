//! Cursor-adjacent, selectable readouts constrained to their plot rectangle.

use crate::{egui, Rect, Vec2};

#[derive(Clone, Copy)]
struct HoverAnchor {
    pointer: egui::Pos2,
    rect: Rect,
}

pub(super) fn plot_hover_pointer(
    ui: &egui::Ui,
    response: &egui::Response,
    bounds: Rect,
) -> Option<egui::Pos2> {
    let pointer = ui.input(|input| input.pointer.hover_pos())?;
    if !bounds.contains(pointer) {
        return None;
    }
    let id = ui.make_persistent_id("plot_hover_readout");
    // Hold the readout still while selecting/copying its text.
    if let Some(anchor) = ui.ctx().data(|data| data.get_temp::<HoverAnchor>(id)) {
        if anchor.rect.contains(pointer)
            && ui.ctx().layer_id_at(pointer) == Some(egui::LayerId::new(egui::Order::Tooltip, id))
        {
            return Some(anchor.pointer);
        }
    }
    response.hover_pos()
}

pub(super) fn hover_rect(pointer: egui::Pos2, requested_size: Vec2, bounds: Rect) -> Rect {
    let size = requested_size.min(bounds.size().max(Vec2::ZERO));
    let gap = 12.0;
    let mut position = pointer + egui::vec2(gap, gap);
    if position.x + size.x > bounds.right() {
        position.x = pointer.x - gap - size.x;
    }
    if position.y + size.y > bounds.bottom() {
        position.y = pointer.y - gap - size.y;
    }
    position.x = position
        .x
        .clamp(bounds.left(), (bounds.right() - size.x).max(bounds.left()));
    position.y = position
        .y
        .clamp(bounds.top(), (bounds.bottom() - size.y).max(bounds.top()));
    Rect::from_min_size(position, size)
}

pub(super) fn paint_plot_hover(
    ui: &egui::Ui,
    pointer: egui::Pos2,
    bounds: Rect,
    rows: &[(String, egui::Color32)],
) -> Rect {
    let frame = egui::Frame::popup(ui.style())
        .inner_margin(8.0)
        .shadow(egui::epaint::Shadow::NONE);
    let margin = frame.total_margin().sum();
    let max_width = (bounds.width() - margin.x).max(1.0);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = max_width;
    for (index, (text, color)) in rows.iter().enumerate() {
        let format = egui::TextFormat {
            font_id: font.clone(),
            color: *color,
            ..Default::default()
        };
        if index > 0 {
            job.append("\n", 0.0, format.clone());
        }
        job.append(text, 0.0, format);
    }
    let text_size = ui.fonts(|fonts| fonts.layout_job(job.clone()).size());
    let rect = hover_rect(pointer, text_size.ceil() + margin, bounds);
    let id = ui.make_persistent_id("plot_hover_readout");
    ui.ctx()
        .data_mut(|data| data.insert_temp(id, HoverAnchor { pointer, rect }));
    egui::Area::new(id)
        .order(egui::Order::Tooltip)
        .fixed_pos(rect.min)
        .default_size(rect.size())
        .constrain_to(bounds)
        .movable(false)
        .sense(egui::Sense::hover())
        .fade_in(false)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(bounds);
            frame.show(ui, |ui| {
                ui.set_width((rect.width() - margin.x).max(1.0));
                egui::ScrollArea::both()
                    .min_scrolled_width(1.0)
                    .min_scrolled_height(1.0)
                    .max_width((rect.width() - margin.x).max(1.0))
                    .max_height((rect.height() - margin.y).max(1.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(job).selectable(true).wrap());
                    });
            });
        })
        .response
        .rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readout_width_tracks_text_and_preserves_field_colors() {
        let context = egui::Context::default();
        let bounds = Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(700.0, 500.0));
        for name in ["temperature", "v", "a_longer_sensor_name"] {
            let rows = vec![
                ("0.123 s".into(), egui::Color32::WHITE),
                (format!("{name}: 42.000000"), egui::Color32::RED),
            ];
            for _ in 0..3 {
                let output = context.run(egui::RawInput::default(), |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        let font = egui::TextStyle::Body.resolve(ui.style());
                        let width = ui
                            .fonts(|fonts| {
                                fonts
                                    .layout_no_wrap(rows[1].0.clone(), font, egui::Color32::RED)
                                    .size()
                                    .x
                            })
                            .ceil();
                        let margin = egui::Frame::popup(ui.style())
                            .inner_margin(8.0)
                            .total_margin()
                            .sum()
                            .x;
                        let rect = paint_plot_hover(ui, bounds.center(), bounds, &rows);
                        assert!(
                            (rect.width() - width - margin).abs() <= 1.0,
                            "{name}: {rect:?} / {width}"
                        );
                    });
                });
                for shape in &output.shapes {
                    if let egui::Shape::Text(text) = &shape.shape {
                        if text.galley.text().contains(name) {
                            assert!(text
                                .galley
                                .job
                                .sections
                                .iter()
                                .any(|section| section.format.color == egui::Color32::RED));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rendered_readouts_stay_inside_plot_with_long_field_names_and_many_rows() {
        let bounds = Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(600.0, 400.0));
        for rows in [2, 50] {
            for pointer in [
                bounds.min + egui::vec2(1.0, 1.0),
                bounds.max - egui::vec2(1.0, 1.0),
            ] {
                let context = egui::Context::default();
                for _ in 0..3 {
                    let _ = context.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(800.0, 600.0),
                            )),
                            ..Default::default()
                        },
                        |context| {
                            egui::CentralPanel::default().show(context, |ui| {
                                let text: Vec<_> = (0..rows)
                                    .map(|row| {
                                        (
                                            format!("a_very_long_field_name_{row}: 123.456789"),
                                            egui::Color32::RED,
                                        )
                                    })
                                    .collect();
                                let actual = paint_plot_hover(ui, pointer, bounds, &text);
                                assert!(bounds.expand(0.1).contains_rect(actual), "{actual:?}");
                            });
                        },
                    );
                }
            }
        }
    }

    #[test]
    fn readout_prefers_below_right_and_flips_at_plot_edges() {
        let bounds = Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(600.0, 500.0));
        let size = egui::vec2(160.0, 80.0);
        for pointer in [
            egui::pos2(110.0, 110.0),
            egui::pos2(590.0, 110.0),
            egui::pos2(110.0, 490.0),
            egui::pos2(590.0, 490.0),
        ] {
            let rect = hover_rect(pointer, size, bounds);
            assert!(bounds.contains_rect(rect));
            if pointer.x < bounds.center().x {
                assert!(rect.left() > pointer.x);
            } else {
                assert!(rect.right() < pointer.x);
            }
            if pointer.y < bounds.center().y {
                assert!(rect.top() > pointer.y);
            } else {
                assert!(rect.bottom() < pointer.y);
            }
        }
        assert_eq!(
            hover_rect(bounds.center(), egui::vec2(1000.0, 1000.0), bounds),
            bounds
        );
    }
}
