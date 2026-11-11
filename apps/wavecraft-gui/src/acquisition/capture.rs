//! Capture lifecycle: acquisition modes and targets, the state of running and
//! completed captures, and starting a capture on its worker.

use crate::{
    open_radio_source, prepare_device, BufWriter, CaptureBatch, CaptureCoordinator, CaptureEvent,
    CaptureFrame, CaptureSession, CaptureSource, CompletionStatus, DeviceId, DeviceModel,
    DiscoveredDevice, DsCopeCalibrationValues, DsCopeCaptureConfig, DsCopeCaptureSource,
    DsCopeRuntimeCalibrationControl, DsCopeRuntimeTriggerControl, DsCopeTriggerSource,
    DsLogicCaptureConfig, DsLogicCaptureSource, DsLogicOperationMode, Duration, File,
    FileDecodedDataRecorder, HashSet, Instant, LinkStatus, LoadedRecording, LogicDecodeOutput,
    LogicDecodeWorker, Path, PathBuf, ProtocolAnalyzerInstance, ProtocolCaptureDecoder,
    ProtocolDecodeSnapshot, RadioAnalyzerRuntime, RadioCaptureRequest, RadioDiscoveryReport,
    RadioDspWorker, RadioRuntimeControl, RadioSingleTrigger, RecordingWriter, SpectrumProcessor,
    SpectrumWindow, WaveformStore, DSCOPE_FRAME_SEQUENCE_STRIDE, RADIO_SPECTRUM_FFT_LENGTH,
    SCOPE_HORIZONTAL_DIVISIONS, SCOPE_ROLLING_MEMORY_BUDGET_BYTES,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum CaptureTarget {
    LogicAnalyzer,
    Oscilloscope,
    RadioReceiver,
    Combined,
}

impl CaptureTarget {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "Logic Analyzer",
            Self::Oscilloscope => "Oscilloscope",
            Self::RadioReceiver => "Radio Receiver",
            Self::Combined => "Combined",
        }
    }

    pub(crate) fn is_available(
        self,
        has_logic_analyzer: bool,
        has_oscilloscope: bool,
        has_radio_receiver: bool,
    ) -> bool {
        match self {
            Self::LogicAnalyzer => has_logic_analyzer,
            Self::Oscilloscope => has_oscilloscope,
            Self::RadioReceiver => has_radio_receiver,
            Self::Combined => has_logic_analyzer && has_oscilloscope,
        }
    }

    pub(crate) fn includes_oscilloscope(self) -> bool {
        matches!(self, Self::Oscilloscope | Self::Combined)
    }

    pub(crate) fn includes_logic_analyzer(self) -> bool {
        matches!(self, Self::LogicAnalyzer | Self::Combined)
    }

    pub(crate) fn includes_radio_receiver(self) -> bool {
        matches!(self, Self::RadioReceiver | Self::Combined)
    }

    pub(crate) fn captures_selected_radio(self, has_selected_radio: bool) -> bool {
        self == Self::RadioReceiver || (self == Self::Combined && has_selected_radio)
    }

    pub(crate) fn includes(self, target: Self) -> bool {
        self == target
            || (self == Self::Combined
                && matches!(
                    target,
                    Self::LogicAnalyzer | Self::Oscilloscope | Self::RadioReceiver
                ))
    }

    pub(crate) fn overlaps(self, other: Self) -> bool {
        (self.includes_logic_analyzer() && other.includes_logic_analyzer())
            || (self.includes_oscilloscope() && other.includes_oscilloscope())
            || (self.includes_radio_receiver() && other.includes_radio_receiver())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AcquisitionMode {
    Single,
    Repetitive,
    Loop,
    Run,
}

impl AcquisitionMode {
    pub(crate) fn runs_continuously(self) -> bool {
        matches!(self, Self::Repetitive | Self::Loop | Self::Run)
    }

    pub(crate) fn logic_label(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::Repetitive => "Repetitive",
            Self::Loop => "Loop",
            Self::Run => "Run",
        }
    }

    pub(crate) fn rearms_on_completion(self) -> bool {
        matches!(self, Self::Repetitive | Self::Run)
    }
}

pub(crate) fn repetitive_interval(
    operation_mode: DsLogicOperationMode,
    configured_seconds: f32,
) -> Duration {
    if operation_mode == DsLogicOperationMode::Stream {
        return Duration::from_millis(100);
    }
    let tenths = (configured_seconds.clamp(0.1, 10.0) * 10.0).round() as u64;
    Duration::from_millis(tenths * 100)
}

pub(crate) fn uses_repetitive_interval(mode: AcquisitionMode, target: CaptureTarget) -> bool {
    mode == AcquisitionMode::Repetitive && target == CaptureTarget::LogicAnalyzer
}

pub(crate) fn acquisition_mode_is_supported(mode: AcquisitionMode, target: CaptureTarget) -> bool {
    match mode {
        AcquisitionMode::Repetitive => matches!(
            target,
            CaptureTarget::LogicAnalyzer | CaptureTarget::Oscilloscope
        ),
        AcquisitionMode::Loop => target == CaptureTarget::LogicAnalyzer,
        AcquisitionMode::Single | AcquisitionMode::Run => true,
    }
}

pub(crate) fn should_rearm_acquisition(
    mode: AcquisitionMode,
    run_requested: bool,
    completion: CompletionStatus,
    failed: bool,
) -> bool {
    mode.rearms_on_completion()
        && run_requested
        && completion == CompletionStatus::Complete
        && !failed
}

pub(crate) fn capture_recording_is_supported(mode: AcquisitionMode) -> bool {
    matches!(mode, AcquisitionMode::Single | AcquisitionMode::Run)
}

pub(crate) fn clears_completed_capture_on_start(
    mode: AcquisitionMode,
    continuing_request: bool,
) -> bool {
    match mode {
        AcquisitionMode::Single => true,
        AcquisitionMode::Run | AcquisitionMode::Loop => !continuing_request,
        AcquisitionMode::Repetitive => false,
    }
}

pub(crate) fn active_continuous_capture_accepts_recording(
    mode: AcquisitionMode,
    recording_active: bool,
) -> bool {
    matches!(mode, AcquisitionMode::Loop | AcquisitionMode::Run) && !recording_active
}

pub(crate) fn decoded_data_recording_is_supported(
    mode: AcquisitionMode,
    target: CaptureTarget,
) -> bool {
    mode == AcquisitionMode::Run && target == CaptureTarget::RadioReceiver
}

pub(crate) fn uses_rolling_scope_buffer(
    _mode: AcquisitionMode,
    _has_scope: bool,
    _trigger_source: DsCopeTriggerSource,
) -> bool {
    // Auto is continuously acquired, but each hardware frame is displayed as
    // a trigger-synchronized sweep. Appending frames into a rolling timeline
    // would make the waveform drift past the trigger marker.
    false
}

/// A scope-only Run uses the driver's continuously queued USB capture. Auto
/// frames continuously replace the synchronized display, while edge-triggered
/// frames replace it only when a trigger arrives. Combined captures stay
/// finite because their logic source must complete in step with the scope source.
pub(crate) fn uses_continuous_scope_capture(mode: AcquisitionMode, target: CaptureTarget) -> bool {
    mode == AcquisitionMode::Run && target == CaptureTarget::Oscilloscope
}

/// Update mode keeps the last completed trace visible until the next hardware
/// trigger starts delivering a replacement frame.
pub(crate) fn scope_updates_on_trigger(
    mode: AcquisitionMode,
    target: CaptureTarget,
    trigger_source: DsCopeTriggerSource,
) -> bool {
    uses_continuous_scope_capture(mode, target) && trigger_source != DsCopeTriggerSource::Auto
}

pub(crate) fn is_scope_frame_start(sequence: u64) -> bool {
    sequence % DSCOPE_FRAME_SEQUENCE_STRIDE == 0
}

pub(crate) fn scope_rolling_sample_capacity(
    config: DsCopeCaptureConfig,
    time_per_division_seconds: Option<f64>,
) -> usize {
    let fallback = config.sample_count() as usize;
    let Some(time_per_division_seconds) =
        time_per_division_seconds.filter(|seconds| seconds.is_finite() && *seconds > 0.0)
    else {
        return fallback;
    };
    let maximum = SCOPE_ROLLING_MEMORY_BUDGET_BYTES
        / config.enabled_channel_count().max(1)
        / std::mem::size_of::<f32>();
    let visible_samples = (time_per_division_seconds
        * SCOPE_HORIZONTAL_DIVISIONS as f64
        * config.sample_rate_hz() as f64)
        .ceil();
    visible_samples.clamp(1.0, maximum as f64) as usize
}

pub(crate) fn scope_trigger_controls_are_enabled(
    background_operation_pending: bool,
    capture_run_requested: bool,
    selected_scope_device: bool,
    active_capture_supports_runtime_trigger: Option<bool>,
) -> bool {
    if capture_run_requested && selected_scope_device {
        return true;
    }
    !background_operation_pending && active_capture_supports_runtime_trigger.unwrap_or(true)
}

pub(crate) fn scope_frontend_controls_are_enabled(
    background_operation_pending: bool,
    capture_run_requested: bool,
    selected_scope_device: bool,
) -> bool {
    !background_operation_pending || (capture_run_requested && selected_scope_device)
}

pub(crate) fn manual_calibration_control_is_enabled(
    background_operation_pending: bool,
    selected_scope_device: bool,
    editor_open: bool,
    active_mode: Option<AcquisitionMode>,
) -> bool {
    !background_operation_pending
        && selected_scope_device
        && !editor_open
        && active_mode.map_or(true, AcquisitionMode::runs_continuously)
}

pub(crate) fn ingest_active_capture_frame(
    active: &mut ActiveCapture,
    frame: CaptureFrame,
    radio_squelch_threshold_dbfs: f32,
) -> Result<bool, String> {
    let event = CaptureEvent::Frame(frame);
    if let Some(recorder) = active.recorder.as_mut() {
        recorder
            .write_event(&event)
            .map_err(|error| format!("failed to write recording: {error}"))?;
    }
    let CaptureEvent::Frame(frame) = event else {
        return Err("capture frame event changed variant".to_owned());
    };
    if matches!(&frame.payload, CaptureBatch::Logic(_)) {
        active.logic_decode.push(frame)?;
        return Ok(false);
    }
    if let CaptureBatch::Iq(iq) = &frame.payload {
        active
            .radio_dsp
            .push(iq.clone(), radio_squelch_threshold_dbfs)?;
    }
    let enabled_scope_channels = active
        .scope_config
        .channels()
        .map(|channel| channel.enabled());
    if active.waveforms.scope_rolling
        && is_scope_frame_start(frame.sequence)
        && matches!(&frame.payload, CaptureBatch::Scope(_))
    {
        active
            .waveforms
            .mark_scope_capture_start(enabled_scope_channels);
    }
    let stages_scope_frame = uses_continuous_scope_capture(active.mode, active.target)
        && !active.waveforms.scope_rolling
        && matches!(&frame.payload, CaptureBatch::Scope(_));
    if stages_scope_frame {
        let mut published = false;
        if is_scope_frame_start(frame.sequence) {
            if let Some(previous) = active.pending_scope_waveforms.take() {
                if previous.scope_sample_count(enabled_scope_channels) > 0 {
                    active.waveforms = previous;
                    published = true;
                }
            }
            let pending = WaveformStore {
                scope_capacity: active.scope_config.sample_count() as usize,
                ..WaveformStore::default()
            };
            active.pending_scope_waveforms = Some(pending);
        }
        let pending = active
            .pending_scope_waveforms
            .get_or_insert_with(|| WaveformStore {
                scope_capacity: active.scope_config.sample_count() as usize,
                ..WaveformStore::default()
            });
        pending.ingest_frame(&frame, enabled_scope_channels);
        if pending.scope_sample_count(enabled_scope_channels)
            >= active.scope_config.sample_count() as usize
        {
            if let Some(complete) = active.pending_scope_waveforms.take() {
                active.waveforms = complete;
                published = true;
            }
        }
        return Ok(published);
    }
    active
        .waveforms
        .ingest_frame(&frame, enabled_scope_channels);
    Ok(matches!(&frame.payload, CaptureBatch::Scope(_)))
}

pub(crate) fn ingest_logic_decode_output(active: &mut ActiveCapture, output: LogicDecodeOutput) {
    active.protocol_decode_snapshot = output.snapshot;
    if let Some(frame) = output.frame {
        active.waveforms.ingest_frame(&frame, [false; 2]);
    }
}

pub(crate) struct ActiveCapture {
    pub(crate) session: CaptureSession,
    pub(crate) mode: AcquisitionMode,
    pub(crate) target: CaptureTarget,
    pub(crate) preserve_display_until_complete: bool,
    pub(crate) scope_config: DsCopeCaptureConfig,
    pub(crate) scope_trigger_controls: Vec<DsCopeRuntimeTriggerControl>,
    pub(crate) scope_calibration_controls: Vec<DsCopeRuntimeCalibrationControl>,
    pub(crate) radio_runtime_control: Option<RadioRuntimeControl>,
    pub(crate) stopped: HashSet<DeviceId>,
    pub(crate) waveforms: WaveformStore,
    /// A non-rolling continuous OSC frame is assembled away from the visible
    /// store. This preserves the last coherent sweep across the RF-era bounded
    /// event polls, matching the effective behavior of the v0.7 GUI.
    pub(crate) pending_scope_waveforms: Option<WaveformStore>,
    pub(crate) protocol_decode_snapshot: ProtocolDecodeSnapshot,
    pub(crate) protocol_analyzers: Vec<ProtocolAnalyzerInstance>,
    pub(crate) logic_decode: LogicDecodeWorker,
    pub(crate) radio_spectrum: SpectrumProcessor,
    pub(crate) radio_dsp: RadioDspWorker,
    pub(crate) radio_trigger: Option<RadioSingleTrigger>,
    pub(crate) recorder: Option<FileRecorder>,
    pub(crate) recording_path: Option<PathBuf>,
    pub(crate) decoded_data_recorder: Option<FileDecodedDataRecorder>,
    pub(crate) decoded_data_path: Option<PathBuf>,
    pub(crate) has_logic: bool,
    pub(crate) has_scope: bool,
    pub(crate) has_radio: bool,
}

pub(crate) fn finish_logic_decode(
    active: &mut ActiveCapture,
) -> (ProtocolCaptureDecoder, Option<String>) {
    match active.logic_decode.finish() {
        Ok((decoder, outputs)) => {
            for output in outputs {
                ingest_logic_decode_output(active, output);
            }
            active.protocol_decode_snapshot = decoder.snapshot();
            (decoder, None)
        }
        Err(error) => {
            let mut decoder = ProtocolCaptureDecoder::new(&active.protocol_analyzers);
            decoder.retain_history_from(&active.protocol_decode_snapshot);
            decoder.finish();
            (decoder, Some(error))
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScheduledRepetitiveCapture {
    pub(crate) due: Instant,
    pub(crate) target: CaptureTarget,
}

pub(crate) struct CompletedCapture {
    pub(crate) waveforms: WaveformStore,
    pub(crate) protocol_decode: ProtocolCaptureDecoder,
    pub(crate) has_logic: bool,
    pub(crate) has_scope: bool,
    pub(crate) has_radio: bool,
}

pub(crate) type FileRecorder = RecordingWriter<BufWriter<File>>;

/// A capture that has started on its worker thread and is waiting to be adopted
/// by the UI thread (see `WaveCraftApp::handle_capture_ready`).
pub(crate) struct CaptureReady {
    pub(crate) session: CaptureSession,
    pub(crate) mode: AcquisitionMode,
    pub(crate) target: CaptureTarget,
    pub(crate) retain_protocol_history: bool,
    pub(crate) logic_config: DsLogicCaptureConfig,
    pub(crate) scope_config: DsCopeCaptureConfig,
    pub(crate) scope_trigger_controls: Vec<DsCopeRuntimeTriggerControl>,
    pub(crate) scope_calibration_controls: Vec<DsCopeRuntimeCalibrationControl>,
    pub(crate) radio_runtime_control: Option<RadioRuntimeControl>,
    pub(crate) radio_spectrum: SpectrumProcessor,
    pub(crate) radio_dsp: RadioDspWorker,
}

pub(crate) enum WorkerSuccess {
    Devices {
        usb: Vec<DiscoveredDevice>,
        radio: RadioDiscoveryReport,
    },
    Capture(Box<CaptureReady>),
    ScopeConfigured,
    ScopeAutoCalibrated(DsCopeCalibrationValues),
    ScopeAutoCalibrationSaved,
    ScopeCalibrationLoaded(DsCopeCalibrationValues),
    ScopeCalibrationSaved,
    RecordingLoaded {
        path: PathBuf,
        recording: Box<LoadedRecording>,
    },
    SaleaeExported(PathBuf),
}

pub(crate) type WorkerResult = Result<WorkerSuccess, String>;

pub(crate) struct StartedCapture {
    pub(crate) session: CaptureSession,
    pub(crate) scope_trigger_controls: Vec<DsCopeRuntimeTriggerControl>,
    pub(crate) scope_calibration_controls: Vec<DsCopeRuntimeCalibrationControl>,
    pub(crate) radio_runtime_control: Option<RadioRuntimeControl>,
    pub(crate) radio_spectrum: SpectrumProcessor,
    pub(crate) radio_dsp: RadioDspWorker,
}

pub(crate) fn start_capture(
    devices: Vec<DiscoveredDevice>,
    radio_request: Option<RadioCaptureRequest>,
    resource_directory: &Path,
    logic_config: DsLogicCaptureConfig,
    scope_config: DsCopeCaptureConfig,
    calibration_values: Option<DsCopeCalibrationValues>,
    continuous_scope_capture: bool,
) -> Result<StartedCapture, String> {
    let iq_correction_enabled = radio_request
        .as_ref()
        .is_some_and(|request| request.iq_correction_enabled);
    let radio_analyzers = radio_request
        .as_ref()
        .map(|request| {
            request
                .analyzer_profiles
                .iter()
                .cloned()
                .zip(request.analyzer_channel_names.iter())
                .zip(request.decoder_configs.iter())
                .map(|((profile, name), decoder_config)| {
                    RadioAnalyzerRuntime::new_named(profile, decoder_config.clone(), name.clone())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let mut sources = Vec::<Box<dyn CaptureSource>>::with_capacity(
        devices.len() + usize::from(radio_request.is_some()),
    );
    let mut scope_trigger_controls = Vec::new();
    let mut scope_calibration_controls = Vec::new();
    let mut radio_runtime_control = None;
    for device in devices {
        if device.link_status() == LinkStatus::Unsupported {
            return Err(format!(
                "{} is connected at an unsupported speed",
                device.profile.display_name
            ));
        }
        let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
        prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
        match device.profile.model {
            model if model.is_logic_analyzer() && model.supports_capture() => {
                sources.push(Box::new(
                    DsLogicCaptureSource::new(device, logic_config)
                        .map_err(|error| error.to_string())?,
                ))
            }
            DeviceModel::DsCopeU3P100 => {
                let (source, control) =
                    DsCopeCaptureSource::with_runtime_trigger_control(device, scope_config)
                        .map_err(|error| error.to_string())?;
                let source = match calibration_values {
                    Some(values) => source.with_calibration_values(values),
                    None => source,
                };
                let source = if continuous_scope_capture {
                    source.with_continuous_capture()
                } else {
                    source
                };
                let (source, calibration_control) = source.with_runtime_calibration_control();
                sources.push(Box::new(source));
                scope_trigger_controls.push(control);
                scope_calibration_controls.push(calibration_control);
            }
            _ => {
                return Err(format!(
                    "{} supports discovery, probing, and FPGA preparation, but its acquisition driver is not implemented",
                    device.profile.display_name
                ));
            }
        }
    }
    if let Some(request) = radio_request {
        let (source, control) =
            open_radio_source(&request.device, request.settings, request.provider_settings)?;
        radio_runtime_control = control;
        sources.push(source);
    }
    let radio_spectrum = SpectrumProcessor::new(RADIO_SPECTRUM_FFT_LENGTH, SpectrumWindow::Hann)?
        .with_iq_correction(iq_correction_enabled);
    let radio_dsp_spectrum =
        SpectrumProcessor::new(RADIO_SPECTRUM_FFT_LENGTH, SpectrumWindow::Hann)?
            .with_iq_correction(iq_correction_enabled);
    let radio_dsp = RadioDspWorker::start(radio_dsp_spectrum, radio_analyzers);
    let session = CaptureCoordinator::start(sources).map_err(|error| error.to_string())?;
    Ok(StartedCapture {
        session,
        scope_trigger_controls,
        scope_calibration_controls,
        radio_runtime_control,
        radio_spectrum,
        radio_dsp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    #[test]
    fn acquisition_modes_rearm_only_successful_continuous_runs() {
        for mode in [AcquisitionMode::Repetitive, AcquisitionMode::Run] {
            assert!(should_rearm_acquisition(
                mode,
                true,
                CompletionStatus::Complete,
                false,
            ));
        }
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Loop,
            true,
            CompletionStatus::Complete,
            false,
        ));
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Run,
            false,
            CompletionStatus::Complete,
            false,
        ));
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Run,
            true,
            CompletionStatus::Failed,
            true,
        ));
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Single,
            true,
            CompletionStatus::Complete,
            false,
        ));
    }

    #[test]
    fn capture_recording_supports_single_and_run_modes() {
        assert!(capture_recording_is_supported(AcquisitionMode::Single));
        assert!(capture_recording_is_supported(AcquisitionMode::Run));
        assert!(!capture_recording_is_supported(AcquisitionMode::Repetitive));
        assert!(!capture_recording_is_supported(AcquisitionMode::Loop));
    }

    #[test]
    fn new_single_run_and_loop_captures_release_the_previous_capture() {
        for mode in [
            AcquisitionMode::Single,
            AcquisitionMode::Run,
            AcquisitionMode::Loop,
        ] {
            assert!(clears_completed_capture_on_start(mode, false));
        }
        assert!(!clears_completed_capture_on_start(
            AcquisitionMode::Repetitive,
            false,
        ));

        assert!(clears_completed_capture_on_start(
            AcquisitionMode::Single,
            true,
        ));
        assert!(!clears_completed_capture_on_start(
            AcquisitionMode::Run,
            true,
        ));
        assert!(!clears_completed_capture_on_start(
            AcquisitionMode::Loop,
            true,
        ));
        assert!(!clears_completed_capture_on_start(
            AcquisitionMode::Repetitive,
            true,
        ));
    }

    #[test]
    fn active_continuous_capture_accepts_a_new_recording() {
        assert!(active_continuous_capture_accepts_recording(
            AcquisitionMode::Run,
            false
        ));
        assert!(active_continuous_capture_accepts_recording(
            AcquisitionMode::Loop,
            false
        ));
        assert!(!active_continuous_capture_accepts_recording(
            AcquisitionMode::Run,
            true
        ));
        assert!(!active_continuous_capture_accepts_recording(
            AcquisitionMode::Single,
            false
        ));
        assert!(!active_continuous_capture_accepts_recording(
            AcquisitionMode::Repetitive,
            false
        ));
    }

    #[test]
    fn decoded_data_recording_requires_rf_run_mode() {
        assert!(decoded_data_recording_is_supported(
            AcquisitionMode::Run,
            CaptureTarget::RadioReceiver,
        ));
        assert!(!decoded_data_recording_is_supported(
            AcquisitionMode::Single,
            CaptureTarget::RadioReceiver,
        ));
        assert!(!decoded_data_recording_is_supported(
            AcquisitionMode::Run,
            CaptureTarget::LogicAnalyzer,
        ));
    }

    #[test]
    fn continuous_scope_sequences_identify_each_hardware_frame() {
        assert!(is_scope_frame_start(0));
        assert!(!is_scope_frame_start(1));
        assert!(is_scope_frame_start(DSCOPE_FRAME_SEQUENCE_STRIDE));
        assert!(!is_scope_frame_start(DSCOPE_FRAME_SEQUENCE_STRIDE + 1));
    }

    #[test]
    fn repetitive_interval_matches_original_stream_and_buffer_behavior() {
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Stream, 7.5),
            Duration::from_millis(100)
        );
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Buffer, 1.5),
            Duration::from_millis(1_500)
        );
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Buffer, 0.0),
            Duration::from_millis(100)
        );
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Buffer, 20.0),
            Duration::from_secs(10)
        );
        assert!(uses_repetitive_interval(
            AcquisitionMode::Repetitive,
            CaptureTarget::LogicAnalyzer,
        ));
        assert!(!uses_repetitive_interval(
            AcquisitionMode::Repetitive,
            CaptureTarget::Oscilloscope,
        ));
    }

    #[test]
    fn repetitive_mode_is_logic_repetitive_or_scope_re_arm() {
        assert!(acquisition_mode_is_supported(
            AcquisitionMode::Repetitive,
            CaptureTarget::LogicAnalyzer,
        ));
        assert!(acquisition_mode_is_supported(
            AcquisitionMode::Repetitive,
            CaptureTarget::Oscilloscope,
        ));
        assert!(!acquisition_mode_is_supported(
            AcquisitionMode::Repetitive,
            CaptureTarget::Combined,
        ));
        assert!(!acquisition_mode_is_supported(
            AcquisitionMode::Loop,
            CaptureTarget::Oscilloscope,
        ));
    }

    #[test]
    fn trigger_controls_stay_enabled_while_continuous_capture_rearms() {
        assert!(scope_trigger_controls_are_enabled(true, true, true, None));
        assert!(scope_trigger_controls_are_enabled(
            false,
            true,
            true,
            Some(true)
        ));
        assert!(!scope_trigger_controls_are_enabled(true, false, true, None));
        assert!(!scope_trigger_controls_are_enabled(
            false,
            true,
            false,
            Some(false)
        ));
        assert!(scope_frontend_controls_are_enabled(true, true, true));
        assert!(!scope_frontend_controls_are_enabled(true, false, true));
        assert!(scope_frontend_controls_are_enabled(false, false, false));
    }

    #[test]
    fn manual_calibration_stays_available_during_continuous_capture() {
        assert!(manual_calibration_control_is_enabled(
            false,
            true,
            false,
            Some(AcquisitionMode::Run)
        ));
        assert!(manual_calibration_control_is_enabled(
            false, true, false, None
        ));
        assert!(!manual_calibration_control_is_enabled(
            true,
            true,
            false,
            Some(AcquisitionMode::Run)
        ));
        assert!(!manual_calibration_control_is_enabled(
            false,
            true,
            false,
            Some(AcquisitionMode::Single)
        ));
        assert!(!manual_calibration_control_is_enabled(
            false,
            true,
            true,
            Some(AcquisitionMode::Run)
        ));
    }

    #[test]
    fn combined_capture_requires_both_instrument_types() {
        assert!(CaptureTarget::LogicAnalyzer.is_available(true, false, false));
        assert!(CaptureTarget::Oscilloscope.is_available(false, true, false));
        assert!(CaptureTarget::RadioReceiver.is_available(false, false, true));
        assert!(!CaptureTarget::Combined.is_available(true, false, false));
        assert!(!CaptureTarget::Combined.is_available(false, true, false));
        assert!(CaptureTarget::Combined.is_available(true, true, false));
        assert!(CaptureTarget::Combined.includes_radio_receiver());
        assert!(CaptureTarget::Combined.captures_selected_radio(true));
        assert!(!CaptureTarget::Combined.captures_selected_radio(false));
        assert!(CaptureTarget::RadioReceiver.captures_selected_radio(false));
        assert!(CaptureTarget::Combined.includes(CaptureTarget::LogicAnalyzer));
        assert!(CaptureTarget::Combined.includes(CaptureTarget::Oscilloscope));
        assert!(CaptureTarget::Combined.includes(CaptureTarget::RadioReceiver));
        assert!(!CaptureTarget::LogicAnalyzer.includes(CaptureTarget::RadioReceiver));
    }

    #[test]
    fn independent_instrument_targets_can_capture_concurrently() {
        assert!(!CaptureTarget::LogicAnalyzer.overlaps(CaptureTarget::Oscilloscope));
        assert!(!CaptureTarget::LogicAnalyzer.overlaps(CaptureTarget::RadioReceiver));
        assert!(!CaptureTarget::Oscilloscope.overlaps(CaptureTarget::RadioReceiver));
        assert!(CaptureTarget::LogicAnalyzer.overlaps(CaptureTarget::LogicAnalyzer));
        assert!(CaptureTarget::Combined.overlaps(CaptureTarget::LogicAnalyzer));
        assert!(CaptureTarget::Combined.overlaps(CaptureTarget::Oscilloscope));
        assert!(CaptureTarget::Combined.overlaps(CaptureTarget::RadioReceiver));
    }
}
