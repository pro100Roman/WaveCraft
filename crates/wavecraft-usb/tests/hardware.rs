// Integration tests may unwrap; the library lint exists for production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};
use wavecraft_usb::{discover_supported_devices, BulkReadOutcome, UsbDeviceSession};

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
