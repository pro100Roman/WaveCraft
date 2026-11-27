//! Power workspace state; transport and device protocol live in wavecraft-serial.
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
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
    output_toggle_due: Option<(Instant, bool)>,
    pub plot_fraction: f32,
    pub layout_changed: bool,
    pub settings: PowerSettings,
    pub baud_draft: Option<String>,
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
            output_toggle_due: None,
            plot_fraction: 0.5,
            layout_changed: false,
            settings: PowerSettings::default(),
            baud_draft: None,
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

    pub(crate) fn edited_settings(&self) -> Result<PowerSettings, CaptureError> {
        let mut settings = self.settings.clone();
        if let Some(draft) = &self.baud_draft {
            settings.baud_rate = draft
                .trim()
                .parse::<u32>()
                .map_err(|_| CaptureError::new("Baud rate must be between 1 and 12000000"))?;
        }
        settings.validate()?;
        Ok(settings)
    }

    pub fn start(&mut self) -> Result<(), CaptureError> {
        self.settings = self.edited_settings()?;
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
        self.output_toggle_due = None;
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

    pub fn output_is_on(&self) -> Option<bool> {
        self.latest.map(|sample| sample.output_state & 1 != 0)
    }

    fn output_control_is_active(&self) -> Option<bool> {
        // A timer can remain active while its physical output is in the OFF phase.
        self.latest.map(|sample| sample.output_state & 9 != 0)
    }

    pub fn output_toggle_pending(&self) -> bool {
        self.output_toggle_due
            .is_some_and(|(due, _)| Instant::now() < due)
    }

    pub fn toggle_output(&mut self) {
        if !self.connected || self.output_toggle_pending() {
            return;
        }
        if let Some(enabled) = self.output_control_is_active() {
            self.request(Request::Write {
                command: 2,
                data: vec![u8::from(!enabled)],
            });
            // Block repeated clicks until fresh telemetry or a bounded timeout.
            self.output_toggle_due = Some((Instant::now() + Duration::from_millis(500), !enabled));
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
                        self.status = if command == 2 {
                            "Output change requested".into()
                        } else {
                            "Device settings sent".into()
                        };
                    }
                    Response::Rejected(error) => {
                        self.output_toggle_due = None;
                        self.error = Some(error);
                    }
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
                            if self.output_toggle_due.is_some_and(|(_, desired)| {
                                self.output_control_is_active() == Some(desired)
                            }) {
                                self.output_toggle_due = None;
                            }
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

pub(crate) fn same_port(left: &str, right: &str) -> bool {
    fn normalized(port: &str) -> String {
        let path = std::fs::canonicalize(port)
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| port.to_owned());
        path.strip_prefix("/dev/cu.")
            .map_or_else(|| path.clone(), |name| format!("/dev/tty.{name}"))
    }
    normalized(left) == normalized(right)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use wavecraft_core::{
        CaptureContext, CaptureSource, DeviceDescriptor, DeviceId, InstrumentKind,
    };

    struct OutputTelemetry(mpsc::Receiver<PowerSample>);

    impl CaptureSource for OutputTelemetry {
        fn descriptor(&self) -> DeviceDescriptor {
            DeviceDescriptor {
                id: DeviceId::new("external-output-changes"),
                name: "External output changes".into(),
                kind: InstrumentKind::Serial,
            }
        }

        fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
            context.synchronize_start()?;
            let mut sequence = 0;
            while !context.is_cancelled() {
                match self.0.recv_timeout(Duration::from_millis(5)) {
                    Ok(sample) => {
                        context.emit(sequence, context.elapsed(), CaptureBatch::Power(sample))?;
                        sequence += 1;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                }
            }
            Ok(())
        }
    }

    #[test]
    fn output_display_follows_external_telemetry_and_waits_for_command_readback() {
        let (samples, incoming) = mpsc::channel();
        let (requests, commands) = mpsc::sync_channel(8);
        let (replies, responses) = mpsc::sync_channel(8);
        let mut state = PowerState {
            connected: true,
            session: Some(
                CaptureCoordinator::start(vec![Box::new(OutputTelemetry(incoming))]).unwrap(),
            ),
            control: Some(Control {
                requests,
                responses,
            }),
            ..Default::default()
        };
        let receive = |state: &mut PowerState, output_state| {
            let sample = PowerSample {
                output_state,
                ..Default::default()
            };
            samples.send(sample).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while state.latest != Some(sample) && Instant::now() < deadline {
                state.poll();
                std::thread::yield_now();
            }
            assert_eq!(state.latest, Some(sample));
        };

        receive(&mut state, 0);
        state.toggle_output();
        assert!(
            matches!(commands.try_recv(), Ok(Request::Write { command: 2, data }) if data == [1])
        );
        assert_eq!(state.output_is_on(), Some(false));
        replies.send(Response::Applied(2)).unwrap();
        state.poll();
        assert_eq!(state.output_is_on(), Some(false));
        receive(&mut state, 1);
        assert_eq!(state.output_is_on(), Some(true));
        assert!(!state.output_toggle_pending());

        // Hardware-button changes arrive without any request from the UI.
        for output_state in [0, 1, 8, 9, 8, 0] {
            receive(&mut state, output_state);
            assert_eq!(state.output_is_on(), Some(output_state & 1 != 0));
            assert!(commands.try_recv().is_err());
        }
        state.stop();
    }

    #[test]
    fn output_toggle_inverts_reported_state_and_debounces_pending_commands() {
        let (requests, rx) = mpsc::sync_channel(8);
        let (_tx, responses) = mpsc::sync_channel(8);
        let mut state = PowerState {
            connected: true,
            latest: Some(PowerSample::default()),
            control: Some(Control {
                requests,
                responses,
            }),
            ..Default::default()
        };
        state.toggle_output();
        assert!(matches!(rx.try_recv(), Ok(Request::Write { command: 2, data }) if data == [1]));
        assert_eq!(state.output_is_on(), Some(false));
        state.toggle_output();
        assert!(rx.try_recv().is_err());
        state.output_toggle_due = None;
        state.latest = Some(PowerSample {
            output_state: 1,
            ..Default::default()
        });
        state.toggle_output();
        assert!(matches!(rx.try_recv(), Ok(Request::Write { command: 2, data }) if data == [0]));
        state.output_toggle_due = None;
        // Timer OFF phase displays OFF; clicking still stops the active sequence.
        state.latest = Some(PowerSample {
            output_state: 8,
            ..Default::default()
        });
        assert_eq!(state.output_is_on(), Some(false));
        state.toggle_output();
        assert!(matches!(rx.try_recv(), Ok(Request::Write { command: 2, data }) if data == [0]));
    }

    #[test]
    fn serial_callout_and_tty_names_identify_the_same_device() {
        assert!(same_port("/dev/cu.usbmodem123", "/dev/tty.usbmodem123"));
        assert!(!same_port("/dev/cu.usbmodem123", "/dev/cu.usbmodem456"));
    }
}
