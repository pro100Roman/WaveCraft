//! Power workspace state; transport and device protocol live in wavecraft-serial.
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use wavecraft_core::{
    CaptureBatch, CaptureCoordinator, CaptureError, CaptureEvent, CaptureSession, PowerSample,
};
use wavecraft_serial::pd_power::{
    Configuration, Control, DeviceSettings, PowerSource, Request, Response,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct PowerSettings {
    pub port: String,
    pub baud_rate: u32,
    pub crc8: bool,
    pub poll_hz: f64,
    pub window_seconds: f64,
    pub device: DeviceSettings,
}

impl Default for PowerSettings {
    fn default() -> Self {
        Self {
            port: String::new(),
            baud_rate: 115200,
            crc8: false,
            poll_hz: 10.0,
            window_seconds: 60.0,
            device: DeviceSettings::default(),
        }
    }
}

impl PowerSettings {
    fn configuration(&self) -> Configuration {
        Configuration {
            port: self.port.clone(),
            baud_rate: self.baud_rate,
            crc8: self.crc8,
            poll_hz: self.poll_hz,
        }
    }
    pub fn validate(&self) -> Result<(), CaptureError> {
        if !self.window_seconds.is_finite() || !(1.0..=3600.0).contains(&self.window_seconds) {
            return Err(CaptureError::new("Graph window must be 1–3600 seconds"));
        }
        // Saving an offline workspace does not require a connected port.
        let mut config = self.configuration();
        if config.port.is_empty() {
            config.port = "offline".into();
        }
        config.validate()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Point {
    pub time: f64,
    pub sample: PowerSample,
}

pub(crate) struct PowerState {
    pub plot_fraction: f32,
    pub layout_changed: bool,
    pub settings: PowerSettings,
    pub ports: Vec<String>,
    pub session: Option<CaptureSession>,
    control: Option<Control>,
    pub connected: bool,
    pub latest: Option<PowerSample>,
    pub points: VecDeque<Point>,
    pub status: String,
    pub error: Option<String>,
    pub confirmation: Option<(u8, Vec<u8>)>,
    pub preset_editor: usize,
}

impl Default for PowerState {
    fn default() -> Self {
        Self {
            plot_fraction: 0.5,
            layout_changed: false,
            settings: PowerSettings::default(),
            ports: Vec::new(),
            session: None,
            control: None,
            connected: false,
            latest: None,
            points: VecDeque::new(),
            status: "Select a port, then Connect".into(),
            error: None,
            confirmation: None,
            preset_editor: 0,
        }
    }
}

impl PowerState {
    pub fn refresh_ports(&mut self) {
        match wavecraft_serial::available_ports() {
            Ok(ports) => self.ports = ports,
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    pub fn start(&mut self) -> Result<(), CaptureError> {
        self.settings.validate()?;
        let (source, control) = PowerSource::new(self.settings.configuration())?;
        let session = CaptureCoordinator::start(vec![Box::new(source)])?;
        self.control = Some(control);
        self.session = Some(session);
        self.latest = None;
        self.points.clear();
        self.connected = false;
        self.error = None;
        self.status = "Connecting — reading device settings…".into();
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(session) = self.session.take() {
            session.request_stop();
        }
        self.control = None;
        self.connected = false;
        self.confirmation = None;
        self.status = "Disconnected".into();
    }

    pub fn request(&mut self, request: Request) {
        if let Some(control) = &self.control {
            if let Err(error) = control.requests.try_send(request) {
                self.error = Some(format!("Cannot queue device command: {error}"));
            }
        }
    }

    pub fn poll(&mut self) {
        if let Some(control) = &self.control {
            while let Ok(response) = control.responses.try_recv() {
                match response {
                    Response::Settings(settings) => {
                        self.settings.device = *settings;
                        self.connected = true;
                        self.status = format!(
                            "Connected · {} · {}",
                            self.settings.port, self.settings.device.version
                        );
                    }
                    Response::Applied(command) => {
                        self.error = None;
                        self.status = format!("Command 0x{command:02X} sent · writes have no acknowledgement; readings show device state");
                    }
                    Response::Rejected(error) => self.error = Some(error),
                }
            }
        }
        let mut stopped = false;
        if let Some(session) = &self.session {
            // Bound work per GUI frame if the renderer falls behind.
            for _ in 0..1000 {
                match session.try_recv() {
                    Ok(CaptureEvent::Frame(frame)) => {
                        if let CaptureBatch::Power(sample) = frame.payload {
                            self.latest = Some(sample);
                            self.points.push_back(Point {
                                time: frame.captured_at.as_secs_f64(),
                                sample,
                            });
                            while self.points.len() > 360_000 {
                                self.points.pop_front();
                            }
                        }
                    }
                    Ok(CaptureEvent::SourceFailed { error, .. }) => {
                        self.error = Some(error.to_string());
                        stopped = true;
                    }
                    Ok(CaptureEvent::SourceStopped { .. }) => {
                        stopped = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        stopped = true;
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        if stopped {
            self.stop();
        }
    }

    pub fn measured_hz(&self) -> Option<f64> {
        let end = self.points.back()?;
        let count = self.points.len().min(100);
        let start = self.points.get(self.points.len() - count)?;
        (count > 1 && end.time > start.time).then(|| (count - 1) as f64 / (end.time - start.time))
    }
}

pub(crate) fn output_mode(sample: PowerSample) -> &'static str {
    match (sample.output_state >> 1) & 3 {
        2 => "OCP",
        1 => "CC",
        _ if sample.output_state & 1 != 0 => "CV",
        _ => "OFF",
    }
}

pub(crate) fn input_mode(state: u8) -> &'static str {
    match state {
        0 => "Waiting",
        1 => "Negotiating PD",
        2 => "Negotiating QC",
        3 => "Input error",
        4 => "QC",
        5 => "PD",
        6 => "DC",
        u8::MAX => "Unavailable",
        _ => "Unknown",
    }
}
