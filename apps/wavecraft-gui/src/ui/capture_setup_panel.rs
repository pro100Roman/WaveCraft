//! The fixed left panel: device selectors and per-instrument capture settings.

use crate::{
    egui, paint_capture_settings, paint_radio_capture_settings, paint_radio_device_selector,
    selected_device_for_role, selected_radio, ui::frame_state::FrameState,
    CaptureSettingsAvailability, CaptureSettingsTab, DeviceEntry, DeviceRole,
    LogicCaptureSettingsEditor, RichText, SavedCaptureSettingsKind, ScopeCaptureSettingsEditor,
    ScopeVisualizationSettingsEditor, WaveformTab, PANEL, TEXT_MUTED,
};

use crate::{ui::frame_actions::FrameActions, WaveCraftApp};

impl WaveCraftApp {
    /// Paints the capture setup panel and records the user's requests in `actions`.
    pub(crate) fn paint_capture_setup_panel(
        &mut self,
        context: &egui::Context,
        frame: &FrameState,
        actions: &mut FrameActions,
    ) {
        let FrameState {
            capture_settings_enabled,
            radio_runtime_controls_enabled,
            radio_trigger_runtime_controls_enabled,
            frontend_controls_enabled,
            manual_calibration_enabled,
            logic_settings_speed,
            ..
        } = *frame;
        let (displayed_logic_sample_count, displayed_logic_sample_rate_hz) = self
            .active_capture_for_view()
            .filter(|active| !active.preserve_display_until_complete)
            .map(|active| &active.waveforms)
            .or_else(|| {
                self.displayed_completed_capture()
                    .map(|capture| &capture.waveforms)
            })
            .map(|waveforms| (waveforms.logic_sample_count, waveforms.logic_rate_hz))
            .unwrap_or((0, 0));

        egui::SidePanel::left("capture_setup_fixed")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(context, |ui| {
                let available_capture_settings_tabs = self.available_capture_settings_tabs();
                let content_width = ui.available_width();
                egui::ScrollArea::vertical()
                    .max_width(content_width)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(content_width);
                        ui.set_max_width(content_width);
                        ui.label(RichText::new("CAPTURE SETUP").strong().color(TEXT_MUTED));
                        ui.add_space(8.0);
                        let (logic_changed, scope_changed) = paint_capture_device_selectors(
                            ui,
                            self.active_waveform_tab,
                            &self.devices,
                            &mut self.selected_logic_device_id,
                            &mut self.selected_scope_device_id,
                            capture_settings_enabled && self.manual_calibration.is_none(),
                        );
                        actions.logic_device_selection_changed |= logic_changed;
                        actions.scope_device_selection_changed |= scope_changed;
                        if self.active_waveform_tab == Some(WaveformTab::RadioReceiver) {
                            actions.radio_device_selection_changed |= paint_radio_device_selector(
                                ui,
                                &self.radios,
                                &mut self.selected_radio_id,
                                capture_settings_enabled,
                            );
                            if actions.radio_device_selection_changed {
                                self.reconcile_radio_settings();
                            }
                            if let Some(radio) =
                                selected_radio(&self.radios, self.selected_radio_id.as_deref())
                                    .cloned()
                            {
                                if paint_radio_capture_settings(
                                    ui,
                                    &radio,
                                    &mut self.radio_settings,
                                    capture_settings_enabled,
                                    radio_runtime_controls_enabled,
                                    radio_trigger_runtime_controls_enabled,
                                    true,
                                ) {
                                    actions.save_capture_settings_kind =
                                        Some(SavedCaptureSettingsKind::RadioFrequency);
                                }
                            }
                        } else if self.active_waveform_tab == Some(WaveformTab::Combined)
                            && !self.radios.is_empty()
                        {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("Radio receiver · included")
                                    .small()
                                    .color(TEXT_MUTED),
                            );
                            actions.radio_device_selection_changed |= paint_radio_device_selector(
                                ui,
                                &self.radios,
                                &mut self.selected_radio_id,
                                capture_settings_enabled,
                            );
                            if actions.radio_device_selection_changed {
                                self.reconcile_radio_settings();
                            }
                        }
                        if self.active_waveform_tab.is_some() {
                            ui.add_space(12.0);
                        }
                        if available_capture_settings_tabs.len() > 1 {
                            ui.horizontal(|ui| {
                                for tab in &available_capture_settings_tabs {
                                    ui.selectable_value(
                                        &mut self.active_capture_settings_tab,
                                        *tab,
                                        tab.label(),
                                    );
                                }
                            });
                            ui.add_space(6.0);
                        }
                        if available_capture_settings_tabs.is_empty()
                            && self.active_waveform_tab != Some(WaveformTab::RadioReceiver)
                        {
                            ui.label(
                                RichText::new(
                                    "Connect a supported instrument to configure capture.",
                                )
                                .color(TEXT_MUTED),
                            );
                        } else if self.active_waveform_tab != Some(WaveformTab::RadioReceiver)
                            && paint_capture_settings(
                                ui,
                                self.active_capture_settings_tab,
                                LogicCaptureSettingsEditor {
                                    settings: &mut self.logic_settings,
                                    speed: logic_settings_speed,
                                    acquisition_mode: &mut self.logic_acquisition_mode,
                                    repetitive_interval_seconds: &mut self
                                        .repetitive_interval_seconds,
                                    smart_timing_enabled: &mut self.logic_smart_timing_enabled,
                                    marker_settings: &mut self.logic_marker_settings,
                                    protocol_analyzer: &mut self.protocol_analyzer,
                                    displayed_sample_count: displayed_logic_sample_count,
                                    displayed_sample_rate_hz: displayed_logic_sample_rate_hz,
                                },
                                ScopeCaptureSettingsEditor {
                                    settings: &mut self.scope_settings,
                                    auto_sample_rate: &mut self.scope_auto_sample_rate,
                                    smart_timing_enabled: &mut self.scope_smart_timing_enabled,
                                },
                                ScopeVisualizationSettingsEditor {
                                    channel_colors: &mut self.scope_channel_colors,
                                    scope_channel_names: &mut self.scope_channel_names,
                                },
                                CaptureSettingsAvailability {
                                    capture: capture_settings_enabled,
                                    sample_rate: frontend_controls_enabled,
                                    channel_enable: frontend_controls_enabled,
                                    input_range: frontend_controls_enabled,
                                    manual_calibration: manual_calibration_enabled,
                                },
                                &mut actions.calibration_action,
                            )
                        {
                            actions.save_capture_settings_kind =
                                Some(match self.active_capture_settings_tab {
                                    CaptureSettingsTab::DsLogic => {
                                        SavedCaptureSettingsKind::LogicAnalyzer
                                    }
                                    CaptureSettingsTab::DsCope => {
                                        SavedCaptureSettingsKind::Oscilloscope
                                    }
                                });
                        }
                    });
            });
    }
}

pub(crate) fn selected_device_text(
    devices: &[DeviceEntry],
    selected_id: Option<&str>,
    role: DeviceRole,
) -> String {
    match selected_id {
        Some(id) => selected_device_for_role(devices, Some(id), role)
            .map(|device| format!("{} · {id}", device.profile.display_name))
            .unwrap_or_else(|| format!("Unavailable · {id}")),
        None => role.empty_label().to_owned(),
    }
}

pub(crate) fn paint_device_selector(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    label: &str,
    devices: &[DeviceEntry],
    selected_id: &mut Option<String>,
    role: DeviceRole,
    enabled: bool,
) -> bool {
    let choices = devices
        .iter()
        .map(|entry| &entry.device)
        .filter(|device| role.matches(device))
        .map(|device| {
            let id = device.location.stable_id();
            (
                id.clone(),
                format!("{} · {id}", device.profile.display_name),
            )
        })
        .collect::<Vec<_>>();
    let before = selected_id.clone();
    ui.label(RichText::new(label).small().color(TEXT_MUTED));
    ui.add_enabled_ui(enabled && !choices.is_empty(), |ui| {
        egui::ComboBox::from_id_salt(id_salt)
            .width(ui.available_width())
            .selected_text(selected_device_text(devices, selected_id.as_deref(), role))
            .show_ui(ui, |ui| {
                for (id, text) in choices {
                    ui.selectable_value(selected_id, Some(id), text);
                }
            });
    });
    before != *selected_id
}

pub(crate) fn paint_capture_device_selectors(
    ui: &mut egui::Ui,
    active_tab: Option<WaveformTab>,
    devices: &[DeviceEntry],
    selected_logic_device_id: &mut Option<String>,
    selected_scope_device_id: &mut Option<String>,
    enabled: bool,
) -> (bool, bool) {
    let mut logic_changed = false;
    let mut scope_changed = false;
    match active_tab {
        Some(WaveformTab::LogicAnalyzer) => {
            logic_changed = paint_device_selector(
                ui,
                "logic_device_selector",
                "Device",
                devices,
                selected_logic_device_id,
                DeviceRole::LogicAnalyzer,
                enabled,
            );
        }
        Some(WaveformTab::Oscilloscope | WaveformTab::Lissajous) => {
            scope_changed = paint_device_selector(
                ui,
                "scope_device_selector",
                "Device",
                devices,
                selected_scope_device_id,
                DeviceRole::Oscilloscope,
                enabled,
            );
        }
        Some(WaveformTab::Combined) => {
            logic_changed = paint_device_selector(
                ui,
                "combined_logic_device_selector",
                "Logic analyzer",
                devices,
                selected_logic_device_id,
                DeviceRole::LogicAnalyzer,
                enabled,
            );
            ui.add_space(4.0);
            scope_changed = paint_device_selector(
                ui,
                "combined_scope_device_selector",
                "Oscilloscope",
                devices,
                selected_scope_device_id,
                DeviceRole::Oscilloscope,
                enabled,
            );
        }
        Some(WaveformTab::RadioReceiver) => {}
        None => {}
    }
    (logic_changed, scope_changed)
}
