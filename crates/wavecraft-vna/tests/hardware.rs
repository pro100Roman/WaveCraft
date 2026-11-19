#![allow(clippy::expect_used, clippy::unwrap_used)]

use wavecraft_vna::{discover_litevna_devices, LiteVnaSession, SweepConfig};

#[test]
#[ignore = "requires a connected LiteVNA"]
fn acquires_multiple_litevna_sweeps_in_one_session() {
    let devices = discover_litevna_devices().expect("serial-port discovery should succeed");
    let device = devices.first().expect("a LiteVNA should be connected");
    let mut session =
        LiteVnaSession::open(&device.port_name).expect("the LiteVNA session should open");
    let config = SweepConfig {
        start_hz: 1_000_000,
        stop_hz: 10_000_000,
        points: 16,
        averaging: 1,
    };
    session
        .enter_usb_sweep_mode(config)
        .expect("Open should enter USB mode without acquiring a displayed sweep");
    let sweep = session
        .acquire_sweep(config)
        .expect("the first LiteVNA sweep should complete");
    let second = session
        .acquire_sweep(config)
        .expect("the second LiteVNA sweep should reuse the session");

    assert_eq!(sweep.identity.device_variant, 2);
    assert_eq!(sweep.identity.protocol_version, 1);
    assert_eq!(sweep.points.len(), 16);
    assert_eq!(second.identity, sweep.identity);
    assert_eq!(second.points.len(), 16);
    assert!(sweep
        .points
        .windows(2)
        .all(|pair| pair[0].frequency_hz < pair[1].frequency_hz));
    assert!(sweep.points.iter().all(|point| {
        point.s11_real.is_finite()
            && point.s11_imaginary.is_finite()
            && point.s21_real.is_finite()
            && point.s21_imaginary.is_finite()
    }));
}
