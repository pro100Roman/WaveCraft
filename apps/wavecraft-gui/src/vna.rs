//! GUI-side LiteVNA state and asynchronous sweep orchestration.

use crate::{
    mpsc, thread, LiteVnaDevice, OnePortCalibration, Receiver, SavedCaptureSettingsKind, VnaSweep,
    WaveCraftApp,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use wavecraft_vna::{LiteVnaSession, SweepConfig};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VnaSettings {
    pub(crate) start_mhz: f64,
    pub(crate) stop_mhz: f64,
    pub(crate) points: u16,
    pub(crate) averaging: u8,
}

impl Default for VnaSettings {
    fn default() -> Self {
        Self {
            start_mhz: 1.0,
            stop_mhz: 100.0,
            points: 201,
            averaging: 1,
        }
    }
}

impl VnaSettings {
    pub(crate) fn sweep_config(self) -> Result<SweepConfig, String> {
        if !self.start_mhz.is_finite() || !self.stop_mhz.is_finite() {
            return Err("VNA frequencies must be finite numbers".to_owned());
        }
        let start_hz = (self.start_mhz * 1_000_000.0).round();
        let stop_hz = (self.stop_mhz * 1_000_000.0).round();
        if !(1.0..=u64::MAX as f64).contains(&start_hz)
            || !(1.0..=u64::MAX as f64).contains(&stop_hz)
        {
            return Err("VNA frequencies are outside the supported range".to_owned());
        }
        let config = SweepConfig {
            start_hz: start_hz as u64,
            stop_hz: stop_hz as u64,
            points: self.points,
            averaging: self.averaging,
        };
        config.validate().map_err(|error| error.to_string())?;
        Ok(config)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CalibrationStandard {
    Open,
    Short,
    Load,
}

impl CalibrationStandard {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::Short => "SHORT",
            Self::Load => "LOAD",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VnaSweepPurpose {
    Measurement,
    Calibration(CalibrationStandard),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct VnaCalibrationCaptures {
    open: Option<VnaSweep>,
    short: Option<VnaSweep>,
    load: Option<VnaSweep>,
}

impl VnaCalibrationCaptures {
    pub(crate) fn has(&self, standard: CalibrationStandard) -> bool {
        match standard {
            CalibrationStandard::Open => self.open.is_some(),
            CalibrationStandard::Short => self.short.is_some(),
            CalibrationStandard::Load => self.load.is_some(),
        }
    }

    fn set(&mut self, standard: CalibrationStandard, sweep: VnaSweep) -> bool {
        let reset = self
            .open
            .as_ref()
            .or(self.short.as_ref())
            .or(self.load.as_ref())
            .is_some_and(|existing| !same_frequency_grid(existing, &sweep));
        if reset {
            *self = Self::default();
        }
        match standard {
            CalibrationStandard::Open => self.open = Some(sweep),
            CalibrationStandard::Short => self.short = Some(sweep),
            CalibrationStandard::Load => self.load = Some(sweep),
        }
        reset
    }

    fn calibration(&self) -> Result<Option<OnePortCalibration>, String> {
        let (Some(open), Some(short), Some(load)) = (&self.open, &self.short, &self.load) else {
            return Ok(None);
        };
        OnePortCalibration::from_sweeps(open, short, load)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

fn same_frequency_grid(left: &VnaSweep, right: &VnaSweep) -> bool {
    left.points.len() == right.points.len()
        && left
            .points
            .iter()
            .zip(&right.points)
            .all(|(left, right)| left.frequency_hz == right.frequency_hz)
}

pub(crate) type VnaWorkerResult = Result<VnaSweep, String>;
pub(crate) type VnaWorkerReceiver = Receiver<VnaWorkerResult>;
pub(crate) type VnaSharedSession = Arc<Mutex<LiteVnaSession>>;
pub(crate) type VnaConnectionResult = Result<Option<VnaSharedSession>, String>;
pub(crate) type VnaConnectionReceiver = Receiver<VnaConnectionResult>;

impl WaveCraftApp {
    pub(crate) fn reconcile_vna_selection(&mut self) {
        let selected_available = self.selected_vna_port.as_ref().is_some_and(|selected| {
            self.vna_devices
                .iter()
                .any(|device| device.port_name == *selected)
        });
        if !selected_available {
            self.selected_vna_port = self
                .vna_devices
                .first()
                .map(|device| device.port_name.clone());
        }
    }

    pub(crate) fn selected_vna(&self) -> Option<&LiteVnaDevice> {
        let selected = self.selected_vna_port.as_deref()?;
        self.vna_devices
            .iter()
            .find(|device| device.port_name == selected)
    }

    pub(crate) fn begin_vna_sweep(&mut self) {
        self.begin_vna_acquisition(VnaSweepPurpose::Measurement);
    }

    pub(crate) fn toggle_vna_connection(&mut self) {
        if self.is_busy() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.vna_connection_pending = Some(receiver);
        if let Some(session) = self.vna_session.take() {
            self.vna_connection_closing = true;
            self.status = "Closing LiteVNA port…".to_owned();
            self.last_error = None;
            thread::spawn(move || {
                drop(session);
                let _ = sender.send(Ok(None));
            });
            return;
        }
        let Some(port_name) = self.selected_vna_port.clone() else {
            self.vna_connection_pending = None;
            self.vna_connection_closing = false;
            self.set_error("Select a connected LiteVNA before opening the port".to_owned());
            return;
        };
        if let Err(error) = self.commit_vna_frequency_inputs() {
            self.vna_connection_pending = None;
            self.vna_connection_closing = false;
            self.set_error(error);
            return;
        }
        let config = match self.vna_settings.sweep_config() {
            Ok(config) => config,
            Err(error) => {
                self.vna_connection_pending = None;
                self.vna_connection_closing = false;
                self.set_error(error);
                return;
            }
        };
        self.vna_connection_closing = false;
        self.status = format!("Opening and configuring LiteVNA port {port_name}…");
        self.last_error = None;
        thread::spawn(move || {
            let result = LiteVnaSession::open(&port_name)
                .and_then(|mut session| {
                    session.enter_usb_sweep_mode(config)?;
                    Ok(session)
                })
                .map(|session| Some(Arc::new(Mutex::new(session))))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    pub(crate) fn poll_vna_connection(&mut self) {
        let Some(receiver) = self.vna_connection_pending.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("LiteVNA connection worker ended unexpectedly".to_owned())
            }
        };
        self.vna_connection_pending = None;
        self.vna_connection_closing = false;
        match result {
            Ok(Some(session)) => {
                self.vna_session = Some(session);
                self.status = "LiteVNA port open; USB mode active".to_owned();
                self.last_error = None;
            }
            Ok(None) => {
                self.status = "LiteVNA port closed; device returned to local mode".to_owned();
                self.last_error = None;
            }
            Err(error) => self.set_error(error),
        }
    }

    pub(crate) fn toggle_vna_run(&mut self) {
        if self.vna_run_active {
            if let Some(cancel) = &self.vna_run_cancel {
                cancel.store(true, Ordering::Release);
            }
            self.vna_run_stop_requested = true;
            self.status = "Stopping VNA run after the current sweep…".to_owned();
            return;
        }
        if self.pending.is_some()
            || self.vna_connection_pending.is_some()
            || self.vna_pending.is_some()
            || !self.captures.is_empty()
        {
            return;
        }
        let Some(session) = self.vna_session.clone() else {
            self.set_error("Open the selected LiteVNA port before starting Run".to_owned());
            return;
        };
        if let Err(error) = self.commit_vna_frequency_inputs() {
            self.set_error(error);
            return;
        }
        let config = match self.vna_settings.sweep_config() {
            Ok(config) => config,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        self.vna_run_active = true;
        self.vna_run_stop_requested = false;
        self.vna_run_cancel = Some(cancel);
        self.vna_run_events = Some(receiver);
        self.status = format!(
            "Running LiteVNA from {:.3} to {:.3} MHz…",
            self.vna_settings.start_mhz, self.vna_settings.stop_mhz
        );
        self.last_error = None;
        thread::spawn(move || {
            let mut session = match session.lock() {
                Ok(session) => session,
                Err(_) => {
                    let _ = sender.send(Err("LiteVNA session lock was poisoned".to_owned()));
                    return;
                }
            };
            while !worker_cancel.load(Ordering::Acquire) {
                let result = session
                    .acquire_sweep(config)
                    .map_err(|error| error.to_string());
                let failed = result.is_err();
                if sender.send(result).is_err() || failed {
                    break;
                }
            }
        });
    }

    pub(crate) fn begin_vna_calibration_capture(&mut self, standard: CalibrationStandard) {
        self.begin_vna_acquisition(VnaSweepPurpose::Calibration(standard));
    }

    fn begin_vna_acquisition(&mut self, purpose: VnaSweepPurpose) {
        if self.pending.is_some()
            || self.vna_connection_pending.is_some()
            || self.vna_pending.is_some()
            || self.vna_run_active
            || !self.captures.is_empty()
        {
            return;
        }
        let Some(session) = self.vna_session.clone() else {
            self.set_error("Open the selected LiteVNA port before starting a sweep".to_owned());
            return;
        };
        if let Err(error) = self.commit_vna_frequency_inputs() {
            self.set_error(error);
            return;
        }
        let config = match self.vna_settings.sweep_config() {
            Ok(config) => config,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        let (sender, receiver) = mpsc::channel();
        self.vna_pending = Some(receiver);
        self.vna_pending_purpose = Some(purpose);
        self.status = match purpose {
            VnaSweepPurpose::Measurement => format!(
                "Sweeping LiteVNA from {:.3} to {:.3} MHz…",
                self.vna_settings.start_mhz, self.vna_settings.stop_mhz
            ),
            VnaSweepPurpose::Calibration(standard) => format!(
                "Measuring {} standard from {:.3} to {:.3} MHz…",
                standard.label(),
                self.vna_settings.start_mhz,
                self.vna_settings.stop_mhz
            ),
        };
        self.last_error = None;
        thread::spawn(move || {
            let result = match session.lock() {
                Ok(mut session) => session
                    .acquire_sweep(config)
                    .map_err(|error| error.to_string()),
                Err(_) => Err("LiteVNA session lock was poisoned".to_owned()),
            };
            let _ = sender.send(result);
        });
    }

    fn commit_vna_frequency_inputs(&mut self) -> Result<(), String> {
        let parse = |input: &str, name: &str| {
            input
                .trim()
                .parse::<f64>()
                .map_err(|_| format!("Enter a valid VNA {name} frequency in MHz"))
                .and_then(|value| {
                    (value.is_finite() && (0.05..=6_300.0).contains(&value))
                        .then_some(value)
                        .ok_or_else(|| {
                            format!("VNA {name} frequency must be between 0.05 and 6300 MHz")
                        })
                })
        };
        let start_mhz = parse(&self.vna_start_frequency_input, "start")?;
        let stop_mhz = parse(&self.vna_stop_frequency_input, "stop")?;
        self.vna_settings.start_mhz = start_mhz;
        self.vna_settings.stop_mhz = stop_mhz;
        Ok(())
    }

    pub(crate) fn clear_vna_calibration(&mut self) {
        self.vna_calibration_captures.clear();
        self.vna_calibration = None;
        self.vna_sweep_calibrated = false;
        self.persist_vna_settings_if_available();
        if self.last_error.is_none() {
            self.status = "VNA calibration cleared".to_owned();
        }
    }

    pub(crate) fn poll_vna_sweep(&mut self) {
        self.poll_vna_run();
        let Some(receiver) = self.vna_pending.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("LiteVNA sweep worker ended unexpectedly".to_owned())
            }
        };
        self.vna_pending = None;
        let purpose = self
            .vna_pending_purpose
            .take()
            .unwrap_or(VnaSweepPurpose::Measurement);
        match result {
            Ok(sweep) => self.finish_vna_sweep(purpose, sweep),
            Err(error) => self.set_error(error),
        }
    }

    fn poll_vna_run(&mut self) {
        let Some(receiver) = self.vna_run_events.as_ref() else {
            return;
        };
        let event = match receiver.try_recv() {
            Ok(event) => Some(event),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.vna_run_events = None;
                self.vna_run_cancel = None;
                self.vna_run_active = false;
                let stopped = std::mem::take(&mut self.vna_run_stop_requested);
                if stopped && self.last_error.is_none() {
                    self.status = "VNA run stopped; LiteVNA returned to local mode".to_owned();
                } else if self.last_error.is_none() {
                    self.set_error("LiteVNA run worker ended unexpectedly".to_owned());
                }
                return;
            }
        };
        match event {
            Some(Ok(sweep)) => {
                self.finish_vna_sweep(VnaSweepPurpose::Measurement, sweep);
                if self.last_error.is_some() {
                    if let Some(cancel) = &self.vna_run_cancel {
                        cancel.store(true, Ordering::Release);
                    }
                }
            }
            Some(Err(error)) => {
                self.vna_run_events = None;
                self.vna_run_cancel = None;
                self.vna_run_active = false;
                self.vna_run_stop_requested = false;
                self.set_error(error);
            }
            None => {}
        }
    }

    fn finish_vna_sweep(&mut self, purpose: VnaSweepPurpose, mut sweep: VnaSweep) {
        match purpose {
            VnaSweepPurpose::Measurement => {
                let point_count = sweep.points.len();
                let start_mhz = sweep.requested.start_hz as f64 / 1_000_000.0;
                let stop_mhz = sweep.actual_stop_hz as f64 / 1_000_000.0;
                let mut calibration_note = "raw";
                self.vna_sweep_calibrated = false;
                if let Some(calibration) = &self.vna_calibration {
                    if calibration.matches_sweep(&sweep) {
                        if let Err(error) = calibration.apply(&mut sweep) {
                            self.set_error(error.to_string());
                            return;
                        }
                        self.vna_sweep_calibrated = true;
                        calibration_note = "SOL calibrated";
                    } else {
                        calibration_note = "raw; saved calibration grid differs";
                    }
                }
                self.vna_sweep = Some(sweep);
                self.status = format!(
                    "LiteVNA sweep complete · {point_count} points · {start_mhz:.3}–{stop_mhz:.3} MHz · {calibration_note}"
                );
                self.last_error = None;
            }
            VnaSweepPurpose::Calibration(standard) => {
                let reset = self.vna_calibration_captures.set(standard, sweep);
                let calibration = match self.vna_calibration_captures.calibration() {
                    Ok(calibration) => calibration,
                    Err(error) => {
                        self.set_error(error);
                        return;
                    }
                };
                if let Some(calibration) = calibration {
                    self.vna_calibration = Some(calibration);
                    self.vna_calibration_captures.clear();
                    self.vna_sweep_calibrated = false;
                    self.persist_vna_settings_if_available();
                    if self.last_error.is_none() {
                        self.status = "SOL calibration complete and saved in WaveCraft".to_owned();
                    }
                } else if reset {
                    self.status = format!(
                        "{} captured; earlier standards were cleared because the sweep grid changed",
                        standard.label()
                    );
                } else {
                    self.status = format!("{} calibration standard captured", standard.label());
                    self.last_error = None;
                }
            }
        }
    }

    fn persist_vna_settings_if_available(&mut self) {
        if self.capture_settings_repository.is_some() {
            self.save_capture_settings(SavedCaptureSettingsKind::VectorNetworkAnalyzer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::app;

    #[test]
    fn gui_settings_convert_megahertz_to_protocol_hertz() {
        let config = VnaSettings {
            start_mhz: 10.5,
            stop_mhz: 20.75,
            points: 101,
            averaging: 4,
        }
        .sweep_config()
        .unwrap();
        assert_eq!(config.start_hz, 10_500_000);
        assert_eq!(config.stop_hz, 20_750_000);
        assert_eq!(config.points, 101);
        assert_eq!(config.averaging, 4);
    }

    #[test]
    fn manually_entered_frequency_text_is_validated_before_a_sweep() {
        let mut app = app();
        app.vna_start_frequency_input = "12.5".to_owned();
        app.vna_stop_frequency_input = "not a frequency".to_owned();
        assert!(app.commit_vna_frequency_inputs().is_err());

        app.vna_stop_frequency_input = "125.25".to_owned();
        app.commit_vna_frequency_inputs().unwrap();
        assert_eq!(app.vna_settings.start_mhz, 12.5);
        assert_eq!(app.vna_settings.stop_mhz, 125.25);
    }

    #[test]
    fn a_sweep_requires_the_explicitly_opened_vna_session() {
        let mut app = app();

        app.begin_vna_sweep();

        assert_eq!(app.status, "Operation failed");
        assert_eq!(
            app.last_error.as_deref(),
            Some("Open the selected LiteVNA port before starting a sweep")
        );
        assert!(app.vna_pending.is_none());
    }
}
