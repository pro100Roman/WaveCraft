//! Tests that cover more than one module of the GUI, or only the application
//! root. Tests for a single module live in that module.

use super::*;
use std::io::Cursor;
use wavecraft_core::{LogicBatch, ScopeBatch};
use wavecraft_sdr::RadioProviderAdapter;

#[test]
fn embedded_wavecraft_app_icon_is_valid() {
    let icon = wavecraft_app_icon().expect("embedded icon decodes");

    assert_eq!((icon.width, icon.height), (512, 512));
    assert_eq!(icon.rgba.len(), 512 * 512 * 4);
    assert_eq!(icon.rgba[3], 0);
}

#[test]
fn motor_controller_example_recording_opens_and_decodes() {
    let recording = include_bytes!("../../../docs/examples/motor-controller-uart.dsv");
    let schema = include_str!("../../../docs/examples/motor-controller.wavecraft-protocol.yaml");
    let mut analyzers = ProtocolAnalyzerSettings::default();
    assert!(analyzers.begin_add("1:uart"));
    let editor = analyzers.editor.clone().unwrap();
    analyzers.save_editor(editor);
    analyzers
        .begin_attach_custom_yaml(
            0,
            PathBuf::from("motor-controller.wavecraft-protocol.yaml"),
            schema.to_owned(),
        )
        .unwrap();
    let editor = analyzers.editor.clone().unwrap();
    analyzers.save_editor(editor);

    let loaded =
        read_gui_recording_with_analyzers(Cursor::new(recording.as_slice()), &analyzers.analyzers)
            .unwrap();
    assert_eq!(loaded.completion, CompletionStatus::Complete);
    assert_eq!(loaded.logic_channel_names[0], "UART RX");

    let mut cache = ProtocolDecodeCache::default();
    let snapshot = loaded.capture.protocol_decode.snapshot();
    let ProtocolDecodeOutcome::Events(events) = &cache.outcomes(
        &loaded.capture.waveforms,
        &analyzers.analyzers,
        Some(&snapshot),
    )[0] else {
        panic!("example recording should decode");
    };
    assert_eq!(
        events
            .iter()
            .map(|event| event.text.as_str())
            .collect::<Vec<_>>(),
        ["Motor 1: 1200 rpm", "Motor 2: 2000 rpm", "Motor 3: 750 rpm"]
    );
}

#[test]
fn protocol_decode_cache_reuses_outcomes_until_inputs_change() {
    let uart = |instance_id| ProtocolAnalyzerInstance {
        instance_id,
        protocol_id: "1:uart",
        channel_mappings: vec![Some(0)],
        data_format: ProtocolDataFormat::Ascii,
        custom_protocol: None,
    };
    let analyzers = vec![uart(1)];
    let mut waveforms = WaveformStore::default();
    let mut cache = ProtocolDecodeCache::default();

    let first = cache.shared_outcomes(&waveforms, &analyzers, None);
    let again = cache.shared_outcomes(&waveforms, &analyzers, None);
    assert!(
        Arc::ptr_eq(&first, &again),
        "unchanged inputs must not recompute or copy outcomes"
    );

    waveforms.logic_revision += 1;
    let after_new_samples = cache.shared_outcomes(&waveforms, &analyzers, None);
    assert!(!Arc::ptr_eq(&first, &after_new_samples));

    let reconfigured = vec![uart(2)];
    let after_edit = cache.shared_outcomes(&waveforms, &reconfigured, None);
    assert!(!Arc::ptr_eq(&after_new_samples, &after_edit));
}

#[test]
fn protocol_decode_cache_shares_a_matching_streamed_snapshot() {
    let analyzers = vec![ProtocolAnalyzerInstance {
        instance_id: 1,
        protocol_id: "1:uart",
        channel_mappings: vec![Some(0)],
        data_format: ProtocolDataFormat::Ascii,
        custom_protocol: None,
    }];
    let snapshot = ProtocolCaptureDecoder::new(&analyzers).snapshot();
    let waveforms = WaveformStore::default();
    let mut cache = ProtocolDecodeCache::default();

    let outcomes = cache.shared_outcomes(&waveforms, &analyzers, Some(&snapshot));

    assert!(Arc::ptr_eq(&outcomes, &snapshot.shared_outcomes()));
}

#[test]
fn streamed_protocol_history_survives_waveform_ring_eviction() {
    let analyzer = ProtocolAnalyzerInstance {
        instance_id: 7,
        protocol_id: "1:uart",
        channel_mappings: vec![Some(0)],
        data_format: ProtocolDataFormat::Ascii,
        custom_protocol: None,
    };
    let samples_per_bit = 10;
    let mut samples = vec![1_u32; samples_per_bit * 2];
    for byte in b"Hi" {
        samples.extend(std::iter::repeat(0).take(samples_per_bit));
        for bit in 0..8 {
            let level = u32::from(byte & (1 << bit) != 0);
            samples.extend(std::iter::repeat(level).take(samples_per_bit));
        }
        samples.extend(std::iter::repeat(1).take(samples_per_bit * 2));
    }

    let mut decoder = ProtocolCaptureDecoder::new(std::slice::from_ref(&analyzer));
    for (sequence, chunk) in samples.chunks(7).enumerate() {
        decoder.ingest_logic_frame("logic", sequence as u64, chunk, 1_152_000);
    }
    decoder.finish();

    let mut waveforms = WaveformStore::default();
    waveforms.configure_logic_capture(1);
    waveforms.ingest_logic_samples(&samples);
    assert!(waveforms.logic_sample_offset > 0);
    assert_eq!(waveforms.logic_sample_count, 1);

    let mut cache = ProtocolDecodeCache::default();
    let snapshot = decoder.snapshot();
    let ProtocolDecodeOutcome::Events(events) =
        &cache.outcomes(&waveforms, std::slice::from_ref(&analyzer), Some(&snapshot))[0]
    else {
        panic!("streamed UART history should remain available");
    };
    assert_eq!(
        events
            .iter()
            .map(|event| event.text.as_str())
            .collect::<Vec<_>>(),
        ["'H'", "'i'"]
    );
}

#[test]
fn combined_session_settings_preserve_both_device_ids() {
    let settings = CombinedSessionSettings {
        logic: LogicSessionSettings {
            device_id: Some("usb-logic".to_owned()),
            ..LogicSessionSettings::default()
        },
        scope: ScopeSessionSettings {
            device_id: Some("usb-scope".to_owned()),
            ..ScopeSessionSettings::default()
        },
        ..CombinedSessionSettings::default()
    };
    let restored = settings.clone();

    assert_eq!(restored.logic.device_id.as_deref(), Some("usb-logic"));
    assert_eq!(restored.scope.device_id.as_deref(), Some("usb-scope"));
}

#[test]
fn i2c_decode_lane_is_inserted_above_mapped_sda_channel() {
    let analyzer = ProtocolAnalyzerInstance {
        instance_id: 0,
        protocol_id: "1:i2c",
        channel_mappings: vec![Some(0), Some(5)],
        data_format: ProtocolDataFormat::Hexadecimal,
        custom_protocol: None,
    };
    let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 800.0));
    let maximum_heights = LogicCaptureSettings::default().maximum_heights;

    let (layout, lanes) = logic_protocol_lane_layout(plot, &maximum_heights, &[analyzer]);

    assert_eq!(lanes.len(), 1);
    assert!(lanes[0].top() > layout.levels[4].1);
    assert!(lanes[0].bottom() < layout.levels[5].0);
}

#[test]
fn decoded_panel_groups_i2c_bytes_into_one_transaction() {
    let event = |kind, text: &str, start_sample, end_sample, warning| ProtocolDecodedEvent {
        start_sample,
        end_sample,
        text: text.to_owned(),
        detail: text.to_owned(),
        warning,
        kind,
    };
    let events = vec![
        event(
            ProtocolDecodedKind::I2cAddress {
                address: 0x50,
                read: false,
            },
            "ADDR 50 W",
            10,
            20,
            false,
        ),
        event(
            ProtocolDecodedKind::Acknowledge { acknowledged: true },
            "ACK",
            20,
            22,
            false,
        ),
        event(
            ProtocolDecodedKind::Data { value: 0x12 },
            "12",
            23,
            31,
            false,
        ),
        event(
            ProtocolDecodedKind::Acknowledge {
                acknowledged: false,
            },
            "NACK",
            31,
            33,
            true,
        ),
    ];

    let entries = decoded_panel_entries(&events);

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title, "I²C WRITE · address 0x50");
    assert!(entries[0].detail.contains("Address 0x50 (W) → ACK"));
    assert!(entries[0].detail.contains("Data 0x12 → NACK"));
    assert!(entries[0].warning);
    assert_eq!((entries[0].start_sample, entries[0].end_sample), (10, 33));
}

#[test]
fn decoded_panel_lists_newest_operations_first() {
    let event = |value, start_sample| ProtocolDecodedEvent {
        start_sample,
        end_sample: start_sample + 9,
        text: format!("{value:02X}"),
        detail: String::new(),
        warning: false,
        kind: ProtocolDecodedKind::Uart { value },
    };

    let entries = decoded_panel_entries(&[event(0x11, 10), event(0x22, 30)]);

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].start_sample, 30);
    assert_eq!(entries[1].start_sample, 10);
}

#[test]
fn decoded_panel_renders_uart_ascii_as_readable_text() {
    let events = b"Hello\r\n"
        .iter()
        .copied()
        .enumerate()
        .map(|(index, value)| ProtocolDecodedEvent {
            start_sample: index as u64 * 10,
            end_sample: index as u64 * 10 + 9,
            text: char::from(value).to_string(),
            detail: String::new(),
            warning: false,
            kind: ProtocolDecodedKind::Uart { value },
        })
        .collect::<Vec<_>>();

    assert_eq!(uart_ascii_text(&events).as_deref(), Some("Hello\r\n"));
}

#[test]
fn protocol_analyzer_panel_renders_catalog_and_channel_mapping() {
    let context = egui::Context::default();
    let mut settings = ProtocolAnalyzerSettings::default();
    settings.picker_open = true;
    settings.search = "SPI flash".to_owned();
    assert!(settings.begin_add("spiflash"));
    let editor = settings.editor.clone().unwrap();
    settings.save_editor(editor);
    let enabled = [true; LOGIC_CHANNEL_COUNT];

    let output = context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            paint_protocol_analyzer_settings(ui, &mut settings, &enabled);
        });
    });

    assert!(!output.shapes.is_empty());
    assert_eq!(settings.analyzers[0].channel_mappings.len(), 4);
}

#[test]
fn protocol_settings_modal_renders_without_committing_the_draft() {
    let context = egui::Context::default();
    let mut settings = ProtocolAnalyzerSettings::default();
    assert!(settings.begin_add("i2cdemux"));
    let names = empty_logic_channel_names();
    let enabled = [true; LOGIC_CHANNEL_COUNT];

    let output = context.run(egui::RawInput::default(), |context| {
        paint_protocol_analyzer_modal(context, &mut settings, &names, &enabled);
    });

    assert!(!output.shapes.is_empty());
    assert!(settings.editor.is_some());
    assert!(settings.analyzers.is_empty());
}

#[test]
fn gui_recording_reader_restores_full_capture_and_scope_settings() {
    let descriptors = vec![
        DeviceDescriptor {
            id: DeviceId::new("logic-test"),
            name: "Logic test".to_owned(),
            kind: InstrumentKind::LogicAnalyzer,
        },
        DeviceDescriptor {
            id: DeviceId::new("scope-test"),
            name: "Scope test".to_owned(),
            kind: InstrumentKind::Oscilloscope,
        },
    ];
    let mut metadata = BTreeMap::new();
    metadata.insert("scope.sample_rate_hz".to_owned(), "1000000".to_owned());
    metadata.insert("scope.sample_count".to_owned(), "1000000".to_owned());
    metadata.insert("scope.ch0.enabled".to_owned(), "false".to_owned());
    metadata.insert("scope.ch1.enabled".to_owned(), "true".to_owned());
    metadata.insert("scope.ch1.volts_per_div_mv".to_owned(), "500".to_owned());
    metadata.insert("scope.ch1.coupling".to_owned(), "ac".to_owned());
    metadata.insert("scope.ch1.probe_attenuation".to_owned(), "10".to_owned());
    metadata.insert("logic.operation_mode".to_owned(), "stream".to_owned());
    metadata.insert("logic.sample_rate_hz".to_owned(), "250000000".to_owned());
    metadata.insert("logic.sample_count".to_owned(), "1250304".to_owned());
    for channel in 0..LOGIC_CHANNEL_COUNT {
        metadata.insert(
            format!("logic.ch{channel}.enabled"),
            matches!(channel, 2 | 9).to_string(),
        );
        metadata.insert(
            format!("logic.ch{channel}.maximum_height"),
            (channel % 5 + 1).to_string(),
        );
    }
    metadata.insert(
        "logic.stop_option".to_owned(),
        "stop_immediately".to_owned(),
    );
    metadata.insert("logic.threshold_voltage".to_owned(), "2.5".to_owned());
    metadata.insert(
        "logic.input_filter".to_owned(),
        "one_sample_clock".to_owned(),
    );
    metadata.insert("logic.rle_enabled".to_owned(), "true".to_owned());
    metadata.insert("logic.external_clock".to_owned(), "true".to_owned());
    metadata.insert("logic.falling_clock_edge".to_owned(), "true".to_owned());
    metadata.insert("logic.trigger.enabled".to_owned(), "true".to_owned());
    metadata.insert("logic.trigger.mode".to_owned(), "simple".to_owned());
    metadata.insert("logic.trigger.channel".to_owned(), "9".to_owned());
    metadata.insert(
        "logic.trigger.condition".to_owned(),
        "falling_edge".to_owned(),
    );
    metadata.insert("logic.trigger.ch2.condition".to_owned(), "high".to_owned());
    metadata.insert("logic.ch0.name".to_owned(), "Clock".to_owned());
    metadata.insert("logic.ch1.name".to_owned(), "  Data  ".to_owned());
    metadata.insert("scope.ch0.name".to_owned(), "Input".to_owned());
    metadata.insert("scope.ch1.name".to_owned(), "Reference".to_owned());
    let mut bytes = Vec::new();
    {
        let mut writer = RecordingWriter::new(&mut bytes, &descriptors, metadata)
            .expect("recording header is valid");
        writer
            .write_event(&CaptureEvent::Frame(CaptureFrame {
                device_id: DeviceId::new("logic-test"),
                sequence: 0,
                captured_at: Duration::ZERO,
                payload: CaptureBatch::Logic(LogicBatch {
                    sample_rate_hz: 2_000_000,
                    channel_count: 4,
                    samples: vec![1, 2, 3],
                }),
            }))
            .expect("logic frame is valid");
        writer
            .write_event(&CaptureEvent::Frame(CaptureFrame {
                device_id: DeviceId::new("scope-test"),
                sequence: 0,
                captured_at: Duration::ZERO,
                payload: CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 1_000_000,
                    trigger_sample_index: Some(1),
                    channels: vec![vec![0.25, -0.5]],
                }),
            }))
            .expect("scope frame is valid");
        writer
            .finish(CompletionStatus::Complete)
            .expect("recording footer is valid");
    }

    let loaded = read_gui_recording(Cursor::new(bytes)).expect("recording should reopen");

    assert_eq!(loaded.completion, CompletionStatus::Complete);
    assert!(loaded.capture.has_logic);
    assert!(loaded.capture.has_scope);
    assert_eq!(loaded.capture.waveforms.logic_rate_hz, 2_000_000);
    assert_eq!(
        loaded.capture.waveforms.logic_samples,
        VecDeque::from(vec![
            LogicSampleBucket::new(1),
            LogicSampleBucket::new(2),
            LogicSampleBucket::new(3),
        ])
    );
    assert_eq!(loaded.capture.waveforms.logic_sample_count, 3);
    assert!(loaded.capture.waveforms.scope_channels[0].is_empty());
    assert_eq!(
        loaded.capture.waveforms.scope_channels[1],
        VecDeque::from(vec![0.25, -0.5])
    );
    let scope_settings = loaded.scope_settings.expect("scope metadata should load");
    assert!(!scope_settings.channels[0].enabled);
    assert!(scope_settings.channels[1].enabled);
    assert_eq!(scope_settings.channels[1].volts_per_div_mv, 500);
    assert_eq!(scope_settings.channels[1].coupling, DsCopeCoupling::Ac);
    assert_eq!(scope_settings.channels[1].probe_attenuation, 10);
    let logic_settings = loaded.logic_settings.expect("logic metadata should load");
    assert_eq!(logic_settings.operation_mode, DsLogicOperationMode::Stream);
    assert_eq!(logic_settings.sample_rate_hz, 250_000_000);
    assert_eq!(logic_settings.sample_count, 1_250_304);
    assert_eq!(
        logic_settings.enabled_channels,
        [
            false, false, true, false, false, false, false, false, false, true, false, false,
            false, false, false, false,
        ]
    );
    assert_eq!(
        logic_settings.stop_option,
        DsLogicStopOption::StopImmediately
    );
    assert_eq!(logic_settings.threshold_voltage, 2.5);
    assert_eq!(
        logic_settings.input_filter,
        DsLogicInputFilter::OneSampleClock
    );
    assert_eq!(
        logic_settings.maximum_heights,
        [1, 2, 3, 4, 5, 1, 2, 3, 4, 5, 1, 2, 3, 4, 5, 1]
    );
    assert!(logic_settings.rle_enabled);
    assert!(logic_settings.external_clock);
    assert!(logic_settings.falling_clock_edge);
    assert!(logic_settings.trigger_enabled);
    assert_eq!(
        logic_settings.trigger_conditions[2],
        Some(DsLogicTriggerCondition::High)
    );
    assert_eq!(
        logic_settings.trigger_conditions[9],
        Some(DsLogicTriggerCondition::FallingEdge)
    );
    assert_eq!(loaded.logic_channel_names[0], "Clock");
    assert_eq!(loaded.logic_channel_names[1], "Data");
    assert_eq!(loaded.scope_channel_names, ["Input", "Reference"]);
}

#[test]
#[ignore = "requires a connected HackRF One and installed libhackrf"]
fn gui_hackrf_capture_pipeline_receives_and_spectrally_processes_iq() {
    let adapter = wavecraft_hackrf::HackRfAdapter::new();
    let device = adapter
        .discover()
        .expect("GUI HackRF discovery should succeed")
        .into_iter()
        .next()
        .expect("a HackRF One should be connected");
    let request = RadioCaptureRequest {
        device,
        settings: RadioCaptureSettings {
            center_frequency_hz: 868_300_000,
            sample_rate_hz: 8_000_000,
            bandwidth_hz: 1_750_000,
            receive_channel: 0,
            sample_limit: Some(4_096),
        },
        provider_settings: RadioProviderSettings::HackRf(wavecraft_sdr::HackRfSettings::default()),
        iq_correction_enabled: true,
        analyzer_profiles: Vec::new(),
        analyzer_channel_names: Vec::new(),
        decoder_configs: Vec::new(),
    };
    let logic_config = LogicCaptureSettings::default().capture_config().unwrap();
    let scope_config = ScopeCaptureSettings::default().capture_config().unwrap();
    let started = start_capture(
        Vec::new(),
        Some(request),
        Path::new("."),
        logic_config,
        scope_config,
        None,
        false,
    )
    .expect("GUI radio capture should start");
    let StartedCapture {
        session,
        radio_dsp,
        radio_runtime_control,
        ..
    } = started;
    let mut radio_dsp = radio_dsp.expect("an RF capture starts its DSP worker");
    assert!(
        radio_runtime_control.is_some(),
        "a HackRF capture can be retuned while it runs"
    );
    let frame = match session
        .recv_timeout(Duration::from_secs(5))
        .expect("GUI radio capture should produce an event")
    {
        CaptureEvent::Frame(frame) => frame,
        CaptureEvent::SourceFailed { error, .. } => {
            panic!("GUI radio capture failed: {error}")
        }
        CaptureEvent::SourceStopped { .. } => {
            panic!("GUI radio capture stopped before producing IQ")
        }
    };
    let CaptureBatch::Iq(iq) = frame.payload else {
        panic!("GUI radio capture should emit IQ batches");
    };
    radio_dsp
        .push(iq.clone(), -50.0)
        .expect("GUI radio DSP worker should accept IQ");
    let spectra = radio_dsp
        .finish()
        .expect("GUI radio DSP worker should finish")
        .into_iter()
        .flat_map(|output| output.spectrum)
        .collect::<Vec<_>>();

    assert!(!iq.samples.is_empty());
    assert_eq!(iq.sample_rate_hz, 8_000_000);
    assert!(!spectra.is_empty());
    assert!(spectra
        .iter()
        .all(|spectrum| spectrum.power_dbfs.len() == RADIO_SPECTRUM_FFT_LENGTH));
    session
        .stop()
        .expect("GUI radio capture should stop cleanly");
}

#[test]
fn scope_measurement_cache_tracks_new_samples_and_visible_ranges() {
    let mut store = WaveformStore {
        scope_capacity: 4,
        ..WaveformStore::default()
    };
    store.ingest(
        &CaptureBatch::Scope(ScopeBatch {
            sample_rate_hz: 1_000,
            trigger_sample_index: None,
            channels: vec![vec![-1.0, 1.0], vec![0.0, 0.0]],
        }),
        [true, true],
    );
    let mut cache = ScopeMeasurementCache::default();

    let first = cache.results(&store, 0, 0..2, 1_000);
    assert_eq!(
        first.get(measurements::ScopeMeasurementKind::Mean),
        Some(0.0)
    );
    assert_eq!(cache.key.unwrap().range_end, 2);

    store.ingest(
        &CaptureBatch::Scope(ScopeBatch {
            sample_rate_hz: 1_000,
            trigger_sample_index: None,
            channels: vec![vec![3.0, 3.0], vec![0.0, 0.0]],
        }),
        [true, true],
    );
    let updated = cache.results(&store, 0, 0..4, 1_000);
    assert_eq!(
        updated.get(measurements::ScopeMeasurementKind::Mean),
        Some(1.5)
    );

    let zoomed = cache.results(&store, 0, 2..4, 1_000);
    assert_eq!(
        zoomed.get(measurements::ScopeMeasurementKind::Mean),
        Some(3.0)
    );
}

#[test]
fn lissajous_coordinates_use_each_channels_voltage_scale() {
    let plot = Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 800.0));
    let x_channel = ScopeChannelSettings {
        volts_per_div_mv: 1_000,
        ..ScopeChannelSettings::default()
    };
    let y_channel = ScopeChannelSettings {
        volts_per_div_mv: 500,
        ..ScopeChannelSettings::default()
    };

    let zero = scope_lissajous_position(plot, x_channel, y_channel, 0.0, 0.0);
    let expected_zero = plot.left_top()
        + Vec2::splat(scope_vertical_offset_ratio(DSCOPE_VERTICAL_OFFSET_CENTER) * plot.width());
    assert!((zero.x - expected_zero.x).abs() < 0.001);
    assert!((zero.y - expected_zero.y).abs() < 0.001);
    let point = scope_lissajous_position(plot, x_channel, y_channel, 1.0, 0.5);
    assert!((point.x - zero.x - 100.0).abs() < 0.001);
    assert!((zero.y - point.y - 100.0).abs() < 0.001);
}

#[test]
fn waveform_tabs_target_their_matching_instruments() {
    assert_eq!(
        WaveformTab::LogicAnalyzer.capture_target(),
        CaptureTarget::LogicAnalyzer
    );
    assert_eq!(
        WaveformTab::Oscilloscope.capture_target(),
        CaptureTarget::Oscilloscope
    );
    assert_eq!(
        WaveformTab::Lissajous.capture_target(),
        CaptureTarget::Oscilloscope
    );
    assert_eq!(
        WaveformTab::RadioReceiver.capture_target(),
        CaptureTarget::RadioReceiver
    );
    assert_eq!(
        WaveformTab::Combined.capture_target(),
        CaptureTarget::Combined
    );

    assert_eq!(
        WaveformTab::LogicAnalyzer.capture_settings_tab(),
        Some(CaptureSettingsTab::DsLogic)
    );
    assert_eq!(
        WaveformTab::Oscilloscope.capture_settings_tab(),
        Some(CaptureSettingsTab::DsCope)
    );
    assert_eq!(
        WaveformTab::Lissajous.capture_settings_tab(),
        Some(CaptureSettingsTab::DsCope)
    );
    assert_eq!(WaveformTab::RadioReceiver.capture_settings_tab(), None);
    assert_eq!(WaveformTab::Combined.capture_settings_tab(), None);
}

#[test]
fn logic_marker_positions_snap_to_captured_samples() {
    let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 400.0));
    let viewport = WaveformViewport::default();

    assert_eq!(
        logic_sample_at_x(plot, viewport, 2_000, 101, 254.0),
        Some(2_025)
    );
    assert_eq!(
        logic_marker_x(plot, viewport, 2_000, 101, 2_025),
        Some(250.0)
    );
    assert_eq!(logic_marker_x(plot, viewport, 2_000, 101, 1_999), None);
}

#[test]
fn logic_marker_edges_snap_on_the_channel_under_the_pointer() {
    let mut waveforms = WaveformStore {
        logic_channels: 2,
        logic_rate_hz: 1_000_000,
        ..WaveformStore::default()
    };
    waveforms.configure_logic_capture(101);
    let samples = (0..101)
        .map(|index| match index {
            0..20 => 0,
            20..80 => 0b01,
            _ => 0b11,
        })
        .collect::<Vec<_>>();
    waveforms.ingest_logic_samples(&samples);

    let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 400.0));
    let viewport = WaveformViewport::default();
    let layout = logic_signal_layout(plot.top(), plot.height(), &[1; LOGIC_CHANNEL_COUNT]);
    let channel_y = |channel: usize| {
        let (high, low) = layout.levels[channel];
        (high + low) * 0.5
    };
    let enabled_channels = [true; LOGIC_CHANNEL_COUNT];

    assert_eq!(
        logic_marker_sample_at_pointer(
            plot,
            viewport,
            &waveforms,
            &layout.levels,
            &enabled_channels,
            Pos2::new(77.0, channel_y(1)),
        ),
        Some(80)
    );
    assert_eq!(
        logic_marker_sample_at_pointer(
            plot,
            viewport,
            &waveforms,
            &layout.levels,
            &enabled_channels,
            Pos2::new(77.0, channel_y(0)),
        ),
        Some(77)
    );
}

#[test]
fn logic_smart_timing_follows_the_pulse_under_the_pointer() {
    let mut waveforms = WaveformStore {
        logic_channels: 1,
        logic_rate_hz: 1_000_000,
        ..WaveformStore::default()
    };
    waveforms.configure_logic_capture(101);
    let samples = (0..101)
        .map(|index| u32::from(matches!(index, 10..30 | 50..70 | 90..=100)))
        .collect::<Vec<_>>();
    waveforms.ingest_logic_samples(&samples);

    let measurement = logic_smart_timing_measurement(&waveforms, 0, 24).unwrap();
    assert_eq!(measurement.channel, 0);
    assert_eq!(measurement.edge_start_sample, 10);
    assert_eq!(measurement.edge_end_sample, 30);
    let cycle = measurement.cycle.unwrap();
    assert_eq!(cycle.period_start_sample, 10);
    assert_eq!(cycle.period_end_sample, 50);
    assert_eq!(cycle.period_samples, 40);
    assert_eq!(cycle.high_samples, 20);
    assert_eq!(
        cycle.period(waveforms.logic_rate_hz),
        Duration::from_micros(40)
    );
    assert_eq!(cycle.frequency_hz(waveforms.logic_rate_hz), 25_000.0);
    assert_eq!(cycle.duty_cycle_percent(), 50.0);

    let low_pulse = logic_smart_timing_measurement(&waveforms, 0, 35).unwrap();
    assert_eq!(low_pulse.edge_start_sample, 30);
    assert_eq!(low_pulse.edge_end_sample, 50);
    assert_eq!(low_pulse.cycle, Some(cycle));
    assert_eq!(logic_smart_timing_measurement(&waveforms, 0, 0), None);
}

#[test]
fn logic_smart_timing_handles_edges_inside_downsampled_buckets() {
    let mut waveforms = WaveformStore {
        logic_channels: 1,
        logic_rate_hz: 1_000_000,
        logic_sample_stride: 4,
        ..WaveformStore::default()
    };
    let samples = (0..37)
        .map(|index| u32::from(matches!(index, 5..13 | 21..29)))
        .collect::<Vec<_>>();
    waveforms.ingest_logic_samples(&samples);

    let high_pulse = logic_smart_timing_measurement(&waveforms, 0, 9).unwrap();
    assert_eq!(high_pulse.edge_start_sample, 6);
    assert_eq!(high_pulse.edge_end_sample, 14);
    assert_eq!(
        high_pulse.cycle,
        Some(LogicTimingMeasurement {
            channel: 0,
            period_start_sample: 6,
            period_end_sample: 22,
            period_samples: 16,
            high_samples: 8,
        })
    );

    let low_pulse = logic_smart_timing_measurement(&waveforms, 0, 17).unwrap();
    assert_eq!(low_pulse.edge_start_sample, 14);
    assert_eq!(low_pulse.edge_end_sample, 22);
    assert_eq!(low_pulse.cycle, high_pulse.cycle);
}

#[test]
fn logic_smart_timing_paint_is_clipped_to_the_waveform_plot() {
    let context = egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let plot = Rect::from_min_max(Pos2::new(100.0, 100.0), Pos2::new(500.0, 400.0));
    let waveforms = WaveformStore {
        logic_channels: 1,
        logic_rate_hz: 1_000_000,
        logic_sample_count: 101,
        ..WaveformStore::default()
    };
    let measurement = LogicSmartTimingMeasurement {
        channel: 0,
        edge_start_sample: 10,
        edge_end_sample: 90,
        cycle: Some(LogicTimingMeasurement {
            channel: 0,
            period_start_sample: 10,
            period_end_sample: 90,
            period_samples: 80,
            high_samples: 40,
        }),
    };

    let output = context.run(
        egui::RawInput {
            screen_rect: Some(screen),
            ..egui::RawInput::default()
        },
        |context| {
            let painter = context.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("smart_timing_clip_test"),
            ));
            paint_logic_timing_hint(
                &painter,
                plot,
                &waveforms,
                WaveformViewport {
                    start: 0.4,
                    span: 0.2,
                    anchor_locked: false,
                },
                measurement,
                ACCENT,
            );
        },
    );

    assert!(!output.shapes.is_empty());
    assert!(output.shapes.iter().all(|shape| shape.clip_rect == plot));
}

#[test]
fn logic_timing_measurement_rejects_unresolved_bucket_activity() {
    let waveforms = WaveformStore {
        logic_samples: VecDeque::from([LogicSampleBucket::from_samples(&[0, 1, 0]).unwrap()]),
        logic_channels: 1,
        logic_rate_hz: 1_000_000,
        logic_sample_count: 3,
        logic_sample_stride: 3,
        ..WaveformStore::default()
    };

    assert_eq!(logic_smart_timing_measurement(&waveforms, 0, 1), None);

    let mut waveforms = WaveformStore {
        logic_channels: 1,
        logic_rate_hz: 1_000_000,
        logic_sample_stride: 4,
        ..WaveformStore::default()
    };
    waveforms.ingest_logic_samples(&[
        0, 0, 0, 0, // stable low
        0, 1, 0, 1, // three transitions cannot be located after compaction
        1, 1, 1, 1, // stable high under the pointer
        1, 0, 0, 0, // falling edge
        0, 0, 0, 0,
    ]);

    assert_ne!(waveforms.logic_samples[1].multiple_transitions & 1, 0);
    assert_eq!(logic_smart_timing_measurement(&waveforms, 0, 9), None);
}

#[test]
fn logic_marker_drag_pans_the_zoomed_viewport_at_plot_edges() {
    let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 400.0));
    let mut viewport = WaveformViewport {
        start: 0.4,
        span: 0.2,
        anchor_locked: true,
    };

    assert!(pan_logic_marker_viewport_at_edge(
        &mut viewport,
        plot,
        plot.right()
    ));
    assert!(viewport.start > 0.4);
    let right_start = viewport.start;
    assert!(pan_logic_marker_viewport_at_edge(
        &mut viewport,
        plot,
        plot.left()
    ));
    assert!(viewport.start < right_start);
    assert!(!viewport.anchor_locked);

    viewport.start = 0.0;
    assert!(!pan_logic_marker_viewport_at_edge(
        &mut viewport,
        plot,
        plot.left()
    ));
}

#[test]
fn scope_markers_are_saved_and_compared_individually() {
    let mut settings = ScopeMarkerSettings::default();
    let viewport = WaveformViewport::default();
    let mut first = ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Time);
    first.position = 0.2;
    first.color = Color32::RED;
    assert!(first.save(&mut settings));

    let mut second = ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Time);
    second.position = 0.7;
    second.comparison = Some(0);
    assert!(second.save(&mut settings));

    let channel = settings.channels[0];
    assert_eq!(channel.horizontal_count, 2);
    assert_eq!(channel.marker(ScopeMarkerKind::Time, 0).unwrap().0, 0.2);
    assert_eq!(
        channel.marker(ScopeMarkerKind::Time, 0).unwrap().2,
        Color32::RED
    );
    assert_eq!(channel.horizontal_comparisons[1], Some(0));
    assert!(
        (scope_time_marker_delta(
            channel.horizontal_positions[1],
            channel.horizontal_positions[0],
            1_000,
            1_000,
        )
        .unwrap()
            - 0.5)
            .abs()
            < 0.000_001
    );

    assert!(settings.channels[0].remove_marker(ScopeMarkerKind::Time, 0));
    assert_eq!(settings.channels[0].horizontal_count, 1);
    assert_eq!(settings.channels[0].horizontal_comparisons[0], None);
    assert_eq!(settings.channels[0].horizontal_positions[0], 0.7);
}

#[test]
fn logic_decimation_preserves_one_kilohertz_pulses_across_a_one_second_span() {
    let mut samples = VecDeque::from(vec![0_u32; 1_000_000]);
    for pulse in 0..1_000 {
        let start = pulse * 1_000 + 100;
        for sample in samples.range_mut(start..start + 20) {
            *sample = 1;
        }
    }
    let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 320.0));
    let mut enabled_channels = [false; LOGIC_CHANNEL_COUNT];
    enabled_channels[0] = true;
    let levels = logic_signal_layout(plot.top(), plot.height(), &[1; LOGIC_CHANNEL_COUNT]);

    let sample_count = samples.len();
    let sample_stride = 500;
    let buckets = samples
        .make_contiguous()
        .chunks(sample_stride)
        .filter_map(LogicSampleBucket::from_samples)
        .collect::<VecDeque<_>>();
    let points = logic_waveform_points(
        LogicSampleView {
            samples: &buckets,
            sample_count,
            sample_stride,
        },
        0..sample_count,
        plot,
        WaveformViewport::default(),
        &levels.levels,
        &enabled_channels,
    );
    let (high, low) = levels.levels[0];
    let pulses_in_first_200_ms = points[0]
        .windows(2)
        .filter(|edge| {
            edge[0].x < 200.0
                && (edge[0].x - edge[1].x).abs() < f32::EPSILON
                && edge[0].y == low
                && edge[1].y == high
        })
        .count();

    assert_eq!(pulses_in_first_200_ms, 200);
}

#[test]
fn logic_waveform_fills_the_plot_when_high_zoom_shows_one_sample() {
    let samples = VecDeque::from(vec![0_u32, 1, 1, 0]);
    let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 320.0));
    let viewport = WaveformViewport {
        start: 0.4,
        span: 0.01,
        anchor_locked: false,
    };
    let visible_range = viewport.sample_range(samples.len());
    assert_eq!(visible_range, 1..2);
    let mut enabled_channels = [false; LOGIC_CHANNEL_COUNT];
    enabled_channels[0] = true;
    let layout = logic_signal_layout(plot.top(), plot.height(), &[1; LOGIC_CHANNEL_COUNT]);
    let buckets = samples
        .iter()
        .copied()
        .filter_map(|sample| LogicSampleBucket::from_samples(&[sample]))
        .collect::<VecDeque<_>>();

    let points = logic_waveform_points(
        LogicSampleView {
            samples: &buckets,
            sample_count: samples.len(),
            sample_stride: 1,
        },
        visible_range,
        plot,
        viewport,
        &layout.levels,
        &enabled_channels,
    );

    assert_eq!(points[0].first().unwrap().x, plot.left());
    assert_eq!(points[0].last().unwrap().x, plot.right());
    assert_eq!(points[0].first().unwrap().y, points[0].last().unwrap().y);
}

#[test]
fn rolling_scope_uses_the_full_span_as_its_sample_domain() {
    let viewport = WaveformViewport::default();
    let range = viewport.sample_range_in_domain(200_000, 1_000_000);
    let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 400.0));

    assert_eq!(range, 0..200_000);
    assert!((waveform_sample_x(plot, viewport, 199_999.0, 1_000_000) - 200.0).abs() < 0.01);
}

#[test]
fn scope_run_uses_synchronized_frames_for_auto_and_update() {
    assert!(!uses_rolling_scope_buffer(
        AcquisitionMode::Run,
        true,
        DsCopeTriggerSource::Auto,
    ));
    assert!(!uses_rolling_scope_buffer(
        AcquisitionMode::Single,
        true,
        DsCopeTriggerSource::Auto,
    ));
    assert!(!uses_rolling_scope_buffer(
        AcquisitionMode::Run,
        true,
        DsCopeTriggerSource::ChannelZero,
    ));
    assert!(uses_continuous_scope_capture(
        AcquisitionMode::Run,
        CaptureTarget::Oscilloscope,
    ));
    assert!(!uses_continuous_scope_capture(
        AcquisitionMode::Single,
        CaptureTarget::Oscilloscope,
    ));
    assert!(!uses_continuous_scope_capture(
        AcquisitionMode::Run,
        CaptureTarget::Combined,
    ));
    assert!(scope_updates_on_trigger(
        AcquisitionMode::Run,
        CaptureTarget::Oscilloscope,
        DsCopeTriggerSource::ChannelZero,
    ));
    assert!(!scope_updates_on_trigger(
        AcquisitionMode::Run,
        CaptureTarget::Oscilloscope,
        DsCopeTriggerSource::Auto,
    ));

    let config = ScopeCaptureSettings::default().capture_config().unwrap();
    assert_eq!(scope_rolling_sample_capacity(config, Some(0.1)), 1_000_000);
    assert_eq!(
        scope_rolling_sample_capacity(config, Some(2.0)),
        SCOPE_ROLLING_MEMORY_BUDGET_BYTES / 2 / std::mem::size_of::<f32>()
    );
}

#[test]
fn time_scale_selects_readable_units_for_the_visible_window() {
    assert_eq!(time_scale_unit(2.0), ("s", 1.0));
    assert_eq!(time_scale_unit(0.002), ("ms", 1.0e3));
    assert_eq!(time_scale_unit(0.000_002), ("µs", 1.0e6));
    assert_eq!(time_scale_unit(0.000_000_002), ("ns", 1.0e9));
    assert_eq!(time_scale_precision(12.0), 0);
    assert_eq!(time_scale_precision(1.2), 1);
    assert_eq!(time_scale_precision(0.008), 4);
    assert_eq!(time_scale_label_stride(1_000.0), 1);
    assert_eq!(time_scale_label_stride(500.0), 2);
    assert_eq!(time_scale_label_stride(250.0), 5);
    assert_eq!(format_duration(Duration::from_nanos(2)), "2.0 ns");
}

#[test]
fn idle_time_scale_uses_the_configured_display_window() {
    let viewport = WaveformViewport::default();
    let (first, last) = time_scale_sample_window(SAMPLE_COUNT as usize, 0, viewport).unwrap();

    assert_eq!(first, 0.0);
    assert_eq!(last, SAMPLE_COUNT as f64 - 1.0);

    let mut zoomed = viewport;
    zoomed.step_timebase(1.0, SAMPLE_COUNT as usize, SAMPLE_RATE_HZ, 0.5);
    let (zoomed_first, zoomed_last) =
        time_scale_sample_window(SAMPLE_COUNT as usize, 0, zoomed).unwrap();
    assert!(zoomed_last - zoomed_first < last - first);
}

#[test]
fn scope_trigger_levels_use_centered_hardware_and_follow_display_offsets() {
    let plot = Rect::from_min_max(Pos2::new(0.0, 20.0), Pos2::new(100.0, 220.0));
    let channel = ScopeChannelSettings::default();

    assert_eq!(scope_trigger_level_from_y(plot, plot.top(), channel), 34);
    assert_eq!(
        scope_trigger_level_from_y(plot, plot.bottom(), channel),
        222
    );
    assert_eq!(
        scope_trigger_level_from_y(plot, scope_trigger_level_y(plot, channel, 128), channel),
        128
    );
    assert!((scope_trigger_threshold_voltage(channel, 128)).abs() < f32::EPSILON);
    assert!((scope_trigger_threshold_voltage(channel, 104) - 1.021_276_6).abs() < 0.000_001);

    let shifted_channel = ScopeChannelSettings {
        vertical_offset: 160,
        ..channel
    };
    let shifted_zero_y = scope_vertical_offset_y(plot, 160);
    assert_eq!(
        scope_trigger_level_y(plot, shifted_channel, DSCOPE_TRIGGER_LEVEL_CENTER),
        shifted_zero_y
    );
    assert_eq!(
        scope_trigger_level_from_y(plot, shifted_zero_y, shifted_channel),
        DSCOPE_TRIGGER_LEVEL_CENTER
    );
    assert_eq!(
        scope_trigger_threshold_voltage(shifted_channel, 104),
        scope_trigger_threshold_voltage(channel, 104)
    );
    assert_eq!(format_trigger_voltage(0.0), "0.00 mV");
    assert_eq!(format_trigger_voltage(0.125), "125 mV");
    assert_eq!(format_trigger_voltage(-1.25), "-1.25 V");

    let trigger = ScopeTriggerSettings {
        source: DsCopeTriggerSource::ChannelZeroAndOne,
        ..ScopeTriggerSettings::default()
    };
    assert_eq!(scope_trigger_channels(trigger), &[0, 1]);
}

#[test]
fn scope_trigger_anchor_interpolates_the_threshold_crossing() {
    let mut waveforms = WaveformStore {
        scope_channels: vec![
            VecDeque::from(vec![-1.0, -1.0, 1.0, 1.0]),
            VecDeque::from(vec![0.0; 4]),
        ],
        scope_trigger_sample_index: Some(3),
        ..WaveformStore::default()
    };
    let settings = ScopeCaptureSettings::default();
    let mut trigger = ScopeTriggerSettings {
        source: DsCopeTriggerSource::ChannelZero,
        slope: DsCopeTriggerSlope::Rising,
        ..ScopeTriggerSettings::default()
    };

    assert_eq!(
        scope_trigger_crossing_sample(&waveforms, settings, trigger),
        Some(ScopeTriggerCrossing {
            sample: 1.5,
            channel_index: 0,
        })
    );

    waveforms.scope_channels[0] = VecDeque::from(vec![1.0, 1.0, -1.0, -1.0]);
    trigger.slope = DsCopeTriggerSlope::Falling;
    assert_eq!(
        scope_trigger_crossing_sample(&waveforms, settings, trigger),
        Some(ScopeTriggerCrossing {
            sample: 1.5,
            channel_index: 0,
        })
    );
}

#[test]
fn scope_trigger_anchor_selects_the_nearest_matching_edge() {
    let mut samples = vec![-1.0; 32];
    samples[11..16].fill(1.0);
    samples[21..].fill(1.0);
    let waveforms = WaveformStore {
        scope_channels: vec![VecDeque::from(samples), VecDeque::from(vec![0.0; 32])],
        scope_trigger_sample_index: Some(19),
        ..WaveformStore::default()
    };
    let trigger = ScopeTriggerSettings {
        source: DsCopeTriggerSource::ChannelZero,
        slope: DsCopeTriggerSlope::Rising,
        ..ScopeTriggerSettings::default()
    };

    assert_eq!(
        scope_trigger_crossing_sample(&waveforms, ScopeCaptureSettings::default(), trigger),
        Some(ScopeTriggerCrossing {
            sample: 20.5,
            channel_index: 0,
        })
    );
}

#[test]
fn scope_auto_trigger_locks_a_free_running_frame_when_it_contains_an_edge() {
    let mut samples = vec![-1.0; 32];
    samples[11..16].fill(1.0);
    samples[21..].fill(1.0);
    let mut waveforms = WaveformStore {
        scope_channels: vec![VecDeque::from(samples), VecDeque::from(vec![0.0; 32])],
        scope_trigger_sample_index: None,
        ..WaveformStore::default()
    };
    let trigger = ScopeTriggerSettings {
        position_percent: 65,
        source: DsCopeTriggerSource::Auto,
        auto_channel: 0,
        slope: DsCopeTriggerSlope::Rising,
        ..ScopeTriggerSettings::default()
    };

    assert_eq!(
        scope_trigger_crossing_sample(&waveforms, ScopeCaptureSettings::default(), trigger),
        Some(ScopeTriggerCrossing {
            sample: 20.5,
            channel_index: 0,
        })
    );

    waveforms.scope_channels[0]
        .iter_mut()
        .for_each(|sample| *sample = 0.0);
    assert_eq!(
        scope_trigger_crossing_sample(&waveforms, ScopeCaptureSettings::default(), trigger),
        None
    );
}

#[test]
fn scope_trigger_detection_ignores_capture_boundaries() {
    let waveforms = WaveformStore {
        scope_channels: vec![
            VecDeque::from(vec![-1.0, -1.0, 1.0, 1.0]),
            VecDeque::from(vec![0.0; 4]),
        ],
        scope_capture_boundaries: VecDeque::from([2]),
        ..WaveformStore::default()
    };

    assert_eq!(
        scope_trigger_crossing_sample(
            &waveforms,
            ScopeCaptureSettings::default(),
            ScopeTriggerSettings::default(),
        ),
        None
    );
}

#[test]
fn scope_trace_and_trigger_use_the_same_sample_coordinates() {
    let plot = Rect::from_min_max(Pos2::new(100.0, 20.0), Pos2::new(900.0, 420.0));
    let viewport = WaveformViewport {
        start: 0.25,
        span: 0.5,
        ..WaveformViewport::default()
    };

    assert!((scope_sample_fraction(1_050.0, 1_000, 101) - 0.5).abs() < f32::EPSILON);
    assert!((waveform_sample_x(plot, viewport, 50.0, 101) - 500.0).abs() < f32::EPSILON);
}

#[test]
fn scope_smart_timing_snaps_the_pointer_to_the_triggering_signal() {
    let waveforms = WaveformStore {
        scope_channels: vec![
            VecDeque::from(vec![-1.0, -1.0, 1.0, 0.25, 0.5]),
            VecDeque::from(vec![0.0; 5]),
        ],
        scope_rate_hz: 1_000_000,
        scope_sample_offset: 1_000,
        scope_trigger_sample_index: Some(1_002),
        ..WaveformStore::default()
    };
    let settings = ScopeCaptureSettings::default();
    let trigger = ScopeTriggerSettings {
        source: DsCopeTriggerSource::ChannelZero,
        slope: DsCopeTriggerSlope::Rising,
        ..ScopeTriggerSettings::default()
    };
    let crossing = scope_trigger_crossing_sample(&waveforms, settings, trigger).unwrap();
    let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 100.0));

    let measurement = scope_smart_timing_measurement(
        &waveforms,
        crossing,
        plot,
        WaveformViewport::default(),
        5,
        Pos2::new(75.0, 90.0),
    )
    .unwrap();

    assert_eq!(measurement.start_sample, 1_001.5);
    assert_eq!(measurement.end_sample, 1_003.0);
    assert_eq!(measurement.channel_index, 0);
    assert_eq!(measurement.end_voltage, 0.25);
    assert_eq!(
        measurement.duration(waveforms.scope_rate_hz),
        Some(Duration::from_nanos(1_500))
    );
    assert_eq!(
        scope_smart_timing_measurement(
            &waveforms,
            crossing,
            plot,
            WaveformViewport::default(),
            5,
            Pos2::new(101.0, 50.0),
        ),
        None
    );
}

#[test]
fn scope_time_marker_coordinates_stay_attached_to_the_capture() {
    let plot = Rect::from_min_max(Pos2::new(100.0, 20.0), Pos2::new(900.0, 420.0));
    let mut viewport = WaveformViewport {
        start: 0.25,
        span: 0.5,
        ..WaveformViewport::default()
    };
    let marker_position = viewport.domain_fraction_at_screen(0.5);

    assert!((marker_position - 0.5).abs() < f32::EPSILON);
    assert!(
        (egui::lerp(
            plot.x_range(),
            viewport.screen_fraction_at_domain(marker_position)
        ) - 500.0)
            .abs()
            < f32::EPSILON
    );

    assert!(viewport.pan_by_screen_delta(-100.0, plot.width()));
    assert!(
        (egui::lerp(
            plot.x_range(),
            viewport.screen_fraction_at_domain(marker_position)
        ) - 400.0)
            .abs()
            < f32::EPSILON
    );
    assert!((scope_time_marker_delta(0.25, 0.5, 1_000, 1_000).unwrap() - 0.25).abs() < 0.000_001);
}

#[test]
fn scope_trigger_position_maps_between_percent_and_plot_x() {
    let plot = Rect::from_min_max(Pos2::new(100.0, 20.0), Pos2::new(900.0, 420.0));

    assert!((scope_trigger_position_x(plot, 50) - 500.0).abs() < f32::EPSILON);
    assert_eq!(scope_trigger_position_from_x(plot, 500.0), 50);
    assert_eq!(
        scope_trigger_position_from_x(plot, plot.left()),
        DSCOPE_TRIGGER_POSITION_MIN_PERCENT
    );
    assert_eq!(
        scope_trigger_position_from_x(plot, plot.right()),
        DSCOPE_TRIGGER_POSITION_MAX_PERCENT
    );

    let mut viewport = WaveformViewport {
        start: 0.25,
        span: 0.5,
        ..WaveformViewport::default()
    };
    assert_eq!(scope_trigger_position_in_view(0.5, viewport), 50);
    assert!(viewport.pan_by_screen_delta(-100.0, 1_000.0));
    assert_eq!(scope_trigger_position_in_view(0.5, viewport), 40);
    let panned_start = viewport.start;
    let mut trigger_settings = ScopeTriggerSettings::default();
    finish_scope_viewport_pan(0.5, &mut viewport, &mut trigger_settings);
    assert_eq!(trigger_settings.position_percent, 40);
    assert_eq!(viewport.start, panned_start);
    assert!(!viewport.anchor_locked);

    viewport.start = 0.25;
    viewport.anchor_locked = true;
    viewport.move_screen_anchor(0.5, 0.25);
    assert!((viewport.start - 0.375).abs() < f32::EPSILON);
    assert!(((0.5 - viewport.start) / viewport.span - 0.25).abs() < f32::EPSILON);

    let mut full_span = WaveformViewport::default();
    full_span.align_trigger_to_screen(0.75, 0.5);
    assert_eq!(full_span.start, 0.0);
    assert_eq!(full_span.span, 1.0);
    assert!(((0.75 - full_span.start) / full_span.span - 0.75).abs() < f32::EPSILON);

    let mut hardware_anchored = WaveformViewport {
        start: 0.25,
        span: 0.5,
        anchor_locked: false,
    };
    hardware_anchored.step_timebase_at_sample(1.0, 1_000_000, 1_000_000, 0.503, 0.5);
    assert!(hardware_anchored.anchor_locked);
    assert!(hardware_anchored.span < 1.0);
    assert!(
        ((0.503 - hardware_anchored.start) / hardware_anchored.span - 0.5).abs() < f32::EPSILON
    );
    hardware_anchored.step_timebase_at_sample(-1.0, 1_000_000, 1_000_000, 0.503, 0.5);
    assert!(hardware_anchored.anchor_locked);
    assert!(
        ((0.503 - hardware_anchored.start) / hardware_anchored.span - 0.5).abs() < f32::EPSILON
    );
}
