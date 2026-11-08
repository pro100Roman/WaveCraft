//! Headless frame tests.
//!
//! These run `WaveCraftApp::run_frame` on a bare `egui::Context` with fake
//! devices, so every panel `update()` paints is executed without a window or
//! hardware. They catch panics and state changes in the per-frame code. They do
//! not click anything: egui widgets are not reachable from here, so the
//! interaction paths are covered only by calling the apply methods directly.

use super::*;
use crate::ui::frame_actions::FrameActions;
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
fn the_scope_tools_paint_inside_the_central_area_in_both_states() {
    let mut app = app_with_every_instrument();
    let context = context();
    for tab in [WaveformTab::Oscilloscope, WaveformTab::LogicAnalyzer] {
        app.select_waveform_tab(tab);
        for open in [true, false, true] {
            app.scope_tools_panel_open = open;
            run_frames(&mut app, &context, 3);
            assert_eq!(app.active_waveform_tab, Some(tab));
            assert_eq!(app.scope_tools_panel_open, open);
            // The tools are part of the central area, not a window-level panel.
            assert!(egui::containers::panel::PanelState::load(
                &context,
                egui::Id::new("scope_tools_fixed")
            )
            .is_none());
        }
    }
}

#[test]
fn the_collapsed_scope_tools_state_survives_a_tab_change() {
    let mut app = app_with_every_instrument();
    let context = context();
    app.select_waveform_tab(WaveformTab::Oscilloscope);
    app.scope_tools_panel_open = false;
    app.select_waveform_tab(WaveformTab::LogicAnalyzer);
    run_frames(&mut app, &context, 2);
    app.select_waveform_tab(WaveformTab::Oscilloscope);
    run_frames(&mut app, &context, 2);
    assert!(!app.scope_tools_panel_open);
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

fn stop_request() -> FrameActions {
    FrameActions {
        stop_clicked: true,
        ..FrameActions::default()
    }
}

#[test]
fn stopping_with_nothing_running_reports_it() {
    let mut app = app_with_every_instrument();
    app.select_waveform_tab(WaveformTab::LogicAnalyzer);
    let frame = app.frame_state();
    app.apply_toolbar_actions(&mut stop_request(), &frame);
    assert_eq!(app.status, "Acquisition stopped");
}

#[test]
fn stop_forgets_the_requested_capture_and_pending_repeats() {
    let mut app = app_with_every_instrument();
    app.select_waveform_tab(WaveformTab::LogicAnalyzer);
    app.capture_requests.insert(CaptureTarget::LogicAnalyzer);
    app.requested_capture_target = Some(CaptureTarget::LogicAnalyzer);
    app.queued_capture_rearms
        .push((AcquisitionMode::Single, CaptureTarget::LogicAnalyzer));
    app.scope_configuration_due = Some(Instant::now());
    let frame = app.frame_state();
    app.apply_toolbar_actions(&mut stop_request(), &frame);
    assert!(app.capture_requests.is_empty());
    assert_eq!(app.requested_capture_target, None);
    assert!(app.queued_capture_rearms.is_empty());
    assert_eq!(app.scope_configuration_due, None);
}

#[test]
fn stop_without_a_view_does_nothing() {
    let mut app = app_with_every_instrument();
    assert_eq!(app.active_waveform_tab, None);
    let status = app.status.clone();
    let frame = app.frame_state();
    app.apply_toolbar_actions(&mut stop_request(), &frame);
    assert_eq!(app.status, status);
}

#[test]
fn the_frame_state_follows_the_selected_view_and_devices() {
    let mut app = app();
    let frame = app.frame_state();
    assert_eq!(frame.control_target, None);
    assert!(!frame.selected_scope_device);
    assert!(!frame.control_target_available);

    let mut app_with_devices = app_with_every_instrument();
    app_with_devices.select_waveform_tab(WaveformTab::Oscilloscope);
    // `run_frame` reconciles the device selection before it computes the state,
    // and selecting a view restores that view's saved (empty) selection.
    app_with_devices.reconcile_device_selections();
    let frame = app_with_devices.frame_state();
    assert_eq!(frame.control_target, Some(CaptureTarget::Oscilloscope));
    assert!(frame.selected_scope_device);
    assert!(frame.control_target_available);
    assert!(frame.capture_settings_enabled);

    // Controls lock while a device is being prepared.
    let (_sender, receiver) = mpsc::channel();
    app_with_devices.pending = Some(receiver);
    assert!(!app_with_devices.frame_state().capture_settings_enabled);

    // The snapshots are taken before painting, so they do not follow later edits.
    app.scope_settings.sample_rate_hz += 1;
    assert_ne!(
        app.frame_state().scope_settings_before_ui,
        frame.scope_settings_before_ui
    );
}

#[test]
fn an_unchanged_scope_frontend_schedules_nothing() {
    let mut app = app_with_every_instrument();
    let frame = app.frame_state();
    app.apply_scope_frontend_changes(&FrameActions::default(), &frame);
    assert_eq!(app.scope_configuration_due, None);
}

#[test]
fn a_scope_frontend_edit_schedules_a_debounced_reconfiguration() {
    let mut app = app_with_every_instrument();
    let frame = app.frame_state();
    app.scope_settings.sample_rate_hz = frame.scope_settings_before_ui.sample_rate_hz / 2;
    app.apply_scope_frontend_changes(&FrameActions::default(), &frame);
    assert!(app.scope_configuration_due.is_some());
    assert_eq!(app.status, "Adjusting DSCope configuration…");
}

#[test]
fn selecting_a_scope_device_schedules_a_configuration_when_idle() {
    let mut app = app_with_every_instrument();
    let frame = app.frame_state();
    let actions = FrameActions {
        scope_device_selection_changed: true,
        ..FrameActions::default()
    };
    app.apply_scope_frontend_changes(&actions, &frame);
    assert!(app.scope_configuration_due.is_some());
}

#[test]
fn selecting_a_scope_device_schedules_nothing_when_none_is_connected() {
    let mut app = app();
    let frame = app.frame_state();
    let actions = FrameActions {
        scope_device_selection_changed: true,
        ..FrameActions::default()
    };
    app.apply_scope_frontend_changes(&actions, &frame);
    assert_eq!(app.scope_configuration_due, None);
}

#[test]
fn reconfiguration_waits_while_the_user_refreshes() {
    let mut app = app_with_every_instrument();
    let frame = app.frame_state();
    app.scope_settings.sample_rate_hz = frame.scope_settings_before_ui.sample_rate_hz / 2;
    // Already due, but a refresh in the same frame holds it back.
    app.scope_configuration_due = Some(Instant::now() - Duration::from_secs(1));
    let actions = FrameActions {
        refresh_clicked: true,
        ..FrameActions::default()
    };
    app.apply_scope_frontend_changes(&actions, &frame);
    assert!(
        app.pending.is_none(),
        "no configuration should have started"
    );
    assert!(app.scope_configuration_due.is_some());
}

/// Every rectangle `run_frame` paints in the last of a few frames.
fn painted_rects(context: &egui::Context, app: &mut WaveCraftApp) -> Vec<egui::epaint::RectShape> {
    fn collect(shape: &egui::Shape, out: &mut Vec<egui::epaint::RectShape>) {
        match shape {
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect(shape, out)),
            egui::Shape::Rect(rect) => out.push(*rect),
            _ => {}
        }
    }
    let mut output = None;
    for _ in 0..3 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            ..egui::RawInput::default()
        };
        output = Some(context.run(input, |context| app.run_frame(context)));
    }
    let mut rects = Vec::new();
    for clipped in &output.expect("a frame ran").shapes {
        collect(&clipped.shape, &mut rects);
    }
    rects
}

/// The rectangles painted with the card fill that are tall enough to be a
/// waveform card or a side panel beside it, ordered left to right.
fn card_like_rects(
    context: &egui::Context,
    app: &mut WaveCraftApp,
) -> Vec<egui::epaint::RectShape> {
    let mut rects = painted_rects(context, app);
    // The window-level panels are square; the cards and their side panels are not.
    rects.retain(|rect| rect.fill == PANEL && rect.rounding.nw > 0.0 && rect.rect.height() > 200.0);
    rects.sort_by(|a, b| a.rect.left().total_cmp(&b.rect.left()));
    rects
}

#[test]
fn the_side_panels_have_the_outline_and_height_of_the_waveform_card() {
    let scenarios = [
        ("scope tools open", WaveformTab::Oscilloscope, true, false),
        (
            "scope tools collapsed",
            WaveformTab::Oscilloscope,
            false,
            false,
        ),
        ("decoded data open", WaveformTab::LogicAnalyzer, true, true),
        (
            "decoded data collapsed",
            WaveformTab::LogicAnalyzer,
            true,
            false,
        ),
    ];
    for (name, tab, scope_tools_open, decoded_data_open) in scenarios {
        let mut app = app_with_every_instrument();
        let context = context();
        app.select_waveform_tab(tab);
        app.scope_tools_panel_open = scope_tools_open;
        app.decoded_data_panel_open = decoded_data_open;
        let rects = card_like_rects(&context, &mut app);
        assert_eq!(
            rects.len(),
            2,
            "{name}: a card and one side panel, got {rects:?}"
        );
        // The logic card sits left of its panel; the scope card sits left too.
        let (card, panel) = (&rects[0], &rects[1]);
        assert_eq!(card.rect.top(), panel.rect.top(), "{name}: same top edge");
        assert_eq!(
            card.rect.bottom(),
            panel.rect.bottom(),
            "{name}: same bottom edge"
        );
        assert_eq!(
            card.rounding, panel.rounding,
            "{name}: same corner rounding"
        );
        assert_eq!(card.stroke, panel.stroke, "{name}: same outline");
        assert!(card.stroke.width > 0.0, "{name}: the outline is drawn");
        assert!(
            card.rect.right() < panel.rect.left(),
            "{name}: side by side"
        );
    }
}

#[test]
fn the_scope_plot_fills_the_whole_oscilloscope_card() {
    for scope_tools_open in [true, false] {
        let mut app = app_with_every_instrument();
        let context = context();
        app.select_waveform_tab(WaveformTab::Oscilloscope);
        app.scope_tools_panel_open = scope_tools_open;
        let rects = painted_rects(&context, &mut app);
        let mut cards = rects
            .iter()
            .filter(|rect| {
                rect.fill == PANEL && rect.rounding.nw > 0.0 && rect.rect.height() > 200.0
            })
            .collect::<Vec<_>>();
        cards.sort_by(|a, b| a.rect.left().total_cmp(&b.rect.left()));
        let card = cards
            .first()
            .expect("the oscilloscope card is painted")
            .rect;
        let plot = rects
            .iter()
            .filter(|rect| rect.fill == BACKGROUND && rect.rounding.nw == 4.0)
            .map(|rect| rect.rect)
            .max_by(|a, b| a.area().total_cmp(&b.area()))
            .expect("the plot background is painted");
        // The card's outline is drawn on the edge of its fill, so the plot
        // should be inset by the card's inner margin alone, with no gap left.
        let expected = card.shrink(12.0);
        for (name, actual, wanted) in [
            ("left", plot.left(), expected.left()),
            ("right", plot.right(), expected.right()),
            ("bottom", plot.bottom(), expected.bottom()),
        ] {
            assert!(
                (actual - wanted).abs() < 0.5,
                "tools open={scope_tools_open}: plot {name} {actual} should reach the card's {wanted}"
            );
        }
    }
}

#[test]
fn a_radio_capture_gives_each_analyzer_channel_the_decoder_of_its_own_sync_word() {
    let mut app = app_with_every_instrument();
    let context = context();
    run_frames(&mut app, &context, 1);
    app.radio_settings.analyzer.demodulation = RadioDemodulation::BinaryGfsk;
    app.radio_settings.analyzer.sync_word = 0x91d3;
    app.radio_settings.analyzer.sync_word_bytes = 2;
    let mut own = RadioAnalyzerChannelSettings::numbered(1, 300_000);
    own.sync_word = "2d d4 aa".to_owned();
    app.radio_settings.analyzer.channels = vec![
        RadioAnalyzerChannelSettings::numbered(0, -300_000),
        own,
        RadioAnalyzerChannelSettings::numbered(2, 0),
    ];

    let request = app
        .radio_capture_request(AcquisitionMode::Run)
        .expect("the settings are valid");

    let sync_words = request
        .decoder_configs
        .iter()
        .map(|config| match config {
            RadioDataDecoderConfig::Raw(raw) => (raw.sync_word, raw.sync_word_bytes),
            RadioDataDecoderConfig::Packets(_) => panic!("the settings are for raw data"),
        })
        .collect::<Vec<_>>();
    assert_eq!(sync_words, [(0x91d3, 2), (0x2dd4aa, 3), (0x91d3, 2)]);
    assert_eq!(request.analyzer_profiles.len(), 3);
    assert_eq!(request.analyzer_channel_names.len(), 3);
}

#[test]
fn a_radio_capture_is_refused_when_a_channel_sync_word_is_not_valid() {
    let mut app = app_with_every_instrument();
    let context = context();
    run_frames(&mut app, &context, 1);
    app.radio_settings.analyzer.demodulation = RadioDemodulation::BinaryGfsk;
    app.radio_settings.analyzer.channels[0].sync_word = "91 d".to_owned();

    let error = app
        .radio_capture_request(AcquisitionMode::Run)
        .err()
        .expect("a half-typed sync word is not a capture setting");

    assert!(error.starts_with("Channel 1: "), "{error}");
}

/// A capture whose radio store holds `count` raw messages with the sync word
/// `91 d3`.
fn radio_capture_with_sync_word_messages(count: u64) -> CompletedCapture {
    let mut analysis = RadioAnalysisStore::new(1_000).unwrap();
    for index in 0..count {
        let mut values = vec![0xaa; 10];
        values.extend([0x91, 0xd3, 0xee, 0x10]);
        let start = index * 1_000;
        analysis.ingest_raw(
            RawRadioOutput {
                bytes: values
                    .iter()
                    .enumerate()
                    .map(|(position, value)| TimedByte {
                        value: *value,
                        range: SampleRange {
                            start: start + position as u64 * 8,
                            end: start + position as u64 * 8 + 8,
                        },
                        status: ByteStatus::default(),
                    })
                    .collect(),
                message_starts: vec![RawRadioMessageStart {
                    sample_index: start,
                    preamble_bytes: 10,
                    maximum_uncertain_bits: usize::MAX,
                }],
                diagnostics: Vec::new(),
                synchronized: false,
            },
            0,
            Arc::from("Channel 1"),
        );
        analysis.finish_channel_capture(0);
    }
    CompletedCapture {
        waveforms: WaveformStore {
            radio_analysis: analysis,
            ..WaveformStore::default()
        },
        protocol_decode: ProtocolCaptureDecoder::new(&[]),
        has_logic: false,
        has_scope: false,
        has_radio: true,
    }
}

#[test]
fn the_sync_words_of_the_last_radio_capture_are_offered_in_the_settings() {
    let mut app = app_with_every_instrument();
    assert!(app.displayed_sync_word_suggestions().is_empty());

    app.radio_capture = Some(radio_capture_with_sync_word_messages(6));

    let suggestions = app.displayed_sync_word_suggestions();
    assert_eq!(suggestions.len(), 1, "{suggestions:?}");
    assert_eq!(suggestions[0].word, 0x91d3);
    assert_eq!(suggestions[0].messages, 6);
}

#[test]
fn the_settings_with_detected_sync_words_paint() {
    let mut app = app_with_every_instrument();
    let context = context();
    app.radio_capture = Some(radio_capture_with_sync_word_messages(6));
    app.select_waveform_tab(WaveformTab::RadioReceiver);
    app.radio_settings.analyzer.demodulation = RadioDemodulation::BinaryGfsk;
    app.settings_open = true;

    run_frames(&mut app, &context, 3);

    assert!(app.settings_open);
}
