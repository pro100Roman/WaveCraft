//! LiteVNA discovery and the SAA2-compatible USB CDC protocol.
//!
//! The device is exposed by the operating system as a serial port. This crate
//! owns that transport and returns device-independent complex S-parameter
//! points; it deliberately has no GUI dependencies.

use serde::{Deserialize, Serialize};
use serialport::{ClearBuffer, SerialPort, SerialPortInfo, SerialPortType};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;

const SERIAL_BAUD_RATE: u32 = 115_200;
const SERIAL_TIMEOUT: Duration = Duration::from_secs(12);
const RECORD_BYTES: usize = 32;
const MAX_FIFO_READ: usize = u8::MAX as usize;
const PROGRESS_FIFO_READ: usize = 64;

const COMMAND_INDICATE: u8 = 0x0d;
const COMMAND_READ: u8 = 0x10;
const COMMAND_READ_FIFO: u8 = 0x18;
const COMMAND_WRITE: u8 = 0x20;
const COMMAND_WRITE2: u8 = 0x21;
const COMMAND_WRITE8: u8 = 0x23;

const REGISTER_SWEEP_START_HZ: u8 = 0x00;
const REGISTER_SWEEP_STEP_HZ: u8 = 0x10;
const REGISTER_SWEEP_POINTS: u8 = 0x20;
const REGISTER_VALUES_PER_FREQUENCY: u8 = 0x22;
const REGISTER_USB_MODE: u8 = 0x26;
const REGISTER_VALUES_FIFO: u8 = 0x30;
const REGISTER_AVERAGE: u8 = 0x40;
const REGISTER_DEVICE_VARIANT: u8 = 0xf0;
const REGISTER_PROTOCOL_VERSION: u8 = 0xf1;
const REGISTER_HARDWARE_REVISION: u8 = 0xf2;
const REGISTER_FIRMWARE_MAJOR: u8 = 0xf3;
const REGISTER_FIRMWARE_MINOR: u8 = 0xf4;

const USB_MODE_HOST_SWEEP: u8 = 0;
const USB_MODE_EXIT: u8 = 2;

/// USB metadata for one serial port that looks like a LiteVNA.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiteVnaDevice {
    pub port_name: String,
    pub display_name: String,
    pub serial_number: Option<String>,
}

/// Version information read without changing sweep settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiteVnaIdentity {
    pub device_variant: u8,
    pub protocol_version: u8,
    pub hardware_revision: u8,
    pub firmware_major: u8,
    pub firmware_minor: u8,
}

/// A complete VNA sweep request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SweepConfig {
    pub start_hz: u64,
    pub stop_hz: u64,
    pub points: u16,
    pub averaging: u8,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            start_hz: 1_000_000,
            stop_hz: 100_000_000,
            points: 201,
            averaging: 1,
        }
    }
}

impl SweepConfig {
    pub fn validate(self) -> Result<(), LiteVnaError> {
        if self.start_hz == 0 {
            return Err(LiteVnaError::InvalidConfig(
                "start frequency must be greater than zero".to_owned(),
            ));
        }
        if self.stop_hz <= self.start_hz {
            return Err(LiteVnaError::InvalidConfig(
                "stop frequency must be greater than start frequency".to_owned(),
            ));
        }
        if self.points < 2 {
            return Err(LiteVnaError::InvalidConfig(
                "a sweep needs at least two points".to_owned(),
            ));
        }
        if self.averaging == 0 {
            return Err(LiteVnaError::InvalidConfig(
                "averaging must be at least one".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn step_hz(self) -> Result<u64, LiteVnaError> {
        self.validate()?;
        let intervals = u64::from(self.points - 1);
        let step = (self.stop_hz - self.start_hz) / intervals;
        if step == 0 {
            return Err(LiteVnaError::InvalidConfig(
                "frequency span is too small for the selected point count".to_owned(),
            ));
        }
        Ok(step)
    }
}

/// One calibrated complex data point. Real and imaginary values are ratios.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VnaPoint {
    pub frequency_hz: u64,
    pub s11_real: f64,
    pub s11_imaginary: f64,
    pub s21_real: f64,
    pub s21_imaginary: f64,
}

impl VnaPoint {
    pub fn s11_magnitude(self) -> f64 {
        self.s11_real.hypot(self.s11_imaginary)
    }

    pub fn s21_magnitude(self) -> f64 {
        self.s21_real.hypot(self.s21_imaginary)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VnaSweep {
    pub identity: LiteVnaIdentity,
    pub requested: SweepConfig,
    pub actual_stop_hz: u64,
    pub points: Vec<VnaPoint>,
}

/// Per-frequency three-term error model produced by ideal Open/Short/Load
/// standards. It is serializable so the host application can persist it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OnePortCalibration {
    frequencies_hz: Vec<u64>,
    directivity: Vec<ComplexValue>,
    source_match: Vec<ComplexValue>,
    reflection_tracking: Vec<ComplexValue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct ComplexValue {
    real: f64,
    imaginary: f64,
}

impl ComplexValue {
    const ONE: Self = Self {
        real: 1.0,
        imaginary: 0.0,
    };

    fn new(real: f64, imaginary: f64) -> Self {
        Self { real, imaginary }
    }

    fn is_finite(self) -> bool {
        self.real.is_finite() && self.imaginary.is_finite()
    }

    fn norm_squared(self) -> f64 {
        self.real
            .mul_add(self.real, self.imaginary * self.imaginary)
    }

    fn add(self, other: Self) -> Self {
        Self::new(self.real + other.real, self.imaginary + other.imaginary)
    }

    fn subtract(self, other: Self) -> Self {
        Self::new(self.real - other.real, self.imaginary - other.imaginary)
    }

    fn multiply(self, other: Self) -> Self {
        Self::new(
            self.real * other.real - self.imaginary * other.imaginary,
            self.real * other.imaginary + self.imaginary * other.real,
        )
    }

    fn divide(self, denominator: Self, context: &str) -> Result<Self, LiteVnaError> {
        let scale = denominator.norm_squared();
        if !scale.is_finite() || scale <= f64::MIN_POSITIVE {
            return Err(LiteVnaError::Calibration(format!(
                "{context} has a zero or non-finite denominator"
            )));
        }
        Ok(Self::new(
            (self.real * denominator.real + self.imaginary * denominator.imaginary) / scale,
            (self.imaginary * denominator.real - self.real * denominator.imaginary) / scale,
        ))
    }
}

impl OnePortCalibration {
    /// Computes ideal-standard one-port error terms for every sweep frequency.
    pub fn from_sweeps(
        open: &VnaSweep,
        short: &VnaSweep,
        load: &VnaSweep,
    ) -> Result<Self, LiteVnaError> {
        ensure_same_grid(open, short, "SHORT")?;
        ensure_same_grid(open, load, "LOAD")?;
        if open.points.is_empty() {
            return Err(LiteVnaError::Calibration(
                "calibration sweeps contain no points".to_owned(),
            ));
        }

        let mut frequencies_hz = Vec::with_capacity(open.points.len());
        let mut directivity = Vec::with_capacity(open.points.len());
        let mut source_match = Vec::with_capacity(open.points.len());
        let mut reflection_tracking = Vec::with_capacity(open.points.len());
        for ((open_point, short_point), load_point) in
            open.points.iter().zip(&short.points).zip(&load.points)
        {
            let measured_open = s11(*open_point);
            let measured_short = s11(*short_point);
            let measured_load = s11(*load_point);
            let open_offset = measured_open.subtract(measured_load);
            let short_offset = measured_short.subtract(measured_load);
            let match_term = open_offset
                .add(short_offset)
                .divide(open_offset.subtract(short_offset), "SOL source match")?;
            let tracking = open_offset.multiply(ComplexValue::ONE.subtract(match_term));
            if !measured_load.is_finite() || !match_term.is_finite() || !tracking.is_finite() {
                return Err(LiteVnaError::Calibration(format!(
                    "non-finite SOL error term at {} Hz",
                    open_point.frequency_hz
                )));
            }
            frequencies_hz.push(open_point.frequency_hz);
            directivity.push(measured_load);
            source_match.push(match_term);
            reflection_tracking.push(tracking);
        }
        let calibration = Self {
            frequencies_hz,
            directivity,
            source_match,
            reflection_tracking,
        };
        calibration.validate()?;
        Ok(calibration)
    }

    pub fn point_count(&self) -> usize {
        self.frequencies_hz.len()
    }

    pub fn start_hz(&self) -> Option<u64> {
        self.frequencies_hz.first().copied()
    }

    pub fn stop_hz(&self) -> Option<u64> {
        self.frequencies_hz.last().copied()
    }

    pub fn matches_sweep(&self, sweep: &VnaSweep) -> bool {
        self.frequencies_hz.len() == sweep.points.len()
            && self
                .frequencies_hz
                .iter()
                .zip(&sweep.points)
                .all(|(frequency, point)| *frequency == point.frequency_hz)
    }

    pub fn matches_sweep_prefix(&self, sweep: &VnaSweep) -> bool {
        sweep.points.len() <= self.frequencies_hz.len()
            && self
                .frequencies_hz
                .iter()
                .zip(&sweep.points)
                .all(|(frequency, point)| *frequency == point.frequency_hz)
    }

    pub fn matches_config(&self, config: SweepConfig) -> bool {
        let Ok(step_hz) = config.step_hz() else {
            return false;
        };
        let actual_stop_hz = config
            .start_hz
            .saturating_add(step_hz.saturating_mul(u64::from(config.points - 1)));
        self.point_count() == usize::from(config.points)
            && self.start_hz() == Some(config.start_hz)
            && self.stop_hz() == Some(actual_stop_hz)
    }

    /// Applies the one-port error model to S11 in place. S21 remains raw.
    pub fn apply(&self, sweep: &mut VnaSweep) -> Result<(), LiteVnaError> {
        self.validate()?;
        if !self.matches_sweep(sweep) {
            return Err(LiteVnaError::Calibration(
                "sweep frequency grid does not match the saved calibration".to_owned(),
            ));
        }
        self.apply_points(&mut sweep.points)
    }

    /// Applies calibration to a cumulative prefix of the calibrated frequency
    /// grid, allowing a sweep to be drawn while it is still arriving.
    pub fn apply_prefix(&self, sweep: &mut VnaSweep) -> Result<(), LiteVnaError> {
        self.validate()?;
        if !self.matches_sweep_prefix(sweep) {
            return Err(LiteVnaError::Calibration(
                "partial sweep frequency grid does not match the saved calibration".to_owned(),
            ));
        }
        self.apply_points(&mut sweep.points)
    }

    fn apply_points(&self, points: &mut [VnaPoint]) -> Result<(), LiteVnaError> {
        for (index, point) in points.iter_mut().enumerate() {
            let measured = s11(*point);
            let offset = measured.subtract(self.directivity[index]);
            let denominator =
                self.reflection_tracking[index].add(self.source_match[index].multiply(offset));
            let corrected = offset.divide(denominator, "SOL correction")?;
            point.s11_real = corrected.real;
            point.s11_imaginary = corrected.imaginary;
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), LiteVnaError> {
        let count = self.frequencies_hz.len();
        if count < 2
            || self.directivity.len() != count
            || self.source_match.len() != count
            || self.reflection_tracking.len() != count
        {
            return Err(LiteVnaError::Calibration(
                "saved calibration arrays have inconsistent lengths".to_owned(),
            ));
        }
        if !self.frequencies_hz.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(LiteVnaError::Calibration(
                "saved calibration frequencies are not strictly increasing".to_owned(),
            ));
        }
        if self
            .directivity
            .iter()
            .chain(&self.source_match)
            .chain(&self.reflection_tracking)
            .any(|value| !value.is_finite())
        {
            return Err(LiteVnaError::Calibration(
                "saved calibration contains non-finite error terms".to_owned(),
            ));
        }
        Ok(())
    }
}

fn s11(point: VnaPoint) -> ComplexValue {
    ComplexValue::new(point.s11_real, point.s11_imaginary)
}

fn ensure_same_grid(
    reference: &VnaSweep,
    candidate: &VnaSweep,
    name: &str,
) -> Result<(), LiteVnaError> {
    if reference.points.len() != candidate.points.len()
        || reference
            .points
            .iter()
            .zip(&candidate.points)
            .any(|(left, right)| left.frequency_hz != right.frequency_hz)
    {
        return Err(LiteVnaError::Calibration(format!(
            "{name} sweep frequency grid differs from OPEN"
        )));
    }
    Ok(())
}

#[derive(Debug)]
pub enum LiteVnaError {
    Discovery(String),
    Open { port: String, detail: String },
    Communication(String),
    UnexpectedResponse(String),
    InvalidConfig(String),
    Calibration(String),
}

impl Display for LiteVnaError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Discovery(detail) => write!(formatter, "LiteVNA discovery failed: {detail}"),
            Self::Open { port, detail } => {
                write!(formatter, "failed to open LiteVNA port {port}: {detail}")
            }
            Self::Communication(detail) => {
                write!(formatter, "LiteVNA communication failed: {detail}")
            }
            Self::UnexpectedResponse(detail) => {
                write!(formatter, "unexpected LiteVNA response: {detail}")
            }
            Self::InvalidConfig(detail) => write!(formatter, "invalid VNA sweep: {detail}"),
            Self::Calibration(detail) => write!(formatter, "VNA calibration failed: {detail}"),
        }
    }
}

impl Error for LiteVnaError {}

/// Returns serial ports whose USB metadata matches LiteVNA's CDC firmware.
/// The protocol identity is verified when a sweep opens the selected port.
pub fn discover_litevna_devices() -> Result<Vec<LiteVnaDevice>, LiteVnaError> {
    let ports = serialport::available_ports()
        .map_err(|error| LiteVnaError::Discovery(error.to_string()))?;
    let devices = ports
        .into_iter()
        .filter_map(device_from_port_info)
        .collect::<Vec<_>>();
    Ok(prefer_callout_devices(devices))
}

fn prefer_callout_devices(devices: Vec<LiteVnaDevice>) -> Vec<LiteVnaDevice> {
    devices
        .iter()
        .filter(|device| {
            let Some(suffix) = device.port_name.strip_prefix("/dev/tty.") else {
                return true;
            };
            let callout = format!("/dev/cu.{suffix}");
            !devices
                .iter()
                .any(|candidate| candidate.port_name == callout)
        })
        .cloned()
        .collect()
}

fn device_from_port_info(info: SerialPortInfo) -> Option<LiteVnaDevice> {
    let port_name_lower = info.port_name.to_ascii_lowercase();
    let usb = match info.port_type {
        SerialPortType::UsbPort(usb) => usb,
        _ if port_name_lower.contains("litevna") || port_name_lower.contains("usbmodemdemo") => {
            return Some(LiteVnaDevice {
                display_name: format!("LiteVNA · {}", info.port_name),
                port_name: info.port_name,
                serial_number: None,
            });
        }
        _ => return None,
    };
    let product = usb.product.as_deref().unwrap_or_default();
    let manufacturer = usb.manufacturer.as_deref().unwrap_or_default();
    let serial = usb.serial_number.as_deref().unwrap_or_default();
    let metadata = format!("{product} {manufacturer} {serial}").to_ascii_lowercase();
    let matches = metadata.contains("litevna")
        || metadata.contains("cdc-acm demo")
        || metadata.contains("cdc_acm demo")
        || serial.eq_ignore_ascii_case("demo")
        || (usb.vid == 0x04b4 && usb.pid == 0x0008);
    matches.then(|| LiteVnaDevice {
        display_name: format!("LiteVNA · {}", info.port_name),
        port_name: info.port_name,
        serial_number: usb.serial_number,
    })
}

/// A long-lived LiteVNA connection. Keeping this value alive preserves USB
/// data mode between sweeps; dropping it returns firmware 2.x to local mode.
pub struct LiteVnaSession {
    port: Box<dyn SerialPort>,
    identity: LiteVnaIdentity,
}

impl LiteVnaSession {
    pub fn open(port_name: &str) -> Result<Self, LiteVnaError> {
        let mut port = serialport::new(port_name, SERIAL_BAUD_RATE)
            .timeout(SERIAL_TIMEOUT)
            .open()
            .map_err(|error| LiteVnaError::Open {
                port: port_name.to_owned(),
                detail: error.to_string(),
            })?;
        port.clear(ClearBuffer::All)
            .map_err(|error| communication("clear serial buffers", error))?;
        port.write_all(&[0; 8])
            .map_err(|error| communication("synchronize command parser", error))?;
        // Always leave any stale USB session first. This matches the
        // NanoVNA-App initialization sequence and recovers the command parser
        // after an interrupted or failed acquisition.
        write_register(port.as_mut(), REGISTER_USB_MODE, USB_MODE_EXIT)?;
        port.flush()
            .map_err(|error| communication("reset USB data mode", error))?;
        port.write_all(&[0; 8])
            .map_err(|error| communication("resynchronize command parser", error))?;
        let identity = read_identity(port.as_mut())?;
        if identity.device_variant != 2 || identity.protocol_version != 1 {
            return Err(LiteVnaError::UnexpectedResponse(format!(
                "device variant {} protocol {} is not SAA2-compatible",
                identity.device_variant, identity.protocol_version
            )));
        }
        // Identity reads prove that the serial transport is working, but the
        // LiteVNA enters USB data mode only after a register write. Enter it
        // explicitly so opening a session owns the instrument immediately,
        // instead of deferring the mode transition until the first sweep.
        write_register(port.as_mut(), REGISTER_USB_MODE, USB_MODE_HOST_SWEEP)?;
        port.flush()
            .map_err(|error| communication("enter USB data mode", error))?;
        Ok(Self { port, identity })
    }

    pub fn identity(&self) -> LiteVnaIdentity {
        self.identity
    }

    /// Programs the USB sweep grid without consuming measurement data.
    ///
    /// LiteVNA firmware 2.2 updates its display to "USB mode" only after it
    /// receives sweep parameters, not merely when the serial port is opened.
    pub fn configure_sweep(&mut self, config: SweepConfig) -> Result<(), LiteVnaError> {
        config.validate()?;
        configure_sweep_on_port(self.port.as_mut(), config)
    }

    /// Configures a sweep and consumes one bootstrap point so LiteVNA 2.2
    /// visibly enters USB mode. A later acquisition clears the FIFO and starts
    /// from point zero again, so this discarded record cannot affect results.
    pub fn enter_usb_sweep_mode(&mut self, config: SweepConfig) -> Result<(), LiteVnaError> {
        self.configure_sweep(config)?;
        let mut bootstrap_record = [0_u8; RECORD_BYTES];
        self.port
            .write_all(&[COMMAND_READ_FIFO, REGISTER_VALUES_FIFO, 1])
            .map_err(|error| communication("request USB mode bootstrap point", error))?;
        self.port
            .read_exact(&mut bootstrap_record)
            .map_err(|error| communication("read USB mode bootstrap point", error))
    }

    pub fn acquire_sweep(&mut self, config: SweepConfig) -> Result<VnaSweep, LiteVnaError> {
        config.validate()?;
        let mut ignore_partial_sweep = |_: VnaSweep| {};
        acquire_sweep_from_port(
            self.port.as_mut(),
            self.identity,
            config,
            MAX_FIFO_READ,
            &mut ignore_partial_sweep,
        )
    }

    /// Acquires a sweep and reports the number of fully received frequency
    /// points after every FIFO block.
    pub fn acquire_sweep_with_progress(
        &mut self,
        config: SweepConfig,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<VnaSweep, LiteVnaError> {
        self.acquire_sweep_streaming(config, |partial_sweep| {
            progress(partial_sweep.points.len(), usize::from(config.points));
        })
    }

    /// Acquires a sweep and publishes the cumulative, fully parsed points
    /// after every FIFO block. Each partial sweep starts at the configured
    /// start frequency and can be rendered immediately.
    pub fn acquire_sweep_streaming(
        &mut self,
        config: SweepConfig,
        mut partial_sweep: impl FnMut(VnaSweep),
    ) -> Result<VnaSweep, LiteVnaError> {
        config.validate()?;
        acquire_sweep_from_port(
            self.port.as_mut(),
            self.identity,
            config,
            PROGRESS_FIFO_READ,
            &mut partial_sweep,
        )
    }
}

impl Drop for LiteVnaSession {
    fn drop(&mut self) {
        // Best-effort: a communication error while leaving USB mode cannot be
        // reported from Drop and must not hide an earlier acquisition error.
        let _ = write_register(self.port.as_mut(), REGISTER_USB_MODE, USB_MODE_EXIT);
        let _ = self.port.flush();
    }
}

/// Opens a LiteVNA, verifies the SAA2 identity, and acquires one sweep.
pub fn acquire_sweep(port_name: &str, config: SweepConfig) -> Result<VnaSweep, LiteVnaError> {
    let mut session = LiteVnaSession::open(port_name)?;
    session.acquire_sweep(config)
}

fn acquire_sweep_from_port(
    port: &mut dyn SerialPort,
    identity: LiteVnaIdentity,
    config: SweepConfig,
    maximum_fifo_read: usize,
    partial_sweep: &mut dyn FnMut(VnaSweep),
) -> Result<VnaSweep, LiteVnaError> {
    configure_sweep_on_port(port, config)?;
    let step_hz = config.step_hz()?;

    let total = usize::from(config.points);
    let actual_stop_hz = config
        .start_hz
        .saturating_add(step_hz.saturating_mul(u64::from(config.points - 1)));
    let mut points = Vec::with_capacity(total);
    let mut remaining = total;
    while remaining > 0 {
        let requested = remaining.min(maximum_fifo_read);
        let requested_u8 = u8::try_from(requested).map_err(|_| {
            LiteVnaError::Communication("FIFO request length overflowed".to_owned())
        })?;
        port.write_all(&[COMMAND_READ_FIFO, REGISTER_VALUES_FIFO, requested_u8])
            .map_err(|error| communication("request sweep FIFO", error))?;
        let mut raw = vec![0; requested * RECORD_BYTES];
        port.read_exact(&mut raw)
            .map_err(|error| communication("read sweep FIFO", error))?;
        let block = parse_points_at(&raw, points.len(), config.start_hz, step_hz)?;
        points.extend(block);
        remaining -= requested;
        partial_sweep(VnaSweep {
            identity,
            requested: config,
            actual_stop_hz,
            points: points.clone(),
        });
    }

    Ok(VnaSweep {
        identity,
        requested: config,
        actual_stop_hz,
        points,
    })
}

fn configure_sweep_on_port(
    port: &mut dyn SerialPort,
    config: SweepConfig,
) -> Result<(), LiteVnaError> {
    let step_hz = config.step_hz()?;
    write_register(port, REGISTER_VALUES_FIFO, 0)?;
    write_register8(port, REGISTER_SWEEP_START_HZ, config.start_hz)?;
    write_register8(port, REGISTER_SWEEP_STEP_HZ, step_hz)?;
    write_register2(port, REGISTER_SWEEP_POINTS, config.points)?;
    write_register2(port, REGISTER_VALUES_PER_FREQUENCY, 1)?;
    write_register(port, REGISTER_AVERAGE, config.averaging)?;
    port.flush()
        .map_err(|error| communication("configure USB sweep", error))
}

fn read_identity(port: &mut dyn SerialPort) -> Result<LiteVnaIdentity, LiteVnaError> {
    port.write_all(&[COMMAND_INDICATE])
        .map_err(|error| communication("send identity probe", error))?;
    let mut response = [0_u8; 1];
    port.read_exact(&mut response)
        .map_err(|error| communication("read identity probe", error))?;
    if response[0] != b'2' {
        return Err(LiteVnaError::UnexpectedResponse(format!(
            "INDICATE returned 0x{:02x} instead of 0x32",
            response[0]
        )));
    }
    Ok(LiteVnaIdentity {
        device_variant: read_register(port, REGISTER_DEVICE_VARIANT)?,
        protocol_version: read_register(port, REGISTER_PROTOCOL_VERSION)?,
        hardware_revision: read_register(port, REGISTER_HARDWARE_REVISION)?,
        firmware_major: read_register(port, REGISTER_FIRMWARE_MAJOR)?,
        firmware_minor: read_register(port, REGISTER_FIRMWARE_MINOR)?,
    })
}

fn read_register(port: &mut dyn SerialPort, address: u8) -> Result<u8, LiteVnaError> {
    port.write_all(&[COMMAND_READ, address])
        .map_err(|error| communication("write register read command", error))?;
    let mut value = [0_u8; 1];
    port.read_exact(&mut value)
        .map_err(|error| communication("read register value", error))?;
    Ok(value[0])
}

fn write_register(port: &mut dyn SerialPort, address: u8, value: u8) -> Result<(), LiteVnaError> {
    port.write_all(&[COMMAND_WRITE, address, value])
        .map_err(|error| communication("write 8-bit register", error))
}

fn write_register2(port: &mut dyn SerialPort, address: u8, value: u16) -> Result<(), LiteVnaError> {
    let value = value.to_le_bytes();
    port.write_all(&[COMMAND_WRITE2, address, value[0], value[1]])
        .map_err(|error| communication("write 16-bit register", error))
}

fn write_register8(port: &mut dyn SerialPort, address: u8, value: u64) -> Result<(), LiteVnaError> {
    let value = value.to_le_bytes();
    let mut command = [0_u8; 10];
    command[0] = COMMAND_WRITE8;
    command[1] = address;
    command[2..].copy_from_slice(&value);
    port.write_all(&command)
        .map_err(|error| communication("write 64-bit register", error))
}

fn communication(context: &str, error: impl Display) -> LiteVnaError {
    LiteVnaError::Communication(format!("{context}: {error}"))
}

fn parse_points_at(
    raw: &[u8],
    start_position: usize,
    start_hz: u64,
    step_hz: u64,
) -> Result<Vec<VnaPoint>, LiteVnaError> {
    if raw.len() % RECORD_BYTES != 0 {
        return Err(LiteVnaError::UnexpectedResponse(format!(
            "FIFO returned {} bytes, not a multiple of {RECORD_BYTES}",
            raw.len()
        )));
    }
    raw.chunks_exact(RECORD_BYTES)
        .enumerate()
        .map(|(position, record)| parse_point(record, start_position + position, start_hz, step_hz))
        .collect()
}

fn parse_point(
    record: &[u8],
    position: usize,
    start_hz: u64,
    step_hz: u64,
) -> Result<VnaPoint, LiteVnaError> {
    let word = |offset: usize| -> Result<i32, LiteVnaError> {
        let bytes: [u8; 4] = record
            .get(offset..offset + 4)
            .ok_or_else(|| LiteVnaError::UnexpectedResponse("truncated FIFO record".to_owned()))?
            .try_into()
            .map_err(|_| LiteVnaError::UnexpectedResponse("invalid FIFO word".to_owned()))?;
        Ok(i32::from_le_bytes(bytes))
    };
    let forward = (f64::from(word(0)?), f64::from(word(4)?));
    let reflected = (f64::from(word(8)?), f64::from(word(12)?));
    let through = (f64::from(word(16)?), f64::from(word(20)?));
    let index_word = word(24)?;
    let index = usize::try_from(index_word).map_err(|_| {
        LiteVnaError::UnexpectedResponse(format!("negative frequency index {index_word}"))
    })?;
    if index != position {
        return Err(LiteVnaError::UnexpectedResponse(format!(
            "frequency index {index} arrived at position {position}"
        )));
    }
    let s11 = complex_ratio(reflected, forward)?;
    let s21 = complex_ratio(through, forward)?;
    Ok(VnaPoint {
        frequency_hz: start_hz.saturating_add(step_hz.saturating_mul(index as u64)),
        s11_real: s11.0,
        s11_imaginary: s11.1,
        s21_real: s21.0,
        s21_imaginary: s21.1,
    })
}

fn complex_ratio(
    numerator: (f64, f64),
    denominator: (f64, f64),
) -> Result<(f64, f64), LiteVnaError> {
    let scale = denominator
        .0
        .mul_add(denominator.0, denominator.1 * denominator.1);
    if scale == 0.0 {
        return Err(LiteVnaError::UnexpectedResponse(
            "forward wave is zero".to_owned(),
        ));
    }
    Ok((
        (numerator.0 * denominator.0 + numerator.1 * denominator.1) / scale,
        (numerator.1 * denominator.0 - numerator.0 * denominator.1) / scale,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_config_rejects_invalid_ranges() {
        assert!(SweepConfig {
            start_hz: 100,
            stop_hz: 100,
            ..SweepConfig::default()
        }
        .validate()
        .is_err());
        assert!(SweepConfig {
            points: 1,
            ..SweepConfig::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn parses_complex_s_parameters_and_frequency_index() {
        let words = [100_i32, 50, 50, 25, -25, 50, 0, 0];
        let raw = words
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect::<Vec<_>>();
        let points = parse_points_at(&raw, 0, 1_000_000, 25_000).unwrap();
        assert_eq!(points.len(), 1);
        assert!((points[0].s11_real - 0.5).abs() < 1.0e-12);
        assert!(points[0].s11_imaginary.abs() < 1.0e-12);
        assert!(points[0].s21_real.abs() < 1.0e-12);
        assert!((points[0].s21_imaginary - 0.5).abs() < 1.0e-12);
        assert_eq!(points[0].frequency_hz, 1_000_000);
    }

    #[test]
    fn recognizes_litevna_usb_metadata() {
        let device = device_from_port_info(SerialPortInfo {
            port_name: "/dev/cu.usbmodemDEMO1".to_owned(),
            port_type: SerialPortType::UsbPort(serialport::UsbPortInfo {
                vid: 0x04b4,
                pid: 0x0008,
                serial_number: Some("DEMO".to_owned()),
                manufacturer: Some("Black Sphere Technologies".to_owned()),
                product: Some("CDC-ACM Demo".to_owned()),
            }),
        });
        assert_eq!(
            device.map(|device| device.port_name),
            Some("/dev/cu.usbmodemDEMO1".to_owned())
        );
    }

    #[test]
    fn macos_callout_and_dialin_nodes_are_one_device() {
        let device = |port_name: &str| LiteVnaDevice {
            port_name: port_name.to_owned(),
            display_name: format!("LiteVNA · {port_name}"),
            serial_number: Some("DEMO".to_owned()),
        };
        let devices = prefer_callout_devices(vec![
            device("/dev/tty.usbmodemDEMO1"),
            device("/dev/cu.usbmodemDEMO1"),
        ]);
        assert_eq!(devices, vec![device("/dev/cu.usbmodemDEMO1")]);
    }

    fn test_sweep(values: &[(u64, ComplexValue)]) -> VnaSweep {
        let start_hz = values.first().map_or(1, |(frequency, _)| *frequency);
        let stop_hz = values
            .last()
            .map_or(start_hz + 1, |(frequency, _)| *frequency);
        VnaSweep {
            identity: LiteVnaIdentity {
                device_variant: 2,
                protocol_version: 1,
                hardware_revision: 2,
                firmware_major: 2,
                firmware_minor: 2,
            },
            requested: SweepConfig {
                start_hz,
                stop_hz,
                points: u16::try_from(values.len()).unwrap(),
                averaging: 1,
            },
            actual_stop_hz: stop_hz,
            points: values
                .iter()
                .map(|(frequency_hz, value)| VnaPoint {
                    frequency_hz: *frequency_hz,
                    s11_real: value.real,
                    s11_imaginary: value.imaginary,
                    s21_real: 0.0,
                    s21_imaginary: 0.0,
                })
                .collect(),
        }
    }

    fn distorted(
        actual: ComplexValue,
        directivity: ComplexValue,
        source_match: ComplexValue,
        tracking: ComplexValue,
    ) -> ComplexValue {
        directivity.add(
            tracking
                .multiply(actual)
                .divide(
                    ComplexValue::ONE.subtract(source_match.multiply(actual)),
                    "test error model",
                )
                .unwrap(),
        )
    }

    #[test]
    fn sol_calibration_recovers_known_reflection_coefficient() {
        let directivity = ComplexValue::new(0.1, 0.02);
        let source_match = ComplexValue::new(0.05, -0.01);
        let tracking = ComplexValue::new(0.8, 0.1);
        let standards = |actual: ComplexValue| {
            [1_000_000_u64, 2_000_000].map(|frequency| {
                (
                    frequency,
                    distorted(actual, directivity, source_match, tracking),
                )
            })
        };
        let open = test_sweep(&standards(ComplexValue::ONE));
        let short = test_sweep(&standards(ComplexValue::new(-1.0, 0.0)));
        let load = test_sweep(&standards(ComplexValue::new(0.0, 0.0)));
        let calibration = OnePortCalibration::from_sweeps(&open, &short, &load).unwrap();

        let actual = ComplexValue::new(0.3, -0.2);
        let mut partial = test_sweep(&standards(actual));
        partial.points.truncate(1);
        calibration.apply_prefix(&mut partial).unwrap();
        assert!((partial.points[0].s11_real - actual.real).abs() < 1.0e-12);
        assert!((partial.points[0].s11_imaginary - actual.imaginary).abs() < 1.0e-12);

        let mut measured = test_sweep(&standards(actual));
        calibration.apply(&mut measured).unwrap();

        for point in measured.points {
            assert!((point.s11_real - actual.real).abs() < 1.0e-12);
            assert!((point.s11_imaginary - actual.imaginary).abs() < 1.0e-12);
        }
    }

    #[test]
    fn calibration_rejects_a_different_frequency_grid() {
        let standard = |value| {
            test_sweep(&[
                (1_000_000, ComplexValue::new(value, 0.0)),
                (2_000_000, ComplexValue::new(value, 0.0)),
            ])
        };
        let calibration =
            OnePortCalibration::from_sweeps(&standard(1.0), &standard(-1.0), &standard(0.0))
                .unwrap();
        let mut different = test_sweep(&[
            (1_000_000, ComplexValue::new(0.2, 0.0)),
            (3_000_000, ComplexValue::new(0.3, 0.0)),
        ]);

        assert!(calibration.apply(&mut different).is_err());
    }
}
