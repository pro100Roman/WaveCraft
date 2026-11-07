//! State that the panels painted by `update()` accumulate during one frame and
//! that `update()` applies after painting.

use crate::{AcquisitionMode, CaptureTarget, SavedCaptureSettingsKind, ScopeCalibrationAction};

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
