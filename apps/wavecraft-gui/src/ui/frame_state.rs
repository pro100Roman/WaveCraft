//! The read-only values `run_frame` derives from application state before it
//! paints: which controls are enabled, the active capture target, and snapshots
//! of settings that the panels may edit.

use crate::{
    active_continuous_capture_accepts_recording, manual_calibration_control_is_enabled,
    radio_trigger_level_controls_are_enabled, radio_trigger_level_runtime_controls_are_enabled,
    scope_frontend_controls_are_enabled, scope_trigger_controls_are_enabled, selected_radio,
    AcquisitionMode, CaptureTarget, DsCopeCalibrationValues, RadioSingleTrigger,
    ScopeCaptureSettings, UsbSpeedClass, WaveCraftApp, WaveformTab,
};

/// Computed once per frame by [`WaveCraftApp::frame_state`] and handed to the
/// panels. Fields ending in `_before_ui` are copies taken before painting; the
/// code that applies the frame compares them with the state afterwards.
#[derive(Clone, Copy)]
pub(crate) struct FrameState {
    pub(crate) control_target: Option<CaptureTarget>,
    pub(crate) control_target_available: bool,
    pub(crate) capture_settings_enabled: bool,
    pub(crate) radio_runtime_controls_enabled: bool,
    pub(crate) radio_trigger_runtime_controls_enabled: bool,
    pub(crate) rf_trigger_level_controls_enabled: bool,
    pub(crate) can_attach_run_capture_recording: bool,
    pub(crate) can_attach_run_decoded_recording: bool,
    pub(crate) logic_settings_speed: UsbSpeedClass,
    pub(crate) selected_scope_device: bool,
    pub(crate) scope_capture_requested: bool,
    pub(crate) manual_calibration_enabled: bool,
    pub(crate) trigger_controls_enabled: bool,
    pub(crate) frontend_controls_enabled: bool,
    pub(crate) scope_settings_before_ui: ScopeCaptureSettings,
    pub(crate) scope_auto_sample_rate_before_ui: bool,
    pub(crate) manual_calibration_before_ui: Option<DsCopeCalibrationValues>,
    pub(crate) manual_calibration_range_before_ui: Option<usize>,
}

impl WaveCraftApp {
    /// Derives the frame's enablement flags and pre-paint snapshots. It only
    /// reads application state.
    pub(crate) fn frame_state(&self) -> FrameState {
        let control_target = self.active_waveform_tab.map(WaveformTab::capture_target);
        let capture_settings_enabled = control_target
            .map_or(true, |target| !self.capture_target_is_active(target))
            && self.pending.is_none();
        let radio_runtime_controls_enabled = self
            .capture_for_target(CaptureTarget::RadioReceiver)
            .is_some_and(|active| {
                active.mode == AcquisitionMode::Run
                    && active.has_radio
                    && active.radio_runtime_control.is_some()
            });
        let can_attach_run_capture_recording = control_target
            .and_then(|target| self.capture_for_target(target))
            .is_some_and(|active| {
                active_continuous_capture_accepts_recording(active.mode, active.recorder.is_some())
            });
        let active_radio_run_recorders = self
            .capture_for_target(CaptureTarget::RadioReceiver)
            .filter(|active| active.mode == AcquisitionMode::Run && active.has_radio)
            .map(|active| {
                (
                    active.recorder.is_some(),
                    active.decoded_data_recorder.is_some(),
                )
            });
        let can_attach_run_decoded_recording =
            active_radio_run_recorders.is_some_and(|(_, decoded_active)| !decoded_active);
        let radio_trigger_runtime_controls_enabled = self
            .capture_for_target(CaptureTarget::RadioReceiver)
            .is_some_and(|active| {
                active.has_radio
                    && active
                        .radio_trigger
                        .as_ref()
                        .is_some_and(RadioSingleTrigger::is_armed)
            });
        let radio_trigger_level_runtime_controls_enabled =
            radio_trigger_level_runtime_controls_are_enabled(
                radio_runtime_controls_enabled,
                radio_trigger_runtime_controls_enabled,
            );
        let rf_trigger_level_controls_enabled = radio_trigger_level_controls_are_enabled(
            capture_settings_enabled,
            radio_trigger_level_runtime_controls_enabled,
        );
        let selected_logic_device = self.selected_logic_device().is_some();
        let logic_settings_speed = self
            .selected_logic_device()
            .map(|device| device.speed)
            .or_else(|| {
                self.devices
                    .iter()
                    .filter(|entry| entry.device.profile.model.is_logic_analyzer())
                    .map(|entry| entry.device.speed)
                    .min()
            })
            .unwrap_or(UsbSpeedClass::HighSpeed);
        let selected_scope_device = self.selected_scope_device_ref().is_some();
        let selected_radio_device =
            selected_radio(&self.radios, self.selected_radio_id.as_deref()).is_some();
        let control_target_available = control_target.is_some_and(|target| {
            target.is_available(
                selected_logic_device,
                selected_scope_device,
                selected_radio_device,
            )
        });
        let scope_capture_requested = self
            .capture_requests
            .iter()
            .any(|target| target.includes_oscilloscope());
        let active_scope_capture = self.capture_for_target(CaptureTarget::Oscilloscope);
        let manual_calibration_enabled = manual_calibration_control_is_enabled(
            self.pending.is_some(),
            selected_scope_device,
            self.manual_calibration.is_some(),
            active_scope_capture.map(|active| active.mode),
        );
        let trigger_controls_enabled = scope_trigger_controls_are_enabled(
            self.pending.is_some(),
            scope_capture_requested,
            selected_scope_device,
            active_scope_capture.map(|active| !active.scope_trigger_controls.is_empty()),
        );
        let frontend_controls_enabled = scope_frontend_controls_are_enabled(
            self.pending.is_some(),
            scope_capture_requested,
            selected_scope_device,
        ) && !((self.record_capture || self.record_decoded_data)
            && self.pending.is_some());
        let scope_settings_before_ui = self.scope_settings;
        let scope_auto_sample_rate_before_ui = self.scope_auto_sample_rate;
        let manual_calibration_before_ui =
            self.manual_calibration.as_ref().map(|editor| editor.values);
        let manual_calibration_range_before_ui =
            self.manual_calibration.as_ref().map(|editor| editor.range);
        FrameState {
            control_target,
            control_target_available,
            capture_settings_enabled,
            radio_runtime_controls_enabled,
            radio_trigger_runtime_controls_enabled,
            rf_trigger_level_controls_enabled,
            can_attach_run_capture_recording,
            can_attach_run_decoded_recording,
            logic_settings_speed,
            selected_scope_device,
            scope_capture_requested,
            manual_calibration_enabled,
            trigger_controls_enabled,
            frontend_controls_enabled,
            scope_settings_before_ui,
            scope_auto_sample_rate_before_ui,
            manual_calibration_before_ui,
            manual_calibration_range_before_ui,
        }
    }
}
