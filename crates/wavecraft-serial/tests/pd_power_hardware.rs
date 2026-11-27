//! Opt-in, read-only verification against a physically connected supply.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};
use wavecraft_core::{CaptureBatch, CaptureCoordinator, CaptureEvent};
use wavecraft_serial::pd_power::{Configuration, PowerSource, Request, Response};

#[test]
#[ignore = "requires WEACT_PD_PORT and a connected WeAct PD Power Mini"]
fn reads_configuration_and_live_telemetry_over_usb() {
    let port = std::env::var("WEACT_PD_PORT").expect("set WEACT_PD_PORT to the USB CDC port");
    let (source, control) = PowerSource::new(Configuration {
        port: port.clone(),
        baud_rate: 115200,
        crc8: false,
        poll_hz: 10.0,
    })
    .unwrap();
    let session = CaptureCoordinator::start(vec![Box::new(source)]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let settings = loop {
        if let Ok(response) = control.responses.try_recv() {
            match response {
                Response::Settings(settings) => break settings,
                other => panic!("unexpected initialization response: {other:?}"),
            }
        }
        match session.recv_timeout(Duration::from_millis(20)) {
            Ok(CaptureEvent::SourceFailed { error, .. }) => panic!("{error}"),
            Ok(CaptureEvent::SourceStopped { .. }) => {
                panic!("source stopped before initialization")
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "initialization timed out");
    };
    println!("Port: {port}\nModel: {}\nFirmware: {}\nPreset: M{}\nPresets (mV/mA): {:?}\nSettings: {:?}\nTimer: {:?}\nLCD: {:?}\nFactory calibration available: {}", settings.model, settings.version, settings.preset, settings.presets, settings.values, settings.timer, settings.lcd_type, settings.factory_data.is_some());
    for rate in [10.0, 20.0, 50.0, 100.0] {
        control.requests.send(Request::PollRate(rate)).unwrap();
        let mut first = None;
        let mut previous = None;
        let mut last = None;
        for _ in 0..30 {
            match session.recv_timeout(Duration::from_secs(2)).unwrap() {
                CaptureEvent::Frame(frame) => {
                    let CaptureBatch::Power(sample) = frame.payload else {
                        panic!("unexpected payload")
                    };
                    if let Some(previous) = previous {
                        assert_eq!(frame.sequence, previous + 1);
                    }
                    previous = Some(frame.sequence);
                    first.get_or_insert(frame.captured_at);
                    last = Some((frame.captured_at, sample));
                }
                event => panic!("unexpected capture event: {event:?}"),
            }
        }
        let (time, sample) = last.unwrap();
        let actual = 29.0 / (time - first.unwrap()).as_secs_f64();
        println!("Requested {rate:.0} Hz; measured {actual:.2} Hz; Vout={:.3} V; Iout={:.3} A; Vin={:.3} V; PD request={:.1} V; output_state=0x{:02X}; input_state={}", f64::from(sample.voltage_mv)/1000.0, f64::from(sample.current_ma)/1000.0, f64::from(sample.input_voltage_mv)/1000.0, f64::from(sample.requested_pd_decivolts)/10.0, sample.output_state, sample.input_state);
    }
    session.stop().unwrap();
    println!("PASS: configuration, telemetry, rate changes, ordered frames, disconnect; no device write commands sent.");
}
