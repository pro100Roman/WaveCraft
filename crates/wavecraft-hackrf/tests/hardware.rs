use std::time::Duration;
use wavecraft_core::{CaptureBatch, CaptureCoordinator, CaptureEvent};
use wavecraft_hackrf::HackRfAdapter;
use wavecraft_sdr::{
    HackRfSettings, RadioCaptureSettings, RadioProviderAdapter, RadioProviderSettings,
};

#[test]
#[ignore = "requires a connected HackRF One and installed libhackrf"]
fn discovers_and_receives_from_hackrf_one() {
    let adapter = HackRfAdapter::new();
    let device = adapter
        .discover()
        .expect("HackRF discovery should succeed")
        .into_iter()
        .next()
        .expect("a HackRF One should be connected");
    let source = adapter
        .open(
            &device,
            RadioCaptureSettings {
                center_frequency_hz: 100_000_000,
                sample_rate_hz: 2_000_000,
                bandwidth_hz: 1_750_000,
                receive_channel: 0,
                sample_limit: Some(4_096),
            },
            RadioProviderSettings::HackRf(HackRfSettings::default()),
        )
        .expect("HackRF source should open");
    let session = CaptureCoordinator::start(vec![source]).expect("capture should start");
    let frame = match session
        .recv_timeout(Duration::from_secs(5))
        .expect("HackRF should produce a capture event")
    {
        CaptureEvent::Frame(frame) => frame,
        CaptureEvent::SourceFailed { error, .. } => panic!("HackRF capture failed: {error}"),
        CaptureEvent::SourceStopped { .. } => panic!("HackRF stopped before producing IQ"),
    };
    let CaptureBatch::Iq(batch) = frame.payload else {
        panic!("HackRF should emit IQ batches");
    };
    assert!(!batch.samples.is_empty());
    assert_eq!(batch.sample_rate_hz, 2_000_000);
    session.stop().expect("HackRF should stop cleanly");
}
