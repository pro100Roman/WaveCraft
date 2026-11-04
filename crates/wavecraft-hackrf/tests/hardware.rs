use std::time::Duration;
use wavecraft_core::{CaptureBatch, CaptureCoordinator, CaptureEvent};
use wavecraft_hackrf::{HackRfAdapter, HackRfRuntimeSettings};
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
    let (source, control) = adapter
        .open_with_runtime_control(
            &device,
            RadioCaptureSettings {
                center_frequency_hz: 100_000_000,
                sample_rate_hz: 2_000_000,
                bandwidth_hz: 1_750_000,
                receive_channel: 0,
                sample_limit: None,
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
    control
        .update(HackRfRuntimeSettings {
            center_frequency_hz: 101_000_000,
            bandwidth_hz: 2_500_000,
            lna_gain_db: Some(24),
            vga_gain_db: Some(20),
            amplifier_enabled: false,
            frequency_correction_milli_ppm: 0,
        })
        .expect("live HackRF settings should be accepted");
    let retuned = loop {
        match session
            .recv_timeout(Duration::from_secs(5))
            .expect("HackRF should produce IQ after retuning")
        {
            CaptureEvent::Frame(frame) => {
                let CaptureBatch::Iq(batch) = frame.payload else {
                    continue;
                };
                if batch.center_frequency_hz == 101_000_000 {
                    break batch;
                }
            }
            CaptureEvent::SourceFailed { error, .. } => {
                panic!("HackRF live update failed: {error}")
            }
            CaptureEvent::SourceStopped { .. } => {
                panic!("HackRF stopped before applying live settings")
            }
        }
    };
    assert!(retuned.discontinuity);
    session.stop().expect("HackRF should stop cleanly");
}
