use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use wavecraft_automation::{
    AutomationManager, CaptureConfiguration, DeviceBackend, DeviceSelector, InstrumentKind,
};
use wavecraft_recording::{CompletionStatus, RecordingReader};

struct TestRecording(PathBuf);

impl TestRecording {
    fn new() -> Self {
        static NEXT_PATH: AtomicU64 = AtomicU64::new(1);
        Self(std::env::temp_dir().join(format!(
            "wavecraft-usb-automation-{}-{}.dsv",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRecording {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
#[ignore = "requires a supported DSLogic instrument connected and idle"]
fn selected_dslogic_completes_the_automation_capture_contract() {
    let resource_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/fpga");
    let manager = AutomationManager::new().with_resource_directory(resource_directory);
    let devices = manager
        .list_devices()
        .expect("USB discovery should succeed");
    let device = devices
        .iter()
        .find(|device| {
            device.backend == DeviceBackend::Usb
                && device.kind == InstrumentKind::LogicAnalyzer
                && device.capabilities.supports_capture
        })
        .cloned()
        .unwrap_or_else(|| {
            panic!("a supported DSLogic instrument is required; found {devices:#?}")
        });
    let connection_id = device.connection_id.clone();
    let channels = (0..device.capabilities.channel_count.min(8)).collect::<Vec<_>>();
    let selected = manager
        .open_device(DeviceSelector::connection_id(&connection_id))
        .expect("the exact DSLogic connection should open");
    let recording = TestRecording::new();
    let configuration =
        CaptureConfiguration::finite_logic(1_000_000, 65_536, channels, recording.path())
            .with_timeout(Duration::from_secs(20));

    let completed = selected
        .start_capture(configuration)
        .expect("DSLogic capture should start")
        .wait()
        .expect("DSLogic capture should complete");

    assert_eq!(completed.summary.device.connection_id, connection_id);
    assert_eq!(completed.summary.captured_samples, 65_536);
    let file = File::open(recording.path()).expect("recording should exist");
    let mut reader =
        RecordingReader::new(BufReader::new(file)).expect("recording header should be valid");
    while reader
        .read_event()
        .expect("recording event should be valid")
        .is_some()
    {}
    assert_eq!(reader.completion_status(), Some(CompletionStatus::Complete));
}

#[test]
#[ignore = "requires a supported DSCope U3P100 connected and idle"]
fn selected_dscope_completes_the_automation_capture_contract() {
    let resource_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/fpga");
    let manager = AutomationManager::new().with_resource_directory(resource_directory);
    let devices = manager
        .list_devices()
        .expect("USB discovery should succeed");
    let device = devices
        .iter()
        .find(|device| {
            device.backend == DeviceBackend::Usb
                && device.kind == InstrumentKind::Oscilloscope
                && device.capabilities.supports_capture
        })
        .cloned()
        .unwrap_or_else(|| panic!("a supported DSCope U3P100 is required; found {devices:#?}"));
    let connection_id = device.connection_id.clone();
    let selected = manager
        .open_device(DeviceSelector::connection_id(&connection_id))
        .expect("the exact DSCope connection should open");
    let recording = TestRecording::new();
    let configuration =
        CaptureConfiguration::finite_scope(1_000_000, 100_000, vec![0, 1], recording.path())
            .with_timeout(Duration::from_secs(20));

    let completed = selected
        .start_capture(configuration)
        .expect("DSCope capture should start")
        .wait()
        .expect("DSCope capture should complete");

    assert_eq!(completed.summary.device.connection_id, connection_id);
    assert_eq!(completed.summary.captured_samples, 100_000);
    let file = File::open(recording.path()).expect("recording should exist");
    let mut reader =
        RecordingReader::new(BufReader::new(file)).expect("recording header should be valid");
    while reader
        .read_event()
        .expect("recording event should be valid")
        .is_some()
    {}
    assert_eq!(reader.completion_status(), Some(CompletionStatus::Complete));
}
