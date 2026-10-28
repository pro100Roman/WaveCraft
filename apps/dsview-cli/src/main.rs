use dsview_core::{CaptureBatch, CaptureCoordinator, CaptureEvent, CaptureSource, DeviceId};
use dsview_recording::{CompletionStatus, RecordingReader, RecordingWriter, FORMAT_VERSION};
use dsview_sim::{SimulatedLogicAnalyzer, SimulatedOscilloscope};
use dsview_usb::{
    discover_supported_devices, load_device_fpga, prepare_device, probe_device, DeviceModel,
    DsCopeCaptureConfig, DsCopeCaptureSource, DsLogicCaptureConfig, DsLogicCaptureSource, FpgaLoad,
    LinkStatus,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: dsview-cli [list | probe | load-fpga <resource-directory> | prepare <resource-directory> | capture-logic <resource-directory> [seconds] [--output <capture.dsv>] | capture-scope <resource-directory> [--output <capture.dsv>] | capture-usb <resource-directory> [--output <capture.dsv>] | inspect <capture.dsv> | simulate <seconds> [--output <capture.dsv>] | <seconds>]";

type FileRecorder = RecordingWriter<BufWriter<File>>;

#[derive(Default)]
struct DeviceStats {
    frames: u64,
    samples: u64,
    first_timestamp: Option<Duration>,
    last_timestamp: Option<Duration>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [command] if command == "list" => return list_devices(),
        [command] if command == "probe" => return probe_devices(),
        [command, path] if command == "inspect" => return inspect_recording(Path::new(path)),
        [command, resource_directory] if command == "load-fpga" => {
            return load_fpgas(Path::new(resource_directory));
        }
        [command, resource_directory] if command == "prepare" => {
            return prepare_devices(Path::new(resource_directory));
        }
        [command, resource_directory] if command == "capture-logic" => {
            return capture_logic(Path::new(resource_directory), 1, None);
        }
        [command, resource_directory, seconds] if command == "capture-logic" => {
            return capture_logic(Path::new(resource_directory), seconds.parse()?, None);
        }
        [command, resource_directory, flag, output]
            if command == "capture-logic" && flag == "--output" =>
        {
            return capture_logic(Path::new(resource_directory), 1, Some(Path::new(output)));
        }
        [command, resource_directory, seconds, flag, output]
            if command == "capture-logic" && flag == "--output" =>
        {
            return capture_logic(
                Path::new(resource_directory),
                seconds.parse()?,
                Some(Path::new(output)),
            );
        }
        [command, resource_directory] if command == "capture-scope" => {
            return capture_scope(Path::new(resource_directory), None);
        }
        [command, resource_directory, flag, output]
            if command == "capture-scope" && flag == "--output" =>
        {
            return capture_scope(Path::new(resource_directory), Some(Path::new(output)));
        }
        [command, resource_directory] if command == "capture-usb" => {
            return capture_usb(Path::new(resource_directory), None);
        }
        [command, resource_directory, flag, output]
            if command == "capture-usb" && flag == "--output" =>
        {
            return capture_usb(Path::new(resource_directory), Some(Path::new(output)));
        }
        [command, seconds, flag, output] if command == "simulate" && flag == "--output" => {
            return run_simulation(seconds.parse()?, Some(Path::new(output)));
        }
        _ => {}
    }

    let run_seconds = match arguments.as_slice() {
        [] => 2,
        [seconds] => seconds.parse::<u64>()?,
        [command, seconds] if command == "simulate" => seconds.parse::<u64>()?,
        _ => return Err(USAGE.into()),
    };

    run_simulation(run_seconds, None)
}

fn capture_usb(resource_directory: &Path, output: Option<&Path>) -> Result<(), Box<dyn Error>> {
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
    let mut metadata = capture_metadata("capture-usb", SAMPLE_RATE_HZ, SAMPLE_COUNT);
    metadata.insert("scope.volts_per_div".to_owned(), "1".to_owned());
    let mut recorder = start_recording(output, session.descriptors(), metadata)?;
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
            let stop_result = session.stop();
            finish_recording(&mut recorder, CompletionStatus::Cancelled)?;
            stop_result?;
            return Err("concurrent USB capture did not finish within 11 seconds".into());
        }
        match session.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(event) => {
                write_recording_event(&mut recorder, &event)?;
                match event {
                    CaptureEvent::Frame(frame) => {
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
                    CaptureEvent::SourceFailed { device_id, error } => {
                        let stop_result = session.stop();
                        finish_recording(&mut recorder, CompletionStatus::Failed)?;
                        stop_result?;
                        return Err(format!("capture source {device_id} failed: {error}").into());
                    }
                    CaptureEvent::SourceStopped { device_id } => {
                        stopped.insert(device_id);
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let stop_result = session.stop();
                finish_recording(&mut recorder, CompletionStatus::Failed)?;
                stop_result?;
                return Err("concurrent USB capture event stream ended unexpectedly".into());
            }
        }
    }
    let stop_result = session.stop();
    let completion = if stop_result.is_ok() {
        CompletionStatus::Complete
    } else {
        CompletionStatus::Failed
    };
    finish_recording(&mut recorder, completion)?;
    stop_result?;

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

fn capture_metadata(
    command: &str,
    sample_rate_hz: u64,
    sample_count: u64,
) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert("capture.command".to_owned(), command.to_owned());
    metadata.insert(
        "capture.sample_rate_hz".to_owned(),
        sample_rate_hz.to_string(),
    );
    metadata.insert(
        "capture.requested_sample_count".to_owned(),
        sample_count.to_string(),
    );
    metadata
}

fn start_recording(
    output: Option<&Path>,
    devices: &[dsview_core::DeviceDescriptor],
    metadata: BTreeMap<String, String>,
) -> Result<Option<FileRecorder>, Box<dyn Error>> {
    let Some(path) = output else {
        return Ok(None);
    };
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to create capture file {} without overwriting an existing file: {error}",
                path.display()
            )
        })?;
    let recorder = RecordingWriter::new(BufWriter::new(file), devices, metadata)?;
    println!("recording full capture to {}", path.display());
    Ok(Some(recorder))
}

fn write_recording_event(
    recorder: &mut Option<FileRecorder>,
    event: &CaptureEvent,
) -> Result<(), Box<dyn Error>> {
    if let Some(recorder) = recorder {
        recorder.write_event(event)?;
    }
    Ok(())
}

fn finish_recording(
    recorder: &mut Option<FileRecorder>,
    status: CompletionStatus,
) -> Result<(), Box<dyn Error>> {
    if let Some(recorder) = recorder {
        recorder.finish(status)?;
    }
    Ok(())
}

fn inspect_recording(path: &Path) -> Result<(), Box<dyn Error>> {
    let file = File::open(path)
        .map_err(|error| format!("failed to open recording {}: {error}", path.display()))?;
    let mut reader = RecordingReader::new(BufReader::new(file))?;
    println!(
        "DSView recording format {}, {} device(s)",
        FORMAT_VERSION,
        reader.header().devices.len()
    );
    for device in &reader.header().devices {
        println!("  {}: {} ({:?})", device.id, device.name, device.kind);
    }
    for (key, value) in &reader.header().metadata {
        println!("  {key}: {value}");
    }

    let mut stats = HashMap::<DeviceId, DeviceStats>::new();
    let mut failures = Vec::new();
    let mut stopped = HashSet::new();
    while let Some(event) = reader.read_event()? {
        match event {
            CaptureEvent::Frame(frame) => {
                let entry = stats.entry(frame.device_id).or_default();
                entry.frames += 1;
                entry.samples += frame.payload.sample_count() as u64;
                entry.first_timestamp.get_or_insert(frame.captured_at);
                entry.last_timestamp = Some(frame.captured_at);
            }
            CaptureEvent::SourceFailed { device_id, error } => {
                failures.push(format!("{device_id}: {error}"));
            }
            CaptureEvent::SourceStopped { device_id } => {
                stopped.insert(device_id);
            }
        }
    }

    let completion = reader
        .completion_status()
        .ok_or("recording has no completion status")?;
    println!(
        "completion: {completion:?}; {} source(s) stopped",
        stopped.len()
    );
    for device in &reader.header().devices {
        if let Some(device_stats) = stats.get(&device.id) {
            println!(
                "  {}: {} frames, {} samples, timeline {:?}..{:?}",
                device.id,
                device_stats.frames,
                device_stats.samples,
                device_stats.first_timestamp.unwrap_or_default(),
                device_stats.last_timestamp.unwrap_or_default()
            );
        } else {
            println!("  {}: no sample frames", device.id);
        }
    }
    for failure in failures {
        println!("  source failure: {failure}");
    }
    Ok(())
}

fn capture_scope(resource_directory: &Path, output: Option<&Path>) -> Result<(), Box<dyn Error>> {
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
    let mut metadata = capture_metadata("capture-scope", SAMPLE_RATE_HZ, SAMPLE_COUNT);
    metadata.insert("scope.volts_per_div".to_owned(), "1".to_owned());
    let mut recorder = start_recording(output, session.descriptors(), metadata)?;
    let deadline = Instant::now() + Duration::from_secs(11);
    let mut stats = DeviceStats::default();
    let mut channel_min = [f32::INFINITY; 2];
    let mut channel_max = [f32::NEG_INFINITY; 2];
    let mut stopped = false;
    while !stopped {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            session.request_stop();
            let stop_result = session.stop();
            finish_recording(&mut recorder, CompletionStatus::Cancelled)?;
            stop_result?;
            return Err("scope capture did not finish within 11 seconds".into());
        }
        match session.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(event) => {
                write_recording_event(&mut recorder, &event)?;
                match event {
                    CaptureEvent::Frame(frame) => {
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
                    CaptureEvent::SourceFailed { device_id, error } => {
                        let stop_result = session.stop();
                        finish_recording(&mut recorder, CompletionStatus::Failed)?;
                        stop_result?;
                        return Err(format!("capture source {device_id} failed: {error}").into());
                    }
                    CaptureEvent::SourceStopped { .. } => stopped = true,
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let stop_result = session.stop();
                finish_recording(&mut recorder, CompletionStatus::Failed)?;
                stop_result?;
                return Err("scope capture event stream ended unexpectedly".into());
            }
        }
    }
    let stop_result = session.stop();
    let completion = if stop_result.is_ok() {
        CompletionStatus::Complete
    } else {
        CompletionStatus::Failed
    };
    finish_recording(&mut recorder, completion)?;
    stop_result?;

    println!(
        "capture complete: {} frames, {} samples/channel, CH0 {:.3}..{:.3} V, CH1 {:.3}..{:.3} V",
        stats.frames, stats.samples, channel_min[0], channel_max[0], channel_min[1], channel_max[1]
    );
    Ok(())
}

fn capture_logic(
    resource_directory: &Path,
    seconds: u64,
    output: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
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
    let metadata = capture_metadata("capture-logic", SAMPLE_RATE_HZ, sample_count);
    let mut recorder = start_recording(output, session.descriptors(), metadata)?;
    let deadline = Instant::now() + Duration::from_secs(seconds.saturating_add(10));
    let mut stats = DeviceStats::default();
    let mut stopped = false;
    while !stopped {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            session.request_stop();
            let stop_result = session.stop();
            finish_recording(&mut recorder, CompletionStatus::Cancelled)?;
            stop_result?;
            return Err(format!(
                "logic capture did not finish within {} seconds",
                seconds + 10
            )
            .into());
        }
        match session.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(event) => {
                write_recording_event(&mut recorder, &event)?;
                match event {
                    CaptureEvent::Frame(frame) => {
                        stats.frames += 1;
                        stats.samples += frame.payload.sample_count() as u64;
                        stats.first_timestamp.get_or_insert(frame.captured_at);
                        stats.last_timestamp = Some(frame.captured_at);
                    }
                    CaptureEvent::SourceFailed { device_id, error } => {
                        let stop_result = session.stop();
                        finish_recording(&mut recorder, CompletionStatus::Failed)?;
                        stop_result?;
                        return Err(format!("capture source {device_id} failed: {error}").into());
                    }
                    CaptureEvent::SourceStopped { .. } => stopped = true,
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let stop_result = session.stop();
                finish_recording(&mut recorder, CompletionStatus::Failed)?;
                stop_result?;
                return Err("logic capture event stream ended unexpectedly".into());
            }
        }
    }
    let stop_result = session.stop();
    let completion = if stop_result.is_ok() {
        CompletionStatus::Complete
    } else {
        CompletionStatus::Failed
    };
    finish_recording(&mut recorder, completion)?;
    stop_result?;

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

fn run_simulation(run_seconds: u64, output: Option<&Path>) -> Result<(), Box<dyn Error>> {
    let logic = SimulatedLogicAnalyzer::new("sim-logic-0", 100_000, 8, 1_024)?;
    let scope = SimulatedOscilloscope::new("sim-scope-0", 50_000, 2, 512)?;
    let session = CaptureCoordinator::start(vec![Box::new(logic), Box::new(scope)])?;
    let mut metadata = BTreeMap::new();
    metadata.insert("capture.command".to_owned(), "simulate".to_owned());
    metadata.insert(
        "capture.duration_seconds".to_owned(),
        run_seconds.to_string(),
    );
    let mut recorder = start_recording(output, session.descriptors(), metadata)?;

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
            Ok(event) => {
                write_recording_event(&mut recorder, &event)?;
                match event {
                    CaptureEvent::Frame(frame) => {
                        let entry = stats.entry(frame.device_id).or_default();
                        entry.frames += 1;
                        entry.samples += frame.payload.sample_count() as u64;
                        entry.first_timestamp.get_or_insert(frame.captured_at);
                        entry.last_timestamp = Some(frame.captured_at);
                    }
                    CaptureEvent::SourceFailed { device_id, error } => {
                        let stop_result = session.stop();
                        finish_recording(&mut recorder, CompletionStatus::Failed)?;
                        stop_result?;
                        return Err(format!("capture source {device_id} failed: {error}").into());
                    }
                    CaptureEvent::SourceStopped { device_id } => {
                        println!("capture source {device_id} stopped");
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let stop_result = session.stop();
    let completion = if stop_result.is_ok() {
        CompletionStatus::Complete
    } else {
        CompletionStatus::Failed
    };
    finish_recording(&mut recorder, completion)?;
    stop_result?;

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
