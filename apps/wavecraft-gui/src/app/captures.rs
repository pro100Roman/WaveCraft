//! Starting, stopping, and polling captures, and the completed captures kept for display.

use crate::{
    acquisition_mode_is_supported, append_error, capture_devices_for_target,
    capture_recording_is_supported, clears_completed_capture_on_start,
    decoded_data_recording_is_supported, finish_decoded_data_recording, finish_gui_recording,
    finish_logic_decode, ingest_active_capture_frame, ingest_logic_decode_output,
    ingest_radio_dsp_output, mpsc, radio_microsecond_sample_count, radio_provider_settings,
    radio_single_sample_limit, record_decoded_radio_packets, record_decoded_raw_messages,
    repetitive_interval, scope_updates_on_trigger, selected_radio, should_rearm_acquisition,
    start_capture, thread, uses_continuous_scope_capture, uses_repetitive_interval,
    AcquisitionMode, ActiveCapture, CaptureBatch, CaptureEvent, CaptureFrame, CaptureReady,
    CaptureTarget, CompletedCapture, CompletionStatus, DsLogicOperationMode,
    FileDecodedDataRecorder, Instant, PathBuf, ProtocolAnalyzerInstance, ProtocolCaptureDecoder,
    ProtocolDecodeCache, RadioCaptureRequest, RadioDspWorker, RadioSingleTrigger,
    ScheduledRepetitiveCapture, ScopeMeasurementCache, SyncWordSuggestion, WaveformStore,
    WaveformTab, WorkerSuccess, MAX_CAPTURE_EVENTS_PER_POLL, MAX_RADIO_DSP_OUTPUTS_PER_POLL,
};

use crate::WaveCraftApp;
use std::ops::ControlFlow;

/// How a polled capture ended: the message to show when it succeeded or the
/// error when it did not, and the completion recorded in its files.
struct CaptureFinish {
    result: Result<String, String>,
    completion: CompletionStatus,
}

impl CaptureFinish {
    fn complete(message: &str) -> Self {
        Self {
            result: Ok(message.to_owned()),
            completion: CompletionStatus::Complete,
        }
    }

    fn failed(error: impl Into<String>) -> Self {
        Self {
            result: Err(error.into()),
            completion: CompletionStatus::Failed,
        }
    }
}

/// What one poll of a running capture found: whether it has ended, and a new
/// status line to show while it runs.
#[derive(Default)]
struct CapturePoll {
    finish: Option<CaptureFinish>,
    status: Option<String>,
}

impl CapturePoll {
    /// Stops the capture and records that it failed with `error`.
    fn fail(&mut self, active: &ActiveCapture, error: impl Into<String>) {
        active.session.request_stop();
        self.finish = Some(CaptureFinish::failed(error));
    }
}

/// The RF trigger settings as they are now. A running single capture re-reads
/// them on every frame, so edits take effect while it waits.
struct RadioTriggerSettings {
    threshold_dbfs: f32,
    posttrigger_samples: Result<u64, String>,
}

/// What is left of a capture once its workers have stopped and its files are closed.
struct FinishedCapture {
    waveforms: WaveformStore,
    protocol_decode: ProtocolCaptureDecoder,
    error: Option<String>,
}

/// Whether a capture that ended keeps its data for display in place of the one
/// already shown. A display that was kept until the new capture completed is
/// replaced only by a capture that did, except that a restart for a scope
/// frontend change replaces it whenever it was not to be kept.
fn completed_capture_replaces_display(
    restart_for_frontend: bool,
    completion: CompletionStatus,
    preserve_display_until_complete: bool,
) -> bool {
    if restart_for_frontend {
        !preserve_display_until_complete
    } else {
        completion == CompletionStatus::Complete || !preserve_display_until_complete
    }
}

/// Reads what a running capture has produced since the last poll. Processing
/// normally stops at `MAX_CAPTURE_EVENTS_PER_POLL`, but a scope frame already
/// in progress is drained to its boundary so it can be published atomically.
/// The logic decoder and RF DSP worker use their own budgets.
fn poll_running_capture(
    active: &mut ActiveCapture,
    analyzers: &[ProtocolAnalyzerInstance],
    radio_trigger: &RadioTriggerSettings,
) -> CapturePoll {
    let mut poll = CapturePoll::default();
    reconfigure_logic_decoder(active, analyzers, &mut poll);
    let mut event_count = 0;
    loop {
        if event_count >= MAX_CAPTURE_EVENTS_PER_POLL && !scope_frame_is_in_progress(active) {
            break;
        }
        match active.session.try_recv() {
            Ok(event) => {
                event_count += 1;
                if handle_capture_event(active, event, radio_trigger, &mut poll).is_break() {
                    break;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                poll.finish = Some(CaptureFinish::failed("capture event stream disconnected"));
                break;
            }
        }
    }
    drain_logic_decode(active, &mut poll);
    drain_radio_dsp(active, &mut poll);
    poll
}

/// Scope USB batches are pieces of one hardware frame. Once the first piece is
/// received, finish that frame in the same UI poll instead of applying the RF
/// event budget in its middle.
fn scope_frame_is_in_progress(active: &ActiveCapture) -> bool {
    if !active.has_scope {
        return false;
    }
    let enabled_channels = active
        .scope_config
        .channels()
        .map(|channel| channel.enabled());
    let expected = active.scope_config.sample_count() as usize;
    let received = active
        .pending_scope_waveforms
        .as_ref()
        .unwrap_or(&active.waveforms)
        .scope_sample_count(enabled_channels);
    received > 0 && received < expected
}

/// Sends the protocol analyzers the user edited to the running logic decoder.
fn reconfigure_logic_decoder(
    active: &mut ActiveCapture,
    analyzers: &[ProtocolAnalyzerInstance],
    poll: &mut CapturePoll,
) {
    if active.protocol_analyzers != analyzers {
        match active.logic_decode.reconfigure(analyzers.to_vec()) {
            Ok(()) => active.protocol_analyzers = analyzers.to_vec(),
            Err(error) => poll.fail(active, error),
        }
    }
}

/// Handles one event of the capture session. `Break` means the poll is over:
/// the capture has ended or failed, and `poll.finish` says which.
fn handle_capture_event(
    active: &mut ActiveCapture,
    event: CaptureEvent,
    radio_trigger: &RadioTriggerSettings,
    poll: &mut CapturePoll,
) -> ControlFlow<()> {
    match event {
        CaptureEvent::Frame(frame) => handle_capture_frame(active, frame, radio_trigger, poll),
        CaptureEvent::SourceFailed { device_id, error } => {
            if let Some(recorder) = active.recorder.as_mut() {
                if let Err(write_error) = recorder.write_event(&CaptureEvent::SourceFailed {
                    device_id: device_id.clone(),
                    error: error.clone(),
                }) {
                    poll.fail(active, format!("failed to write recording: {write_error}"));
                    return ControlFlow::Break(());
                }
            }
            poll.fail(active, format!("{device_id}: {error}"));
            ControlFlow::Break(())
        }
        CaptureEvent::SourceStopped { device_id } => {
            if let Some(recorder) = active.recorder.as_mut() {
                if let Err(error) = recorder.write_event(&CaptureEvent::SourceStopped {
                    device_id: device_id.clone(),
                }) {
                    poll.fail(active, format!("failed to write recording: {error}"));
                    return ControlFlow::Break(());
                }
            }
            active.stopped.insert(device_id);
            if active.stopped.len() == active.session.descriptors().len() {
                poll.finish = Some(CaptureFinish::complete("Capture complete"));
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
}

/// Handles one frame of samples: keeps the RF trigger up to date and lets it
/// decide which samples are kept, records the frame, and feeds the displays and
/// decoders.
fn handle_capture_frame(
    active: &mut ActiveCapture,
    frame: CaptureFrame,
    radio_trigger: &RadioTriggerSettings,
    poll: &mut CapturePoll,
) -> ControlFlow<()> {
    let frame_is_scope = matches!(&frame.payload, CaptureBatch::Scope(_));
    if let Some(trigger) = active.radio_trigger.as_mut() {
        let update_result = radio_trigger
            .posttrigger_samples
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|samples| {
                trigger.set_threshold_dbfs(radio_trigger.threshold_dbfs)?;
                trigger.set_posttrigger_samples(*samples);
                Ok(())
            });
        if let Err(error) = update_result {
            poll.fail(active, format!("RF trigger update failed: {error}"));
            return ControlFlow::Break(());
        }
    }
    let trigger_armed = active
        .radio_trigger
        .as_ref()
        .is_some_and(RadioSingleTrigger::is_armed);
    if trigger_armed {
        if let CaptureBatch::Iq(iq) = &frame.payload {
            let Some(radio_spectrum) = active.radio_spectrum.as_mut() else {
                poll.fail(
                    active,
                    "RF trigger is armed without an RF display processor",
                );
                return ControlFlow::Break(());
            };
            for spectrum in radio_spectrum.push(iq) {
                active.waveforms.radio.ingest(spectrum);
            }
        }
    }
    let frame_is_iq = matches!(&frame.payload, CaptureBatch::Iq(_));
    let (frames, trigger_event, trigger_complete) =
        if let (Some(trigger), true) = (active.radio_trigger.as_mut(), frame_is_iq) {
            match trigger.push(frame) {
                Ok(output) => (output.frames, output.event, output.complete),
                Err(error) => {
                    poll.fail(active, format!("RF trigger failed: {error}"));
                    return ControlFlow::Break(());
                }
            }
        } else {
            (vec![frame], None, false)
        };
    if let Some(event) = trigger_event {
        if let Some(radio_spectrum) = active.radio_spectrum.as_mut() {
            radio_spectrum.reset();
        }
        poll.status = Some(format!(
            "RF trigger · {:.3} MHz at {:.1} dBFS; capturing…",
            event.frequency_hz / 1_000_000.0,
            event.power_dbfs
        ));
    }
    let mut scope_frame_published = false;
    for frame in frames {
        match ingest_active_capture_frame(active, frame, radio_trigger.threshold_dbfs) {
            Ok(published) => scope_frame_published |= published,
            Err(error) => {
                poll.fail(active, error);
                break;
            }
        }
    }
    if active.preserve_display_until_complete
        && active.mode == AcquisitionMode::Run
        && active.target == CaptureTarget::Oscilloscope
        && !active.waveforms.scope_rolling
        && frame_is_scope
        && scope_frame_published
    {
        // Reveal only a coherent hardware sweep. Before RF introduced bounded
        // event polling, v0.7 effectively ingested every scope chunk before a
        // repaint; publishing a partial frame here recreates old-waveform
        // artifacts at extreme time scales.
        active.preserve_display_until_complete = false;
    }
    if poll.finish.is_some() {
        return ControlFlow::Break(());
    }
    if trigger_complete {
        active.session.request_stop();
        poll.finish = Some(CaptureFinish::complete("RF triggered capture complete"));
        return ControlFlow::Break(());
    }
    ControlFlow::Continue(())
}

/// Takes the output the logic decoder has ready.
fn drain_logic_decode(active: &mut ActiveCapture, poll: &mut CapturePoll) {
    loop {
        match active.logic_decode.try_recv() {
            Ok(output) => ingest_logic_decode_output(active, output),
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if poll.finish.is_none() {
                    poll.fail(active, "logic decode worker disconnected");
                }
                break;
            }
        }
    }
}

/// Takes the output the radio DSP worker has ready and records the decoded data.
fn drain_radio_dsp(active: &mut ActiveCapture, poll: &mut CapturePoll) {
    for _ in 0..MAX_RADIO_DSP_OUTPUTS_PER_POLL {
        let Some(radio_dsp) = active.radio_dsp.as_mut() else {
            return;
        };
        match radio_dsp.try_recv() {
            Ok(output) => {
                if let Err(error) =
                    record_decoded_radio_packets(&mut active.decoded_data_recorder, &output)
                {
                    poll.fail(active, error);
                    break;
                }
                ingest_radio_dsp_output(&mut active.waveforms, output);
                if let Err(error) = record_decoded_raw_messages(
                    &mut active.decoded_data_recorder,
                    &active.waveforms.radio_analysis,
                ) {
                    poll.fail(active, error);
                    break;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if poll.finish.is_none() {
                    poll.fail(active, "radio DSP worker disconnected");
                }
                break;
            }
        }
    }
}

/// Stops a capture that has ended and closes its files. The error covers
/// everything that went wrong on the way, starting with `result_error`; any
/// failure marks the recordings as failed whatever `completion` says.
fn shut_down_capture(
    mut active: ActiveCapture,
    result_error: Option<String>,
    completion: CompletionStatus,
) -> FinishedCapture {
    let (protocol_decode, logic_decode_error) = finish_logic_decode(&mut active);
    let stop_result = active.session.stop();
    let mut error = result_error;
    if let Err(stop_error) = &stop_result {
        append_error(&mut error, format!("failed to stop capture: {stop_error}"));
    }
    finish_decoding(
        &mut active.radio_dsp,
        &mut active.decoded_data_recorder,
        &mut active.waveforms,
        logic_decode_error,
        &mut error,
    );
    let recording_completion = if stop_result.is_ok() && error.is_none() {
        completion
    } else {
        CompletionStatus::Failed
    };
    let recording_result = finish_gui_recording(&mut active.recorder, recording_completion);
    let decoded_recording_result =
        finish_decoded_data_recording(&mut active.decoded_data_recorder, recording_completion);
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
    FinishedCapture {
        waveforms: active.waveforms,
        protocol_decode,
        error,
    }
}

/// Collects what the radio DSP worker still holds, closes the raw-message
/// capture, and adds the logic decoder's error, in that order. Every way a
/// capture ends goes through it, after the session has stopped.
fn finish_decoding(
    radio_dsp: &mut Option<RadioDspWorker>,
    decoded_data_recorder: &mut Option<FileDecodedDataRecorder>,
    waveforms: &mut WaveformStore,
    logic_decode_error: Option<String>,
    error: &mut Option<String>,
) {
    if let Some(radio_dsp) = radio_dsp.as_mut() {
        match radio_dsp.finish() {
            Ok(outputs) => {
                for output in outputs {
                    if let Err(recording_error) =
                        record_decoded_radio_packets(decoded_data_recorder, &output)
                    {
                        append_error(error, recording_error);
                    }
                    ingest_radio_dsp_output(waveforms, output);
                    if let Err(recording_error) = record_decoded_raw_messages(
                        decoded_data_recorder,
                        &waveforms.radio_analysis,
                    ) {
                        append_error(error, recording_error);
                    }
                }
            }
            Err(dsp_error) => append_error(error, dsp_error),
        }
    }
    if let Some(logic_decode_error) = logic_decode_error {
        append_error(error, logic_decode_error);
    }
    waveforms.radio_analysis.finish_raw_capture();
    if let Err(recording_error) =
        record_decoded_raw_messages(decoded_data_recorder, &waveforms.radio_analysis)
    {
        append_error(error, recording_error);
    }
}

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
        let provider_settings = radio_provider_settings(device.provider, &self.radio_settings)?;
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
        let decoder_configs = if analyzer_profiles.is_empty() {
            Vec::new()
        } else {
            self.radio_settings.analyzer.decoder_configs()?
        };
        Ok(RadioCaptureRequest {
            device,
            settings,
            provider_settings,
            iq_correction_enabled: self.radio_settings.iq_correction_enabled,
            analyzer_profiles,
            analyzer_channel_names,
            decoder_configs,
        })
    }

    /// Restores trigger-locked positioning and applies the selected horizontal
    /// resolution only when a scope acquisition is about to run. While
    /// stopped, the same selection is a display-only zoom and must not mutate
    /// the configured sample rate.
    fn prepare_scope_capture_view(&mut self, target: CaptureTarget) {
        if !target.includes_oscilloscope() {
            return;
        }
        // A new acquisition returns to trigger-locked positioning. Panning a
        // stopped trace must not leave Auto sweeps drifting between frames.
        self.scope_viewport.anchor_locked = true;
        if !self.scope_auto_sample_rate {
            return;
        }
        if let Some(time_per_division) = self.scope_time_per_division_seconds {
            self.scope_settings.sample_rate_hz = self
                .scope_settings
                .sample_rate_for_timebase(time_per_division);
        }
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
        let radio_request = if target == CaptureTarget::RadioReceiver {
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
        self.prepare_scope_capture_view(target);
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
        if !acquisition_mode_is_supported(mode, target) {
            self.record_capture = false;
            self.record_decoded_data = false;
            self.last_error = Some(match mode {
                AcquisitionMode::Repetitive => {
                    "Repetitive is available for Logic Analyzer and Re-Arm for Oscilloscope capture only"
                }
                AcquisitionMode::Loop => "Loop mode is available for Logic Analyzer capture only",
                AcquisitionMode::Single | AcquisitionMode::Run => {
                    "The selected acquisition mode is not available for this target"
                }
            }
            .to_owned());
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
        let update_scope_display =
            scope_updates_on_trigger(mode, target, scope_config.trigger().source());
        if clears_completed_capture_on_start(mode, continuing_request) && !update_scope_display {
            self.clear_completed_capture_for_target(target);
        }
        self.status = match mode {
            AcquisitionMode::Single => {
                format!("Preparing {} single acquisition…", target.label())
            }
            AcquisitionMode::Repetitive => {
                if target == CaptureTarget::Oscilloscope {
                    "Preparing Oscilloscope Re-Arm acquisition…".to_owned()
                } else {
                    format!("Preparing {} repetitive acquisition…", target.label())
                }
            }
            AcquisitionMode::Loop => {
                format!("Preparing {} loop acquisition…", target.label())
            }
            AcquisitionMode::Run => {
                format!("Preparing {} continuous acquisition…", target.label())
            }
        };
        self.last_error = None;
        let continuous_scope_capture = uses_continuous_scope_capture(mode, target);
        thread::spawn(move || {
            let result = start_capture(
                devices,
                radio_request,
                &resource_directory,
                logic_config,
                scope_config,
                calibration_values,
                continuous_scope_capture,
            )
            .map(|started| {
                WorkerSuccess::Capture(Box::new(CaptureReady {
                    session: started.session,
                    mode,
                    target,
                    retain_protocol_history,
                    logic_config,
                    scope_config,
                    scope_trigger_controls: started.scope_trigger_controls,
                    scope_calibration_controls: started.scope_calibration_controls,
                    radio_runtime_control: started.radio_runtime_control,
                    radio_spectrum: started.radio_spectrum,
                    radio_dsp: started.radio_dsp,
                }))
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
        let mut error = stop_result.as_ref().err().map(ToString::to_string);
        finish_decoding(
            &mut active.radio_dsp,
            &mut active.decoded_data_recorder,
            &mut active.waveforms,
            logic_decode_error,
            &mut error,
        );
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
        let radio_trigger = RadioTriggerSettings {
            threshold_dbfs: f32::from(self.radio_settings.trigger_level_dbfs_tenths) / 10.0,
            posttrigger_samples: radio_microsecond_sample_count(
                self.radio_settings.capture.sample_rate_hz,
                self.radio_settings.trigger_posttrigger_micros,
            ),
        };
        let poll = poll_running_capture(
            &mut active,
            &self.protocol_analyzer.analyzers,
            &radio_trigger,
        );
        if let Some(status) = poll.status {
            self.status = status;
        }
        match poll.finish {
            Some(finish) => self.finish_capture(active, finish),
            None => self.captures.push(active),
        }
    }

    /// Ends a capture that has run its course or failed: stops its workers,
    /// closes its recordings, keeps what it captured for display, and then
    /// starts whatever was waiting for it (a calibration, or the next acquisition).
    fn finish_capture(&mut self, mut active: ActiveCapture, finish: CaptureFinish) {
        let CaptureFinish { result, completion } = finish;
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
        let FinishedCapture {
            waveforms,
            protocol_decode,
            error,
        } = shut_down_capture(active, result.as_ref().err().cloned(), completion);
        if completed_capture_replaces_display(
            restart_for_frontend,
            completion,
            preserve_display_until_complete,
        ) {
            self.set_completed_capture_for_target(
                target,
                CompletedCapture {
                    waveforms,
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
                let capture_status = result.ok().unwrap_or_else(|| "Capture complete".to_owned());
                self.status = decoded_data_path
                    .or(recording_path)
                    .map_or(capture_status.clone(), |path| {
                        format!("{capture_status} · recording saved to {}", path.display())
                    });
            }
        }
        if repeat {
            self.start_next_acquisition(mode, target, restart_for_frontend);
        }
    }

    /// Starts the capture that follows a finished one: Logic Repetitive waits
    /// for its interval, Oscilloscope Re-Arm re-arms immediately, and a
    /// background operation queues either until it is ready.
    fn start_next_acquisition(
        &mut self,
        mode: AcquisitionMode,
        target: CaptureTarget,
        restart_for_frontend: bool,
    ) {
        if uses_repetitive_interval(mode, target) && !restart_for_frontend {
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

    /// The sync words that the raw messages of the radio capture on display
    /// have in common: of the running capture, or else of the last one.
    pub(crate) fn displayed_sync_word_suggestions(&self) -> Vec<SyncWordSuggestion> {
        self.capture_for_target(CaptureTarget::RadioReceiver)
            .filter(|active| !active.preserve_display_until_complete)
            .map(|active| &active.waveforms)
            .or_else(|| {
                self.radio_capture
                    .as_ref()
                    .or(self.combined_capture.as_ref())
                    .map(|capture| &capture.waveforms)
            })
            .map(|waveforms| waveforms.radio_analysis.sync_word_suggestions())
            .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{
        app, poll_until, poll_until_finished, ready, ready_scripted, request, scratch_directory,
        Script, SourceEnding,
    };
    use crate::*;

    use std::sync::mpsc;
    use std::time::Duration;
    use wavecraft_core::{Iq8, IqBatch, LogicBatch};

    #[test]
    fn starting_an_automatic_scope_capture_applies_timebase_and_relocks_trigger() {
        let mut app = app();
        app.scope_auto_sample_rate = true;
        app.scope_settings.sample_rate_hz = DSCOPE_SAMPLE_RATES_HZ[0];
        app.scope_time_per_division_seconds = Some(1.0e-6);
        app.scope_viewport.anchor_locked = false;
        let expected_rate = app.scope_settings.sample_rate_for_timebase(1.0e-6);

        app.prepare_scope_capture_view(CaptureTarget::Oscilloscope);

        assert_eq!(app.scope_settings.sample_rate_hz, expected_rate);
        assert!(app.scope_viewport.anchor_locked);
    }

    #[test]
    fn starting_a_manual_rate_capture_relocks_without_changing_the_rate() {
        let mut app = app();
        app.scope_auto_sample_rate = false;
        app.scope_settings.sample_rate_hz = DSCOPE_SAMPLE_RATES_HZ[0];
        app.scope_time_per_division_seconds = Some(1.0e-6);
        app.scope_viewport.anchor_locked = false;

        app.prepare_scope_capture_view(CaptureTarget::Oscilloscope);

        assert_eq!(app.scope_settings.sample_rate_hz, DSCOPE_SAMPLE_RATES_HZ[0]);
        assert!(app.scope_viewport.anchor_locked);
    }

    fn logic_batch(app: &WaveCraftApp, samples: Vec<u32>) -> CaptureBatch {
        let sample_rate_hz = app
            .logic_settings
            .capture_config()
            .expect("the default logic settings are valid")
            .sample_rate_hz();
        CaptureBatch::Logic(LogicBatch {
            sample_rate_hz,
            channel_count: 16,
            samples,
        })
    }

    /// Starts a capture of one fake logic analyzer that follows `script`.
    fn start_logic_capture(app: &mut WaveCraftApp, mode: AcquisitionMode, script: Script) {
        request(app, CaptureTarget::LogicAnalyzer);
        let started = ready_scripted(
            app,
            vec![(InstrumentKind::LogicAnalyzer, script)],
            mode,
            CaptureTarget::LogicAnalyzer,
        );
        app.handle_capture_ready(started);
        assert_eq!(app.captures.len(), 1, "{:?}", app.last_error);
    }

    fn finishing_with(app: &WaveCraftApp, samples: Vec<u32>) -> Script {
        Script::sending(vec![logic_batch(app, samples)], SourceEnding::Finish)
    }

    fn failing_with(message: &str) -> Script {
        Script::sending(Vec::new(), SourceEnding::Fail(message.to_owned()))
    }

    fn record_to(app: &mut WaveCraftApp, path: &Path) {
        app.record_capture = true;
        app.recording_path = path.to_string_lossy().into_owned();
    }

    fn recorded_completion(app: &WaveCraftApp, path: &Path) -> CompletionStatus {
        load_gui_recording(path, &app.protocol_analyzer.analyzers)
            .expect("the recording was finished")
            .completion
    }

    #[test]
    fn a_finished_capture_replaces_the_display_unless_that_was_to_be_kept() {
        use CompletionStatus::{Cancelled, Complete, Failed};
        // (restart for a frontend change, completion, display kept until
        // complete) -> whether the finished capture is shown
        for (restart, completion, preserve, replaces) in [
            (false, Complete, false, true),
            (false, Complete, true, true),
            (false, Failed, false, true),
            (false, Failed, true, false),
            (false, Cancelled, true, false),
            (true, Complete, false, true),
            (true, Complete, true, false),
            (true, Failed, false, true),
            (true, Failed, true, false),
        ] {
            assert_eq!(
                completed_capture_replaces_display(restart, completion, preserve),
                replaces,
                "restart = {restart}, {completion:?}, preserve = {preserve}"
            );
        }
    }

    #[test]
    fn a_capture_whose_source_finishes_is_completed_and_stored() {
        let mut app = app();
        let script = finishing_with(&app, vec![0, 1, 2, 3]);
        start_logic_capture(&mut app, AcquisitionMode::Single, script);

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        assert_eq!(app.status, "Capture complete");
        assert!(app.last_error.is_none());
        let completed = app
            .completed_capture_for_target(CaptureTarget::LogicAnalyzer)
            .expect("the finished capture is kept for display");
        assert!(completed.has_logic && !completed.has_scope && !completed.has_radio);
        assert_eq!(completed.waveforms.logic_sample_count, 4);
        assert!(!app.capture_is_requested(CaptureTarget::LogicAnalyzer));
        assert!(app.queued_capture_rearms.is_empty());
        assert!(app.scheduled_repetitive_capture.is_none());
    }

    #[test]
    fn a_running_capture_stays_active_between_polls() {
        let mut app = app();
        start_logic_capture(&mut app, AcquisitionMode::Run, Script::idle());
        let status = app.status.clone();

        for _ in 0..5 {
            app.poll_capture_target(CaptureTarget::LogicAnalyzer);
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(app.captures.len(), 1);
        assert!(app.captures[0].stopped.is_empty());
        assert_eq!(app.status, status);
    }

    #[test]
    fn scope_update_holds_the_old_trace_then_stays_active_with_the_triggered_frame() {
        let mut app = app();
        app.scope_settings.sample_count = 1_024;
        app.scope_settings.trigger.source = DsCopeTriggerSource::ChannelZero;
        let previous = crate::app::test_support::completed_capture(&app, false, true, false);
        app.set_completed_capture_for_target(CaptureTarget::Oscilloscope, previous);
        request(&mut app, CaptureTarget::Oscilloscope);
        let started = ready_scripted(
            &app,
            vec![(
                InstrumentKind::Oscilloscope,
                Script::sending(
                    vec![CaptureBatch::Scope(ScopeBatch {
                        sample_rate_hz: 1_000_000,
                        trigger_sample_index: Some(1),
                        channels: vec![vec![0.1; 1_024], vec![-0.1; 1_024]],
                    })],
                    SourceEnding::Idle,
                ),
            )],
            AcquisitionMode::Run,
            CaptureTarget::Oscilloscope,
        );
        app.handle_capture_ready(started);

        assert!(app.captures[0].preserve_display_until_complete);
        poll_until(&mut app, CaptureTarget::Oscilloscope, |app| {
            app.capture_for_target(CaptureTarget::Oscilloscope)
                .is_some_and(|capture| {
                    !capture.preserve_display_until_complete
                        && capture
                            .waveforms
                            .scope_channels
                            .first()
                            .is_some_and(|samples| samples.len() == 1_024)
                })
        });

        for _ in 0..5 {
            app.poll_capture_target(CaptureTarget::Oscilloscope);
        }
        assert!(app
            .capture_for_target(CaptureTarget::Oscilloscope)
            .is_some());
        assert!(app.capture_is_requested(CaptureTarget::Oscilloscope));
        app.stop_capture(CaptureTarget::Oscilloscope);
    }

    #[test]
    fn continuous_scope_publishes_only_complete_instrument_frames() {
        let mut app = app();
        app.scope_settings.sample_count = 1_024;
        request(&mut app, CaptureTarget::Oscilloscope);
        let started = ready(
            &app,
            InstrumentKind::Oscilloscope,
            AcquisitionMode::Run,
            CaptureTarget::Oscilloscope,
        );
        app.handle_capture_ready(started);
        let active = app
            .capture_for_target_mut(CaptureTarget::Oscilloscope)
            .unwrap();
        assert!(active.radio_spectrum.is_none());
        assert!(active.radio_dsp.is_none());
        active.waveforms.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000,
                trigger_sample_index: Some(512),
                channels: vec![vec![9.0; 1_024], vec![-9.0; 1_024]],
            }),
            [true, true],
        );

        let first = CaptureFrame {
            device_id: DeviceId::new("scope"),
            sequence: 0,
            captured_at: Duration::ZERO,
            payload: CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000,
                trigger_sample_index: Some(512),
                channels: vec![vec![1.0; 512], vec![-1.0; 512]],
            }),
        };
        assert!(!ingest_active_capture_frame(active, first, -100.0).unwrap());
        assert_eq!(active.waveforms.scope_channels[0][0], 9.0);
        assert_eq!(
            active
                .pending_scope_waveforms
                .as_ref()
                .unwrap()
                .scope_sample_count([true, true]),
            512
        );

        let second = CaptureFrame {
            device_id: DeviceId::new("scope"),
            sequence: 1,
            captured_at: Duration::from_micros(512),
            payload: CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000,
                trigger_sample_index: None,
                channels: vec![vec![2.0; 512], vec![-2.0; 512]],
            }),
        };
        assert!(ingest_active_capture_frame(active, second, -100.0).unwrap());
        assert!(active.pending_scope_waveforms.is_none());
        assert_eq!(active.waveforms.scope_channels[0].len(), 1_024);
        assert_eq!(active.waveforms.scope_channels[0][0], 1.0);
        assert_eq!(active.waveforms.scope_channels[0][1_023], 2.0);
    }

    #[test]
    fn one_poll_finishes_a_scope_frame_larger_than_the_shared_event_budget() {
        let mut app = app();
        app.scope_settings.sample_count = 1_000_000;
        let mut remaining = app.scope_settings.sample_count as usize;
        let mut batches = Vec::new();
        while remaining > 0 {
            let count = remaining.min(65_536);
            batches.push(CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000,
                trigger_sample_index: batches.is_empty().then_some(500_000),
                channels: vec![vec![1.0; count], vec![-1.0; count]],
            }));
            remaining -= count;
        }
        assert!(batches.len() > MAX_CAPTURE_EVENTS_PER_POLL);

        request(&mut app, CaptureTarget::Oscilloscope);
        let started = ready_scripted(
            &app,
            vec![(
                InstrumentKind::Oscilloscope,
                Script::sending(batches, SourceEnding::Idle),
            )],
            AcquisitionMode::Run,
            CaptureTarget::Oscilloscope,
        );
        app.handle_capture_ready(started);
        std::thread::sleep(Duration::from_millis(100));

        app.poll_capture_target(CaptureTarget::Oscilloscope);

        let active = app.capture_for_target(CaptureTarget::Oscilloscope).unwrap();
        assert!(active.pending_scope_waveforms.is_none());
        assert_eq!(active.waveforms.scope_sample_count([true, true]), 1_000_000);
        app.stop_capture(CaptureTarget::Oscilloscope);
    }

    #[test]
    fn polling_a_target_without_a_capture_does_nothing() {
        let mut app = app();
        let status = app.status.clone();

        app.poll_capture_target(CaptureTarget::Oscilloscope);
        app.poll_capture();

        assert_eq!(app.status, status);
        assert!(app.last_error.is_none());
    }

    #[test]
    fn poll_capture_polls_every_running_capture() {
        let mut app = app();
        request(&mut app, CaptureTarget::LogicAnalyzer);
        let logic = ready_scripted(
            &app,
            vec![(
                InstrumentKind::LogicAnalyzer,
                finishing_with(&app, vec![1, 2]),
            )],
            AcquisitionMode::Single,
            CaptureTarget::LogicAnalyzer,
        );
        app.handle_capture_ready(logic);
        request(&mut app, CaptureTarget::Oscilloscope);
        let scope = ready_scripted(
            &app,
            vec![(
                InstrumentKind::Oscilloscope,
                Script::sending(Vec::new(), SourceEnding::Finish),
            )],
            AcquisitionMode::Single,
            CaptureTarget::Oscilloscope,
        );
        app.handle_capture_ready(scope);
        assert_eq!(app.captures.len(), 2);

        let deadline = Instant::now() + Duration::from_secs(5);
        while !app.captures.is_empty() {
            assert!(Instant::now() < deadline, "{:?}", app.status);
            app.poll_capture();
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(app
            .completed_capture_for_target(CaptureTarget::LogicAnalyzer)
            .is_some());
        assert!(app
            .completed_capture_for_target(CaptureTarget::Oscilloscope)
            .is_some());
    }

    #[test]
    fn a_run_capture_that_finishes_starts_the_next_one() {
        let mut app = app();
        let script = finishing_with(&app, vec![1]);
        start_logic_capture(&mut app, AcquisitionMode::Run, script);

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        // The next start goes through begin_capture, which needs a selected
        // DSLogic and so reports that none is connected here.
        assert!(app
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("DSLogic")));
        assert!(app.capture_is_requested(CaptureTarget::LogicAnalyzer));
    }

    #[test]
    fn a_run_capture_that_finishes_while_a_worker_is_busy_queues_the_next_start_once() {
        let mut app = app();
        let (_sender, receiver) = mpsc::channel::<WorkerResult>();
        app.pending = Some(receiver);
        let script = finishing_with(&app, vec![1]);
        start_logic_capture(&mut app, AcquisitionMode::Run, script);

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        assert_eq!(
            app.queued_capture_rearms,
            vec![(AcquisitionMode::Run, CaptureTarget::LogicAnalyzer)]
        );
        assert!(app.last_error.is_none());

        // A start that is already queued for the target is not queued twice.
        let mut app = crate::app::test_support::app();
        let (_sender, receiver) = mpsc::channel::<WorkerResult>();
        app.pending = Some(receiver);
        app.queued_capture_rearms
            .push((AcquisitionMode::Run, CaptureTarget::LogicAnalyzer));
        let script = finishing_with(&app, vec![1]);
        start_logic_capture(&mut app, AcquisitionMode::Run, script);

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        assert_eq!(app.queued_capture_rearms.len(), 1);
    }

    #[test]
    fn a_single_capture_that_finishes_is_not_started_again() {
        let mut app = app();
        app.pending = None;
        let script = finishing_with(&app, vec![1]);
        start_logic_capture(&mut app, AcquisitionMode::Single, script);

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        assert!(app.last_error.is_none());
        assert!(app.queued_capture_rearms.is_empty());
        assert!(!app.capture_is_requested(CaptureTarget::LogicAnalyzer));
    }

    #[test]
    fn a_repetitive_capture_schedules_the_next_acquisition_after_the_interval() {
        for (operation_mode, expected) in [
            (
                DsLogicOperationMode::Buffer,
                "Repetitive · next acquisition in 2.0 s",
            ),
            (
                DsLogicOperationMode::Stream,
                "Repetitive · next acquisition in 0.1 s",
            ),
        ] {
            let mut app = app();
            app.logic_settings.operation_mode = operation_mode;
            app.repetitive_interval_seconds = 2.0;
            let script = finishing_with(&app, vec![1]);
            start_logic_capture(&mut app, AcquisitionMode::Repetitive, script);

            poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

            assert_eq!(app.status, expected, "{operation_mode:?}");
            let scheduled = app
                .scheduled_repetitive_capture
                .expect("the next start is scheduled");
            assert_eq!(scheduled.target, CaptureTarget::LogicAnalyzer);
            assert!(scheduled.due > Instant::now());
            assert!(app.last_error.is_none());
        }
    }

    #[test]
    fn a_failing_source_ends_the_capture_with_its_error_and_no_restart() {
        let mut app = app();
        start_logic_capture(&mut app, AcquisitionMode::Run, failing_with("boom"));

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        assert_eq!(app.last_error.as_deref(), Some("fake:0: boom"));
        assert!(app.queued_capture_rearms.is_empty());
        assert!(!app.capture_is_requested(CaptureTarget::LogicAnalyzer));
        assert!(app
            .completed_capture_for_target(CaptureTarget::LogicAnalyzer)
            .is_some());
    }

    #[test]
    fn a_finished_capture_names_the_file_it_was_recorded_to() {
        let directory = scratch_directory("poll-recorded");
        let path = directory.join("capture.dsv");
        let mut app = app();
        record_to(&mut app, &path);
        let script = finishing_with(&app, vec![5, 6, 7]);
        start_logic_capture(&mut app, AcquisitionMode::Single, script);

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        assert_eq!(
            app.status,
            format!("Capture complete · recording saved to {}", path.display())
        );
        assert_eq!(recorded_completion(&app, &path), CompletionStatus::Complete);
        assert!(!app.record_capture);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_failing_source_marks_the_recording_as_failed() {
        let directory = scratch_directory("poll-failed-recording");
        let path = directory.join("capture.dsv");
        let mut app = app();
        record_to(&mut app, &path);
        start_logic_capture(&mut app, AcquisitionMode::Single, failing_with("boom"));

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        assert_eq!(app.last_error.as_deref(), Some("fake:0: boom"));
        assert_eq!(recorded_completion(&app, &path), CompletionStatus::Failed);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_capture_is_complete_only_when_every_instrument_has_stopped() {
        let mut app = app();
        request(&mut app, CaptureTarget::Combined);
        let started = ready_scripted(
            &app,
            vec![
                (InstrumentKind::LogicAnalyzer, finishing_with(&app, vec![1])),
                (InstrumentKind::Oscilloscope, Script::idle()),
            ],
            AcquisitionMode::Single,
            CaptureTarget::Combined,
        );
        app.handle_capture_ready(started);

        poll_until(&mut app, CaptureTarget::Combined, |app| {
            app.captures
                .first()
                .is_some_and(|capture| capture.stopped.len() == 1)
        });
        for _ in 0..3 {
            app.poll_capture_target(CaptureTarget::Combined);
        }

        assert_eq!(app.captures.len(), 1, "one instrument is still running");

        // With both instruments stopping, the capture completes.
        let mut app = crate::app::test_support::app();
        request(&mut app, CaptureTarget::Combined);
        let started = ready_scripted(
            &app,
            vec![
                (InstrumentKind::LogicAnalyzer, finishing_with(&app, vec![1])),
                (
                    InstrumentKind::Oscilloscope,
                    Script::sending(Vec::new(), SourceEnding::Finish),
                ),
            ],
            AcquisitionMode::Single,
            CaptureTarget::Combined,
        );
        app.handle_capture_ready(started);

        poll_until_finished(&mut app, CaptureTarget::Combined);

        assert_eq!(app.status, "Capture complete");
        let completed = app
            .completed_capture_for_target(CaptureTarget::Combined)
            .expect("the combined capture is kept");
        assert!(completed.has_logic && completed.has_scope);
    }

    #[test]
    fn a_capture_is_read_in_bounded_batches_of_events() {
        let mut app = app();
        let batches = (0..20)
            .map(|index| logic_batch(&app, vec![index]))
            .collect();
        start_logic_capture(
            &mut app,
            AcquisitionMode::Single,
            Script::sending(batches, SourceEnding::Finish),
        );
        // Let the source send all of its events before the first poll.
        std::thread::sleep(Duration::from_millis(100));

        app.poll_capture_target(CaptureTarget::LogicAnalyzer);

        assert_eq!(
            app.captures.len(),
            1,
            "{MAX_CAPTURE_EVENTS_PER_POLL} events per poll cannot reach the end of 21 events"
        );

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        let completed = app
            .completed_capture_for_target(CaptureTarget::LogicAnalyzer)
            .unwrap();
        assert_eq!(completed.waveforms.logic_sample_count, 20);
    }

    #[test]
    fn a_display_kept_until_complete_is_replaced_only_by_a_capture_that_completes() {
        // (mode, how the source ends, whether the finished capture replaces the
        // one on display)
        for (mode, fails, replaced) in [
            (AcquisitionMode::Run, false, true),
            (AcquisitionMode::Run, true, false),
            (AcquisitionMode::Single, true, true),
        ] {
            let mut app = app();
            let mut displayed = completed_capture_with_samples(&app, 42);
            displayed.has_logic = true;
            app.set_completed_capture_for_target(CaptureTarget::LogicAnalyzer, displayed);
            let script = if fails {
                failing_with("boom")
            } else {
                finishing_with(&app, vec![1, 2, 3, 4])
            };
            start_logic_capture(&mut app, mode, script);

            poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

            let count = app
                .completed_capture_for_target(CaptureTarget::LogicAnalyzer)
                .map(|capture| capture.waveforms.logic_sample_count);
            let expected = if replaced {
                if fails {
                    Some(0)
                } else {
                    Some(4)
                }
            } else {
                Some(42)
            };
            assert_eq!(count, expected, "{mode:?}, fails = {fails}");
        }
    }

    fn completed_capture_with_samples(app: &WaveCraftApp, samples: usize) -> CompletedCapture {
        let mut capture = crate::app::test_support::completed_capture(app, true, false, false);
        capture.waveforms.logic_sample_count = samples as _;
        capture
    }

    #[test]
    fn a_scope_frontend_change_restarts_the_finished_capture() {
        let mut app = app();
        request(&mut app, CaptureTarget::Oscilloscope);
        let started = ready_scripted(
            &app,
            vec![(
                InstrumentKind::Oscilloscope,
                Script::sending(Vec::new(), SourceEnding::Finish),
            )],
            AcquisitionMode::Run,
            CaptureTarget::Oscilloscope,
        );
        app.handle_capture_ready(started);
        app.scope_capture_restart_requested = true;

        poll_until_finished(&mut app, CaptureTarget::Oscilloscope);

        assert!(!app.scope_capture_restart_requested);
        assert_eq!(app.status, "Reconfiguring oscilloscope frontend…");
        // The restart goes through begin_capture, which needs a selected DSCope
        // and so reports that none is connected here.
        assert!(app
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("DSCope")));
        assert!(app.capture_is_requested(CaptureTarget::Oscilloscope));
    }

    #[test]
    fn a_scope_frontend_change_does_not_restart_a_capture_nobody_requested() {
        let mut app = app();
        request(&mut app, CaptureTarget::LogicAnalyzer);
        let script = finishing_with(&app, vec![1]);
        let started = ready_scripted(
            &app,
            vec![(InstrumentKind::LogicAnalyzer, script)],
            AcquisitionMode::Single,
            CaptureTarget::LogicAnalyzer,
        );
        app.handle_capture_ready(started);
        app.scope_capture_restart_requested = true;

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        // The flag is consumed, but a logic capture is not restarted for it.
        assert!(!app.scope_capture_restart_requested);
        assert_eq!(app.status, "Capture complete");
    }

    #[test]
    fn a_requested_manual_calibration_load_starts_after_the_capture_finishes() {
        let mut app = app();
        app.manual_calibration_load_requested = true;
        let script = finishing_with(&app, vec![1]);
        start_logic_capture(&mut app, AcquisitionMode::Single, script);
        let status_while_running = app.status.clone();

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        assert!(!app.manual_calibration_load_requested);
        // The calibration load takes over from the capture-complete message (it
        // does nothing here without a DSCope), and the request is left as is.
        assert_eq!(app.status, status_while_running);
        assert!(app.capture_is_requested(CaptureTarget::LogicAnalyzer));
    }

    #[test]
    fn a_failed_capture_drops_the_requested_manual_calibration_load() {
        let mut app = app();
        app.manual_calibration_load_requested = true;
        start_logic_capture(&mut app, AcquisitionMode::Single, failing_with("boom"));

        poll_until_finished(&mut app, CaptureTarget::LogicAnalyzer);

        assert!(!app.manual_calibration_load_requested);
        assert_eq!(app.last_error.as_deref(), Some("fake:0: boom"));
        assert_eq!(app.status, "Operation failed");
    }

    #[test]
    fn protocol_analyzers_edited_during_a_capture_reach_its_decoder() {
        let mut app = app();
        start_logic_capture(&mut app, AcquisitionMode::Run, Script::idle());
        assert!(app.captures[0].protocol_analyzers.is_empty());
        let analyzers = vec![ProtocolAnalyzerInstance {
            instance_id: 1,
            protocol_id: "1:uart",
            channel_mappings: vec![Some(0)],
            data_format: ProtocolDataFormat::Ascii,
            custom_protocol: None,
        }];
        app.protocol_analyzer.analyzers = analyzers.clone();

        app.poll_capture_target(CaptureTarget::LogicAnalyzer);

        assert_eq!(app.captures[0].protocol_analyzers, analyzers);
        assert!(app.last_error.is_none());
    }

    #[test]
    fn an_rf_trigger_that_cannot_be_updated_ends_the_capture_with_an_error() {
        let mut app = app();
        request(&mut app, CaptureTarget::RadioReceiver);
        app.radio_settings.trigger_enabled = true;
        let batch = CaptureBatch::Iq(IqBatch {
            sample_rate_hz: app.radio_settings.capture.sample_rate_hz,
            center_frequency_hz: app.radio_settings.capture.center_frequency_hz,
            first_sample_index: 0,
            discontinuity: false,
            samples: vec![Iq8 { i: 1, q: 1 }; 16],
        });
        let started = ready_scripted(
            &app,
            vec![(
                InstrumentKind::RadioReceiver,
                Script::sending(vec![batch], SourceEnding::Idle),
            )],
            AcquisitionMode::Single,
            CaptureTarget::RadioReceiver,
        );
        app.handle_capture_ready(started);
        assert!(app.captures[0].radio_trigger.is_some());
        // The user edits the sample rate into something the trigger rejects.
        app.radio_settings.capture.sample_rate_hz = 0;

        poll_until_finished(&mut app, CaptureTarget::RadioReceiver);

        assert_eq!(
            app.last_error.as_deref(),
            Some("RF trigger update failed: RF sample rate must be greater than zero")
        );
    }

    #[test]
    fn an_rf_trigger_that_fires_completes_the_capture_with_the_triggered_samples() {
        let mut app = app();
        request(&mut app, CaptureTarget::RadioReceiver);
        app.radio_settings.trigger_enabled = true;
        app.radio_settings.trigger_level_dbfs_tenths = -1_000;
        app.radio_settings.trigger_posttrigger_micros = 1;
        // A tone at a quarter of the sample rate, far above the -100 dBFS level.
        let tone = (0..2 * RADIO_SPECTRUM_FFT_LENGTH)
            .map(|index| match index % 4 {
                0 => Iq8 { i: 100, q: 0 },
                1 => Iq8 { i: 0, q: 100 },
                2 => Iq8 { i: -100, q: 0 },
                _ => Iq8 { i: 0, q: -100 },
            })
            .collect();
        let batch = CaptureBatch::Iq(IqBatch {
            sample_rate_hz: app.radio_settings.capture.sample_rate_hz,
            center_frequency_hz: app.radio_settings.capture.center_frequency_hz,
            first_sample_index: 0,
            discontinuity: false,
            samples: tone,
        });
        let started = ready_scripted(
            &app,
            vec![(
                InstrumentKind::RadioReceiver,
                Script::sending(vec![batch], SourceEnding::Idle),
            )],
            AcquisitionMode::Single,
            CaptureTarget::RadioReceiver,
        );
        app.handle_capture_ready(started);
        assert!(app.captures[0].radio_trigger.is_some());

        poll_until_finished(&mut app, CaptureTarget::RadioReceiver);

        assert_eq!(app.status, "RF triggered capture complete");
        assert!(app.last_error.is_none(), "{:?}", app.last_error);
        let completed = app
            .completed_capture_for_target(CaptureTarget::RadioReceiver)
            .expect("the triggered capture is kept");
        assert!(completed.has_radio);
        assert!(
            completed.waveforms.radio.newest_spectrum().is_some(),
            "the spectrum seen while the trigger was armed is displayed"
        );
    }

    #[test]
    fn an_armed_rf_trigger_shows_the_spectrum_while_it_waits() {
        let mut app = app();
        request(&mut app, CaptureTarget::RadioReceiver);
        app.radio_settings.trigger_enabled = true;
        app.radio_settings.trigger_level_dbfs_tenths = 0;
        // Silence stays below a 0 dBFS trigger level.
        let silence = CaptureBatch::Iq(IqBatch {
            sample_rate_hz: app.radio_settings.capture.sample_rate_hz,
            center_frequency_hz: app.radio_settings.capture.center_frequency_hz,
            first_sample_index: 0,
            discontinuity: false,
            samples: vec![Iq8 { i: 0, q: 0 }; 2 * RADIO_SPECTRUM_FFT_LENGTH],
        });
        let started = ready_scripted(
            &app,
            vec![(
                InstrumentKind::RadioReceiver,
                Script::sending(vec![silence], SourceEnding::Idle),
            )],
            AcquisitionMode::Single,
            CaptureTarget::RadioReceiver,
        );
        app.handle_capture_ready(started);

        poll_until(&mut app, CaptureTarget::RadioReceiver, |app| {
            app.captures[0].waveforms.radio.newest_spectrum().is_some()
        });

        assert_eq!(app.captures.len(), 1, "the trigger is still waiting");
        assert!(app.captures[0]
            .radio_trigger
            .as_ref()
            .is_some_and(RadioSingleTrigger::is_armed));
    }

    #[test]
    fn an_rf_trigger_that_has_fired_reports_it_while_the_capture_continues() {
        let mut app = app();
        request(&mut app, CaptureTarget::RadioReceiver);
        app.radio_settings.trigger_enabled = true;
        app.radio_settings.trigger_level_dbfs_tenths = -1_000;
        // A post-trigger interval much longer than the one batch the source sends.
        app.radio_settings.trigger_posttrigger_micros = 1_000_000;
        let tone = (0..2 * RADIO_SPECTRUM_FFT_LENGTH)
            .map(|index| match index % 4 {
                0 => Iq8 { i: 100, q: 0 },
                1 => Iq8 { i: 0, q: 100 },
                2 => Iq8 { i: -100, q: 0 },
                _ => Iq8 { i: 0, q: -100 },
            })
            .collect();
        let batch = CaptureBatch::Iq(IqBatch {
            sample_rate_hz: app.radio_settings.capture.sample_rate_hz,
            center_frequency_hz: app.radio_settings.capture.center_frequency_hz,
            first_sample_index: 0,
            discontinuity: false,
            samples: tone,
        });
        let started = ready_scripted(
            &app,
            vec![(
                InstrumentKind::RadioReceiver,
                Script::sending(vec![batch], SourceEnding::Idle),
            )],
            AcquisitionMode::Single,
            CaptureTarget::RadioReceiver,
        );
        app.handle_capture_ready(started);

        poll_until(&mut app, CaptureTarget::RadioReceiver, |app| {
            app.status.starts_with("RF trigger · ")
        });

        assert!(app.status.ends_with("dBFS; capturing…"), "{}", app.status);
        assert_eq!(
            app.captures.len(),
            1,
            "the capture waits for its post-trigger samples"
        );
        assert!(app.captures[0]
            .radio_trigger
            .as_ref()
            .is_some_and(|trigger| !trigger.is_armed()));
    }

    #[test]
    fn a_capture_whose_event_stream_closes_early_ends_with_an_error() {
        let mut app = app();
        request(&mut app, CaptureTarget::Combined);
        let started = ready_scripted(
            &app,
            vec![
                (InstrumentKind::LogicAnalyzer, finishing_with(&app, vec![1])),
                (
                    InstrumentKind::Oscilloscope,
                    Script::sending(Vec::new(), SourceEnding::Panic),
                ),
            ],
            AcquisitionMode::Single,
            CaptureTarget::Combined,
        );
        app.handle_capture_ready(started);

        poll_until_finished(&mut app, CaptureTarget::Combined);

        // The instrument that crashed never reported that it stopped, so the
        // capture cannot complete; stopping it also finds the crashed worker.
        assert_eq!(
            app.last_error.as_deref(),
            Some(
                "capture event stream disconnected; \
                 failed to stop capture: a capture worker panicked"
            )
        );
    }

    #[test]
    fn a_capture_that_also_fails_to_stop_reports_both_errors() {
        let mut app = app();
        request(&mut app, CaptureTarget::Combined);
        let started = ready_scripted(
            &app,
            vec![
                (InstrumentKind::LogicAnalyzer, failing_with("boom")),
                (
                    InstrumentKind::Oscilloscope,
                    Script {
                        panic_when_stopped: true,
                        ..Script::idle()
                    },
                ),
            ],
            AcquisitionMode::Single,
            CaptureTarget::Combined,
        );
        app.handle_capture_ready(started);

        poll_until_finished(&mut app, CaptureTarget::Combined);

        assert_eq!(
            app.last_error.as_deref(),
            Some("fake:0: boom; failed to stop capture: a capture worker panicked")
        );
    }

    /// Starts a capture of one fake oscilloscope that finishes at once, with a
    /// scope frontend change waiting to be applied.
    fn start_scope_capture_awaiting_restart(
        app: &mut WaveCraftApp,
        mode: AcquisitionMode,
        script: Script,
    ) {
        request(app, CaptureTarget::Oscilloscope);
        let started = ready_scripted(
            app,
            vec![(InstrumentKind::Oscilloscope, script)],
            mode,
            CaptureTarget::Oscilloscope,
        );
        app.handle_capture_ready(started);
        app.scope_capture_restart_requested = true;
    }

    #[test]
    fn a_scope_frontend_restart_keeps_a_display_that_was_to_be_kept_until_complete() {
        for (displayed, kept) in [(true, true), (false, false)] {
            let mut app = app();
            if displayed {
                app.set_completed_capture_for_target(
                    CaptureTarget::Oscilloscope,
                    completed_capture_with_samples(&app, 42),
                );
            }
            start_scope_capture_awaiting_restart(
                &mut app,
                AcquisitionMode::Run,
                Script::sending(Vec::new(), SourceEnding::Finish),
            );

            poll_until_finished(&mut app, CaptureTarget::Oscilloscope);

            let count = app
                .completed_capture_for_target(CaptureTarget::Oscilloscope)
                .map(|capture| capture.waveforms.logic_sample_count);
            assert_eq!(
                count,
                Some(if kept { 42 } else { 0 }),
                "displayed = {displayed}"
            );
        }
    }

    #[test]
    fn a_failed_capture_is_not_restarted_for_a_scope_frontend_change() {
        let mut app = app();
        start_scope_capture_awaiting_restart(&mut app, AcquisitionMode::Run, failing_with("boom"));

        poll_until_finished(&mut app, CaptureTarget::Oscilloscope);

        // A restart would have gone through begin_capture and replaced the error
        // with the missing-DSCope message.
        assert_eq!(app.last_error.as_deref(), Some("fake:0: boom"));
    }

    #[test]
    fn a_repetitive_capture_restarts_at_once_for_a_scope_frontend_change() {
        let mut app = app();
        start_scope_capture_awaiting_restart(
            &mut app,
            AcquisitionMode::Repetitive,
            Script::sending(Vec::new(), SourceEnding::Finish),
        );

        poll_until_finished(&mut app, CaptureTarget::Oscilloscope);

        assert!(app.scheduled_repetitive_capture.is_none());
        assert_eq!(app.status, "Reconfiguring oscilloscope frontend…");
        assert!(app
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("DSCope")));
    }

    #[test]
    fn stopping_a_capture_stores_it_and_names_the_recording() {
        let directory = scratch_directory("stop-recorded");
        let path = directory.join("capture.dsv");
        let mut app = app();
        record_to(&mut app, &path);
        let script = Script::sending(vec![logic_batch(&app, vec![9, 9])], SourceEnding::Idle);
        start_logic_capture(&mut app, AcquisitionMode::Run, script);

        app.stop_capture(CaptureTarget::LogicAnalyzer);

        assert!(app.captures.is_empty());
        assert_eq!(
            app.status,
            format!("Capture stopped · recording saved to {}", path.display())
        );
        assert!(app.last_error.is_none());
        assert!(!app.capture_is_requested(CaptureTarget::LogicAnalyzer));
        assert!(app
            .completed_capture_for_target(CaptureTarget::LogicAnalyzer)
            .is_some());
        assert_eq!(
            recorded_completion(&app, &path),
            CompletionStatus::Cancelled
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn stopping_a_capture_without_a_recording_says_only_that_it_stopped() {
        let mut app = app();
        start_logic_capture(&mut app, AcquisitionMode::Run, Script::idle());

        app.stop_capture(CaptureTarget::LogicAnalyzer);

        assert_eq!(app.status, "Capture stopped");
        assert!(!app.record_capture && !app.record_decoded_data);
    }

    #[test]
    fn stopping_a_capture_that_fails_to_stop_reports_the_error_and_fails_the_recording() {
        let directory = scratch_directory("stop-failure");
        let path = directory.join("capture.dsv");
        let mut app = app();
        record_to(&mut app, &path);
        request(&mut app, CaptureTarget::LogicAnalyzer);
        let started = ready_scripted(
            &app,
            vec![(
                InstrumentKind::LogicAnalyzer,
                Script {
                    panic_when_stopped: true,
                    ..Script::idle()
                },
            )],
            AcquisitionMode::Run,
            CaptureTarget::LogicAnalyzer,
        );
        app.handle_capture_ready(started);

        app.stop_capture(CaptureTarget::LogicAnalyzer);

        assert_eq!(app.last_error.as_deref(), Some("a capture worker panicked"));
        assert_eq!(recorded_completion(&app, &path), CompletionStatus::Failed);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn stopping_a_capture_cancels_its_queued_and_scheduled_restarts() {
        let mut app = app();
        start_logic_capture(&mut app, AcquisitionMode::Run, Script::idle());
        app.queued_capture_rearms
            .push((AcquisitionMode::Run, CaptureTarget::LogicAnalyzer));
        app.queued_capture_rearms
            .push((AcquisitionMode::Run, CaptureTarget::Oscilloscope));
        app.scheduled_repetitive_capture = Some(ScheduledRepetitiveCapture {
            due: Instant::now(),
            target: CaptureTarget::LogicAnalyzer,
        });

        app.stop_capture(CaptureTarget::LogicAnalyzer);

        assert_eq!(
            app.queued_capture_rearms,
            vec![(AcquisitionMode::Run, CaptureTarget::Oscilloscope)]
        );
        assert!(app.scheduled_repetitive_capture.is_none());
    }

    #[test]
    fn stopping_keeps_the_displayed_capture_when_the_new_one_had_not_replaced_it_yet() {
        let mut app = app();
        app.set_completed_capture_for_target(
            CaptureTarget::LogicAnalyzer,
            completed_capture_with_samples(&app, 42),
        );
        start_logic_capture(&mut app, AcquisitionMode::Run, Script::idle());

        app.stop_capture(CaptureTarget::LogicAnalyzer);

        let displayed = app
            .completed_capture_for_target(CaptureTarget::LogicAnalyzer)
            .unwrap();
        assert_eq!(displayed.waveforms.logic_sample_count, 42);
    }

    #[test]
    fn stopping_a_target_that_is_not_running_changes_nothing() {
        let mut app = app();
        app.status = "unchanged".to_owned();

        app.stop_capture(CaptureTarget::LogicAnalyzer);

        assert_eq!(app.status, "unchanged");
    }
}
