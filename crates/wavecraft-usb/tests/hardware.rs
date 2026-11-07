// Integration tests may unwrap; the library lint exists for production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use wavecraft_core::{CaptureCoordinator, CaptureEvent, CaptureSession, InstrumentKind};
use wavecraft_usb::{
    discover_supported_devices, prepare_device, BulkReadOutcome, DeviceModel, DiscoveredDevice,
    DsCopeCaptureConfig, DsCopeCaptureSource, DsLogicCaptureConfig, DsLogicCaptureSource,
    DsLogicOperationMode, UsbDeviceSession,
};

/// The tests share one libusb context and real devices, so they run one at a
/// time even when the harness uses several threads.
static HARDWARE: Mutex<()> = Mutex::new(());

fn exclusive_hardware() -> MutexGuard<'static, ()> {
    HARDWARE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Finds the first connected device accepted by `wanted` and loads its FPGA
/// bitstream so it is ready to capture.
fn prepared_device(
    description: &str,
    wanted: impl Fn(&DiscoveredDevice) -> bool,
) -> DiscoveredDevice {
    let report = discover_supported_devices().expect("USB discovery should succeed");
    let device = report
        .devices
        .into_iter()
        .find(|device| wanted(device))
        .unwrap_or_else(|| panic!("a connected {description} is required"));
    let bitstream = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../resources/fpga")
        .join(device.profile.fpga_bitstream_name);
    prepare_device(&device, &bitstream).expect("the device should prepare");
    device
}

fn dslogic() -> DiscoveredDevice {
    prepared_device("DSLogic logic analyzer", |device| {
        device.profile.instrument_kind == InstrumentKind::LogicAnalyzer
    })
}

fn dscope() -> DiscoveredDevice {
    prepared_device("DSCope U3P100", |device| {
        device.profile.model == DeviceModel::DsCopeU3P100
    })
}

#[derive(Debug, Default)]
struct Observed {
    frames: usize,
    samples: usize,
    stopped: bool,
}

/// Reads capture events until `done` is satisfied or the source stops. A
/// failed source, a closed stream, or the timeout fails the test.
fn observe(
    session: &CaptureSession,
    timeout: Duration,
    mut done: impl FnMut(&Observed) -> bool,
) -> Observed {
    let deadline = Instant::now() + timeout;
    let mut observed = Observed::default();
    while !observed.stopped && !done(&observed) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match session.recv_timeout(remaining) {
            Ok(CaptureEvent::Frame(frame)) => {
                observed.frames += 1;
                observed.samples += frame.payload.sample_count();
            }
            Ok(CaptureEvent::SourceFailed { error, .. }) => {
                panic!("the capture source failed after {observed:?}: {error}")
            }
            Ok(CaptureEvent::SourceStopped { .. }) => observed.stopped = true,
            Err(error) => panic!("no capture event after {observed:?}: {error}"),
        }
    }
    observed
}

/// Lets a capture that runs until stopped deliver a few frames, stops it, and
/// returns what arrived. Stopping has to cancel every queued transfer and end
/// the source without an error.
fn run_until_stopped(session: CaptureSession, minimum_frames: usize) -> Observed {
    let running = observe(&session, Duration::from_secs(20), |observed| {
        observed.frames >= minimum_frames
    });
    assert!(
        !running.stopped,
        "the capture stopped before it was asked to: {running:?}"
    );
    session.request_stop();
    let finished = observe(&session, Duration::from_secs(10), |_| false);
    assert!(finished.stopped, "the source should report that it stopped");
    session.stop().expect("the capture workers should join");
    Observed {
        frames: running.frames + finished.frames,
        samples: running.samples + finished.samples,
        stopped: true,
    }
}

#[test]
#[ignore = "requires a supported DSLogic with onboard buffer connected and idle"]
fn dslogic_buffer_capture_delivers_every_sample() {
    let _hardware = exclusive_hardware();
    let device = dslogic();
    let config = DsLogicCaptureConfig::new(1_000_000, 65_536).expect("configuration is valid");
    let source = DsLogicCaptureSource::new(device, config).expect("the source should be valid");

    let session = CaptureCoordinator::start(vec![Box::new(source)]).expect("capture should start");
    let observed = observe(&session, Duration::from_secs(20), |_| false);

    assert!(observed.stopped);
    assert_eq!(observed.samples, 65_536);
    session.stop().expect("the capture workers should join");
}

#[test]
#[ignore = "requires a supported DSLogic connected and idle"]
fn dslogic_loop_stream_capture_runs_until_stopped() {
    let _hardware = exclusive_hardware();
    let device = dslogic();
    let config = DsLogicCaptureConfig::new(1_000_000, 65_536)
        .expect("configuration is valid")
        .with_operation_mode(DsLogicOperationMode::Stream)
        .with_loop_mode(true);
    let source = DsLogicCaptureSource::new(device, config).expect("the source should be valid");

    let session = CaptureCoordinator::start(vec![Box::new(source)]).expect("capture should start");
    let observed = run_until_stopped(session, 3);

    assert!(observed.samples > 0);
}

#[test]
#[ignore = "requires a supported DSCope U3P100 connected and idle"]
fn dscope_continuous_capture_runs_until_stopped() {
    let _hardware = exclusive_hardware();
    let device = dscope();
    let config = DsCopeCaptureConfig::new(1_000_000, 100_000).expect("configuration is valid");
    let source = DsCopeCaptureSource::new(device, config)
        .expect("the source should be valid")
        .with_continuous_capture();

    let session = CaptureCoordinator::start(vec![Box::new(source)]).expect("capture should start");
    let observed = run_until_stopped(session, 3);

    assert!(observed.samples > 0);
}

#[test]
#[ignore = "requires supported USB instruments connected and idle"]
fn idle_bulk_reads_cancel_promptly() {
    let report = discover_supported_devices().expect("USB discovery should succeed");
    assert!(
        !report.devices.is_empty(),
        "at least one supported USB instrument is required"
    );

    for device in report.devices {
        let session = UsbDeviceSession::open(&device).expect("device should open");
        let started = Instant::now();
        let cancel_at = started + Duration::from_millis(125);
        let mut buffer = vec![0_u8; 1_024];
        let outcome = session
            .read_bulk_cancellable(&mut buffer, || Instant::now() >= cancel_at)
            .expect("idle bulk read should cancel cleanly");

        assert_eq!(outcome, BulkReadOutcome::Cancelled);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "bulk cancellation took too long for {}",
            device.location.stable_id()
        );
        session.close().expect("interface should release cleanly");
    }
}
