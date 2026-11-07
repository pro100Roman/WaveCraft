//! Modal dialogs and the instrument list shown in settings.

use crate::{
    egui, CalibrationModalAction, CalibrationProgressModal, Color32, DeviceEntry, LinkStatus,
    RichText, Sense, Vec2, ACCENT, BACKGROUND, SCOPE_COLORS, TEXT_MUTED,
};

pub(crate) fn paint_calibration_progress_modal(
    context: &egui::Context,
    state: &CalibrationProgressModal,
) -> Option<CalibrationModalAction> {
    let screen_rect = context.input(|input| input.screen_rect());
    egui::Area::new(egui::Id::new("calibration_progress_modal_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen_rect.min)
        .show(context, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, Color32::from_black_alpha(112));
        });

    let mut action = None;
    egui::Window::new("DSCope Auto Calibration")
        .id(egui::Id::new("calibration_progress_modal"))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(430.0)
        .show(context, |ui| {
            if state.completed {
                ui.label(RichText::new(&state.phase).strong().color(ACCENT));
                ui.add_space(8.0);
                ui.add(egui::ProgressBar::new(1.0).show_percentage().fill(ACCENT));
                ui.add_space(12.0);
                if let Some(values) = state.values {
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            action = Some(CalibrationModalAction::Close);
                        }
                        if ui.button("Save").clicked() {
                            action = Some(CalibrationModalAction::Save(values));
                        }
                    });
                } else if ui.button("Close").clicked() {
                    action = Some(CalibrationModalAction::Close);
                }
            } else {
                ui.label(RichText::new(&state.phase).strong());
                ui.add_space(8.0);
                ui.add(
                    egui::ProgressBar::new(state.progress)
                        .show_percentage()
                        .animate(true)
                        .fill(ACCENT),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Keep all probes disconnected. Do not disconnect the oscilloscope.",
                    )
                    .color(Color32::LIGHT_RED),
                );
            }
        });
    action
}

pub(crate) fn paint_instrument_settings(ui: &mut egui::Ui, devices: &[DeviceEntry]) {
    if devices.is_empty() {
        ui.label(RichText::new("No supported USB instruments").color(TEXT_MUTED));
        return;
    }

    for entry in devices {
        ui.add_space(4.0);
        egui::Frame::none()
            .fill(BACKGROUND)
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(10.0))
            .show(ui, |ui| {
                ui.label(RichText::new(entry.device.profile.display_name).strong());
                ui.small(RichText::new(entry.device.location.stable_id()).color(TEXT_MUTED));
                let (link_text, link_color) = match entry.device.link_status() {
                    LinkStatus::Optimal => (
                        format!("{} · optimal", entry.device.speed.description()),
                        ACCENT,
                    ),
                    LinkStatus::ReducedPerformance => (
                        format!("{} · reduced performance", entry.device.speed.description()),
                        SCOPE_COLORS[0],
                    ),
                    LinkStatus::Unsupported => {
                        ("Unsupported link speed".to_owned(), Color32::LIGHT_RED)
                    }
                };
                ui.small(RichText::new(link_text).color(link_color));
                ui.small(
                    RichText::new(if entry.device.profile.model.supports_capture() {
                        "Acquisition driver available"
                    } else {
                        "Discovery, probe, and FPGA preparation available"
                    })
                    .color(TEXT_MUTED),
                );
            });
    }
}
