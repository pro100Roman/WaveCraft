//! DSCope calibration and idle configuration requests, and polling their progress.

use crate::{
    apply_dscope_configuration, auto_calibrate_dscope_with_progress, mpsc, prepare_device,
    read_dscope_calibration, thread, write_dscope_calibration, AcquisitionMode, CalibrationDisplay,
    CalibrationProgressModal, CaptureBatch, CaptureTarget, DsCopeCalibrationValues, PathBuf,
    ScopeBatch, TryRecvError, WaveformStore, WaveformTab, WorkerSuccess,
    SCOPE_HORIZONTAL_DIVISIONS,
};

use crate::WaveCraftApp;

impl WaveCraftApp {
    pub(crate) fn begin_scope_configuration(&mut self) {
        if self.pending.is_some() || self.capture_target_is_active(CaptureTarget::Oscilloscope) {
            return;
        }
        self.scope_configuration_due = None;
        let Some(device) = self.selected_scope_device_ref().cloned() else {
            self.status = "DSCope settings will be applied when capture starts".to_owned();
            return;
        };
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.status = format!(
            "Applying DSCope configuration to {}…",
            device.location.stable_id()
        );
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeConfigured)
            })();
            let _ = sender.send(result);
        });
    }

    pub(crate) fn begin_auto_calibration(&mut self) {
        if self.pending.is_some() || self.capture_target_is_active(CaptureTarget::Oscilloscope) {
            return;
        }
        let Some(device) = self.selected_scope_device_for_operation() else {
            return;
        };
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        let (progress_sender, progress_receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.calibration_progress = Some(progress_receiver);
        self.calibration_display = None;
        self.calibration_active = true;
        self.calibration_modal = Some(CalibrationProgressModal {
            progress: 0.0,
            phase: "Preparing oscilloscope…".to_owned(),
            completed: false,
            values: None,
        });
        self.select_waveform_tab(WaveformTab::Oscilloscope);
        self.status = "Auto-calibrating DSCope offsets and ADC branches…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                let values = auto_calibrate_dscope_with_progress(&device, |frame| {
                    let _ = progress_sender.send(frame);
                })
                .map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeAutoCalibrated(values))
            })();
            let _ = sender.send(result);
        });
    }

    pub(crate) fn begin_save_auto_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.pending.is_some() || self.capture_target_is_active(CaptureTarget::Oscilloscope) {
            return;
        }
        let Some(device) = self.selected_scope_device_for_operation() else {
            return;
        };
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        if let Some(modal) = self.calibration_modal.as_mut() {
            modal.progress = 0.0;
            modal.phase = "Saving calibration to device NVM…".to_owned();
            modal.completed = false;
            modal.values = None;
        }
        self.status = "Saving DSCope calibration to device NVM…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                write_dscope_calibration(&device, values).map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeAutoCalibrationSaved)
            })();
            let _ = sender.send(result);
        });
    }

    pub(crate) fn begin_manual_calibration(&mut self) {
        if self.pending.is_some() || self.capture_target_is_active(CaptureTarget::Oscilloscope) {
            return;
        }
        let Some(device) = self.selected_scope_device_for_operation() else {
            return;
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.status = "Loading DSCope calibration values…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                let values = read_dscope_calibration(&device).map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeCalibrationLoaded(values))
            })();
            let _ = sender.send(result);
        });
    }

    pub(crate) fn request_manual_calibration(&mut self) {
        if self.pending.is_some() || self.manual_calibration.is_some() {
            return;
        }
        if let Some(active) = self.capture_for_target(CaptureTarget::Oscilloscope) {
            if active.mode != AcquisitionMode::Run {
                return;
            }
            active.session.request_stop();
            self.manual_calibration_load_requested = true;
            self.scope_capture_restart_requested = false;
            self.status = "Loading calibration while continuous acquisition pauses…".to_owned();
            self.last_error = None;
        } else {
            self.begin_manual_calibration();
        }
    }

    pub(crate) fn begin_save_manual_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.pending.is_some() || self.capture_target_is_active(CaptureTarget::Oscilloscope) {
            return;
        }
        let Some(device) = self.selected_scope_device_for_operation() else {
            return;
        };
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.status = "Saving DSCope calibration values to device NVM…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                write_dscope_calibration(&device, values).map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeCalibrationSaved)
            })();
            let _ = sender.send(result);
        });
    }

    pub(crate) fn request_save_manual_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.pending.is_some() {
            return;
        }
        if let Some(active) = self.capture_for_target(CaptureTarget::Oscilloscope) {
            active.session.request_stop();
            self.manual_calibration_save_requested = Some(values);
            self.scope_capture_restart_requested = false;
            self.status = "Pausing acquisition to save calibration…".to_owned();
            self.last_error = None;
        } else {
            self.begin_save_manual_calibration(values);
        }
    }

    pub(crate) fn poll_calibration_progress(&mut self) {
        let mut latest = None;
        let mut disconnected = false;
        if let Some(receiver) = self.calibration_progress.as_ref() {
            loop {
                match receiver.try_recv() {
                    Ok(frame) => latest = Some(frame),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.calibration_progress = None;
        }
        let Some(frame) = latest else {
            return;
        };

        let config = frame.config();
        if let Some(modal) = self.calibration_modal.as_mut() {
            modal.progress = modal.progress.max(frame.progress());
            modal.phase = frame.phase().to_owned();
        }
        let enabled_channels = config.channels().map(|channel| channel.enabled());
        let mut waveforms = WaveformStore {
            scope_capacity: config.sample_count() as usize,
            ..WaveformStore::default()
        };
        waveforms.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: config.sample_rate_hz(),
                trigger_sample_index: None,
                channels: frame.channels().to_vec(),
            }),
            enabled_channels,
        );
        self.calibration_display = Some(CalibrationDisplay {
            waveforms,
            scope_config: config,
        });
        self.scope_viewport.reset();
        self.scope_time_per_division_seconds = Some(
            config.sample_count() as f64
                / config.sample_rate_hz() as f64
                / SCOPE_HORIZONTAL_DIVISIONS as f64,
        );
        let volts_per_div_mv = config.channels()[0].volts_per_div_mv();
        self.status = format!(
            "Auto-calibrating DSCope · {volts_per_div_mv} mV/div · {}",
            if config.enabled_channel_count() == 1 {
                "interleaved ADC"
            } else {
                "dual-channel ADC"
            }
        );
    }
}
