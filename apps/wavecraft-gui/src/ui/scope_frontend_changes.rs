//! Reacts to oscilloscope frontend, trigger, and manual calibration edits made
//! while `update()` painted the panels.

use crate::{
    CaptureTarget, DsCopeCalibrationValues, Instant, ScopeCaptureSettings, DSCOPE_VOLTS_PER_DIV_MV,
    SCOPE_CONFIGURATION_DEBOUNCE,
};

use crate::{ui::frame_actions::FrameActions, WaveCraftApp};

/// Values `update()` captured before painting, to compare with the state after.
#[derive(Clone, Copy)]
pub(crate) struct ScopeFrontendInputs {
    pub(crate) scope_settings_before_ui: ScopeCaptureSettings,
    pub(crate) manual_calibration_before_ui: Option<DsCopeCalibrationValues>,
    pub(crate) manual_calibration_range_before_ui: Option<usize>,
    pub(crate) scope_capture_requested: bool,
    pub(crate) selected_scope_device: bool,
}

impl WaveCraftApp {
    /// Compares the oscilloscope settings with their state before painting and
    /// applies runtime updates, schedules a debounced reconfiguration, or
    /// restarts the capture as needed.
    ///
    /// Reconfiguration is held back while the user starts, stops, or refreshes
    /// in the same frame.
    pub(crate) fn apply_scope_frontend_changes(
        &mut self,
        actions: &FrameActions,
        inputs: ScopeFrontendInputs,
    ) {
        let ScopeFrontendInputs {
            scope_settings_before_ui,
            manual_calibration_before_ui,
            manual_calibration_range_before_ui,
            scope_capture_requested,
            selected_scope_device,
        } = inputs;
        if actions.scope_device_selection_changed
            && self.selected_scope_device_ref().is_some()
            && self.pending.is_none()
            && !self.capture_target_is_active(CaptureTarget::Oscilloscope)
        {
            self.scope_configuration_due = Some(Instant::now() + SCOPE_CONFIGURATION_DEBOUNCE);
        }
        let manual_calibration_range_after_ui =
            self.manual_calibration.as_ref().map(|editor| editor.range);
        if manual_calibration_range_after_ui != manual_calibration_range_before_ui {
            if let Some(range) = manual_calibration_range_after_ui {
                let volts_per_div_mv = DSCOPE_VOLTS_PER_DIV_MV[range];
                for channel in &mut self.scope_settings.channels {
                    channel.volts_per_div_mv = volts_per_div_mv;
                }
            }
        }
        let manual_calibration_changed =
            self.manual_calibration.as_ref().map(|editor| editor.values)
                != manual_calibration_before_ui;
        let runtime_frontend_changed = self
            .scope_settings
            .runtime_frontend_differs_from(scope_settings_before_ui);
        let trigger_changed = self
            .scope_settings
            .trigger
            .hardware_differs_from(scope_settings_before_ui.trigger);
        let mut runtime_calibration_applied = false;
        if manual_calibration_changed && !runtime_frontend_changed && scope_capture_requested {
            if let (Some(values), Some(active)) = (
                self.manual_calibration.as_ref().map(|editor| editor.values),
                self.capture_for_target(CaptureTarget::Oscilloscope),
            ) {
                runtime_calibration_applied = active
                    .scope_calibration_controls
                    .iter()
                    .all(|control| control.update(values).is_ok());
            }
        }
        let manual_calibration_requires_restart =
            manual_calibration_changed && !runtime_calibration_applied;
        if (runtime_frontend_changed || manual_calibration_requires_restart)
            && scope_capture_requested
            && selected_scope_device
        {
            self.scope_configuration_due = Some(Instant::now() + SCOPE_CONFIGURATION_DEBOUNCE);
            self.status = "Adjusting oscilloscope frontend…".to_owned();
            self.last_error = None;
        } else if runtime_calibration_applied {
            self.scope_configuration_due = None;
            self.status = "Applying manual calibration during capture…".to_owned();
            self.last_error = None;
        } else if trigger_changed && self.capture_target_is_active(CaptureTarget::Oscilloscope) {
            let update_result = self
                .scope_settings
                .trigger
                .capture_config()
                .and_then(|trigger| {
                    let Some(active) = self.capture_for_target_mut(CaptureTarget::Oscilloscope)
                    else {
                        return Err("capture ended before the trigger update".to_owned());
                    };
                    let updated_config = active
                        .scope_config
                        .with_trigger_config(trigger)
                        .map_err(|error| error.to_string())?;
                    for control in &active.scope_trigger_controls {
                        control.update(trigger).map_err(|error| error.to_string())?;
                    }
                    active.scope_config = updated_config;
                    Ok(())
                });
            match update_result {
                Ok(()) => {
                    self.scope_configuration_due = None;
                    self.status = "Updating trigger during capture…".to_owned();
                    self.last_error = None;
                }
                Err(error) => self.set_error(error),
            }
        } else if self
            .scope_settings
            .frontend_differs_from(scope_settings_before_ui)
        {
            self.scope_configuration_due = Some(Instant::now() + SCOPE_CONFIGURATION_DEBOUNCE);
            self.status = "Adjusting DSCope configuration…".to_owned();
        }
        let scope_configuration_is_due = self
            .scope_configuration_due
            .is_some_and(|due| Instant::now() >= due);
        if !actions.refresh_clicked
            && actions.start_request.is_none()
            && !actions.stop_clicked
            && scope_configuration_is_due
        {
            if scope_capture_requested && selected_scope_device {
                self.scope_configuration_due = None;
                self.scope_capture_restart_requested = true;
                if let Some(active) = self.capture_for_target(CaptureTarget::Oscilloscope) {
                    active.session.request_stop();
                }
                self.status = "Reconfiguring oscilloscope frontend…".to_owned();
            } else if self.pending.is_none()
                && !self.capture_target_is_active(CaptureTarget::Oscilloscope)
            {
                self.begin_scope_configuration();
            }
        }
    }
}
