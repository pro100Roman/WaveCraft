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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    use std::sync::mpsc;
    use std::time::Duration;
    use wavecraft_core::{CaptureContext, CaptureError, DeviceDescriptor, DeviceId};
    use wavecraft_usb::{DeviceLocation, DSLOGIC_U3PRO16};

    fn app() -> WaveCraftApp {
        WaveCraftApp::with_settings(None, CaptureSettingsDocument::default())
    }

    /// A capture source that produces nothing and runs until it is stopped, so a
    /// real `CaptureSession` can be built without hardware.
    struct IdleSource {
        descriptor: DeviceDescriptor,
        /// Makes stopping the session fail, as a crashed capture worker would.
        panic_when_stopped: bool,
    }

    impl CaptureSource for IdleSource {
        fn descriptor(&self) -> DeviceDescriptor {
            self.descriptor.clone()
        }

        fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
            context.synchronize_start()?;
            while !context.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            if self.panic_when_stopped {
                panic!("the idle source was told to fail when stopped");
            }
            Ok(())
        }
    }

    /// A started capture of one idle instrument of the given kind, as the
    /// background thread would hand it to the UI.
    fn ready(
        app: &WaveCraftApp,
        kind: InstrumentKind,
        mode: AcquisitionMode,
        target: CaptureTarget,
    ) -> CaptureReady {
        ready_from(app, kind, false, mode, target)
    }

    fn ready_from(
        app: &WaveCraftApp,
        kind: InstrumentKind,
        panic_when_stopped: bool,
        mode: AcquisitionMode,
        target: CaptureTarget,
    ) -> CaptureReady {
        let source = IdleSource {
            descriptor: DeviceDescriptor {
                id: DeviceId::new("fake:1"),
                name: "Fake instrument".to_owned(),
                kind,
            },
            panic_when_stopped,
        };
        let spectrum = || {
            SpectrumProcessor::new(RADIO_SPECTRUM_FFT_LENGTH, SpectrumWindow::Hann)
                .expect("the spectrum processor accepts the GUI FFT length")
        };
        CaptureReady {
            session: CaptureCoordinator::start(vec![Box::new(source)])
                .expect("the idle source starts"),
            mode,
            target,
            retain_protocol_history: false,
            logic_config: app
                .logic_settings
                .capture_config()
                .expect("the default logic settings are valid"),
            scope_config: app
                .scope_settings
                .capture_config()
                .expect("the default scope settings are valid"),
            scope_trigger_controls: Vec::new(),
            scope_calibration_controls: Vec::new(),
            radio_runtime_control: None,
            radio_spectrum: spectrum(),
            radio_dsp: RadioDspWorker::start(spectrum(), Vec::new()),
        }
    }

    /// Marks `target` as requested the way pressing Run/Single does.
    fn request(app: &mut WaveCraftApp, target: CaptureTarget) {
        app.capture_requests.insert(target);
        app.requested_capture_target = Some(target);
    }

    /// Queues `result` as the finished background operation and polls it.
    fn deliver(app: &mut WaveCraftApp, result: WorkerResult) {
        let (sender, receiver) = mpsc::channel();
        sender.send(result).unwrap();
        app.pending = Some(receiver);
        app.poll_worker();
    }

    fn scratch_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("wavecraft-worker-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn completed_capture(
        app: &WaveCraftApp,
        has_logic: bool,
        has_scope: bool,
        has_radio: bool,
    ) -> CompletedCapture {
        CompletedCapture {
            waveforms: WaveformStore::default(),
            protocol_decode: ProtocolCaptureDecoder::new(&app.protocol_analyzer.analyzers),
            has_logic,
            has_scope,
            has_radio,
        }
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
        let started = ready_from(
            &app,
            InstrumentKind::LogicAnalyzer,
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
        app.recording_path = path.to_string_lossy().into_owned();
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
    fn a_single_rf_capture_with_the_trigger_enabled_waits_for_the_trigger() {
        for (trigger_enabled, waits) in [(true, true), (false, false)] {
            let mut app = app();
            request(&mut app, CaptureTarget::RadioReceiver);
            app.radio_settings.trigger_enabled = trigger_enabled;
            let started = ready(
                &app,
                InstrumentKind::RadioReceiver,
                AcquisitionMode::Single,
                CaptureTarget::RadioReceiver,
            );

            app.handle_capture_ready(started);

            assert_eq!(app.captures.len(), 1);
            assert!(app.captures[0].has_radio);
            assert_eq!(
                app.captures[0].radio_trigger.is_some(),
                waits,
                "trigger_enabled = {trigger_enabled}"
            );
            assert_eq!(
                app.status.contains("waiting for RF trigger"),
                waits,
                "{}",
                app.status
            );
        }
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
