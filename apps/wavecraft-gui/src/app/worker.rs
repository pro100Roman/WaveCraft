//! Handling the results of the background device, calibration, and file work.

use crate::{
    create_decoded_data_recorder, finish_gui_recording, radio_single_trigger_config,
    recording_capture_target, recording_completion_label, scope_rolling_sample_capacity,
    start_gui_recording, usb_speed_warning, uses_rolling_scope_buffer, AcquisitionMode,
    ActiveCapture, CaptureReady, CaptureSettingsTab, CaptureTarget, CompletionStatus, DeviceEntry,
    DiscoveredDevice, DsCopeCalibrationValues, DsCopeTriggerSource, GuiRecordingSettings, HashSet,
    InstrumentKind, LoadedRecording, LogicDecodeWorker, ManualCalibrationEditor, PathBuf,
    ProtocolCaptureDecoder, RadioDiscoveryReport, RadioSingleTrigger, ScopeMeasurementCache,
    TryRecvError, WaveformStore, WaveformTab, WorkerSuccess, DSCOPE_VOLTS_PER_DIV_MV,
};

use crate::WaveCraftApp;

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
        let CaptureReady {
            session,
            mode,
            target,
            retain_protocol_history,
            logic_config,
            mut scope_config,
            scope_trigger_controls,
            scope_calibration_controls,
            radio_runtime_control,
            radio_spectrum,
            radio_dsp,
        } = ready;
        if !self.capture_is_requested(target) {
            self.requested_capture_target = None;
            self.record_capture = false;
            self.record_decoded_data = false;
            match session.stop() {
                Ok(()) => self.status = "Acquisition stopped".to_owned(),
                Err(error) => self.set_error(error.to_string()),
            }
            return;
        }
        if self.scope_capture_restart_requested {
            self.scope_capture_restart_requested = false;
            match session.stop() {
                Ok(()) => {
                    self.status = "Reconfiguring oscilloscope frontend…".to_owned();
                    self.begin_capture(mode, target);
                }
                Err(error) => self.set_error(error.to_string()),
            }
            return;
        }
        if !scope_trigger_controls.is_empty() {
            let desired_trigger = match self.scope_settings.trigger.capture_config() {
                Ok(trigger) => trigger,
                Err(error) => {
                    let cleanup = session.stop().err();
                    self.set_error(match cleanup {
                        Some(cleanup) => {
                            format!("{error}; additionally failed to stop: {cleanup}")
                        }
                        None => error,
                    });
                    return;
                }
            };
            if desired_trigger != scope_config.trigger() {
                let update_result = scope_config
                    .with_trigger_config(desired_trigger)
                    .map_err(|error| error.to_string())
                    .and_then(|updated_config| {
                        for control in &scope_trigger_controls {
                            control
                                .update(desired_trigger)
                                .map_err(|error| error.to_string())?;
                        }
                        Ok(updated_config)
                    });
                match update_result {
                    Ok(updated_config) => scope_config = updated_config,
                    Err(error) => {
                        let cleanup = session.stop().err();
                        self.set_error(match cleanup {
                            Some(cleanup) => format!(
                                "failed to apply the latest trigger settings: {error}; additionally failed to stop: {cleanup}"
                            ),
                            None => format!(
                                "failed to apply the latest trigger settings: {error}"
                            ),
                        });
                        return;
                    }
                }
            }
        }
        let count = session.descriptors().len();
        let recording_path = self
            .record_capture
            .then(|| PathBuf::from(self.recording_path.trim()));
        let mut recorder = match recording_path.as_deref() {
            Some(path) => {
                match start_gui_recording(
                    path,
                    session.descriptors(),
                    GuiRecordingSettings {
                        logic_config,
                        logic_settings: self.logic_settings,
                        scope_config,
                        scope_settings: self.scope_settings,
                        radio_settings: &self.radio_settings,
                        logic_channel_names: &self.logic_channel_names,
                        scope_channel_names: &self.scope_channel_names,
                    },
                ) {
                    Ok(recorder) => Some(recorder),
                    Err(error) => {
                        let cleanup = session.stop().err();
                        self.set_error(match cleanup {
                            Some(cleanup) => {
                                format!("{error}; additionally failed to stop: {cleanup}")
                            }
                            None => error,
                        });
                        return;
                    }
                }
            }
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
                    let cleanup = session.stop().err();
                    self.set_error(match cleanup {
                        Some(cleanup) => {
                            format!("{error}; additionally failed to stop: {cleanup}")
                        }
                        None => error,
                    });
                    return;
                }
            },
            None => None,
        };
        let has_logic = session
            .descriptors()
            .iter()
            .any(|device| device.kind == InstrumentKind::LogicAnalyzer);
        let has_scope = session
            .descriptors()
            .iter()
            .any(|device| device.kind == InstrumentKind::Oscilloscope);
        let has_radio = session
            .descriptors()
            .iter()
            .any(|device| device.kind == InstrumentKind::RadioReceiver);
        let radio_trigger = if mode == AcquisitionMode::Single
            && target.includes_radio_receiver()
            && has_radio
            && self.radio_settings.trigger_enabled
        {
            let config = radio_single_trigger_config(&self.radio_settings);
            match config.and_then(RadioSingleTrigger::new) {
                Ok(trigger) => Some(trigger),
                Err(error) => {
                    let cleanup = session.stop().err();
                    self.set_error(match cleanup {
                        Some(cleanup) => format!(
                            "failed to arm RF trigger: {error}; additionally failed to stop: {cleanup}"
                        ),
                        None => format!("failed to arm RF trigger: {error}"),
                    });
                    return;
                }
            }
        } else {
            None
        };
        let waiting_for_radio_trigger = radio_trigger.is_some();
        let has_scope_trigger = !scope_trigger_controls.is_empty();
        let has_logic_trigger = has_logic && logic_config.trigger().is_some();
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
        self.captures.push(ActiveCapture {
            session,
            mode,
            target,
            preserve_display_until_complete,
            scope_config,
            scope_trigger_controls,
            scope_calibration_controls,
            radio_runtime_control,
            stopped: HashSet::new(),
            waveforms,
            protocol_decode_snapshot,
            protocol_analyzers,
            logic_decode,
            radio_spectrum,
            radio_dsp,
            radio_trigger,
            recorder,
            recording_path: recording_path.clone(),
            decoded_data_recorder,
            decoded_data_path: decoded_data_path.clone(),
            has_logic,
            has_scope,
            has_radio,
        });
        self.requested_capture_target = None;
        self.record_capture = false;
        self.record_decoded_data = false;
        self.status = match (mode, recording_path.as_ref()) {
            (AcquisitionMode::Single, Some(path)) if waiting_for_radio_trigger => format!(
                "Single · waiting for RF trigger at {:.1} dBFS; recording to {}",
                f32::from(self.radio_settings.trigger_level_dbfs_tenths) / 10.0,
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
                    format!(
                        "Single · waiting for RF trigger at {:.1} dBFS",
                        f32::from(self.radio_settings.trigger_level_dbfs_tenths) / 10.0
                    )
                } else if has_logic_trigger {
                    "Single · waiting for logic trigger".to_owned()
                } else if has_scope_trigger
                    && scope_config.trigger().source() != DsCopeTriggerSource::Auto
                {
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
                if has_scope_trigger && scope_config.trigger().source() == DsCopeTriggerSource::Auto
                {
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
        };
        if let Some(path) = decoded_data_path {
            self.status = format!(
                "RF Run acquisition from {count} instrument(s) · decoded data to {}",
                path.display()
            );
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
