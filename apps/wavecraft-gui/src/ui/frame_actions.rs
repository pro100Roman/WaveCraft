//! State that the panels painted by `update()` accumulate during one frame and
//! that `update()` applies after painting.

use crate::{
    choose_decoded_data_save_path, choose_recording_save_path, choose_recording_to_open,
    choose_saleae_export_path, ui::frame_state::FrameState, AcquisitionMode, CaptureTarget,
    SavedCaptureSettingsKind, ScopeCalibrationAction, WaveCraftApp,
};

/// What the user asked for this frame. Panels set fields while they paint; the
/// application applies them once all panels are done, so painting never starts
/// or stops captures itself.
#[derive(Default)]
pub(crate) struct FrameActions {
    pub(crate) refresh_clicked: bool,
    pub(crate) snapshot_clicked: bool,
    pub(crate) record_now_clicked: bool,
    pub(crate) record_decoded_data_clicked: bool,
    pub(crate) open_recording_clicked: bool,
    pub(crate) export_saleae_clicked: bool,
    pub(crate) start_request: Option<(AcquisitionMode, CaptureTarget)>,
    pub(crate) stop_clicked: bool,
    pub(crate) calibration_action: Option<ScopeCalibrationAction>,
    pub(crate) save_capture_settings_kind: Option<SavedCaptureSettingsKind>,
    pub(crate) logic_device_selection_changed: bool,
    pub(crate) scope_device_selection_changed: bool,
    pub(crate) radio_device_selection_changed: bool,
}

impl WaveCraftApp {
    /// Applies the toolbar requests in `actions`: recording, opening and
    /// exporting files, refreshing devices, starting, and stopping.
    ///
    /// A recording request may add a start request, so `actions` is mutable.
    pub(crate) fn apply_toolbar_actions(&mut self, actions: &mut FrameActions, frame: &FrameState) {
        let FrameState {
            control_target,
            can_attach_run_capture_recording,
            can_attach_run_decoded_recording,
            ..
        } = *frame;
        if actions.record_now_clicked {
            self.record_capture = false;
            self.record_decoded_data = false;
            if let Some(path) = choose_recording_save_path(&self.recording_path) {
                self.recording_path = path.to_string_lossy().into_owned();
                if can_attach_run_capture_recording {
                    if let Some(target) = control_target {
                        self.attach_run_capture_recording(target, path);
                    }
                } else if let Some(target) = control_target {
                    self.record_capture = true;
                    let mode = if target == CaptureTarget::RadioReceiver {
                        AcquisitionMode::Run
                    } else {
                        AcquisitionMode::Single
                    };
                    actions.start_request = Some((mode, target));
                }
            }
        }
        if actions.record_decoded_data_clicked {
            self.record_capture = false;
            self.record_decoded_data = false;
            if let Some(path) = choose_decoded_data_save_path(&self.decoded_data_path) {
                self.decoded_data_path = path.to_string_lossy().into_owned();
                if can_attach_run_decoded_recording {
                    self.attach_run_decoded_data_recording(path);
                } else if control_target == Some(CaptureTarget::RadioReceiver) {
                    self.record_decoded_data = true;
                    actions.start_request =
                        Some((AcquisitionMode::Run, CaptureTarget::RadioReceiver));
                }
            }
        }
        if actions.open_recording_clicked {
            if let Some(path) = choose_recording_to_open() {
                self.open_selected_recording(path);
            }
        }
        if actions.export_saleae_clicked {
            if let Some(path) = choose_saleae_export_path() {
                self.begin_export_saleae(path);
            }
        }
        if actions.refresh_clicked {
            self.begin_refresh();
        }
        if !actions.open_recording_clicked {
            if let Some((mode, target)) = actions.start_request {
                self.begin_capture(mode, target);
            }
        }
        if actions.stop_clicked {
            if let Some(target) = control_target {
                let requested = self
                    .capture_requests
                    .iter()
                    .copied()
                    .find(|requested| requested.includes(target));
                if let Some(requested) = requested {
                    self.capture_requests.remove(&requested);
                    self.queued_capture_rearms
                        .retain(|(_, target)| *target != requested);
                    if self.requested_capture_target == Some(requested) {
                        self.requested_capture_target = None;
                    }
                    if self
                        .scheduled_repetitive_capture
                        .is_some_and(|scheduled| scheduled.target == requested)
                    {
                        self.scheduled_repetitive_capture = None;
                    }
                }
                self.scope_capture_restart_requested = false;
                self.scope_configuration_due = None;
                if self.capture_target_is_active(target) {
                    self.stop_capture(target);
                } else if self.pending.is_some() {
                    self.status = "Stopping acquisition after device preparation…".to_owned();
                } else {
                    self.status = "Acquisition stopped".to_owned();
                }
            }
        }
    }
}
