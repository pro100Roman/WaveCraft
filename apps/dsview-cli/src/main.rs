use dsview_core::{CaptureBatch, CaptureCoordinator, CaptureEvent, CaptureSource, DeviceId};
use dsview_sim::{SimulatedLogicAnalyzer, SimulatedOscilloscope};
use dsview_usb::{
    discover_supported_devices, probe_device, DeviceModel, DiscoveredDevice, DsCopeCaptureConfig,
    DsCopeCaptureSource, DsLogicCaptureConfig, DsLogicCaptureSource, FirmwareVersion,
    HardwareStatus, HdlVersion, LinkStatus, UsbDeviceSession,
};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Default)]
struct DeviceStats {
    frames: u64,
    samples: u64,
    first_timestamp: Option<Duration>,
    last_timestamp: Option<Duration>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.first().is_some_and(|argument| argument == "list") {
        return list_devices();
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "probe")
    {
        return probe_devices();
    }
    if let [command, resource_directory] = arguments.as_slice() {
        match command.as_str() {
            "load-fpga" => return load_fpgas(Path::new(resource_directory)),
            "prepare" => return prepare_devices(Path::new(resource_directory)),
            "capture-logic" => {
                return capture_logic(Path::new(resource_directory), 1);
            }
            "capture-scope" => return capture_scope(Path::new(resource_directory)),
            "capture-usb" => return capture_usb(Path::new(resource_directory)),
            _ => {}
        }
    }
    if let [command, resource_directory, seconds] = arguments.as_slice() {
        if command == "capture-logic" {
            return capture_logic(Path::new(resource_directory), seconds.parse()?);
        }
    }

    let run_seconds = match arguments.as_slice() {
        [] => 2,
        [seconds] => seconds.parse::<u64>()?,
        [command, seconds] if command == "simulate" => seconds.parse::<u64>()?,
        _ => {
            return Err(
                "usage: dsview-cli [list | probe | load-fpga <resource-directory> | prepare <resource-directory> | capture-logic <resource-directory> [seconds] | capture-scope <resource-directory> | capture-usb <resource-directory> | simulate <seconds> | <seconds>]"
                    .into(),
            )
        }
    };

    run_simulation(run_seconds)
}

fn capture_usb(resource_directory: &Path) -> Result<(), Box<dyn Error>> {
    const SAMPLE_RATE_HZ: u64 = 1_000_000;
    const SAMPLE_COUNT: u64 = 1_000_000;
    const CAPTURE_TIMEOUT: Duration = Duration::from_secs(11);

    let logic_config = DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)?;
    let scope_config = DsCopeCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)?;
    let report = discover_supported_devices()?;
    let logic_device = report
        .devices
        .iter()
        .find(|device| device.profile.model == DeviceModel::DsLogicU3Pro16)
        .cloned()
        .ok_or("no DSLogic U3Pro16 detected")?;
    let scope_device = report
        .devices
        .iter()
        .find(|device| device.profile.model == DeviceModel::DsCopeU3P100)
        .cloned()
        .ok_or("no DSCope U3P100 detected")?;

    for device in [&logic_device, &scope_device] {
        let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
        let prepared = prepare_device(device, &bitstream_path)?;
        println!(
            "{} ready: {} firmware {}, HDL {}",
            device.location.stable_id(),
            device.profile.display_name,
            prepared.firmware,
            prepared.hdl
        );
    }

    let logic_id = DeviceId::new(logic_device.location.stable_id());
    let scope_id = DeviceId::new(scope_device.location.stable_id());
    let sources: Vec<Box<dyn CaptureSource>> = vec![
        Box::new(DsLogicCaptureSource::new(logic_device, logic_config)?),
        Box::new(DsCopeCaptureSource::new(scope_device, scope_config)?),
    ];
    let session = CaptureCoordinator::start(sources)?;
    println!(
        "capturing concurrently from {} physical instruments at {SAMPLE_RATE_HZ} Hz",
        session.descriptors().len()
    );

    let deadline = Instant::now() + CAPTURE_TIMEOUT;
    let mut stats = HashMap::<DeviceId, DeviceStats>::new();
    let mut stopped = HashSet::<DeviceId>::new();
    let mut scope_min = [f32::INFINITY; 2];
    let mut scope_max = [f32::NEG_INFINITY; 2];
    while stopped.len() < 2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            session.request_stop();
            session.stop()?;
            return Err("concurrent USB capture did not finish within 11 seconds".into());
        }
        match session.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(CaptureEvent::Frame(frame)) => {
                let entry = stats.entry(frame.device_id).or_default();
                entry.frames += 1;
                entry.samples += frame.payload.sample_count() as u64;
                entry.first_timestamp.get_or_insert(frame.captured_at);
                entry.last_timestamp = Some(frame.captured_at);
                if let CaptureBatch::Scope(batch) = frame.payload {
                    for (index, channel) in batch.channels.iter().enumerate().take(2) {
                        for value in channel {
                            scope_min[index] = scope_min[index].min(*value);
                            scope_max[index] = scope_max[index].max(*value);
                        }
                    }
                }
            }
            Ok(CaptureEvent::SourceFailed { device_id, error }) => {
                session.stop()?;
                return Err(format!("capture source {device_id} failed: {error}").into());
            }
            Ok(CaptureEvent::SourceStopped { device_id }) => {
                stopped.insert(device_id);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                session.stop()?;
                return Err("concurrent USB capture event stream ended unexpectedly".into());
            }
        }
    }
    session.stop()?;

    let logic_stats = stats
        .get(&logic_id)
        .ok_or("DSLogic emitted no capture frames")?;
    let scope_stats = stats
        .get(&scope_id)
        .ok_or("DSCope emitted no capture frames")?;
    let start_skew = duration_difference(
        logic_stats.first_timestamp.unwrap_or_default(),
        scope_stats.first_timestamp.unwrap_or_default(),
    );
    println!("capture complete; shared-timeline start skew {start_skew:?}");
    println!(
        "  {logic_id}: {} frames, {} logic samples, timeline {:?}..{:?}",
        logic_stats.frames,
        logic_stats.samples,
        logic_stats.first_timestamp.unwrap_or_default(),
        logic_stats.last_timestamp.unwrap_or_default()
    );
    println!(
        "  {scope_id}: {} frames, {} samples/channel, timeline {:?}..{:?}, CH0 {:.3}..{:.3} V, CH1 {:.3}..{:.3} V",
        scope_stats.frames,
        scope_stats.samples,
        scope_stats.first_timestamp.unwrap_or_default(),
        scope_stats.last_timestamp.unwrap_or_default(),
        scope_min[0],
        scope_max[0],
        scope_min[1],
        scope_max[1]
    );
    Ok(())
}

fn duration_difference(left: Duration, right: Duration) -> Duration {
    left.max(right) - left.min(right)
}

fn capture_scope(resource_directory: &Path) -> Result<(), Box<dyn Error>> {
    const SAMPLE_RATE_HZ: u64 = 1_000_000;
    const SAMPLE_COUNT: u64 = 1_000_000;
    let config = DsCopeCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)?;
    let report = discover_supported_devices()?;
    let device = report
        .devices
        .into_iter()
        .find(|device| device.profile.model == DeviceModel::DsCopeU3P100)
        .ok_or("no DSCope U3P100 detected")?;

    let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
    let prepared = prepare_device(&device, &bitstream_path)?;
    println!(
        "{} ready: firmware {}, HDL {}; requesting up to {} samples/channel from two channels at {} Hz, factory-calibrated 1 V/div",
        device.location.stable_id(),
        prepared.firmware,
        prepared.hdl,
        config.sample_count(),
        config.sample_rate_hz()
    );

    let source = DsCopeCaptureSource::new(device, config)?;
    let session = CaptureCoordinator::start(vec![Box::new(source)])?;
    let deadline = Instant::now() + Duration::from_secs(11);
    let mut stats = DeviceStats::default();
    let mut channel_min = [f32::INFINITY; 2];
    let mut channel_max = [f32::NEG_INFINITY; 2];
    let mut stopped = false;
    while !stopped {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            session.request_stop();
            session.stop()?;
            return Err("scope capture did not finish within 11 seconds".into());
        }
        match session.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(CaptureEvent::Frame(frame)) => {
                stats.frames += 1;
                stats.samples += frame.payload.sample_count() as u64;
                stats.first_timestamp.get_or_insert(frame.captured_at);
                stats.last_timestamp = Some(frame.captured_at);
                if let CaptureBatch::Scope(batch) = frame.payload {
                    for (index, channel) in batch.channels.iter().enumerate().take(2) {
                        for value in channel {
                            channel_min[index] = channel_min[index].min(*value);
                            channel_max[index] = channel_max[index].max(*value);
                        }
                    }
                }
            }
            Ok(CaptureEvent::SourceFailed { device_id, error }) => {
                session.stop()?;
                return Err(format!("capture source {device_id} failed: {error}").into());
            }
            Ok(CaptureEvent::SourceStopped { .. }) => stopped = true,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => stopped = true,
        }
    }
    session.stop()?;

    println!(
        "capture complete: {} frames, {} samples/channel, CH0 {:.3}..{:.3} V, CH1 {:.3}..{:.3} V",
        stats.frames, stats.samples, channel_min[0], channel_max[0], channel_min[1], channel_max[1]
    );
    Ok(())
}

fn capture_logic(resource_directory: &Path, seconds: u64) -> Result<(), Box<dyn Error>> {
    if seconds == 0 {
        return Err("capture duration must be at least one second".into());
    }
    const SAMPLE_RATE_HZ: u64 = 1_000_000;
    let sample_count = SAMPLE_RATE_HZ
        .checked_mul(seconds)
        .ok_or("capture duration is too large")?;
    let config = DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, sample_count)?;
    let report = discover_supported_devices()?;
    let device = report
        .devices
        .into_iter()
        .find(|device| device.profile.model == DeviceModel::DsLogicU3Pro16)
        .ok_or("no DSLogic U3Pro16 detected")?;

    let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
    let prepared = prepare_device(&device, &bitstream_path)?;
    println!(
        "{} ready: firmware {}, HDL {}; capturing {} samples at {} Hz",
        device.location.stable_id(),
        prepared.firmware,
        prepared.hdl,
        config.sample_count(),
        config.sample_rate_hz()
    );

    let source = DsLogicCaptureSource::new(device, config)?;
    let session = CaptureCoordinator::start(vec![Box::new(source)])?;
    let deadline = Instant::now() + Duration::from_secs(seconds.saturating_add(10));
    let mut stats = DeviceStats::default();
    let mut stopped = false;
    while !stopped {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            session.request_stop();
            session.stop()?;
            return Err(format!(
                "logic capture did not finish within {} seconds",
                seconds + 10
            )
            .into());
        }
        match session.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(CaptureEvent::Frame(frame)) => {
                stats.frames += 1;
                stats.samples += frame.payload.sample_count() as u64;
                stats.first_timestamp.get_or_insert(frame.captured_at);
                stats.last_timestamp = Some(frame.captured_at);
            }
            Ok(CaptureEvent::SourceFailed { device_id, error }) => {
                session.stop()?;
                return Err(format!("capture source {device_id} failed: {error}").into());
            }
            Ok(CaptureEvent::SourceStopped { .. }) => stopped = true,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => stopped = true,
        }
    }
    session.stop()?;

    println!(
        "capture complete: {} frames, {} samples, timeline {:?}..{:?}",
        stats.frames,
        stats.samples,
        stats.first_timestamp.unwrap_or_default(),
        stats.last_timestamp.unwrap_or_default()
    );
    Ok(())
}

fn load_fpgas(resource_directory: &Path) -> Result<(), Box<dyn Error>> {
    let report = discover_supported_devices()?;
    if report.devices.is_empty() {
        println!("no supported instruments detected");
        return Ok(());
    }

    let mut failures = 0_usize;
    for device in report.devices {
        let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
        print!(
            "{}: {} using {}: ",
            device.location.stable_id(),
            device.profile.display_name,
            bitstream_path.display()
        );
        match load_device_fpga(&device, &bitstream_path) {
            Ok(FpgaLoad::AlreadyConfigured(status)) => {
                println!("already configured (status 0x{:02x})", status.raw);
            }
            Ok(FpgaLoad::Configured { bytes, status }) => {
                println!(
                    "configured with {bytes} bytes (status 0x{:02x})",
                    status.raw
                );
            }
            Err(error) => {
                failures += 1;
                eprintln!("failed\n  {error}");
            }
        }
    }

    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} FPGA load(s) failed").into())
    }
}

enum FpgaLoad {
    AlreadyConfigured(HardwareStatus),
    Configured {
        bytes: usize,
        status: HardwareStatus,
    },
}

fn load_device_fpga(
    device: &DiscoveredDevice,
    bitstream_path: &Path,
) -> Result<FpgaLoad, Box<dyn Error>> {
    with_device_session(device, |session| {
        require_compatible_firmware(session)?;
        ensure_fpga_configured(session, bitstream_path)
    })
}

fn prepare_devices(resource_directory: &Path) -> Result<(), Box<dyn Error>> {
    let report = discover_supported_devices()?;
    if report.devices.is_empty() {
        println!("no supported instruments detected");
        return Ok(());
    }

    let mut failures = 0_usize;
    for device in report.devices {
        let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
        print!(
            "{}: {}: ",
            device.location.stable_id(),
            device.profile.display_name
        );
        match prepare_device(&device, &bitstream_path) {
            Ok(prepared) => {
                let fpga = match prepared.fpga {
                    FpgaLoad::AlreadyConfigured(_) => "FPGA already configured".to_owned(),
                    FpgaLoad::Configured { bytes, .. } => {
                        format!("FPGA loaded with {bytes} bytes")
                    }
                };
                println!(
                    "ready; firmware {}, HDL {}, {fpga}",
                    prepared.firmware, prepared.hdl
                );
            }
            Err(error) => {
                failures += 1;
                eprintln!("failed\n  {error}");
            }
        }
    }

    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} device preparation(s) failed").into())
    }
}

struct PreparedDevice {
    firmware: FirmwareVersion,
    hdl: HdlVersion,
    fpga: FpgaLoad,
}

fn prepare_device(
    device: &DiscoveredDevice,
    bitstream_path: &Path,
) -> Result<PreparedDevice, Box<dyn Error>> {
    with_device_session(device, |session| {
        let firmware = require_compatible_firmware(session)?;
        let fpga = ensure_fpga_configured(session, bitstream_path)?;
        let hdl = session.hdl_version()?;
        if !hdl.is_compatible() {
            return Err(format!("HDL {hdl} is incompatible; version 0x0e is required").into());
        }
        Ok(PreparedDevice {
            firmware,
            hdl,
            fpga,
        })
    })
}

fn require_compatible_firmware(
    session: &UsbDeviceSession,
) -> Result<FirmwareVersion, Box<dyn Error>> {
    let firmware = session.firmware_version()?;
    if !firmware.is_compatible() {
        return Err(
            format!("firmware {firmware} is incompatible; major version 2 is required").into(),
        );
    }
    Ok(firmware)
}

fn ensure_fpga_configured(
    session: &UsbDeviceSession,
    bitstream_path: &Path,
) -> Result<FpgaLoad, Box<dyn Error>> {
    let current_status = session.hardware_status()?;
    if current_status.fpga_configured {
        return Ok(FpgaLoad::AlreadyConfigured(current_status));
    }
    let bitstream = std::fs::read(bitstream_path).map_err(|error| {
        format!(
            "failed to read FPGA bitstream {}: {error}",
            bitstream_path.display()
        )
    })?;
    let status = session.configure_fpga(&bitstream)?;
    Ok(FpgaLoad::Configured {
        bytes: bitstream.len(),
        status,
    })
}

fn with_device_session<T>(
    device: &DiscoveredDevice,
    operation: impl FnOnce(&UsbDeviceSession) -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    let session = UsbDeviceSession::open(device)?;
    let operation_result = operation(&session);
    let release_result = session.close();
    match (operation_result, release_result) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(Box::new(error)),
        (Err(operation_error), Err(release_error)) => Err(format!(
            "{operation_error}; additionally failed to release the interface: {release_error}"
        )
        .into()),
    }
}

fn probe_devices() -> Result<(), Box<dyn Error>> {
    let report = discover_supported_devices()?;
    if report.devices.is_empty() {
        println!("no supported instruments detected");
        return Ok(());
    }

    let mut failures = 0_usize;
    for device in report.devices {
        println!(
            "{}: {} at {}",
            device.location.stable_id(),
            device.profile.display_name,
            device.location.platform_path
        );
        match probe_device(&device) {
            Ok(probe) => {
                println!(
                    "  configuration {}, interface {}, alternate setting {}",
                    probe.configuration, probe.interface, probe.alternate_setting
                );
                println!(
                    "  bulk endpoints: OUT 0x{:02x} ({} bytes), IN 0x{:02x} ({} bytes)",
                    probe.bulk_out.address,
                    probe.bulk_out.max_packet_size,
                    probe.bulk_in.address,
                    probe.bulk_in.max_packet_size
                );
                let compatibility = if probe.firmware_version.is_compatible() {
                    "compatible"
                } else {
                    failures += 1;
                    "incompatible"
                };
                println!(
                    "  firmware {}, {compatibility}; interface claim and release succeeded",
                    probe.firmware_version
                );
                println!(
                    "  hardware status 0x{:02x}: FPGA {}, initialized {}, GPIF {}, FIFO {}",
                    probe.hardware_status.raw,
                    status_word(probe.hardware_status.fpga_configured, "configured"),
                    yes_no(probe.hardware_status.fpga_initialized),
                    status_word(probe.hardware_status.gpif_done, "done"),
                    status_word(probe.hardware_status.fifo_overflow, "overflow")
                );
            }
            Err(error) => {
                failures += 1;
                eprintln!("  probe failed: {error}");
            }
        }
    }

    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} device probe(s) failed").into())
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn status_word(value: bool, enabled: &'static str) -> &'static str {
    if value {
        enabled
    } else {
        "not set"
    }
}

fn list_devices() -> Result<(), Box<dyn Error>> {
    let report = discover_supported_devices()?;
    if report.devices.is_empty() {
        println!("no supported instruments detected");
    }
    for device in report.devices {
        println!(
            "{}: {} at {} ({})",
            device.location.stable_id(),
            device.profile.display_name,
            device.location.platform_path,
            device.speed.description()
        );
        match device.link_status() {
            LinkStatus::Optimal => {}
            LinkStatus::ReducedPerformance => eprintln!(
                "warning: {} is using {}; connect it to a USB 3 port with a USB 3 cable for full capture performance",
                device.profile.display_name,
                device.speed.description()
            ),
            LinkStatus::Unsupported => eprintln!(
                "error: {} is using {}; {} or faster is required, and USB 3 is recommended",
                device.profile.display_name,
                device.speed.description(),
                device.profile.minimum_speed.description()
            ),
        }
    }
    Ok(())
}

fn run_simulation(run_seconds: u64) -> Result<(), Box<dyn Error>> {
    let logic = SimulatedLogicAnalyzer::new("sim-logic-0", 100_000, 8, 1_024)?;
    let scope = SimulatedOscilloscope::new("sim-scope-0", 50_000, 2, 512)?;
    let session = CaptureCoordinator::start(vec![Box::new(logic), Box::new(scope)])?;

    println!(
        "capturing from {} instruments for {run_seconds}s",
        session.descriptors().len()
    );
    for descriptor in session.descriptors() {
        println!(
            "  {}: {} ({:?})",
            descriptor.id, descriptor.name, descriptor.kind
        );
    }

    let deadline = Instant::now() + Duration::from_secs(run_seconds);
    let mut stats = HashMap::<DeviceId, DeviceStats>::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match session.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(CaptureEvent::Frame(frame)) => {
                let entry = stats.entry(frame.device_id).or_default();
                entry.frames += 1;
                entry.samples += frame.payload.sample_count() as u64;
                entry.first_timestamp.get_or_insert(frame.captured_at);
                entry.last_timestamp = Some(frame.captured_at);
            }
            Ok(CaptureEvent::SourceFailed { device_id, error }) => {
                return Err(format!("capture source {device_id} failed: {error}").into());
            }
            Ok(CaptureEvent::SourceStopped { device_id }) => {
                println!("capture source {device_id} stopped");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    session.stop()?;

    println!("capture summary:");
    for (device_id, device_stats) in stats {
        println!(
            "  {device_id}: {} frames, {} samples, timeline {:?}..{:?}",
            device_stats.frames,
            device_stats.samples,
            device_stats.first_timestamp.unwrap_or_default(),
            device_stats.last_timestamp.unwrap_or_default()
        );
    }
    Ok(())
}
