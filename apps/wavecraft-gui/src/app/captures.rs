//! Starting, stopping, and polling captures, and the completed captures kept for display.

use crate::{
    append_error, capture_devices_for_target, capture_recording_is_supported,
    clears_completed_capture_on_start, decoded_data_recording_is_supported,
    finish_decoded_data_recording, finish_gui_recording, finish_logic_decode,
    ingest_active_capture_frame, ingest_logic_decode_output, ingest_radio_dsp_output, mpsc,
    radio_gain_u32, radio_microsecond_sample_count, radio_single_sample_limit,
    record_decoded_radio_packets, record_decoded_raw_messages, repetitive_interval, selected_radio,
    should_rearm_acquisition, start_capture, thread, AcquisitionMode, ActiveCapture, CaptureBatch,
    CaptureEvent, CaptureTarget, CompletedCapture, CompletionStatus, DsLogicOperationMode,
    HackRfSettings, Instant, PathBuf, ProtocolDecodeCache, RadioCaptureRequest,
    RadioDataDecoderConfig, RadioProvider, RadioProviderSettings, RadioSingleTrigger,
    ScheduledRepetitiveCapture, ScopeMeasurementCache, WaveformTab, WorkerSuccess,
    MAX_CAPTURE_EVENTS_PER_POLL,
};

use crate::WaveCraftApp;

impl WaveCraftApp {
    pub(crate) fn radio_capture_request(
        &self,
        mode: AcquisitionMode,
    ) -> Result<RadioCaptureRequest, String> {
        let device = selected_radio(&self.radios, self.selected_radio_id.as_deref())
            .cloned()
            .ok_or_else(|| "Select a connected radio receiver in Capture Setup".to_owned())?;
        let mut settings = self.radio_settings.capture.clone();
        let waits_for_trigger =
            mode == AcquisitionMode::Single && self.radio_settings.trigger_enabled;
        settings.sample_limit = (mode == AcquisitionMode::Single && !waits_for_trigger)
            .then(|| {
                radio_single_sample_limit(
                    settings.sample_rate_hz,
                    self.radio_settings.single_duration_millis,
                )
            })
            .transpose()?;
        settings
            .validate(&device.capabilities)
            .map_err(|error| error.to_string())?;
        let provider_settings = match device.provider {
            RadioProvider::HackRf => RadioProviderSettings::HackRf(HackRfSettings {
                lna_gain_db: radio_gain_u32(&self.radio_settings, "lna")?,
                vga_gain_db: radio_gain_u32(&self.radio_settings, "vga")?,
                amplifier_enabled: self.radio_settings.amplifier_enabled,
                antenna_power_enabled: self.radio_settings.antenna_power_enabled,
                frequency_correction_milli_ppm: self.radio_settings.frequency_correction_milli_ppm,
            }),
            provider => {
                return Err(format!(
                    "capture is not implemented for the {provider:?} radio provider"
                ));
            }
        };
        let analyzer_profiles = self.radio_settings.analyzer.signal_profiles()?;
        let analyzer_channel_names = (0..analyzer_profiles.len())
            .map(|index| self.radio_settings.analyzer.channel_name(index))
            .collect();
        let half_sample_rate_hz = settings.sample_rate_hz / 2;
        if !analyzer_profiles.is_empty()
            && self
                .radio_settings
                .analyzer
                .channels
                .iter()
                .any(|channel| channel.frequency_offset_hz.unsigned_abs() > half_sample_rate_hz)
        {
            return Err(
                "signal analyzer channel offset lies outside the captured RF span".to_owned(),
            );
        }
        let decoder_config = if analyzer_profiles.is_empty() {
            RadioDataDecoderConfig::Raw(self.radio_settings.analyzer.raw_config())
        } else {
            self.radio_settings.analyzer.decoder_config()?
        };
        Ok(RadioCaptureRequest {
            device,
            settings,
            provider_settings,
            iq_correction_enabled: self.radio_settings.iq_correction_enabled,
            analyzer_profiles,
            analyzer_channel_names,
            decoder_config,
        })
    }

    pub(crate) fn begin_capture(&mut self, mode: AcquisitionMode, target: CaptureTarget) {
        let continuing_request = self.capture_is_requested(target);
        if self.pending.is_some()
            || self.capture_target_is_active(target)
            || self
                .capture_requests
                .iter()
                .any(|requested| *requested != target && requested.overlaps(target))
        {
            return;
        }
        if !continuing_request && target.includes_logic_analyzer() {
            self.logic_marker_settings.clear();
        }
        self.scheduled_repetitive_capture = None;
        if self.record_capture && !capture_recording_is_supported(mode) {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error =
                Some("Capture recording is available in Single and Run modes only".to_owned());
            return;
        }
        if self.record_decoded_data && !decoded_data_recording_is_supported(mode, target) {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some("Decoded data recording requires RF Run mode".to_owned());
            return;
        }
        let devices = match capture_devices_for_target(
            &self.devices,
            self.selected_logic_device_id.as_deref(),
            self.selected_scope_device_id.as_deref(),
            target,
        ) {
            Ok(devices) => devices,
            Err(error) => {
                self.record_capture = false;
                self.record_decoded_data = false;
                self.last_error = Some(error);
                return;
            }
        };
        let radio_request = if target.captures_selected_radio(
            selected_radio(&self.radios, self.selected_radio_id.as_deref()).is_some(),
        ) {
            match self.radio_capture_request(mode) {
                Ok(request) => Some(request),
                Err(error) => {
                    self.record_capture = false;
                    self.record_decoded_data = false;
                    self.last_error = Some(error);
                    return;
                }
            }
        } else {
            None
        };
        if self.record_capture && self.recording_path.trim().is_empty() {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some("Enter a recording file path".to_owned());
            return;
        }
        if self.record_decoded_data && self.decoded_data_path.trim().is_empty() {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some("Enter a decoded data recording file path".to_owned());
            return;
        }
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.record_capture = false;
                self.record_decoded_data = false;
                self.last_error = Some(error);
                return;
            }
        };
        let logic_config = match self.logic_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.record_capture = false;
                self.record_decoded_data = false;
                self.last_error = Some(error);
                return;
            }
        }
        .with_loop_mode(mode == AcquisitionMode::Loop);
        if matches!(mode, AcquisitionMode::Repetitive | AcquisitionMode::Loop)
            && target != CaptureTarget::LogicAnalyzer
        {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some(
                "Repetitive and Loop modes are available for Logic Analyzer capture only"
                    .to_owned(),
            );
            return;
        }
        if mode == AcquisitionMode::Loop
            && logic_config.operation_mode() != DsLogicOperationMode::Stream
        {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some("Loop mode requires DSLogic Stream mode".to_owned());
            return;
        }
        let calibration_values = self.manual_calibration.as_ref().map(|editor| editor.values);

        let retain_protocol_history = mode == AcquisitionMode::Repetitive && continuing_request;
        self.capture_requests.insert(target);
        self.queued_capture_rearms
            .retain(|(_, queued_target)| *queued_target != target);
        self.requested_capture_target = Some(target);
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        if clears_completed_capture_on_start(mode, continuing_request) {
            self.clear_completed_capture_for_target(target);
        }
        self.status = match mode {
            AcquisitionMode::Single => {
                format!("Preparing {} single acquisition…", target.label())
            }
            AcquisitionMode::Repetitive => {
                format!("Preparing {} repetitive acquisition…", target.label())
            }
            AcquisitionMode::Loop => {
                format!("Preparing {} loop acquisition…", target.label())
            }
            AcquisitionMode::Run => {
                format!("Preparing {} continuous acquisition…", target.label())
            }
        };
        self.last_error = None;
        thread::spawn(move || {
            let result = start_capture(
                devices,
                radio_request,
                &resource_directory,
                logic_config,
                scope_config,
                calibration_values,
            )
            .map(|started| WorkerSuccess::Capture {
                session: started.session,
                mode,
                target,
                retain_protocol_history,
                logic_config: Box::new(logic_config),
                scope_config,
                scope_trigger_controls: started.scope_trigger_controls,
                scope_calibration_controls: started.scope_calibration_controls,
                radio_runtime_control: started.radio_runtime_control,
                radio_spectrum: Box::new(started.radio_spectrum),
                radio_dsp: Box::new(started.radio_dsp),
            });
            let _ = sender.send(result);
        });
    }

    pub(crate) fn stop_capture(&mut self, target: CaptureTarget) {
        let Some(index) = self
            .captures
            .iter()
            .position(|capture| capture.target.includes(target))
        else {
            return;
        };
        let mut active = self.captures.swap_remove(index);
        self.capture_requests.remove(&active.target);
        self.queued_capture_rearms
            .retain(|(_, target)| *target != active.target);
        if self
            .scheduled_repetitive_capture
            .is_some_and(|scheduled| scheduled.target == active.target)
        {
            self.scheduled_repetitive_capture = None;
        }
        self.status = "Stopping capture…".to_owned();
        active.session.request_stop();
        let (protocol_decode, logic_decode_error) = finish_logic_decode(&mut active);
        let stop_result = active.session.stop();
        let radio_dsp_result = active.radio_dsp.finish();
        let mut error = stop_result.as_ref().err().map(ToString::to_string);
        match radio_dsp_result {
            Ok(outputs) => {
                for output in outputs {
                    if let Err(recording_error) =
                        record_decoded_radio_packets(&mut active.decoded_data_recorder, &output)
                    {
                        append_error(&mut error, recording_error);
                    }
                    ingest_radio_dsp_output(&mut active.waveforms, output);
                    if let Err(recording_error) = record_decoded_raw_messages(
                        &mut active.decoded_data_recorder,
                        &active.waveforms.radio_analysis,
                    ) {
                        append_error(&mut error, recording_error);
                    }
                }
            }
            Err(dsp_error) => append_error(&mut error, dsp_error),
        }
        if let Some(logic_decode_error) = logic_decode_error {
            append_error(&mut error, logic_decode_error);
        }
        active.waveforms.radio_analysis.finish_raw_capture();
        if let Err(recording_error) = record_decoded_raw_messages(
            &mut active.decoded_data_recorder,
            &active.waveforms.radio_analysis,
        ) {
            append_error(&mut error, recording_error);
        }
        let completion = if stop_result.is_ok() && error.is_none() {
            CompletionStatus::Cancelled
        } else {
            CompletionStatus::Failed
        };
        if let Err(recording_error) = finish_gui_recording(&mut active.recorder, completion) {
            append_error(&mut error, recording_error);
        }
        if let Err(recording_error) =
            finish_decoded_data_recording(&mut active.decoded_data_recorder, completion)
        {
            append_error(&mut error, recording_error);
        }
        let recording_path = active.recording_path.take();
        let decoded_data_path = active.decoded_data_path.take();
        self.record_capture = false;
        self.record_decoded_data = false;
        let has_logic = active.has_logic;
        let has_scope = active.has_scope;
        let has_radio = active.has_radio;
        if !active.preserve_display_until_complete {
            self.set_completed_capture_for_target(
                active.target,
                CompletedCapture {
                    waveforms: active.waveforms,
                    protocol_decode,
                    has_logic,
                    has_scope,
                    has_radio,
                },
            );
        }
        match error {
            Some(error) => self.set_error(error),
            None => {
                let saved_path = decoded_data_path.or(recording_path);
                self.status = saved_path.map_or_else(
                    || "Capture stopped".to_owned(),
                    |path| format!("Capture stopped · recording saved to {}", path.display()),
                );
            }
        }
    }

    pub(crate) fn poll_capture(&mut self) {
        let targets = self
            .captures
            .iter()
            .map(|capture| capture.target)
            .collect::<Vec<_>>();
        for target in targets {
            self.poll_capture_target(target);
        }
    }

    pub(crate) fn poll_capture_target(&mut self, target: CaptureTarget) {
        let Some(index) = self
            .captures
            .iter()
            .position(|capture| capture.target == target)
        else {
            return;
        };
        let mut active = self.captures.swap_remove(index);
        let mut finish_status = None::<(Result<String, String>, CompletionStatus)>;
        let mut capture_status = None::<String>;
        let protocol_analyzers = self.protocol_analyzer.analyzers.clone();
        let radio_trigger_threshold_dbfs =
            f32::from(self.radio_settings.trigger_level_dbfs_tenths) / 10.0;
        let radio_trigger_posttrigger_samples = radio_microsecond_sample_count(
            self.radio_settings.capture.sample_rate_hz,
            self.radio_settings.trigger_posttrigger_micros,
        );
        {
            let active = &mut active;
            if active.protocol_analyzers != protocol_analyzers {
                match active.logic_decode.reconfigure(protocol_analyzers.clone()) {
                    Ok(()) => active.protocol_analyzers = protocol_analyzers.clone(),
                    Err(error) => {
                        active.session.request_stop();
                        finish_status = Some((Err(error), CompletionStatus::Failed));
                    }
                }
            }
            for _ in 0..MAX_CAPTURE_EVENTS_PER_POLL {
                match active.session.try_recv() {
                    Ok(event) => match event {
                        CaptureEvent::Frame(frame) => {
                            if let Some(trigger) = active.radio_trigger.as_mut() {
                                let update_result = radio_trigger_posttrigger_samples
                                    .as_ref()
                                    .map_err(Clone::clone)
                                    .and_then(|samples| {
                                        trigger.set_threshold_dbfs(radio_trigger_threshold_dbfs)?;
                                        trigger.set_posttrigger_samples(*samples);
                                        Ok(())
                                    });
                                if let Err(error) = update_result {
                                    active.session.request_stop();
                                    finish_status = Some((
                                        Err(format!("RF trigger update failed: {error}")),
                                        CompletionStatus::Failed,
                                    ));
                                    break;
                                }
                            }
                            let trigger_armed = active
                                .radio_trigger
                                .as_ref()
                                .is_some_and(RadioSingleTrigger::is_armed);
                            if trigger_armed {
                                if let CaptureBatch::Iq(iq) = &frame.payload {
                                    for spectrum in active.radio_spectrum.push(iq) {
                                        active.waveforms.radio.ingest(spectrum);
                                    }
                                }
                            }
                            let frame_is_iq = matches!(&frame.payload, CaptureBatch::Iq(_));
                            let (frames, trigger_event, trigger_complete) =
                                if let (Some(trigger), true) =
                                    (active.radio_trigger.as_mut(), frame_is_iq)
                                {
                                    match trigger.push(frame) {
                                        Ok(output) => {
                                            (output.frames, output.event, output.complete)
                                        }
                                        Err(error) => {
                                            active.session.request_stop();
                                            finish_status = Some((
                                                Err(format!("RF trigger failed: {error}")),
                                                CompletionStatus::Failed,
                                            ));
                                            break;
                                        }
                                    }
                                } else {
                                    (vec![frame], None, false)
                                };
                            if let Some(event) = trigger_event {
                                active.radio_spectrum.reset();
                                capture_status = Some(format!(
                                    "RF trigger · {:.3} MHz at {:.1} dBFS; capturing…",
                                    event.frequency_hz / 1_000_000.0,
                                    event.power_dbfs
                                ));
                            }
                            for frame in frames {
                                if let Err(error) = ingest_active_capture_frame(
                                    active,
                                    frame,
                                    radio_trigger_threshold_dbfs,
                                ) {
                                    active.session.request_stop();
                                    finish_status = Some((Err(error), CompletionStatus::Failed));
                                    break;
                                }
                            }
                            if finish_status.is_some() {
                                break;
                            }
                            if trigger_complete {
                                active.session.request_stop();
                                finish_status = Some((
                                    Ok("RF triggered capture complete".to_owned()),
                                    CompletionStatus::Complete,
                                ));
                                break;
                            }
                        }
                        CaptureEvent::SourceFailed { device_id, error } => {
                            if let Some(recorder) = active.recorder.as_mut() {
                                if let Err(write_error) =
                                    recorder.write_event(&CaptureEvent::SourceFailed {
                                        device_id: device_id.clone(),
                                        error: error.clone(),
                                    })
                                {
                                    active.session.request_stop();
                                    finish_status = Some((
                                        Err(format!("failed to write recording: {write_error}")),
                                        CompletionStatus::Failed,
                                    ));
                                    break;
                                }
                            }
                            active.session.request_stop();
                            finish_status = Some((
                                Err(format!("{device_id}: {error}")),
                                CompletionStatus::Failed,
                            ));
                            break;
                        }
                        CaptureEvent::SourceStopped { device_id } => {
                            if let Some(recorder) = active.recorder.as_mut() {
                                if let Err(error) =
                                    recorder.write_event(&CaptureEvent::SourceStopped {
                                        device_id: device_id.clone(),
                                    })
                                {
                                    active.session.request_stop();
                                    finish_status = Some((
                                        Err(format!("failed to write recording: {error}")),
                                        CompletionStatus::Failed,
                                    ));
                                    break;
                                }
                            }
                            active.stopped.insert(device_id);
                            if active.stopped.len() == active.session.descriptors().len() {
                                finish_status = Some((
                                    Ok("Capture complete".to_owned()),
                                    CompletionStatus::Complete,
                                ));
                                break;
                            }
                        }
                    },
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finish_status = Some((
                            Err("capture event stream disconnected".to_owned()),
                            CompletionStatus::Failed,
                        ));
                        break;
                    }
                }
            }
            loop {
                match active.logic_decode.try_recv() {
                    Ok(output) => ingest_logic_decode_output(active, output),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        if finish_status.is_none() {
                            active.session.request_stop();
                            finish_status = Some((
                                Err("logic decode worker disconnected".to_owned()),
                                CompletionStatus::Failed,
                            ));
                        }
                        break;
                    }
                }
            }
            loop {
                match active.radio_dsp.try_recv() {
                    Ok(output) => {
                        if let Err(error) =
                            record_decoded_radio_packets(&mut active.decoded_data_recorder, &output)
                        {
                            active.session.request_stop();
                            finish_status = Some((Err(error), CompletionStatus::Failed));
                            break;
                        }
                        ingest_radio_dsp_output(&mut active.waveforms, output);
                        if let Err(error) = record_decoded_raw_messages(
                            &mut active.decoded_data_recorder,
                            &active.waveforms.radio_analysis,
                        ) {
                            active.session.request_stop();
                            finish_status = Some((Err(error), CompletionStatus::Failed));
                            break;
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        if finish_status.is_none() {
                            active.session.request_stop();
                            finish_status = Some((
                                Err("radio DSP worker disconnected".to_owned()),
                                CompletionStatus::Failed,
                            ));
                        }
                        break;
                    }
                }
            }
        }

        if let Some(status) = capture_status {
            self.status = status;
        }

        let Some((result, completion)) = finish_status else {
            self.captures.push(active);
            return;
        };
        {
            let mode = active.mode;
            let target = active.target;
            let preserve_display_until_complete = active.preserve_display_until_complete;
            let recording_path = active.recording_path.take();
            let decoded_data_path = active.decoded_data_path.take();
            let has_logic = active.has_logic;
            let has_scope = active.has_scope;
            let has_radio = active.has_radio;
            let restart_for_frontend = self.scope_capture_restart_requested
                && self.capture_is_requested(target)
                && target.includes_oscilloscope();
            self.scope_capture_restart_requested = false;
            let (protocol_decode, logic_decode_error) = finish_logic_decode(&mut active);
            let stop_result = active.session.stop();
            let mut error = result.as_ref().err().cloned();
            if let Err(stop_error) = &stop_result {
                append_error(&mut error, format!("failed to stop capture: {stop_error}"));
            }
            match active.radio_dsp.finish() {
                Ok(outputs) => {
                    for output in outputs {
                        if let Err(recording_error) =
                            record_decoded_radio_packets(&mut active.decoded_data_recorder, &output)
                        {
                            append_error(&mut error, recording_error);
                        }
                        ingest_radio_dsp_output(&mut active.waveforms, output);
                        if let Err(recording_error) = record_decoded_raw_messages(
                            &mut active.decoded_data_recorder,
                            &active.waveforms.radio_analysis,
                        ) {
                            append_error(&mut error, recording_error);
                        }
                    }
                }
                Err(dsp_error) => append_error(&mut error, dsp_error),
            }
            if let Some(decode_error) = logic_decode_error {
                append_error(&mut error, decode_error);
            }
            active.waveforms.radio_analysis.finish_raw_capture();
            if let Err(recording_error) = record_decoded_raw_messages(
                &mut active.decoded_data_recorder,
                &active.waveforms.radio_analysis,
            ) {
                append_error(&mut error, recording_error);
            }
            let recording_completion = if stop_result.is_ok() && error.is_none() {
                completion
            } else {
                CompletionStatus::Failed
            };
            let recording_result = finish_gui_recording(&mut active.recorder, recording_completion);
            let decoded_recording_result = finish_decoded_data_recording(
                &mut active.decoded_data_recorder,
                recording_completion,
            );
            if let Err(recording_error) = recording_result {
                append_error(
                    &mut error,
                    format!("failed to finish recording: {recording_error}"),
                );
            }
            if let Err(recording_error) = decoded_recording_result {
                append_error(
                    &mut error,
                    format!("failed to finish decoded data recording: {recording_error}"),
                );
            }
            let update_completed_capture = if restart_for_frontend {
                !preserve_display_until_complete
            } else {
                completion == CompletionStatus::Complete || !preserve_display_until_complete
            };
            if update_completed_capture {
                self.set_completed_capture_for_target(
                    target,
                    CompletedCapture {
                        waveforms: active.waveforms,
                        protocol_decode,
                        has_logic,
                        has_scope,
                        has_radio,
                    },
                );
            }
            if error.is_none() && self.manual_calibration_load_requested {
                self.manual_calibration_load_requested = false;
                self.begin_manual_calibration();
                return;
            }
            if error.is_none() {
                if let Some(values) = self.manual_calibration_save_requested.take() {
                    self.begin_save_manual_calibration(values);
                    return;
                }
            }
            let repeat = error.is_none()
                && (restart_for_frontend
                    || should_rearm_acquisition(
                        mode,
                        self.capture_is_requested(target),
                        completion,
                        false,
                    ));
            if !repeat {
                self.capture_requests.remove(&target);
            }
            self.record_capture = false;
            self.record_decoded_data = false;
            match error {
                Some(error) => self.set_error(error),
                None if restart_for_frontend => {
                    self.status = "Reconfiguring oscilloscope frontend…".to_owned();
                }
                None => {
                    let capture_status =
                        result.ok().unwrap_or_else(|| "Capture complete".to_owned());
                    self.status = decoded_data_path
                        .or(recording_path)
                        .map_or(capture_status.clone(), |path| {
                            format!("{capture_status} · recording saved to {}", path.display())
                        });
                }
            }
            if repeat {
                if mode == AcquisitionMode::Repetitive && !restart_for_frontend {
                    let interval = repetitive_interval(
                        self.logic_settings.operation_mode,
                        self.repetitive_interval_seconds,
                    );
                    self.scheduled_repetitive_capture = Some(ScheduledRepetitiveCapture {
                        due: Instant::now() + interval,
                        target,
                    });
                    self.status = format!(
                        "Repetitive · next acquisition in {:.1} s",
                        interval.as_secs_f32()
                    );
                } else if self.pending.is_none() {
                    self.begin_capture(mode, target);
                } else if !self
                    .queued_capture_rearms
                    .iter()
                    .any(|(_, queued_target)| *queued_target == target)
                {
                    self.queued_capture_rearms.push((mode, target));
                }
            }
        }
    }

    pub(crate) fn poll_repetitive_capture(&mut self) {
        let Some(scheduled) = self.scheduled_repetitive_capture else {
            return;
        };
        if !self.capture_is_requested(scheduled.target) {
            self.scheduled_repetitive_capture = None;
            return;
        }
        if Instant::now() < scheduled.due {
            return;
        }
        self.scheduled_repetitive_capture = None;
        self.begin_capture(AcquisitionMode::Repetitive, scheduled.target);
    }

    pub(crate) fn poll_queued_capture_rearms(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let Some(index) = self.queued_capture_rearms.iter().position(|(_, target)| {
            self.capture_is_requested(*target) && !self.capture_target_is_active(*target)
        }) else {
            return;
        };
        let (mode, target) = self.queued_capture_rearms.remove(index);
        self.begin_capture(mode, target);
    }

    pub(crate) fn completed_capture_for_target(
        &self,
        target: CaptureTarget,
    ) -> Option<&CompletedCapture> {
        match target {
            CaptureTarget::LogicAnalyzer => self.logic_sessions.active_capture(),
            CaptureTarget::Oscilloscope => self.scope_sessions.active_capture(),
            CaptureTarget::RadioReceiver => self.radio_capture.as_ref(),
            CaptureTarget::Combined => self.combined_capture.as_ref(),
        }
    }

    pub(crate) fn displayed_completed_capture(&self) -> Option<&CompletedCapture> {
        match self.active_waveform_tab {
            Some(WaveformTab::LogicAnalyzer) => self.logic_sessions.active_capture(),
            Some(WaveformTab::Oscilloscope | WaveformTab::Lissajous) => {
                self.scope_sessions.active_capture()
            }
            Some(WaveformTab::RadioReceiver) => self.radio_capture.as_ref(),
            Some(WaveformTab::Combined) => self.combined_capture.as_ref(),
            None => None,
        }
    }

    pub(crate) fn has_displayed_completed_capture(&self) -> bool {
        self.displayed_completed_capture()
            .is_some_and(|capture| capture.has_logic || capture.has_scope)
    }

    pub(crate) fn clear_completed_capture_for_target(&mut self, target: CaptureTarget) {
        match target {
            CaptureTarget::LogicAnalyzer => self.logic_sessions.clear_active(),
            CaptureTarget::Oscilloscope => self.scope_sessions.clear_active(),
            CaptureTarget::RadioReceiver => self.radio_capture = None,
            CaptureTarget::Combined => self.combined_capture = None,
        }
        if target.includes_logic_analyzer() {
            self.protocol_decode_cache = ProtocolDecodeCache::default();
        }
        if target.includes_oscilloscope() {
            self.scope_measurement_caches = [ScopeMeasurementCache::default(); 2];
        }
        if target.includes_radio_receiver() {
            self.radio_view.borrow_mut().clear_capture();
        }
    }

    pub(crate) fn take_completed_capture_for_target(
        &mut self,
        target: CaptureTarget,
    ) -> Option<CompletedCapture> {
        match target {
            CaptureTarget::LogicAnalyzer => self.logic_sessions.take_active(),
            CaptureTarget::Oscilloscope => self.scope_sessions.take_active(),
            CaptureTarget::RadioReceiver => self.radio_capture.take(),
            CaptureTarget::Combined => self.combined_capture.take(),
        }
    }

    pub(crate) fn set_completed_capture_for_target(
        &mut self,
        target: CaptureTarget,
        capture: CompletedCapture,
    ) {
        match target {
            CaptureTarget::LogicAnalyzer => self.logic_sessions.replace_active(capture),
            CaptureTarget::Oscilloscope => self.scope_sessions.replace_active(capture),
            CaptureTarget::RadioReceiver => self.radio_capture = Some(capture),
            CaptureTarget::Combined => self.combined_capture = Some(capture),
        }
    }

    pub(crate) fn capture_is_requested(&self, target: CaptureTarget) -> bool {
        self.capture_requests.contains(&target)
    }

    pub(crate) fn capture_for_target(&self, target: CaptureTarget) -> Option<&ActiveCapture> {
        self.captures
            .iter()
            .find(|capture| capture.target.includes(target))
    }

    pub(crate) fn capture_for_target_mut(
        &mut self,
        target: CaptureTarget,
    ) -> Option<&mut ActiveCapture> {
        self.captures
            .iter_mut()
            .find(|capture| capture.target.includes(target))
    }

    pub(crate) fn capture_target_is_active(&self, target: CaptureTarget) -> bool {
        self.captures
            .iter()
            .any(|capture| capture.target.overlaps(target))
    }

    pub(crate) fn capture_target_can_start(&self, target: CaptureTarget) -> bool {
        self.pending.is_none()
            && !self.capture_target_is_active(target)
            && !self
                .capture_requests
                .iter()
                .any(|requested| requested.overlaps(target))
    }

    pub(crate) fn active_capture_for_view(&self) -> Option<&ActiveCapture> {
        self.active_waveform_tab
            .map(WaveformTab::capture_target)
            .and_then(|target| self.capture_for_target(target))
    }
}
