use dsview_core::{CaptureCoordinator, CaptureEvent, DeviceId};
use dsview_sim::{SimulatedLogicAnalyzer, SimulatedOscilloscope};
use dsview_usb::discover_supported_devices;
use std::collections::HashMap;
use std::error::Error;
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

    let run_seconds = match arguments.as_slice() {
        [] => 2,
        [seconds] => seconds.parse::<u64>()?,
        [command, seconds] if command == "simulate" => seconds.parse::<u64>()?,
        _ => return Err("usage: dsview-cli [list | simulate <seconds> | <seconds>]".into()),
    };

    run_simulation(run_seconds)
}

fn list_devices() -> Result<(), Box<dyn Error>> {
    let report = discover_supported_devices()?;
    if report.devices.is_empty() {
        println!("no supported instruments detected");
    }
    for device in report.devices {
        println!(
            "{}: {} at {} ({:?})",
            device.location.stable_id(),
            device.profile.display_name,
            device.location.platform_path,
            device.speed
        );
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
