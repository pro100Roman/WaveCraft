//! Handling the results of the background device, calibration, and file work.

use crate::{
    create_decoded_data_recorder, finish_gui_recording, radio_single_trigger_config,
    recording_capture_target, recording_completion_label, scope_rolling_sample_capacity,
    start_gui_recording, usb_speed_warning, uses_rolling_scope_buffer, AcquisitionMode,
    ActiveCapture, CaptureReady, CaptureSession, CaptureSettingsTab, CaptureTarget,
    CompletionStatus, DeviceDescriptor, DeviceEntry, DiscoveredDevice, DsCopeCalibrationValues,
    DsCopeCaptureConfig, DsCopeRuntimeTriggerControl, DsCopeTriggerSource, DsLogicCaptureConfig,
    FileDecodedDataRecorder, FileRecorder, GuiRecordingSettings, HashSet, InstrumentKind,
    LoadedRecording, LogicDecodeWorker, ManualCalibrationEditor, Path, PathBuf,
    ProtocolAnalyzerInstance, ProtocolCaptureDecoder, ProtocolDecodeSnapshot, RadioDiscoveryReport,
    RadioSingleTrigger, ScopeMeasurementCache, TryRecvError, WaveformStore, WaveformTab,
    WorkerSuccess, DSCOPE_VOLTS_PER_DIV_MV,
};

use crate::WaveCraftApp;

/// The files a starting capture writes to, with the paths they were opened at.
struct CaptureRecorders {
    recorder: Option<FileRecorder>,
    recording_path: Option<PathBuf>,
    decoded_data_recorder: Option<FileDecodedDataRecorder>,
    decoded_data_path: Option<PathBuf>,
}

/// What a starting capture shows: the waveforms to fill and the decoder that
/// reads the logic samples while they arrive.
struct CaptureDisplay {
    preserve_display_until_complete: bool,
    waveforms: WaveformStore,
    protocol_decode_snapshot: ProtocolDecodeSnapshot,
    protocol_analyzers: Vec<ProtocolAnalyzerInstance>,
    logic_decode: LogicDecodeWorker,
}

/// Everything the status line of a started capture depends on.
#[derive(Clone, Copy)]
struct StartedCaptureFacts<'a> {
    mode: AcquisitionMode,
    target: CaptureTarget,
    instrument_count: usize,
    recording_path: Option<&'a Path>,
    decoded_data_path: Option<&'a Path>,
    waiting_for_radio_trigger: bool,
    has_logic_trigger: bool,
    has_scope_trigger: bool,
    scope_trigger_is_auto: bool,
    radio_trigger_level_dbfs: f32,
}

fn started_capture_status(facts: &StartedCaptureFacts) -> String {
    let StartedCaptureFacts {
        mode,
        target,
        instrument_count: count,
        recording_path,
        decoded_data_path,
        waiting_for_radio_trigger,
        has_logic_trigger,
        has_scope_trigger,
        scope_trigger_is_auto,
        radio_trigger_level_dbfs: level,
    } = *facts;
    if let Some(path) = decoded_data_path {
        return format!(
            "RF Run acquisition from {count} instrument(s) · decoded data to {}",
            path.display()
        );
    }
    match (mode, recording_path) {
        (AcquisitionMode::Single, Some(path)) if waiting_for_radio_trigger => format!(
            "Single · waiting for RF trigger at {level:.1} dBFS; recording to {}",
            path.display()
        ),
        (AcquisitionMode::Single, Some(path)) if has_logic_trigger => format!(
            "Single · waiting for logic trigger; recording to {}",
            path.display()
        ),
        (AcquisitionMode::Single, Some(path)) => {
            format!(
                "{} Single acquisition from {count} instrument(s) to {}",
                target.label(),
                path.display()
            )
        }
        (AcquisitionMode::Single, None) => {
            if waiting_for_radio_trigger {
                format!("Single · waiting for RF trigger at {level:.1} dBFS")
            } else if has_logic_trigger {
                "Single · waiting for logic trigger".to_owned()
            } else if has_scope_trigger && !scope_trigger_is_auto {
                "Single · waiting for trigger".to_owned()
            } else {
                format!(
                    "{} Single acquisition from {count} instrument(s)",
                    target.label()
                )
            }
        }
        (AcquisitionMode::Repetitive, _) => format!(
            "{} Repetitive acquisition from {count} instrument(s)",
            target.label()
        ),
        (AcquisitionMode::Loop, _) => format!(
            "{} Loop acquisition from {count} instrument(s)",
            target.label()
        ),
        (AcquisitionMode::Run, Some(path)) => format!(
            "{} Run acquisition from {count} instrument(s) · recording to {}",
            target.label(),
            path.display()
        ),
        (AcquisitionMode::Run, None) => {
            if has_scope_trigger && scope_trigger_is_auto {
                "Run · Auto".to_owned()
            } else if has_scope_trigger {
                "Run · waiting for trigger".to_owned()
            } else {
                format!(
                    "{} Run acquisition from {count} instrument(s)",
                    target.label()
                )
            }
        }
    }
}

impl WaveCraftApp {
    pub(crate) fn poll_worker(&mut self) {
        let Some(receiver) = self.pending.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err("background operation ended unexpectedly".to_owned())
            }
        };
        self.pending = None;
        match result {
            Ok(WorkerSuccess::Devices { usb, radio }) => self.handle_devices_found(usb, radio),
            Ok(WorkerSuccess::Capture(ready)) => self.handle_capture_ready(*ready),
            Ok(WorkerSuccess::RecordingLoaded { path, recording }) => {
                self.handle_recording_loaded(path, recording)
            }
            Ok(WorkerSuccess::SaleaeExported(path)) => {
                self.status = format!("Exported Saleae capture to {}", path.display());
                self.last_error = None;
            }
            Ok(WorkerSuccess::ScopeConfigured) => {
                self.status = "Applied DSCope configuration to the selected instrument".to_owned();
            }
            Ok(WorkerSuccess::ScopeAutoCalibrated(values)) => {
                self.handle_scope_auto_calibrated(values)
            }
            Ok(WorkerSuccess::ScopeAutoCalibrationSaved) => {
                self.handle_scope_auto_calibration_saved()
            }
            Ok(WorkerSuccess::ScopeCalibrationLoaded(values)) => {
                self.handle_scope_calibration_loaded(values)
            }
            Ok(WorkerSuccess::ScopeCalibrationSaved) => self.handle_scope_calibration_saved(),
            Err(error) => {
                self.record_capture = false;
                self.record_decoded_data = false;
                self.set_error(error);
            }
        }
    }

    /// Records the instruments found by a refresh and reconciles the selection.
    pub(crate) fn handle_devices_found(
        &mut self,
        usb: Vec<DiscoveredDevice>,
        radio: RadioDiscoveryReport,
    ) {
        self.usb_speed_warning = usb_speed_warning(&usb);
        self.devices = usb
            .into_iter()
            .map(|device| DeviceEntry { device })
            .collect();
        self.radios = radio.devices;
        self.radio_discovery_failures = radio.failures;
        self.reconcile_device_selections();
        let detected_count = self.devices.len() + self.radios.len();
        self.status = if detected_count == 0 {
            "No supported instruments detected".to_owned()
        } else {
            format!("Detected {detected_count} supported instrument(s)")
        };
    }

    /// Takes over a capture that has started on its worker: starts the recorders,
    /// decides whether the display continues the previous capture, and adds it to
    /// the running captures. A capture nobody wants any more is stopped instead.
    pub(crate) fn handle_capture_ready(&mut self, ready: CaptureReady) {
        let Some(ready) = self.discard_capture_nobody_wants(ready) else {
            return;
        };
        let CaptureReady {
            session,
            mode,
            target,
            retain_protocol_history,
            logic_config,
            scope_config,
            scope_trigger_controls,
            scope_calibration_controls,
            radio_runtime_control,
            radio_spectrum,
            radio_dsp,
        } = ready;
        let scope_config =
            match self.scope_config_with_latest_trigger(scope_config, &scope_trigger_controls) {
                Ok(scope_config) => scope_config,
                Err(error) => {
                    self.abort_capture(session, error);
                    return;
                }
            };
        let count = session.descriptors().len();
        let mut recorders =
            match self.start_capture_recorders(session.descriptors(), logic_config, scope_config) {
                Ok(recorders) => recorders,
                Err(error) => {
                    self.abort_capture(session, error);
                    return;
                }
            };
        let has_instrument = |kind| {
            session
                .descriptors()
                .iter()
                .any(|device| device.kind == kind)
        };
        let has_logic = has_instrument(InstrumentKind::LogicAnalyzer);
        let has_scope = has_instrument(InstrumentKind::Oscilloscope);
        let has_radio = has_instrument(InstrumentKind::RadioReceiver);
        let radio_trigger = match self.arm_radio_trigger(mode, target, has_radio) {
            Ok(trigger) => trigger,
            Err(error) => {
                // The capture file is already open; do not leave it without a footer.
                let _ = finish_gui_recording(&mut recorders.recorder, CompletionStatus::Failed);
                self.abort_capture(session, error);
                return;
            }
        };
        let display = self.prepare_capture_display(
            mode,
            target,
            has_scope,
            retain_protocol_history,
            logic_config,
            scope_config,
        );
        let status = started_capture_status(&StartedCaptureFacts {
            mode,
            target,
            instrument_count: count,
            recording_path: recorders.recording_path.as_deref(),
            decoded_data_path: recorders.decoded_data_path.as_deref(),
            waiting_for_radio_trigger: radio_trigger.is_some(),
            has_logic_trigger: has_logic && logic_config.trigger().is_some(),
            has_scope_trigger: !scope_trigger_controls.is_empty(),
            scope_trigger_is_auto: scope_config.trigger().source() == DsCopeTriggerSource::Auto,
            radio_trigger_level_dbfs: f32::from(self.radio_settings.trigger_level_dbfs_tenths)
                / 10.0,
        });
        self.captures.push(ActiveCapture {
            session,
            mode,
            target,
            preserve_display_until_complete: display.preserve_display_until_complete,
            scope_config,
            scope_trigger_controls,
            scope_calibration_controls,
            radio_runtime_control,
            stopped: HashSet::new(),
            waveforms: display.waveforms,
            protocol_decode_snapshot: display.protocol_decode_snapshot,
            protocol_analyzers: display.protocol_analyzers,
            logic_decode: display.logic_decode,
            radio_spectrum,
            radio_dsp,
            radio_trigger,
            recorder: recorders.recorder,
            recording_path: recorders.recording_path,
            decoded_data_recorder: recorders.decoded_data_recorder,
            decoded_data_path: recorders.decoded_data_path,
            has_logic,
            has_scope,
            has_radio,
        });
        self.requested_capture_target = None;
        self.record_capture = false;
        self.record_decoded_data = false;
        self.status = status;
    }

    /// Stops a capture that must not be shown: one nobody asks for any more, or
    /// one that a scope frontend change has already made out of date (which then
    /// starts again). Returns the capture when it should be shown.
    fn discard_capture_nobody_wants(&mut self, ready: CaptureReady) -> Option<CaptureReady> {
        if !self.capture_is_requested(ready.target) {
            self.requested_capture_target = None;
            self.record_capture = false;
            self.record_decoded_data = false;
            match ready.session.stop() {
                Ok(()) => self.status = "Acquisition stopped".to_owned(),
                Err(error) => self.set_error(error.to_string()),
            }
            return None;
        }
        if self.scope_capture_restart_requested {
            self.scope_capture_restart_requested = false;
            match ready.session.stop() {
                Ok(()) => {
                    self.status = "Reconfiguring oscilloscope frontend…".to_owned();
                    self.begin_capture(ready.mode, ready.target);
                }
                Err(error) => self.set_error(error.to_string()),
            }
            return None;
        }
        Some(ready)
    }

    /// Stops a capture that could not be set up and reports why, adding the
    /// reason the stop failed too when it did.
    fn abort_capture(&mut self, session: CaptureSession, error: String) {
        let cleanup = session.stop().err();
        self.set_error(match cleanup {
            Some(cleanup) => format!("{error}; additionally failed to stop: {cleanup}"),
            None => error,
        });
    }

    /// The scope config with the trigger the user has chosen since the capture was
    /// requested. The new trigger is also sent to the running source, which is
    /// the only place it can change without restarting.
    fn scope_config_with_latest_trigger(
        &self,
        scope_config: DsCopeCaptureConfig,
        trigger_controls: &[DsCopeRuntimeTriggerControl],
    ) -> Result<DsCopeCaptureConfig, String> {
        if trigger_controls.is_empty() {
            return Ok(scope_config);
        }
        let desired_trigger = self.scope_settings.trigger.capture_config()?;
        if desired_trigger == scope_config.trigger() {
            return Ok(scope_config);
        }
        scope_config
            .with_trigger_config(desired_trigger)
            .map_err(|error| error.to_string())
            .and_then(|updated_config| {
                for control in trigger_controls {
                    control
                        .update(desired_trigger)
                        .map_err(|error| error.to_string())?;
                }
                Ok(updated_config)
            })
            .map_err(|error| format!("failed to apply the latest trigger settings: {error}"))
    }

    /// Opens the capture and decoded-data files the user asked for. A decoded-data
    /// file that cannot be created marks the capture file as failed.
    fn start_capture_recorders(
        &self,
        descriptors: &[DeviceDescriptor],
        logic_config: DsLogicCaptureConfig,
        scope_config: DsCopeCaptureConfig,
    ) -> Result<CaptureRecorders, String> {
        let recording_path = self
            .record_capture
            .then(|| PathBuf::from(self.recording_path.trim()));
        let mut recorder = match recording_path.as_deref() {
            Some(path) => Some(start_gui_recording(
                path,
                descriptors,
                GuiRecordingSettings {
                    logic_config,
                    logic_settings: self.logic_settings,
                    scope_config,
                    scope_settings: self.scope_settings,
                    radio_settings: &self.radio_settings,
                    logic_channel_names: &self.logic_channel_names,
                    scope_channel_names: &self.scope_channel_names,
                },
            )?),
            None => None,
        };
        let decoded_data_path = self
            .record_decoded_data
            .then(|| PathBuf::from(self.decoded_data_path.trim()));
        let decoded_data_recorder = match decoded_data_path.as_deref() {
            Some(path) => match create_decoded_data_recorder(path, &self.radio_settings) {
                Ok(recorder) => Some(recorder),
                Err(error) => {
                    let _ = finish_gui_recording(&mut recorder, CompletionStatus::Failed);
                    return Err(error);
                }
            },
            None => None,
        };
        Ok(CaptureRecorders {
            recorder,
            recording_path,
            decoded_data_recorder,
            decoded_data_path,
        })
    }

    /// The single-shot RF trigger for a capture that waits for one, if it does.
    fn arm_radio_trigger(
        &self,
        mode: AcquisitionMode,
        target: CaptureTarget,
        has_radio: bool,
    ) -> Result<Option<RadioSingleTrigger>, String> {
        let waits_for_trigger = mode == AcquisitionMode::Single
            && target.includes_radio_receiver()
            && has_radio
            && self.radio_settings.trigger_enabled;
        if !waits_for_trigger {
            return Ok(None);
        }
        radio_single_trigger_config(&self.radio_settings)
            .and_then(RadioSingleTrigger::new)
            .map(Some)
            .map_err(|error| format!("failed to arm RF trigger: {error}"))
    }

    /// Decides whether the new capture continues the finished one on display or
    /// replaces it, and builds the waveform store and decoder it will fill.
    fn prepare_capture_display(
        &mut self,
        mode: AcquisitionMode,
        target: CaptureTarget,
        has_scope: bool,
        retain_protocol_history: bool,
        logic_config: DsLogicCaptureConfig,
        scope_config: DsCopeCaptureConfig,
    ) -> CaptureDisplay {
        let rolling = uses_rolling_scope_buffer(mode, has_scope, scope_config.trigger().source());
        let scope_capacity = if rolling {
            scope_rolling_sample_capacity(scope_config, self.scope_time_per_division_seconds)
        } else {
            scope_config.sample_count() as usize
        };
        let continue_roll = rolling
            && self
                .completed_capture_for_target(target)
                .is_some_and(|capture| {
                    capture
                        .waveforms
                        .can_continue_scope_roll(scope_config, scope_capacity)
                });
        let continue_logic_loop = mode == AcquisitionMode::Loop
            && self
                .completed_capture_for_target(target)
                .is_some_and(|capture| {
                    capture.has_logic && capture.waveforms.can_continue_logic_loop(logic_config)
                });
        let preserve_display_until_complete =
            mode == AcquisitionMode::Run || mode == AcquisitionMode::Repetitive;
        let preserve_display_until_complete = preserve_display_until_complete
            && self.completed_capture_for_target(target).is_some()
            && !continue_roll
            && target != CaptureTarget::RadioReceiver;
        let prior_protocol_history = retain_protocol_history
            .then(|| self.completed_capture_for_target(target))
            .flatten()
            .map(|capture| capture.protocol_decode.snapshot());
        let continued_capture = if continue_roll || continue_logic_loop {
            self.take_completed_capture_for_target(target)
        } else {
            None
        };
        let (mut waveforms, mut protocol_decode) = continued_capture.map_or_else(
            || {
                (
                    WaveformStore::default(),
                    ProtocolCaptureDecoder::new(&self.protocol_analyzer.analyzers),
                )
            },
            |capture| (capture.waveforms, capture.protocol_decode),
        );
        if let Some(snapshot) = &prior_protocol_history {
            protocol_decode.retain_history_from(snapshot);
        }
        protocol_decode.reconfigure(&self.protocol_analyzer.analyzers);
        let protocol_decode_snapshot = protocol_decode.snapshot();
        let protocol_analyzers = self.protocol_analyzer.analyzers.clone();
        let logic_decode = LogicDecodeWorker::start(protocol_decode);
        if !continue_logic_loop {
            waveforms.configure_logic_capture(logic_config.sample_count());
        }
        waveforms.scope_capacity = scope_capacity;
        waveforms.scope_rolling = rolling;
        CaptureDisplay {
            preserve_display_until_complete,
            waveforms,
            protocol_decode_snapshot,
            protocol_analyzers,
            logic_decode,
        }
    }

    /// Shows a recording that was opened from a file.
    pub(crate) fn handle_recording_loaded(
        &mut self,
        path: PathBuf,
        recording: Box<LoadedRecording>,
    ) {
        if let Some(active) = self.active_waveform_tab {
            self.save_waveform_view_settings(active);
        }
        let LoadedRecording {
            capture,
            logic_settings,
            scope_settings,
            radio_settings,
            logic_channel_names,
            scope_channel_names,
            completion,
        } = *recording;
        let Some(target) =
            recording_capture_target(capture.has_logic, capture.has_scope, capture.has_radio)
        else {
            self.set_error("recording does not contain a supported capture".to_owned());
            return;
        };
        if let Some(logic_settings) = logic_settings {
            self.logic_settings = logic_settings;
        }
        if let Some(scope_settings) = scope_settings {
            self.scope_settings = scope_settings;
            self.scope_auto_sample_rate = false;
        }
        if let Some(radio_settings) = radio_settings {
            self.radio_settings = radio_settings;
        }
        self.set_completed_capture_for_target(target, capture);
        self.logic_marker_settings.clear();
        self.logic_channel_names = logic_channel_names;
        self.scope_channel_names = scope_channel_names;
        self.record_capture = false;
        self.record_decoded_data = false;
        self.logic_viewport.reset();
        self.scope_viewport.reset();
        self.combined_viewport.reset();
        self.scope_fft_viewport.reset();
        self.scope_measurement_caches = [ScopeMeasurementCache::default(); 2];
        self.scope_time_per_division_seconds = None;
        match target {
            CaptureTarget::LogicAnalyzer => {
                self.save_active_logic_session_settings();
                self.active_waveform_tab = Some(WaveformTab::LogicAnalyzer);
                self.active_capture_settings_tab = CaptureSettingsTab::DsLogic;
            }
            CaptureTarget::Oscilloscope => {
                self.save_active_scope_session_settings();
                self.active_waveform_tab = Some(WaveformTab::Oscilloscope);
                self.active_capture_settings_tab = CaptureSettingsTab::DsCope;
            }
            CaptureTarget::RadioReceiver => {
                self.active_waveform_tab = Some(WaveformTab::RadioReceiver);
            }
            CaptureTarget::Combined => {
                self.combined_settings = self.current_combined_session_settings();
                self.active_waveform_tab = Some(WaveformTab::Combined);
            }
        }
        self.status = format!(
            "Opened {} · {} recording",
            path.display(),
            recording_completion_label(completion)
        );
        self.last_error = None;
    }

    /// Offers to save the result of a finished DSCope auto calibration.
    pub(crate) fn handle_scope_auto_calibrated(&mut self, values: DsCopeCalibrationValues) {
        self.calibration_active = false;
        self.calibration_progress = None;
        self.calibration_display = None;
        if let Some(modal) = self.calibration_modal.as_mut() {
            modal.progress = 1.0;
            modal.phase = "Calibration is done. Save the result to device NVM?".to_owned();
            modal.completed = true;
            modal.values = Some(values);
        }
        self.status = "DSCope auto calibration finished · result not saved".to_owned();
    }

    /// Reports that the auto calibration result was saved to the device.
    pub(crate) fn handle_scope_auto_calibration_saved(&mut self) {
        if let Some(modal) = self.calibration_modal.as_mut() {
            modal.progress = 1.0;
            modal.phase = "Calibration was saved to device NVM.".to_owned();
            modal.completed = true;
            modal.values = None;
        }
        self.status = "DSCope auto calibration saved and verified".to_owned();
    }

    /// Opens the manual calibration editor with the values read from the device.
    pub(crate) fn handle_scope_calibration_loaded(&mut self, values: DsCopeCalibrationValues) {
        let range = DSCOPE_VOLTS_PER_DIV_MV
            .iter()
            .position(|value| *value == self.scope_settings.channels[0].volts_per_div_mv)
            .unwrap_or(0);
        for channel in &mut self.scope_settings.channels {
            channel.volts_per_div_mv = DSCOPE_VOLTS_PER_DIV_MV[range];
        }
        self.manual_calibration = Some(ManualCalibrationEditor { values, range });
        self.status = "Loaded DSCope calibration values".to_owned();
        if let Some(target) = self.requested_capture_target {
            if self.capture_is_requested(target) {
                self.begin_capture(AcquisitionMode::Run, target);
            }
        }
    }

    /// Reports that the manual calibration was saved to the device.
    pub(crate) fn handle_scope_calibration_saved(&mut self) {
        self.manual_calibration = None;
        self.status = "DSCope manual calibration saved and verified".to_owned();
        if let Some(target) = self.requested_capture_target {
            if self.capture_is_requested(target) {
                self.begin_capture(AcquisitionMode::Run, target);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    use crate::app::test_support::{
        app, completed_capture, ready, ready_with, request, scratch_directory,
    };
    use std::sync::mpsc;
    use wavecraft_usb::{DeviceLocation, DSLOGIC_U3PRO16};

    /// Queues `result` as the finished background operation and polls it.
    fn deliver(app: &mut WaveCraftApp, result: WorkerResult) {
        let (sender, receiver) = mpsc::channel();
        sender.send(result).unwrap();
        app.pending = Some(receiver);
        app.poll_worker();
    }

    fn loaded_recording(
        app: &WaveCraftApp,
        (has_logic, has_scope, has_radio): (bool, bool, bool),
    ) -> Box<LoadedRecording> {
        let mut logic_channel_names = empty_logic_channel_names();
        logic_channel_names[0] = "CLK".to_owned();
        Box::new(LoadedRecording {
            capture: completed_capture(app, has_logic, has_scope, has_radio),
            logic_settings: None,
            scope_settings: None,
            radio_settings: None,
            logic_channel_names,
            scope_channel_names: ["Probe A".to_owned(), "Probe B".to_owned()],
            completion: CompletionStatus::Complete,
        })
    }

    fn status_facts() -> StartedCaptureFacts<'static> {
        StartedCaptureFacts {
            mode: AcquisitionMode::Run,
            target: CaptureTarget::LogicAnalyzer,
            instrument_count: 2,
            recording_path: None,
            decoded_data_path: None,
            waiting_for_radio_trigger: false,
            has_logic_trigger: false,
            has_scope_trigger: false,
            scope_trigger_is_auto: false,
            radio_trigger_level_dbfs: -42.5,
        }
    }

    #[test]
    fn the_started_capture_status_describes_every_mode_and_trigger() {
        let file = Some(Path::new("cap.dsv"));
        let single = StartedCaptureFacts {
            mode: AcquisitionMode::Single,
            ..status_facts()
        };
        let run = status_facts();
        let cases = [
            (
                "single, RF trigger, recording",
                StartedCaptureFacts {
                    recording_path: file,
                    waiting_for_radio_trigger: true,
                    // The RF trigger is named first when a logic trigger exists too.
                    has_logic_trigger: true,
                    ..single
                },
                "Single · waiting for RF trigger at -42.5 dBFS; recording to cap.dsv",
            ),
            (
                "single, logic trigger, recording",
                StartedCaptureFacts {
                    recording_path: file,
                    has_logic_trigger: true,
                    ..single
                },
                "Single · waiting for logic trigger; recording to cap.dsv",
            ),
            (
                "single, recording",
                StartedCaptureFacts {
                    recording_path: file,
                    // Only the logic and RF triggers are named while recording.
                    has_scope_trigger: true,
                    ..single
                },
                "Logic Analyzer Single acquisition from 2 instrument(s) to cap.dsv",
            ),
            (
                "single, RF trigger",
                StartedCaptureFacts {
                    waiting_for_radio_trigger: true,
                    has_logic_trigger: true,
                    ..single
                },
                "Single · waiting for RF trigger at -42.5 dBFS",
            ),
            (
                "single, logic trigger",
                StartedCaptureFacts {
                    has_logic_trigger: true,
                    has_scope_trigger: true,
                    ..single
                },
                "Single · waiting for logic trigger",
            ),
            (
                "single, scope trigger",
                StartedCaptureFacts {
                    has_scope_trigger: true,
                    ..single
                },
                "Single · waiting for trigger",
            ),
            (
                "single, scope auto trigger does not wait",
                StartedCaptureFacts {
                    has_scope_trigger: true,
                    scope_trigger_is_auto: true,
                    ..single
                },
                "Logic Analyzer Single acquisition from 2 instrument(s)",
            ),
            (
                "single",
                single,
                "Logic Analyzer Single acquisition from 2 instrument(s)",
            ),
            (
                "repetitive ignores the recording",
                StartedCaptureFacts {
                    mode: AcquisitionMode::Repetitive,
                    recording_path: file,
                    ..run
                },
                "Logic Analyzer Repetitive acquisition from 2 instrument(s)",
            ),
            (
                "loop",
                StartedCaptureFacts {
                    mode: AcquisitionMode::Loop,
                    target: CaptureTarget::Combined,
                    ..run
                },
                "Combined Loop acquisition from 2 instrument(s)",
            ),
            (
                "run, recording",
                StartedCaptureFacts {
                    recording_path: file,
                    has_scope_trigger: true,
                    ..run
                },
                "Logic Analyzer Run acquisition from 2 instrument(s) · recording to cap.dsv",
            ),
            (
                "run, scope auto trigger",
                StartedCaptureFacts {
                    has_scope_trigger: true,
                    scope_trigger_is_auto: true,
                    ..run
                },
                "Run · Auto",
            ),
            (
                "run, scope trigger",
                StartedCaptureFacts {
                    has_scope_trigger: true,
                    ..run
                },
                "Run · waiting for trigger",
            ),
            (
                "run",
                run,
                "Logic Analyzer Run acquisition from 2 instrument(s)",
            ),
            (
                "decoded data replaces the mode text",
                StartedCaptureFacts {
                    target: CaptureTarget::RadioReceiver,
                    recording_path: file,
                    decoded_data_path: Some(Path::new("decoded.dsv")),
                    ..run
                },
                "RF Run acquisition from 2 instrument(s) · decoded data to decoded.dsv",
            ),
        ];
        for (label, facts, expected) in cases {
            assert_eq!(started_capture_status(&facts), expected, "{label}");
        }
    }

    #[test]
    fn poll_worker_leaves_an_operation_that_is_still_running_pending() {
        let mut app = app();
        let (_sender, receiver) = mpsc::channel::<WorkerResult>();
        app.pending = Some(receiver);

        app.poll_worker();

        assert!(app.pending.is_some());
        assert!(app.last_error.is_none());
    }

    #[test]
    fn poll_worker_reports_an_operation_that_ended_without_a_result() {
        let mut app = app();
        let (sender, receiver) = mpsc::channel::<WorkerResult>();
        app.pending = Some(receiver);
        drop(sender);

        app.poll_worker();

        assert!(app.pending.is_none());
        assert_eq!(
            app.last_error.as_deref(),
            Some("background operation ended unexpectedly")
        );
    }

    #[test]
    fn a_failed_operation_is_reported_and_cancels_the_pending_recordings() {
        let mut app = app();
        app.record_capture = true;
        app.record_decoded_data = true;

        deliver(&mut app, Err("the device went away".to_owned()));

        assert!(app.pending.is_none());
        assert!(!app.record_capture);
        assert!(!app.record_decoded_data);
        assert_eq!(app.last_error.as_deref(), Some("the device went away"));
    }

    #[test]
    fn a_finished_saleae_export_clears_the_previous_error() {
        let mut app = app();
        app.last_error = Some("old failure".to_owned());

        deliver(
            &mut app,
            Ok(WorkerSuccess::SaleaeExported(PathBuf::from("out.sal"))),
        );

        assert_eq!(app.status, "Exported Saleae capture to out.sal");
        assert!(app.last_error.is_none());
    }

    #[test]
    fn applying_the_scope_configuration_reports_it_in_the_status() {
        let mut app = app();

        deliver(&mut app, Ok(WorkerSuccess::ScopeConfigured));

        assert_eq!(
            app.status,
            "Applied DSCope configuration to the selected instrument"
        );
    }

    #[test]
    fn a_refresh_without_instruments_reports_that_none_were_detected() {
        let mut app = app();

        deliver(
            &mut app,
            Ok(WorkerSuccess::Devices {
                usb: Vec::new(),
                radio: RadioDiscoveryReport::default(),
            }),
        );

        assert!(app.devices.is_empty());
        assert!(app.usb_speed_warning.is_none());
        assert_eq!(app.status, "No supported instruments detected");
    }

    #[test]
    fn found_instruments_are_listed_selected_and_counted() {
        let mut app = app();
        let device = |platform_path: &str, speed| DiscoveredDevice {
            profile: &DSLOGIC_U3PRO16,
            location: DeviceLocation {
                platform_path: platform_path.to_owned(),
            },
            speed,
        };
        let fast = device("usb:1-1", UsbSpeedClass::SuperSpeed);
        let slow = device("usb:1-2", UsbSpeedClass::HighSpeed);
        let first_id = fast.location.stable_id();

        app.handle_devices_found(vec![fast, slow], RadioDiscoveryReport::default());

        assert_eq!(app.devices.len(), 2);
        assert_eq!(app.status, "Detected 2 supported instrument(s)");
        assert_eq!(app.selected_logic_device_id, Some(first_id));
        let warning = app
            .usb_speed_warning
            .expect("one link is slower than USB 3");
        assert_eq!(warning.devices.len(), 1);
        assert_eq!(warning.devices[0].location.platform_path, "usb:1-2");
    }

    #[test]
    fn a_capture_nobody_requested_any_more_is_stopped_instead_of_shown() {
        let mut app = app();
        app.record_capture = true;
        app.record_decoded_data = true;
        let started = ready(
            &app,
            InstrumentKind::LogicAnalyzer,
            AcquisitionMode::Run,
            CaptureTarget::LogicAnalyzer,
        );

        app.handle_capture_ready(started);

        assert!(app.captures.is_empty());
        assert!(!app.record_capture);
        assert!(!app.record_decoded_data);
        assert_eq!(app.status, "Acquisition stopped");
    }

    #[test]
    fn a_requested_capture_becomes_active_and_clears_the_request() {
        let mut app = app();
        request(&mut app, CaptureTarget::LogicAnalyzer);
        let started = ready(
            &app,
            InstrumentKind::LogicAnalyzer,
            AcquisitionMode::Run,
            CaptureTarget::LogicAnalyzer,
        );

        app.handle_capture_ready(started);

        assert_eq!(app.captures.len(), 1);
        let capture = &app.captures[0];
        assert_eq!(capture.target, CaptureTarget::LogicAnalyzer);
        assert!(capture.has_logic && !capture.has_scope && !capture.has_radio);
        assert!(!capture.preserve_display_until_complete);
        assert!(capture.radio_trigger.is_none());
        assert!(capture.recorder.is_none());
        assert!(app.requested_capture_target.is_none());
        assert_eq!(
            app.status,
            "Logic Analyzer Run acquisition from 1 instrument(s)"
        );
    }

    #[test]
    fn the_status_names_the_acquisition_mode() {
        for (mode, expected) in [
            (
                AcquisitionMode::Single,
                "Logic Analyzer Single acquisition from 1 instrument(s)",
            ),
            (
                AcquisitionMode::Repetitive,
                "Logic Analyzer Repetitive acquisition from 1 instrument(s)",
            ),
            (
                AcquisitionMode::Loop,
                "Logic Analyzer Loop acquisition from 1 instrument(s)",
            ),
            (
                AcquisitionMode::Run,
                "Logic Analyzer Run acquisition from 1 instrument(s)",
            ),
        ] {
            let mut app = app();
            request(&mut app, CaptureTarget::LogicAnalyzer);
            let started = ready(
                &app,
                InstrumentKind::LogicAnalyzer,
                mode,
                CaptureTarget::LogicAnalyzer,
            );

            app.handle_capture_ready(started);

            assert_eq!(app.captures.len(), 1, "{mode:?}");
            assert_eq!(app.status, expected, "{mode:?}");
        }
    }

    #[test]
    fn run_and_repetitive_keep_the_finished_capture_on_display_until_the_new_one_completes() {
        for (mode, keeps_display) in [
            (AcquisitionMode::Run, true),
            (AcquisitionMode::Repetitive, true),
            (AcquisitionMode::Single, false),
            (AcquisitionMode::Loop, false),
        ] {
            let mut app = app();
            let finished = completed_capture(&app, true, false, false);
            app.set_completed_capture_for_target(CaptureTarget::LogicAnalyzer, finished);
            request(&mut app, CaptureTarget::LogicAnalyzer);
            let started = ready(
                &app,
                InstrumentKind::LogicAnalyzer,
                mode,
                CaptureTarget::LogicAnalyzer,
            );

            app.handle_capture_ready(started);

            assert_eq!(
                app.captures[0].preserve_display_until_complete, keeps_display,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_loop_capture_continues_the_finished_logic_waveform_only_when_the_settings_match() {
        // (mode, sample rate offset from the finished capture, continues)
        for (mode, rate_offset, continues) in [
            (AcquisitionMode::Loop, 0, true),
            (AcquisitionMode::Loop, 1, false),
            (AcquisitionMode::Run, 0, false),
            (AcquisitionMode::Single, 0, false),
        ] {
            let mut app = app();
            let config = app.logic_settings.capture_config().unwrap();
            let mut finished = completed_capture(&app, true, false, false);
            finished
                .waveforms
                .configure_logic_capture(config.sample_count());
            finished.waveforms.logic_rate_hz = config.sample_rate_hz() + rate_offset;
            finished.waveforms.logic_sample_count = 42;
            app.set_completed_capture_for_target(CaptureTarget::LogicAnalyzer, finished);
            request(&mut app, CaptureTarget::LogicAnalyzer);
            let started = ready(
                &app,
                InstrumentKind::LogicAnalyzer,
                mode,
                CaptureTarget::LogicAnalyzer,
            );

            app.handle_capture_ready(started);

            let label = format!("{mode:?} with a rate offset of {rate_offset}");
            assert_eq!(
                app.captures[0].waveforms.logic_sample_count,
                if continues { 42 } else { 0 },
                "{label}"
            );
            // A continued waveform moves into the running capture; otherwise the
            // finished one stays on display.
            assert_eq!(
                app.completed_capture_for_target(CaptureTarget::LogicAnalyzer)
                    .is_none(),
                continues,
                "{label}"
            );
        }
    }

    #[test]
    fn a_pending_scope_frontend_change_restarts_the_capture_instead_of_showing_it() {
        let mut app = app();
        request(&mut app, CaptureTarget::Oscilloscope);
        app.scope_capture_restart_requested = true;
        let started = ready(
            &app,
            InstrumentKind::Oscilloscope,
            AcquisitionMode::Run,
            CaptureTarget::Oscilloscope,
        );

        app.handle_capture_ready(started);

        assert!(!app.scope_capture_restart_requested);
        assert!(app.captures.is_empty());
        assert_eq!(app.status, "Reconfiguring oscilloscope frontend…");
        // The restart goes through begin_capture, which needs a selected DSCope
        // and so reports that none is connected here.
        assert!(app
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("DSCope")));
    }

    #[test]
    fn a_recording_file_that_cannot_be_created_stops_the_capture_and_reports_the_error() {
        let directory = scratch_directory("recording-failure");
        let mut app = app();
        request(&mut app, CaptureTarget::LogicAnalyzer);
        app.record_capture = true;
        app.recording_path = directory
            .join("missing")
            .join("capture.dsv")
            .to_string_lossy()
            .into_owned();
        let started = ready(
            &app,
            InstrumentKind::LogicAnalyzer,
            AcquisitionMode::Run,
            CaptureTarget::LogicAnalyzer,
        );

        app.handle_capture_ready(started);

        assert!(app.captures.is_empty());
        assert!(app.last_error.is_some());
        assert!(app.requested_capture_target.is_none());
        assert!(!app.capture_is_requested(CaptureTarget::LogicAnalyzer));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_capture_that_also_fails_to_stop_is_reported_together_with_the_original_error() {
        let directory = scratch_directory("recording-and-stop-failure");
        let mut app = app();
        request(&mut app, CaptureTarget::LogicAnalyzer);
        app.record_capture = true;
        app.recording_path = directory
            .join("missing")
            .join("capture.dsv")
            .to_string_lossy()
            .into_owned();
        let started = ready_with(
            &app,
            &[InstrumentKind::LogicAnalyzer],
            true,
            AcquisitionMode::Run,
            CaptureTarget::LogicAnalyzer,
        );

        app.handle_capture_ready(started);

        assert!(app.captures.is_empty());
        let error = app.last_error.expect("the failure is reported");
        assert!(
            error.contains("; additionally failed to stop: a capture worker panicked"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_decoded_data_file_that_cannot_be_created_stops_the_capture_and_reports_the_error() {
        let directory = scratch_directory("decoded-failure");
        let mut app = app();
        request(&mut app, CaptureTarget::RadioReceiver);
        app.record_decoded_data = true;
        app.decoded_data_path = directory
            .join("missing")
            .join("decoded.dsv")
            .to_string_lossy()
            .into_owned();
        let started = ready(
            &app,
            InstrumentKind::RadioReceiver,
            AcquisitionMode::Run,
            CaptureTarget::RadioReceiver,
        );

        app.handle_capture_ready(started);

        assert!(app.captures.is_empty());
        assert!(app.last_error.is_some());
        assert!(!app.capture_is_requested(CaptureTarget::RadioReceiver));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_requested_recording_is_opened_and_named_in_the_status() {
        let directory = scratch_directory("recording-ok");
        let path = directory.join("capture.dsv");
        let mut app = app();
        request(&mut app, CaptureTarget::LogicAnalyzer);
        app.record_capture = true;
        // Whitespace around the path typed into the field is not part of the name.
        app.recording_path = format!("  {} ", path.display());
        let started = ready(
            &app,
            InstrumentKind::LogicAnalyzer,
            AcquisitionMode::Run,
            CaptureTarget::LogicAnalyzer,
        );

        app.handle_capture_ready(started);

        assert!(app.last_error.is_none(), "{:?}", app.last_error);
        assert_eq!(app.captures.len(), 1);
        assert!(app.captures[0].recorder.is_some());
        assert_eq!(app.captures[0].recording_path.as_deref(), Some(&*path));
        assert!(path.is_file());
        assert!(!app.record_capture);
        assert!(app.status.contains("recording to"));
        assert!(app.status.contains("capture.dsv"));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn only_a_single_rf_capture_with_the_trigger_enabled_waits_for_the_trigger() {
        // (target, mode, trigger enabled, whether the capture waits)
        for (target, mode, trigger_enabled, waits) in [
            (
                CaptureTarget::RadioReceiver,
                AcquisitionMode::Single,
                true,
                true,
            ),
            (
                CaptureTarget::RadioReceiver,
                AcquisitionMode::Single,
                false,
                false,
            ),
            (
                CaptureTarget::RadioReceiver,
                AcquisitionMode::Run,
                true,
                false,
            ),
            (CaptureTarget::Combined, AcquisitionMode::Single, true, true),
        ] {
            let mut app = app();
            request(&mut app, target);
            app.radio_settings.trigger_enabled = trigger_enabled;
            app.radio_settings.trigger_level_dbfs_tenths = -425;
            let started = ready_with(&app, &[InstrumentKind::RadioReceiver], false, mode, target);

            app.handle_capture_ready(started);

            let label = format!("{target:?} {mode:?} trigger_enabled = {trigger_enabled}");
            assert_eq!(app.captures.len(), 1, "{label}");
            assert!(app.captures[0].has_radio, "{label}");
            assert_eq!(app.captures[0].radio_trigger.is_some(), waits, "{label}");
            assert_eq!(
                app.status == "Single · waiting for RF trigger at -42.5 dBFS",
                waits,
                "{label}: {}",
                app.status
            );
        }
    }

    #[test]
    fn a_single_rf_capture_without_a_radio_instrument_does_not_wait_for_the_trigger() {
        let mut app = app();
        request(&mut app, CaptureTarget::Combined);
        app.radio_settings.trigger_enabled = true;
        let started = ready_with(
            &app,
            &[InstrumentKind::LogicAnalyzer, InstrumentKind::Oscilloscope],
            false,
            AcquisitionMode::Single,
            CaptureTarget::Combined,
        );

        app.handle_capture_ready(started);

        assert!(app.captures[0].radio_trigger.is_none());
    }

    #[test]
    fn an_rf_trigger_that_cannot_be_armed_stops_the_capture_and_reports_the_error() {
        let mut app = app();
        request(&mut app, CaptureTarget::RadioReceiver);
        app.radio_settings.trigger_enabled = true;
        app.radio_settings.trigger_posttrigger_micros = u64::MAX;
        let started = ready(
            &app,
            InstrumentKind::RadioReceiver,
            AcquisitionMode::Single,
            CaptureTarget::RadioReceiver,
        );

        app.handle_capture_ready(started);

        assert!(app.captures.is_empty());
        assert!(app
            .last_error
            .as_deref()
            .is_some_and(|error| error.starts_with("failed to arm RF trigger")));
    }

    /// An idle scope capture whose trigger can be changed while it runs. The
    /// returned source must stay alive for the control to keep working.
    fn scope_trigger_control(
        app: &WaveCraftApp,
    ) -> (DsCopeCaptureSource, DsCopeRuntimeTriggerControl) {
        let device = DiscoveredDevice {
            profile: &wavecraft_usb::DSCOPE_U3P100,
            location: DeviceLocation {
                platform_path: "usb:scope".to_owned(),
            },
            speed: UsbSpeedClass::SuperSpeed,
        };
        DsCopeCaptureSource::with_runtime_trigger_control(
            device,
            app.scope_settings.capture_config().unwrap(),
        )
        .expect("a DSCope source can be built without hardware")
    }

    #[test]
    fn a_scope_capture_that_can_update_its_trigger_starts_with_the_latest_one() {
        let mut app = app();
        request(&mut app, CaptureTarget::Oscilloscope);
        let (_source, control) = scope_trigger_control(&app);
        let mut started = ready(
            &app,
            InstrumentKind::Oscilloscope,
            AcquisitionMode::Run,
            CaptureTarget::Oscilloscope,
        );
        started.scope_trigger_controls = vec![control];
        assert_eq!(
            started.scope_config.trigger().source(),
            DsCopeTriggerSource::Auto
        );
        // The user changed the trigger while the capture was starting.
        app.scope_settings.trigger.source = DsCopeTriggerSource::ChannelZero;

        app.handle_capture_ready(started);

        assert!(app.last_error.is_none(), "{:?}", app.last_error);
        assert_eq!(app.captures.len(), 1);
        assert_eq!(
            app.captures[0].scope_config.trigger().source(),
            DsCopeTriggerSource::ChannelZero
        );
        assert_eq!(app.status, "Run · waiting for trigger");
    }

    #[test]
    fn the_status_of_a_scope_capture_names_its_trigger() {
        for (mode, source, expected) in [
            (
                AcquisitionMode::Run,
                DsCopeTriggerSource::Auto,
                "Run · Auto",
            ),
            (
                AcquisitionMode::Run,
                DsCopeTriggerSource::ChannelOne,
                "Run · waiting for trigger",
            ),
            (
                AcquisitionMode::Single,
                DsCopeTriggerSource::ChannelOne,
                "Single · waiting for trigger",
            ),
            (
                AcquisitionMode::Single,
                DsCopeTriggerSource::Auto,
                "Oscilloscope Single acquisition from 1 instrument(s)",
            ),
        ] {
            let mut app = app();
            app.scope_settings.trigger.source = source;
            request(&mut app, CaptureTarget::Oscilloscope);
            let (_source, control) = scope_trigger_control(&app);
            let mut started = ready(
                &app,
                InstrumentKind::Oscilloscope,
                mode,
                CaptureTarget::Oscilloscope,
            );
            started.scope_trigger_controls = vec![control];

            app.handle_capture_ready(started);

            assert_eq!(app.status, expected, "{mode:?} {source:?}");
        }
    }

    #[test]
    fn a_trigger_that_the_running_capture_no_longer_accepts_stops_it_and_reports_the_error() {
        let mut app = app();
        request(&mut app, CaptureTarget::Oscilloscope);
        let (source, control) = scope_trigger_control(&app);
        let mut started = ready(
            &app,
            InstrumentKind::Oscilloscope,
            AcquisitionMode::Run,
            CaptureTarget::Oscilloscope,
        );
        started.scope_trigger_controls = vec![control];
        app.scope_settings.trigger.source = DsCopeTriggerSource::ChannelZero;
        // Dropping the source closes the channel the control sends on.
        drop(source);

        app.handle_capture_ready(started);

        assert!(app.captures.is_empty());
        assert_eq!(
            app.last_error.as_deref(),
            Some(
                "failed to apply the latest trigger settings: \
                 DSCope capture is no longer running"
            )
        );
    }

    #[test]
    fn an_invalid_scope_trigger_setting_stops_the_capture_and_reports_the_error() {
        let mut app = app();
        request(&mut app, CaptureTarget::Oscilloscope);
        let (_source, control) = scope_trigger_control(&app);
        let mut started = ready(
            &app,
            InstrumentKind::Oscilloscope,
            AcquisitionMode::Run,
            CaptureTarget::Oscilloscope,
        );
        started.scope_trigger_controls = vec![control];
        app.scope_settings.trigger.position_percent = 255;

        app.handle_capture_ready(started);

        assert!(app.captures.is_empty());
        let error = app
            .last_error
            .as_deref()
            .expect("the invalid trigger is reported");
        assert!(!error.starts_with("failed to apply"), "{error}");
        assert!(!app.capture_is_requested(CaptureTarget::Oscilloscope));
    }

    #[test]
    fn a_scope_capture_without_trigger_controls_keeps_the_trigger_it_started_with() {
        let mut app = app();
        request(&mut app, CaptureTarget::Oscilloscope);
        let started = ready(
            &app,
            InstrumentKind::Oscilloscope,
            AcquisitionMode::Run,
            CaptureTarget::Oscilloscope,
        );
        app.scope_settings.trigger.source = DsCopeTriggerSource::ChannelZero;

        app.handle_capture_ready(started);

        assert_eq!(
            app.captures[0].scope_config.trigger().source(),
            DsCopeTriggerSource::Auto
        );
    }

    #[test]
    fn a_combined_capture_counts_and_flags_every_instrument() {
        let mut app = app();
        request(&mut app, CaptureTarget::Combined);
        let started = ready_with(
            &app,
            &[InstrumentKind::LogicAnalyzer, InstrumentKind::Oscilloscope],
            false,
            AcquisitionMode::Run,
            CaptureTarget::Combined,
        );

        app.handle_capture_ready(started);

        let capture = &app.captures[0];
        assert!(capture.has_logic && capture.has_scope && !capture.has_radio);
        assert_eq!(app.status, "Combined Run acquisition from 2 instrument(s)");
    }

    #[test]
    fn a_single_logic_capture_with_a_trigger_waits_for_it() {
        let mut app = app();
        request(&mut app, CaptureTarget::LogicAnalyzer);
        app.logic_settings.trigger_enabled = true;
        let started = ready(
            &app,
            InstrumentKind::LogicAnalyzer,
            AcquisitionMode::Single,
            CaptureTarget::LogicAnalyzer,
        );
        assert!(
            started.logic_config.trigger().is_some(),
            "the enabled logic trigger reaches the capture config"
        );

        app.handle_capture_ready(started);

        assert_eq!(app.status, "Single · waiting for logic trigger");
    }

    #[test]
    fn a_finished_radio_capture_is_not_kept_on_display_when_a_new_run_starts() {
        let mut app = app();
        let finished = completed_capture(&app, false, false, true);
        app.set_completed_capture_for_target(CaptureTarget::RadioReceiver, finished);
        request(&mut app, CaptureTarget::RadioReceiver);
        let started = ready(
            &app,
            InstrumentKind::RadioReceiver,
            AcquisitionMode::Run,
            CaptureTarget::RadioReceiver,
        );

        app.handle_capture_ready(started);

        assert!(!app.captures[0].preserve_display_until_complete);
    }

    #[test]
    fn a_decoded_data_file_that_cannot_be_created_marks_the_capture_file_as_failed() {
        let directory = scratch_directory("decoded-failure-finishes-recording");
        let recording = directory.join("capture.dsv");
        let mut app = app();
        request(&mut app, CaptureTarget::RadioReceiver);
        app.record_capture = true;
        app.recording_path = recording.to_string_lossy().into_owned();
        app.record_decoded_data = true;
        app.decoded_data_path = directory
            .join("missing")
            .join("decoded.dsv")
            .to_string_lossy()
            .into_owned();
        let started = ready(
            &app,
            InstrumentKind::RadioReceiver,
            AcquisitionMode::Run,
            CaptureTarget::RadioReceiver,
        );

        app.handle_capture_ready(started);

        assert!(app.captures.is_empty());
        assert!(app.last_error.is_some());
        let loaded = load_gui_recording(&recording, &app.protocol_analyzer.analyzers)
            .expect("the capture file was finished, not left half written");
        assert_eq!(loaded.completion, CompletionStatus::Failed);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn an_rf_trigger_that_cannot_be_armed_marks_the_capture_file_as_failed() {
        let directory = scratch_directory("rf-trigger-failure-finishes-recording");
        let recording = directory.join("capture.dsv");
        let mut app = app();
        request(&mut app, CaptureTarget::RadioReceiver);
        app.record_capture = true;
        app.recording_path = recording.to_string_lossy().into_owned();
        app.radio_settings.trigger_enabled = true;
        app.radio_settings.trigger_posttrigger_micros = u64::MAX;
        let started = ready(
            &app,
            InstrumentKind::RadioReceiver,
            AcquisitionMode::Single,
            CaptureTarget::RadioReceiver,
        );

        app.handle_capture_ready(started);

        assert!(app.captures.is_empty());
        assert!(app
            .last_error
            .as_deref()
            .is_some_and(|error| error.starts_with("failed to arm RF trigger")));
        let loaded = load_gui_recording(&recording, &app.protocol_analyzer.analyzers)
            .expect("the capture file was finished, not left half written");
        assert_eq!(loaded.completion, CompletionStatus::Failed);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_recording_without_any_capture_data_is_rejected() {
        let mut app = app();
        let recording = loaded_recording(&app, (false, false, false));

        app.handle_recording_loaded(PathBuf::from("empty.dsv"), recording);

        assert_eq!(
            app.last_error.as_deref(),
            Some("recording does not contain a supported capture")
        );
        assert!(app.active_waveform_tab.is_none());
    }

    #[test]
    fn an_opened_recording_is_shown_on_the_tab_of_the_instruments_it_contains() {
        for (contents, target, tab) in [
            (
                (true, false, false),
                CaptureTarget::LogicAnalyzer,
                WaveformTab::LogicAnalyzer,
            ),
            (
                (false, true, false),
                CaptureTarget::Oscilloscope,
                WaveformTab::Oscilloscope,
            ),
            (
                (false, false, true),
                CaptureTarget::RadioReceiver,
                WaveformTab::RadioReceiver,
            ),
            (
                (true, true, false),
                CaptureTarget::Combined,
                WaveformTab::Combined,
            ),
        ] {
            let mut app = app();
            app.last_error = Some("old failure".to_owned());
            app.record_capture = true;
            let recording = loaded_recording(&app, contents);

            app.handle_recording_loaded(PathBuf::from("opened.dsv"), recording);

            assert_eq!(app.active_waveform_tab, Some(tab), "{contents:?}");
            assert!(
                app.completed_capture_for_target(target).is_some(),
                "{contents:?}"
            );
            assert_eq!(app.logic_channel_names[0], "CLK");
            assert_eq!(app.scope_channel_names[1], "Probe B");
            assert!(!app.record_capture);
            assert!(app.last_error.is_none());
            assert_eq!(app.status, "Opened opened.dsv · complete recording");
        }
    }

    #[test]
    fn an_opened_logic_recording_selects_the_dslogic_settings_and_an_oscilloscope_the_dscope_ones()
    {
        let mut app = app();
        let logic = loaded_recording(&app, (true, false, false));
        app.handle_recording_loaded(PathBuf::from("logic.dsv"), logic);
        assert_eq!(app.active_capture_settings_tab, CaptureSettingsTab::DsLogic);

        let scope = loaded_recording(&app, (false, true, false));
        app.handle_recording_loaded(PathBuf::from("scope.dsv"), scope);
        assert_eq!(app.active_capture_settings_tab, CaptureSettingsTab::DsCope);
    }

    #[test]
    fn a_saved_auto_calibration_marks_the_open_calibration_dialog_as_done() {
        let mut app = app();
        app.calibration_modal = Some(CalibrationProgressModal {
            progress: 0.5,
            phase: "Calibrating".to_owned(),
            completed: false,
            values: None,
        });

        deliver(&mut app, Ok(WorkerSuccess::ScopeAutoCalibrationSaved));

        let modal = app
            .calibration_modal
            .as_ref()
            .expect("the dialog stays open");
        assert_eq!(modal.progress, 1.0);
        assert!(modal.completed);
        assert!(modal.values.is_none());
        assert_eq!(modal.phase, "Calibration was saved to device NVM.");
        assert_eq!(app.status, "DSCope auto calibration saved and verified");
    }

    #[test]
    fn saving_a_saved_calibration_without_a_dialog_still_reports_it() {
        let mut app = app();

        app.handle_scope_auto_calibration_saved();

        assert!(app.calibration_modal.is_none());
        assert_eq!(app.status, "DSCope auto calibration saved and verified");
    }

    #[test]
    fn a_saved_manual_calibration_resumes_the_capture_that_was_waiting_for_it() {
        let mut app = app();
        request(&mut app, CaptureTarget::Oscilloscope);

        deliver(&mut app, Ok(WorkerSuccess::ScopeCalibrationSaved));

        assert!(app.manual_calibration.is_none());
        // begin_capture ran for the waiting request; it needs a selected DSCope
        // and so reports that none is connected here.
        assert!(app
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("DSCope")));
    }

    #[test]
    fn a_saved_manual_calibration_starts_nothing_when_no_capture_was_waiting() {
        let mut app = app();

        deliver(&mut app, Ok(WorkerSuccess::ScopeCalibrationSaved));

        assert_eq!(app.status, "DSCope manual calibration saved and verified");
        assert!(app.last_error.is_none());
        assert!(app.captures.is_empty());
    }
}
