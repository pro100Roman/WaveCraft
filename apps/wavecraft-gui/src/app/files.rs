//! Recording files and snapshots: opening, exporting, attaching to a running capture, and finishing a pending snapshot.

use crate::{
    create_decoded_data_recorder, create_saleae_export_file, crop_snapshot_image,
    decoded_data_open_request, egui, load_gui_decoded_data, load_gui_recording, mpsc,
    saleae_capture_from_display, save_snapshot_image, start_gui_recording, thread, write_saleae,
    AcquisitionMode, BufWriter, CaptureTarget, GuiRecordingSettings, PathBuf, WorkerSuccess,
};

use crate::WaveCraftApp;

impl WaveCraftApp {
    pub(crate) fn finish_pending_snapshot(&mut self, context: &egui::Context) {
        if self.pending_snapshot.is_none() {
            return;
        }
        let screenshot = context.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(screenshot) = screenshot else {
            return;
        };
        let Some(pending) = self.pending_snapshot.take() else {
            return;
        };
        let snapshot = match crop_snapshot_image(
            screenshot.as_ref(),
            pending.plot_rect,
            pending.pixels_per_point,
        ) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.status = "Snapshot failed".to_owned();
                self.last_error = Some(format!("Snapshot failed: {error}"));
                return;
            }
        };

        let Some(directory) = rfd::FileDialog::new()
            .set_title("Select snapshot directory")
            .pick_folder()
        else {
            self.status = "Snapshot canceled".to_owned();
            return;
        };
        match save_snapshot_image(&directory, &snapshot) {
            Ok(path) => {
                self.status = format!("Snapshot saved to {}", path.display());
                self.last_error = None;
            }
            Err(error) => {
                self.status = "Snapshot failed".to_owned();
                self.last_error = Some(format!("Snapshot failed: {error}"));
            }
        }
    }

    pub(crate) fn begin_open_recording(&mut self, path: PathBuf) {
        if self.pending.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.requested_capture_target = None;
        self.scope_configuration_due = None;
        self.status = format!("Opening capture {}…", path.display());
        self.last_error = None;
        let protocol_analyzers = self.protocol_analyzer.analyzers.clone();
        thread::spawn(move || {
            let result = load_gui_recording(&path, &protocol_analyzers).map(|recording| {
                WorkerSuccess::RecordingLoaded {
                    path,
                    recording: Box::new(recording),
                }
            });
            let _ = sender.send(result);
        });
    }

    pub(crate) fn begin_open_decoded_data(&mut self, path: PathBuf) {
        if self.pending.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.requested_capture_target = None;
        self.scope_configuration_due = None;
        self.status = format!("Opening decoded RF data {}…", path.display());
        self.last_error = None;
        thread::spawn(move || {
            let result =
                load_gui_decoded_data(&path).map(|recording| WorkerSuccess::RecordingLoaded {
                    path,
                    recording: Box::new(recording),
                });
            let _ = sender.send(result);
        });
    }

    pub(crate) fn open_selected_recording(&mut self, path: PathBuf) {
        match decoded_data_open_request(&path, self.active_waveform_tab) {
            Ok(true) => self.begin_open_decoded_data(path),
            Ok(false) => self.begin_open_recording(path),
            Err(error) => self.set_error(error),
        }
    }

    pub(crate) fn begin_export_saleae(&mut self, path: PathBuf) {
        if self.pending.is_some() {
            return;
        }
        let Some(completed) = self.displayed_completed_capture() else {
            self.set_error("There is no completed capture to export".to_owned());
            return;
        };
        let capture = match saleae_capture_from_display(
            completed,
            &self.logic_settings,
            &self.logic_channel_names,
            &self.scope_channel_names,
        ) {
            Ok(capture) => capture,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.status = format!("Exporting Saleae capture {}…", path.display());
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let file = create_saleae_export_file(&path)?;
                write_saleae(BufWriter::new(file), &capture).map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::SaleaeExported(path))
            })();
            let _ = sender.send(result);
        });
    }

    pub(crate) fn attach_run_capture_recording(&mut self, target: CaptureTarget, path: PathBuf) {
        let logic_config = match self.logic_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        let recorder = {
            let Some(active) = self.capture_for_target(target) else {
                self.set_error(format!("{} Run capture is not active", target.label()));
                return;
            };
            if !matches!(active.mode, AcquisitionMode::Loop | AcquisitionMode::Run) {
                self.set_error(
                    "Recording can only attach to an active Run or Loop acquisition".to_owned(),
                );
                return;
            }
            if active.recorder.is_some() {
                self.set_error("Capture recording is already active".to_owned());
                return;
            }
            match start_gui_recording(
                &path,
                active.session.descriptors(),
                GuiRecordingSettings {
                    logic_config,
                    logic_settings: self.logic_settings,
                    scope_config: active.scope_config,
                    scope_settings: self.scope_settings,
                    radio_settings: &self.radio_settings,
                    logic_channel_names: &self.logic_channel_names,
                    scope_channel_names: &self.scope_channel_names,
                },
            ) {
                Ok(recorder) => recorder,
                Err(error) => {
                    self.set_error(error);
                    return;
                }
            }
        };
        let Some(active) = self.capture_for_target_mut(target) else {
            self.set_error("Run capture ended before recording could start".to_owned());
            return;
        };
        active.recorder = Some(recorder);
        active.recording_path = Some(path.clone());
        self.status = format!(
            "{} continuous acquisition · recording to {}",
            target.label(),
            path.display()
        );
        self.last_error = None;
    }

    pub(crate) fn attach_run_decoded_data_recording(&mut self, path: PathBuf) {
        let last_raw_message_sequence = {
            let Some(active) = self.capture_for_target(CaptureTarget::RadioReceiver) else {
                self.set_error("RF Run capture is not active".to_owned());
                return;
            };
            if active.mode != AcquisitionMode::Run || !active.has_radio {
                self.set_error("Decoded data recording can only attach to RF Run mode".to_owned());
                return;
            }
            if active.decoded_data_recorder.is_some() {
                self.set_error("Decoded data recording is already active".to_owned());
                return;
            }
            active
                .waveforms
                .radio_analysis
                .raw_messages()
                .back()
                .map_or(0, |message| message.sequence())
        };
        let mut recorder = match create_decoded_data_recorder(&path, &self.radio_settings) {
            Ok(recorder) => recorder,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        recorder.skip_raw_messages_through(last_raw_message_sequence);
        let Some(active) = self.capture_for_target_mut(CaptureTarget::RadioReceiver) else {
            self.set_error("RF Run capture ended before recording could start".to_owned());
            return;
        };
        active.decoded_data_recorder = Some(recorder);
        active.decoded_data_path = Some(path.clone());
        self.status = format!("RF Run · recording decoded data to {}", path.display());
        self.last_error = None;
    }
}
