//! WeAct PDPowerMini V1 Buck USB CDC / UART protocol.
//! Commands are serialized; reads have bounded lengths and deadlines. Connecting
//! only reads configuration and never changes the output or saved device setup.

use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};
use wavecraft_core::{
    CaptureBatch, CaptureContext, CaptureError, CaptureSource, DeviceDescriptor, DeviceId,
    InstrumentKind, PowerSample,
};

pub const MIN_POLL_HZ: f64 = 0.1;
pub const MAX_POLL_HZ: f64 = 100.0;
/// Standard host port rates, including USB CDC line-coding choices.
pub const BAUD_RATES: [u32; 28] = [
    110, 300, 600, 1200, 2400, 4800, 9600, 14400, 19200, 28800, 38400, 57600, 76800, 115200,
    128000, 153600, 230400, 256000, 460800, 500000, 576000, 921600, 1000000, 1152000, 1500000,
    2000000, 2500000, 3000000,
];
/// Rates exposed by the physical UART firmware's device menu.
pub const UART_BAUD_RATES: [u32; 7] = [9600, 19200, 38400, 57600, 115200, 230400, 460800];

#[derive(Clone, Debug)]
pub struct Configuration {
    pub port: String,
    pub baud_rate: u32,
    pub crc8: bool,
    pub poll_hz: f64,
}

impl Configuration {
    pub fn validate(&self) -> Result<(), CaptureError> {
        if self.port.trim().is_empty() || !BAUD_RATES.contains(&self.baud_rate) {
            return Err(CaptureError::new("Select a port and a supported baud rate"));
        }
        if self.crc8 && !UART_BAUD_RATES.contains(&self.baud_rate) {
            return Err(CaptureError::new("UART baud rate must match one of the device menu's rates (9600–460800); extended rates are available for USB CDC"));
        }
        validate_poll_hz(self.poll_hz)
    }
}

fn validate_poll_hz(value: f64) -> Result<(), CaptureError> {
    if !value.is_finite() || !(MIN_POLL_HZ..=MAX_POLL_HZ).contains(&value) {
        return Err(CaptureError::new("Polling rate must be 0.1–100 Hz"));
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSettings {
    #[serde(default)]
    pub input_state_supported: bool,
    pub model: String,
    pub version: String,
    pub serial: String,
    pub preset: u8,
    pub presets: [[u16; 2]; 5],
    // Entries indexed by command. None means the command is unavailable.
    pub values: [Option<u8>; 16],
    pub timer: Option<[u16; 3]>,
    pub lcd_type: Option<u8>,
    pub factory_data: Option<[u16; 32]>,
    pub pd_decivolts: Option<u16>,
}

impl DeviceSettings {
    pub fn supports(&self, minimum: [u16; 4]) -> bool {
        let Some(head) = self
            .version
            .strip_prefix('V')
            .and_then(|s| s.split('_').next())
        else {
            return false;
        };
        let parts = head
            .split('.')
            .map(str::parse::<u16>)
            .collect::<Result<Vec<_>, _>>();
        parts.is_ok_and(|parts| {
            parts.len() == 4 && parts[0] != 255 && parts.as_slice() >= minimum.as_slice()
        })
    }

    pub fn supports_timer(&self) -> bool {
        self.supports(if self.model.contains("V1.1") {
            [1, 0, 2, 0]
        } else {
            [1, 0, 3, 0]
        })
    }

    pub fn supports_pd(&self) -> bool {
        self.supports(if self.model.contains("V1.1") {
            [1, 0, 1, 0]
        } else {
            [1, 0, 2, 0]
        })
    }
}

#[derive(Clone, Debug)]
pub enum Request {
    PollRate(f64),
    Refresh,
    Write { command: u8, data: Vec<u8> },
}

#[derive(Clone, Debug)]
pub enum Response {
    Settings(Box<DeviceSettings>),
    Applied(u8),
    Rejected(String),
}

pub struct Control {
    pub requests: SyncSender<Request>,
    pub responses: Receiver<Response>,
}

pub struct PowerSource {
    configuration: Configuration,
    requests: Receiver<Request>,
    responses: SyncSender<Response>,
}

impl PowerSource {
    pub fn new(configuration: Configuration) -> Result<(Self, Control), CaptureError> {
        configuration.validate()?;
        let (tx, requests) = mpsc::sync_channel(32);
        let (responses, rx) = mpsc::sync_channel(32);
        Ok((
            Self {
                configuration,
                requests,
                responses,
            },
            Control {
                requests: tx,
                responses: rx,
            },
        ))
    }

    fn run_transport(
        self,
        transport: impl Read + Write,
        context: CaptureContext,
    ) -> Result<(), CaptureError> {
        let mut wire = Wire {
            transport,
            crc8: self.configuration.crc8,
            baud: self.configuration.baud_rate,
            context: &context,
            input_state_supported: true,
        };
        context.synchronize_start()?;
        let mut settings = wire.settings()?;
        let _ = self
            .responses
            .try_send(Response::Settings(Box::new(settings.clone())));
        let mut interval = Duration::from_secs_f64(1.0 / self.configuration.poll_hz);
        let mut due = Instant::now();
        let mut sequence = 0;
        while !context.is_cancelled() {
            match self.requests.try_recv() {
                Ok(Request::PollRate(value)) => match validate_poll_hz(value) {
                    Ok(()) => {
                        interval = Duration::from_secs_f64(1.0 / value);
                        due = Instant::now();
                    }
                    Err(error) => {
                        let _ = self
                            .responses
                            .try_send(Response::Rejected(error.to_string()));
                    }
                },
                Ok(Request::Refresh) => {
                    settings = wire.settings()?;
                    let _ = self
                        .responses
                        .try_send(Response::Settings(Box::new(settings.clone())));
                }
                Ok(Request::Write { command, data }) => {
                    let validation = validate_write(command, &data).and_then(|()| {
                        if matches!(command, 0x0b..=0x0f) && !settings.supports_timer() {
                            return Err(CaptureError::new("Timer requires newer firmware"));
                        }
                        if command == 0x0a {
                            if !settings.supports_pd() || !settings.input_state_supported {
                                return Err(CaptureError::new(
                                    "PD voltage command requires newer firmware",
                                ));
                            }
                            let sample = wire.sample()?;
                            if sample.output_state & 1 != 0
                                || sample.voltage_mv >= 5000
                                || sample.input_state != 5
                            {
                                return Err(CaptureError::new(
                                    "PD voltage requires output OFF and measured output below 5 V",
                                ));
                            }
                        }
                        Ok(())
                    });
                    if let Err(error) = validation {
                        let _ = self
                            .responses
                            .try_send(Response::Rejected(error.to_string()));
                        continue;
                    }
                    wire.send(command, &data)?;
                    // Firmware sends no write acknowledgement. Do not report a
                    // verified setting; expose a fresh readback to the caller.
                    let _ = self.responses.try_send(Response::Applied(command));
                    if matches!(command, 0x40 | 0x45) {
                        return Ok(());
                    }
                    if command != 0x02 && command != 0x09 {
                        settings = wire.settings()?;
                        let _ = self
                            .responses
                            .try_send(Response::Settings(Box::new(settings.clone())));
                    }
                    due = Instant::now();
                }
                Err(TryRecvError::Disconnected) => return Ok(()),
                Err(TryRecvError::Empty) => {}
            }
            if Instant::now() >= due {
                let started = Instant::now();
                let sample = wire.sample()?;
                context.emit(sequence, context.elapsed(), CaptureBatch::Power(sample))?;
                sequence += 1;
                // No catch-up bursts when the device or line is slower than requested.
                due = started + interval;
            } else {
                std::thread::sleep(
                    due.saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(5)),
                );
            }
        }
        Ok(())
    }
}

impl CaptureSource for PowerSource {
    fn descriptor(&self) -> DeviceDescriptor {
        DeviceDescriptor {
            id: DeviceId::new(format!("weact:{}", self.configuration.port)),
            name: "WeAct PD Power Mini V1 Buck".into(),
            kind: InstrumentKind::Serial,
        }
    }
    fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
        let port = serialport::new(&self.configuration.port, self.configuration.baud_rate)
            .timeout(Duration::from_millis(20))
            .open()
            .map_err(|e| {
                CaptureError::new(format!("Cannot open {}: {e}", self.configuration.port))
            })?;
        self.run_transport(port, context)
    }
}

pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0xff, |mut crc, byte| {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x31
            } else {
                crc << 1
            };
        }
        crc
    })
}

pub fn validate_write(command: u8, data: &[u8]) -> Result<(), CaptureError> {
    let valid = match (command, data) {
        (0x02 | 0x06 | 0x07 | 0x09 | 0x0b | 0x0c | 0x0e | 0x46, [v]) => *v <= 1,
        (0x03, [id]) => *id < 5,
        (0x04, [id, vl, vh, il, ih]) => {
            *id < 5
                && (500..=20000).contains(&u16::from_le_bytes([*vl, *vh]))
                && (5..=3100).contains(&u16::from_le_bytes([*il, *ih]))
        }
        (0x08, [v]) => (1..=100).contains(v),
        (0x0a, [l, h]) => (80..=240).contains(&u16::from_le_bytes([*l, *h])),
        (0x0d, [v]) => (1..=200).contains(v),
        (0x0f, [mode, el, eh, ol, oh]) => {
            *mode <= 1
                && u16::from_le_bytes([*el, *eh]) <= 59999
                && u16::from_le_bytes([*ol, *oh]) <= 59999
        }
        (0x40 | 0x44 | 0x45, []) => true,
        (0x47, bytes) => bytes.len() == 64,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(CaptureError::new(format!(
            "Invalid data for WeAct command 0x{command:02x}"
        )))
    }
}

struct Wire<'a, T> {
    transport: T,
    crc8: bool,
    baud: u32,
    context: &'a CaptureContext,
    input_state_supported: bool,
}

impl<T: Read + Write> Wire<'_, T> {
    fn send(&mut self, command: u8, data: &[u8]) -> Result<(), CaptureError> {
        if self.context.is_cancelled() {
            return Err(CaptureError::new("Power monitoring cancelled"));
        }
        let mut bytes = vec![command];
        bytes.extend_from_slice(data);
        bytes.push(if self.crc8 { crc8(&bytes) } else { 0x0a });
        self.transport.write_all(&bytes).map_err(io_error)?;
        self.transport.flush().map_err(io_error)?;
        // UART frames are split by USART IDLE: allow airtime plus four
        // characters, as specified by the manufacturer's Python implementation.
        let gap = if self.crc8 {
            Duration::from_secs_f64((bytes.len() + 4) as f64 * 10.0 / f64::from(self.baud))
        } else {
            Duration::from_millis(2)
        };
        std::thread::sleep(gap);
        Ok(())
    }

    fn read(
        &mut self,
        command: u8,
        data: &[u8],
        length: Option<usize>,
    ) -> Result<Vec<u8>, CaptureError> {
        self.read_reply(command, data, length, false)?
            .ok_or_else(|| CaptureError::new("Missing WeAct reply"))
    }

    fn read_reply(
        &mut self,
        command: u8,
        data: &[u8],
        length: Option<usize>,
        optional: bool,
    ) -> Result<Option<Vec<u8>>, CaptureError> {
        self.send(command | 0x80, data)?;
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut frame = Vec::new();
        let mut expected = length.map(|n| n + 2);
        loop {
            if self.context.is_cancelled() {
                return Err(CaptureError::new("Power monitoring cancelled"));
            }
            if Instant::now() >= deadline {
                // Early V1 firmware has no input-state command. Probe it once
                // during setup; malformed or partial replies remain errors.
                if optional && frame.is_empty() {
                    return Ok(None);
                }
                return Err(CaptureError::new(format!(
                    "WeAct command 0x{command:02x} timed out; check USB/UART protocol and firmware"
                )));
            }
            let mut byte = [0];
            match self.transport.read(&mut byte) {
                Ok(0) => return Err(CaptureError::new("WeAct port closed")),
                Ok(_) => frame.push(byte[0]),
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                    ) =>
                {
                    continue
                }
                Err(e) => return Err(io_error(e)),
            }
            if frame.len() == 1 && frame[0] & 0x7f != command {
                return Err(CaptureError::new("Unexpected WeAct response command"));
            }
            if self.crc8 && length.is_none() && frame.len() == 2 {
                expected = Some(usize::from(frame[1]) + 3);
            }
            if frame.len() > 258 {
                return Err(CaptureError::new("WeAct response exceeds protocol limit"));
            }
            let complete = expected
                .map_or(!self.crc8 && frame.len() >= 2 && byte[0] == 0x0a, |n| {
                    frame.len() == n
                });
            if complete {
                let end = frame.len() - 1;
                if frame[end] != if self.crc8 { crc8(&frame[..end]) } else { 0x0a } {
                    return Err(CaptureError::new(
                        "WeAct response checksum/terminator mismatch",
                    ));
                }
                return Ok(Some(
                    frame[if self.crc8 && length.is_none() { 2 } else { 1 }..end].to_vec(),
                ));
            }
        }
    }

    fn sample(&mut self) -> Result<PowerSample, CaptureError> {
        let display = self.read(5, &[], Some(4))?;
        let output = self.read(2, &[], Some(1))?;
        let input = if self.input_state_supported {
            self.read(10, &[], Some(5))?
        } else {
            vec![u8::MAX, 0, 0, 0, 0]
        };
        Ok(PowerSample {
            voltage_mv: word(&display, 0),
            current_ma: word(&display, 2),
            output_state: output[0],
            input_state: input[0],
            input_voltage_mv: word(&input, 1),
            requested_pd_decivolts: word(&input, 3),
        })
    }

    fn settings(&mut self) -> Result<DeviceSettings, CaptureError> {
        let mut settings = DeviceSettings {
            model: String::from_utf8_lossy(&self.read(1, &[], None)?).into_owned(),
            version: String::from_utf8_lossy(&self.read(0x42, &[], None)?).into_owned(),
            serial: String::from_utf8_lossy(&self.read(0x43, &[], None)?).into_owned(),
            ..DeviceSettings::default()
        };
        if !matches!(
            settings.model.trim(),
            "WeAct Studio PD Power Mini V1 BUCK" | "WeAct Studio PD Power Mini V1.1 BUCK"
        ) {
            return Err(CaptureError::new(format!(
                "Selected port is not a WeAct PD Power Mini: {}",
                settings.model
            )));
        }
        settings.preset = self.read(3, &[], Some(1))?[0];
        if settings.preset > 4 {
            return Err(CaptureError::new("WeAct returned an invalid preset index"));
        }
        for id in 0..5 {
            let data = self.read(4, &[id], Some(5))?;
            if data[0] != id {
                return Err(CaptureError::new(
                    "WeAct preset response does not match request",
                ));
            }
            settings.presets[usize::from(id)] = [word(&data, 1), word(&data, 3)];
        }
        for command in [6, 7, 8, 11, 12, 13, 14] {
            if command < 11 || settings.supports_timer() {
                settings.values[usize::from(command)] = Some(self.read(command, &[], Some(1))?[0]);
            }
        }
        if settings.supports_timer() {
            let t = self.read(15, &[], Some(5))?;
            settings.timer = Some([u16::from(t[0]), word(&t, 1), word(&t, 3)]);
        }
        let input = self.read_reply(10, &[], Some(5), true)?;
        self.input_state_supported = input.is_some();
        settings.input_state_supported = self.input_state_supported;
        settings.pd_decivolts = input.map(|input| word(&input, 3));
        settings.lcd_type = Some(self.read(0x46, &[], Some(1))?[0]);
        let data = self.read(0x47, &[], Some(64))?;
        settings.factory_data = Some(std::array::from_fn(|i| word(&data, i * 2)));
        Ok(settings)
    }
}

fn word(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn io_error(error: std::io::Error) -> CaptureError {
    CaptureError::new(format!("WeAct communication failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};
    use wavecraft_core::{CaptureCoordinator, CaptureEvent};

    struct Emulator {
        bytes: VecDeque<u8>,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        crc: bool,
        corrupt: bool,
        legacy: bool,
    }
    impl Read for Emulator {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            // Split every reply into single-byte chunks, including embedded LF.
            if let Some(byte) = self.bytes.pop_front() {
                buffer[0] = byte;
                Ok(1)
            } else {
                Err(ErrorKind::TimedOut.into())
            }
        }
    }
    impl Write for Emulator {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.writes.lock().unwrap().push(data.to_vec());
            if data[0] & 0x80 != 0 {
                let command = data[0] & 0x7f;
                if self.legacy && command == 10 {
                    return Ok(data.len());
                }
                let value = match command {
                    1 => b"WeAct Studio PD Power Mini V1 BUCK".to_vec(),
                    0x42 => {
                        if self.legacy {
                            b"V1.0.0.1_e5298860".to_vec()
                        } else {
                            b"V1.0.3.0_20261116".to_vec()
                        }
                    }
                    0x43 => b"TEST1234".to_vec(),
                    2 => vec![3], // Enabled + CC.
                    3 => vec![0],
                    4 => vec![data[1], 0x88, 0x13, 0xe8, 3],
                    5 => vec![0x0a, 0x13, 0xfa, 0], // Embedded LF is data.
                    10 => vec![5, 0xc0, 0x5d, 240, 0],
                    15 => vec![1, 10, 0, 0, 0],
                    0x47 => vec![0; 64],
                    _ => vec![1],
                };
                let mut frame = vec![data[0]];
                if self.crc && matches!(command, 1 | 0x42 | 0x43) {
                    frame.push(value.len() as u8);
                }
                frame.extend(value);
                let tail = if self.crc { crc8(&frame) } else { 10 };
                frame.push(if self.corrupt && command == 5 {
                    tail ^ 1
                } else {
                    tail
                });
                self.bytes.extend(frame);
            }
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    struct FakeSource {
        source: PowerSource,
        emulator: Emulator,
    }
    impl CaptureSource for FakeSource {
        fn descriptor(&self) -> DeviceDescriptor {
            self.source.descriptor()
        }
        fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
            self.source.run_transport(self.emulator, context)
        }
    }
    type WriteLog = Arc<Mutex<Vec<Vec<u8>>>>;
    fn fake(port: &str, crc: bool, corrupt: bool) -> (Box<dyn CaptureSource>, Control, WriteLog) {
        let (source, control) = PowerSource::new(Configuration {
            port: port.into(),
            baud_rate: 460800,
            crc8: crc,
            poll_hz: 100.0,
        })
        .unwrap();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let emulator = Emulator {
            bytes: VecDeque::new(),
            writes: writes.clone(),
            crc,
            corrupt,
            legacy: false,
        };
        (Box::new(FakeSource { source, emulator }), control, writes)
    }

    #[test]
    fn two_supplies_decode_usb_and_crc_uart_preserve_order_and_cancel_without_writes() {
        let (usb, _usb_control, usb_writes) = fake("usb", false, false);
        let (uart, _uart_control, uart_writes) = fake("uart", true, false);
        let session = CaptureCoordinator::start(vec![usb, uart]).unwrap();
        let mut sequences = BTreeMap::<String, Vec<u64>>::new();
        while sequences.len() < 2 || sequences.values().any(|v| v.len() < 3) {
            match session.recv_timeout(Duration::from_secs(3)).unwrap() {
                CaptureEvent::Frame(frame) => {
                    let CaptureBatch::Power(sample) = frame.payload else {
                        panic!("typed power payload")
                    };
                    assert_eq!(sample.voltage_mv, 4874);
                    assert_eq!(sample.current_ma, 250);
                    assert_eq!(sample.input_voltage_mv, 24000);
                    assert_eq!(sample.output_state, 3);
                    sequences
                        .entry(frame.device_id.to_string())
                        .or_default()
                        .push(frame.sequence);
                }
                event => panic!("unexpected event: {event:?}"),
            }
        }
        let started = Instant::now();
        session.stop().unwrap();
        assert!(started.elapsed() < Duration::from_millis(200));
        for values in sequences.values() {
            assert!(values.windows(2).all(|v| v[1] == v[0] + 1));
        }
        for log in [usb_writes, uart_writes] {
            assert!(log.lock().unwrap().iter().all(|bytes| bytes[0] & 0x80 != 0));
        }
    }

    #[test]
    fn corrupt_reply_propagates_failure_and_stops_other_source() {
        let (bad, _bad_control, _) = fake("bad", true, true);
        let (good, _good_control, _) = fake("good", false, false);
        let session = CaptureCoordinator::start(vec![bad, good]).unwrap();
        let mut failure = false;
        let mut stopped = 0;
        while stopped < 2 {
            match session.recv_timeout(Duration::from_secs(3)).unwrap() {
                CaptureEvent::SourceFailed { error, .. } => {
                    assert!(error.to_string().contains("checksum"));
                    failure = true;
                }
                CaptureEvent::SourceStopped { .. } => stopped += 1,
                _ => {}
            }
        }
        assert!(failure);
        session.stop().unwrap();
    }

    #[test]
    fn legacy_firmware_without_input_command_keeps_streaming_output_measurements() {
        let (source, control) = PowerSource::new(Configuration {
            port: "legacy".into(),
            baud_rate: 115200,
            crc8: false,
            poll_hz: 100.0,
        })
        .unwrap();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let emulator = Emulator {
            bytes: VecDeque::new(),
            writes: writes.clone(),
            crc: false,
            corrupt: false,
            legacy: true,
        };
        let session =
            CaptureCoordinator::start(vec![Box::new(FakeSource { source, emulator })]).unwrap();
        for _ in 0..3 {
            let CaptureEvent::Frame(frame) = session.recv_timeout(Duration::from_secs(2)).unwrap()
            else {
                panic!("legacy stream must stay active")
            };
            let CaptureBatch::Power(sample) = frame.payload else {
                panic!("power measurement")
            };
            assert_eq!(sample.voltage_mv, 4874);
            assert_eq!(sample.input_state, u8::MAX);
        }
        let Response::Settings(settings) = control.responses.try_recv().unwrap() else {
            panic!("initial settings")
        };
        assert!(!settings.input_state_supported);
        assert_eq!(settings.pd_decivolts, None);
        session.stop().unwrap();
        let log = writes.lock().unwrap();
        assert_eq!(log.iter().filter(|bytes| bytes[0] == 0x8a).count(), 1);
        assert!(log.iter().all(|bytes| bytes[0] & 0x80 != 0));
    }

    #[test]
    fn extended_usb_baud_rates_preserve_physical_uart_limits() {
        let mut configuration = Configuration {
            port: "usb".into(),
            baud_rate: 3000000,
            crc8: false,
            poll_hz: 10.0,
        };
        assert!(configuration.validate().is_ok());
        configuration.crc8 = true;
        assert!(configuration.validate().is_err());
        configuration.baud_rate = 460800;
        assert!(configuration.validate().is_ok());
    }

    #[test]
    fn rejects_invalid_writes_and_guards_firmware_features() {
        assert_eq!(crc8(&[0x85]), 0x23);
        for (command, bytes) in [
            (4, vec![5, 0, 0, 0, 0]),
            (10, vec![79, 0]),
            (15, vec![1, 0xff, 0xff, 0, 0]),
            (8, vec![101]),
            (0x47, vec![0; 63]),
        ] {
            assert!(validate_write(command, &bytes).is_err());
        }
        assert!(validate_write(4, &[4, 0xf4, 1, 5, 0]).is_ok());
        let mut settings = DeviceSettings {
            model: "V1".into(),
            version: "V1.0.2.0_abc".into(),
            ..Default::default()
        };
        assert!(settings.supports_pd());
        assert!(!settings.supports_timer());
        settings.version = "V255.255.255.255".into();
        assert!(!settings.supports_timer());
        assert!(validate_poll_hz(f64::NAN).is_err());
    }
}
