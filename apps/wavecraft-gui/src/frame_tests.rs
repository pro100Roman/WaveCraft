//! Headless frame tests.
//!
//! These run `WaveCraftApp::run_frame` on a bare `egui::Context` with fake
//! devices, so every panel `update()` paints is executed without a window or
//! hardware. They catch panics and state changes in the per-frame code. They do
//! not click anything: egui widgets are not reachable from here, so the
//! interaction paths are covered only by calling the apply methods directly.

use super::*;
use crate::ui::{
    frame_actions::FrameActions, scope_frontend_changes::ScopeFrontendInputs,
    toolbar::ToolbarInputs,
};
use wavecraft_core::{DeviceDescriptor, DeviceId, InstrumentKind};
use wavecraft_sdr::{ConnectionBackend, RadioModel, RadioProvider};
use wavecraft_usb::{DeviceLocation, DeviceProfile, DSCOPE_U3P100, DSLOGIC_U3PRO16};

fn app() -> WaveCraftApp {
    WaveCraftApp::with_settings(None, CaptureSettingsDocument::default())
}

fn usb_device(profile: &'static DeviceProfile, path: &str) -> DeviceEntry {
    DeviceEntry {
        device: DiscoveredDevice {
            profile,
            location: DeviceLocation {
                platform_path: path.to_owned(),
            },
            speed: UsbSpeedClass::SuperSpeed,
        },
    }
}

fn radio() -> DiscoveredRadio {
    DiscoveredRadio {
        descriptor: DeviceDescriptor {
            id: DeviceId::new("hackrf:test"),
            name: "HackRF One".to_owned(),
            kind: InstrumentKind::RadioReceiver,
        },
        provider: RadioProvider::HackRf,
        model: RadioModel::HackRfOne,
        connection_backend: ConnectionBackend::Usb,
        persistent_id: Some("test".to_owned()),
        capabilities: wavecraft_hackrf::hackrf_one_capabilities(),
    }
}

/// An app that has a DSLogic, a DSCope, and a HackRF "connected".
fn app_with_every_instrument() -> WaveCraftApp {
    let mut app = app();
    app.devices = vec![
        usb_device(&DSLOGIC_U3PRO16, "test-logic"),
        usb_device(&DSCOPE_U3P100, "test-scope"),
    ];
    app.radios = vec![radio()];
    app.reconcile_device_selections();
    app
}

fn context() -> egui::Context {
    let context = egui::Context::default();
    configure_style(&context);
    context
}

fn run_frames(app: &mut WaveCraftApp, context: &egui::Context, count: usize) {
    for _ in 0..count {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            ..egui::RawInput::default()
        };
        let _ = context.run(input, |context| app.run_frame(context));
    }
}

#[test]
fn an_app_without_instruments_paints_and_stays_idle() {
    let mut app = app();
    let context = context();
    run_frames(&mut app, &context, 3);
    assert_eq!(app.active_waveform_tab, None);
    assert!(app.captures.is_empty());
    assert!(app.pending.is_none());
}

#[test]
fn every_waveform_tab_paints_with_every_instrument_connected() {
    let mut app = app_with_every_instrument();
    let context = context();
    // The Lissajous tab only exists once the user enables it from the scope
    // view, and selecting a tab restores that view's saved settings, so enable
    // it while the oscilloscope tab is active, as the UI does.
    app.select_waveform_tab(WaveformTab::Oscilloscope);
    app.scope_function_settings.lissajous.enabled = true;
    let expected = [
        WaveformTab::LogicAnalyzer,
        WaveformTab::Oscilloscope,
        WaveformTab::Lissajous,
        WaveformTab::RadioReceiver,
        WaveformTab::Combined,
    ];
    assert_eq!(app.available_waveform_tabs(), expected);
    for tab in expected {
        app.select_waveform_tab(tab);
        run_frames(&mut app, &context, 3);
        assert_eq!(
            app.active_waveform_tab,
            Some(tab),
            "{tab:?} should stay selected after painting"
        );
    }
}

#[test]
fn the_first_available_view_is_chosen_when_none_is_active() {
    let mut app = app_with_every_instrument();
    let context = context();
    run_frames(&mut app, &context, 1);
    assert_eq!(app.active_waveform_tab, Some(WaveformTab::Combined));
    assert!(app.selected_logic_device().is_some());
    assert!(app.selected_scope_device_ref().is_some());
    assert!(app.selected_radio_id.is_some());
}

#[test]
fn windows_and_dialogs_paint_when_open() {
    let mut app = app_with_every_instrument();
    let context = context();
    app.select_waveform_tab(WaveformTab::Oscilloscope);
    app.settings_open = true;
    app.auto_calibration_confirmation_open = true;
    app.usb_speed_warning = usb_speed_warning(&[DiscoveredDevice {
        profile: &DSLOGIC_U3PRO16,
        location: DeviceLocation {
            platform_path: "slow".to_owned(),
        },
        speed: UsbSpeedClass::HighSpeed,
    }]);
    run_frames(&mut app, &context, 3);
    assert!(app.settings_open);
    assert!(app.auto_calibration_confirmation_open);
}

#[test]
fn the_calibration_progress_modal_paints_and_stays_until_closed() {
    let mut app = app_with_every_instrument();
    let context = context();
    app.select_waveform_tab(WaveformTab::Oscilloscope);
    app.calibration_modal = Some(CalibrationProgressModal {
        progress: 0.4,
        phase: "Calibrating channel 1".to_owned(),
        completed: false,
        values: None,
    });
    run_frames(&mut app, &context, 3);
    assert!(app.calibration_modal.is_some());
}

#[test]
fn radio_and_scope_panels_paint_in_each_capture_settings_tab() {
    let mut app = app_with_every_instrument();
    let context = context();
    for tab in [CaptureSettingsTab::DsLogic, CaptureSettingsTab::DsCope] {
        app.select_waveform_tab(WaveformTab::Combined);
        app.active_capture_settings_tab = tab;
        run_frames(&mut app, &context, 2);
        assert_eq!(app.active_capture_settings_tab, tab);
    }
    app.select_waveform_tab(WaveformTab::RadioReceiver);
    run_frames(&mut app, &context, 2);
}

fn toolbar_inputs(target: Option<CaptureTarget>) -> ToolbarInputs {
    ToolbarInputs {
        control_target: target,
        control_target_available: target.is_some(),
        can_attach_run_capture_recording: false,
        can_attach_run_decoded_recording: false,
    }
}

#[test]
fn stopping_with_nothing_running_reports_it() {
    let mut app = app_with_every_instrument();
    let mut actions = FrameActions {
        stop_clicked: true,
        ..FrameActions::default()
    };
    app.apply_toolbar_actions(
        &mut actions,
        toolbar_inputs(Some(CaptureTarget::LogicAnalyzer)),
    );
    assert_eq!(app.status, "Acquisition stopped");
}

#[test]
fn stop_forgets_the_requested_capture_and_pending_repeats() {
    let mut app = app_with_every_instrument();
    app.capture_requests.insert(CaptureTarget::LogicAnalyzer);
    app.requested_capture_target = Some(CaptureTarget::LogicAnalyzer);
    app.queued_capture_rearms
        .push((AcquisitionMode::Single, CaptureTarget::LogicAnalyzer));
    app.scope_configuration_due = Some(Instant::now());
    let mut actions = FrameActions {
        stop_clicked: true,
        ..FrameActions::default()
    };
    app.apply_toolbar_actions(
        &mut actions,
        toolbar_inputs(Some(CaptureTarget::LogicAnalyzer)),
    );
    assert!(app.capture_requests.is_empty());
    assert_eq!(app.requested_capture_target, None);
    assert!(app.queued_capture_rearms.is_empty());
    assert_eq!(app.scope_configuration_due, None);
}

#[test]
fn stop_without_a_view_does_nothing() {
    let mut app = app_with_every_instrument();
    let status = app.status.clone();
    let mut actions = FrameActions {
        stop_clicked: true,
        ..FrameActions::default()
    };
    app.apply_toolbar_actions(&mut actions, toolbar_inputs(None));
    assert_eq!(app.status, status);
}

fn scope_inputs(before: ScopeCaptureSettings) -> ScopeFrontendInputs {
    ScopeFrontendInputs {
        scope_settings_before_ui: before,
        manual_calibration_before_ui: None,
        manual_calibration_range_before_ui: None,
        scope_capture_requested: false,
        selected_scope_device: true,
    }
}

#[test]
fn an_unchanged_scope_frontend_schedules_nothing() {
    let mut app = app_with_every_instrument();
    let before = app.scope_settings;
    app.apply_scope_frontend_changes(&FrameActions::default(), scope_inputs(before));
    assert_eq!(app.scope_configuration_due, None);
}

#[test]
fn a_scope_frontend_edit_schedules_a_debounced_reconfiguration() {
    let mut app = app_with_every_instrument();
    let before = app.scope_settings;
    app.scope_settings.sample_rate_hz = before.sample_rate_hz / 2;
    app.apply_scope_frontend_changes(&FrameActions::default(), scope_inputs(before));
    assert!(app.scope_configuration_due.is_some());
    assert_eq!(app.status, "Adjusting DSCope configuration…");
}

#[test]
fn selecting_a_scope_device_schedules_a_configuration_when_idle() {
    let mut app = app_with_every_instrument();
    let before = app.scope_settings;
    let actions = FrameActions {
        scope_device_selection_changed: true,
        ..FrameActions::default()
    };
    app.apply_scope_frontend_changes(&actions, scope_inputs(before));
    assert!(app.scope_configuration_due.is_some());
}

#[test]
fn selecting_a_scope_device_schedules_nothing_when_none_is_connected() {
    let mut app = app();
    let before = app.scope_settings;
    let actions = FrameActions {
        scope_device_selection_changed: true,
        ..FrameActions::default()
    };
    app.apply_scope_frontend_changes(
        &actions,
        ScopeFrontendInputs {
            selected_scope_device: false,
            ..scope_inputs(before)
        },
    );
    assert_eq!(app.scope_configuration_due, None);
}

#[test]
fn reconfiguration_waits_while_the_user_refreshes() {
    let mut app = app_with_every_instrument();
    let before = app.scope_settings;
    app.scope_settings.sample_rate_hz = before.sample_rate_hz / 2;
    // Already due, but a refresh in the same frame holds it back.
    app.scope_configuration_due = Some(Instant::now() - Duration::from_secs(1));
    let actions = FrameActions {
        refresh_clicked: true,
        ..FrameActions::default()
    };
    app.apply_scope_frontend_changes(&actions, scope_inputs(before));
    assert!(
        app.pending.is_none(),
        "no configuration should have started"
    );
    assert!(app.scope_configuration_due.is_some());
}
