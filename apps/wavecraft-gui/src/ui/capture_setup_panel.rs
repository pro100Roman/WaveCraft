//! The fixed left panel: device selectors and per-instrument capture settings.

use crate::{
    egui, paint_capture_device_selectors, paint_capture_settings, paint_radio_capture_settings,
    paint_radio_device_selector, selected_radio, CaptureSettingsAvailability, CaptureSettingsTab,
    LogicCaptureSettingsEditor, RichText, SavedCaptureSettingsKind, ScopeCaptureSettingsEditor,
    ScopeVisualizationSettingsEditor, UsbSpeedClass, WaveformTab, PANEL, TEXT_MUTED,
};

use crate::{ui::frame_actions::FrameActions, WaveCraftApp};

/// Values `update()` computes before painting that the capture setup panel reads.
#[derive(Clone, Copy)]
pub(crate) struct CaptureSetupInputs {
    pub(crate) capture_settings_enabled: bool,
    pub(crate) radio_runtime_controls_enabled: bool,
    pub(crate) radio_trigger_runtime_controls_enabled: bool,
    pub(crate) frontend_controls_enabled: bool,
    pub(crate) manual_calibration_enabled: bool,
    pub(crate) logic_settings_speed: UsbSpeedClass,
}

impl WaveCraftApp {
    /// Paints the capture setup panel and records the user's requests in `actions`.
    pub(crate) fn paint_capture_setup_panel(
        &mut self,
        context: &egui::Context,
        inputs: CaptureSetupInputs,
        actions: &mut FrameActions,
    ) {
        let CaptureSetupInputs {
            capture_settings_enabled,
            radio_runtime_controls_enabled,
            radio_trigger_runtime_controls_enabled,
            frontend_controls_enabled,
            manual_calibration_enabled,
            logic_settings_speed,
        } = inputs;
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
