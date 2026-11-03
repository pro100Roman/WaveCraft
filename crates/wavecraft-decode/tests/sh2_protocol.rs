use std::collections::BTreeMap;

use wavecraft_decode::{
    load_protocol_schema, ByteStatus, CustomProtocolDecoder, CustomProtocolRuntime, DecodedValue,
    FramingSchema, PacketCompleteness, SampleRange, Severity, TimedByte, TransportKind,
    TransportPacket,
};

const SH2_I2C_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/examples/bno08x-sh2-i2c.wavecraft-protocol.yaml"
));

fn bytes(values: &[u8]) -> Vec<TimedByte> {
    bytes_at(values, 0)
}

fn bytes_at(values: &[u8], first_sample: u64) -> Vec<TimedByte> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| TimedByte {
            value: *value,
            range: SampleRange {
                start: first_sample + (index * 10) as u64,
                end: first_sample + (index * 10 + 8) as u64,
            },
            status: ByteStatus::default(),
        })
        .collect()
}

#[test]
fn decodes_documented_sh2_accelerometer_report() {
    let decoder = CustomProtocolDecoder::new(load_protocol_schema(SH2_I2C_SCHEMA).unwrap());
    let packet = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[
                0x13, 0x00, 0x03, 0x07, 0xFB, 0x0A, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x05, 0x00,
                0x01, 0x00, 0xFF, 0x00, 0x02,
            ]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };

    let message = decoder.decode_packet(&packet).unwrap().unwrap();

    assert_eq!(message.message_id, "accelerometer");
    assert_eq!(message.fields[0].value, DecodedValue::Integer(19));
    assert_eq!(
        message.summary,
        "Accelerometer (includes gravity) · X 1 m/s^2 · Y -1 m/s^2 · Z 2 m/s^2 · accuracy high"
    );
    assert_eq!(
        message.fields[5].display.as_deref(),
        Some("accelerometer (acceleration including gravity)")
    );
    assert_eq!(message.fields[7].display.as_deref(), Some("high"));
    assert_eq!(message.fields[9].display.as_deref(), Some("1 m/s^2"));
    assert_eq!(message.severity, Severity::Info);
}

#[test]
fn decodes_sh2_vector_selection_and_feature_configuration() {
    let decoder = CustomProtocolDecoder::new(load_protocol_schema(SH2_I2C_SCHEMA).unwrap());
    let get_request = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[0x06, 0x00, 0x02, 0x21, 0xFE, 0x16]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };
    let set_command = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[
                0x15, 0x00, 0x02, 0x22, 0xFD, 0x16, 0x03, 0x00, 0x01, 0x10, 0x27, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };
    let get_response = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[
                0x15, 0x00, 0x02, 0x23, 0xFC, 0x16, 0x03, 0x00, 0x01, 0x10, 0x27, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };

    let request = decoder.decode_packet(&get_request).unwrap().unwrap();
    let command = decoder.decode_packet(&set_command).unwrap().unwrap();
    let response = decoder.decode_packet(&get_response).unwrap().unwrap();

    assert_eq!(request.message_id, "get_feature_request");
    assert_eq!(
        request.fields[4].display.as_deref(),
        Some("raw_magnetometer")
    );
    assert_eq!(command.message_id, "set_feature_command");
    assert_eq!(
        command.fields[4].display.as_deref(),
        Some("raw_magnetometer")
    );
    assert_eq!(command.fields[7].display.as_deref(), Some("10000 µs"));
    assert_eq!(response.message_id, "get_feature_response");
    assert_eq!(
        response.fields[4].display.as_deref(),
        Some("raw_magnetometer")
    );

    let flags = &command.fields[5].children;
    assert_eq!(
        flags
            .iter()
            .find(|field| field.name == "change_sensitivity_relative")
            .map(|field| &field.value),
        Some(&DecodedValue::Boolean(true))
    );
    assert_eq!(
        flags
            .iter()
            .find(|field| field.name == "change_sensitivity_enabled")
            .map(|field| &field.value),
        Some(&DecodedValue::Boolean(true))
    );
}

#[test]
fn labels_and_scales_sh2_gyroscope_report() {
    let decoder = CustomProtocolDecoder::new(load_protocol_schema(SH2_I2C_SCHEMA).unwrap());
    let packet = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[
                0x13, 0x00, 0x03, 0x08, 0xFB, 0x00, 0x00, 0x00, 0x00, 0x02, 0x01, 0x02, 0x00, 0x00,
                0x02, 0x00, 0xFF, 0x00, 0x04,
            ]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };

    let message = decoder.decode_packet(&packet).unwrap().unwrap();

    assert_eq!(message.message_id, "gyroscope_calibrated");
    assert_eq!(
        message.summary,
        "Calibrated gyroscope (angular velocity) · X 1 rad/s · Y -0.5 rad/s · Z 2 rad/s · accuracy medium"
    );
    assert_eq!(message.fields[9].name, "angular_velocity_x");
    assert_eq!(message.fields[9].display.as_deref(), Some("1 rad/s"));
}

#[test]
fn decodes_all_sh2_raw_physical_vectors() {
    let decoder = CustomProtocolDecoder::new(load_protocol_schema(SH2_I2C_SCHEMA).unwrap());

    for (report_id, expected_message_id, expected_x_field) in [
        (0x14, "raw_accelerometer", "raw_acceleration_x"),
        (0x15, "raw_gyroscope", "raw_angular_velocity_x"),
        (0x16, "raw_magnetometer", "raw_magnetic_field_x"),
    ] {
        let packet = TransportPacket {
            transport: TransportKind::I2c,
            streams: BTreeMap::from([(
                "data".to_owned(),
                bytes(&[
                    0x19, 0x00, 0x03, 0x6B, 0xFB, 0x00, 0x00, 0x00, 0x00, report_id, 0x07, 0x03,
                    0x00, 0x34, 0x12, 0xFE, 0xFF, 0x00, 0x80, 0x00, 0x00, 0x40, 0xE2, 0x01, 0x00,
                ]),
            )]),
            attributes: BTreeMap::new(),
            completeness: PacketCompleteness::Complete,
        };

        let message = decoder.decode_packet(&packet).unwrap().unwrap();

        assert_eq!(message.message_id, expected_message_id);
        assert_eq!(message.fields[9].name, expected_x_field);
        assert_eq!(
            message.fields[9].display.as_deref(),
            Some("4660 ADC counts")
        );
        assert_eq!(message.fields[13].name, "sensor_timestamp");
        assert_eq!(message.fields[13].display.as_deref(), Some("123456 us"));
    }
}

#[test]
fn decodes_raw_accelerometer_and_magnetometer_without_inventing_gyroscope() {
    let schema = load_protocol_schema(SH2_I2C_SCHEMA).unwrap();
    let mut runtime = CustomProtocolRuntime::new(schema).unwrap();
    let packet = TransportPacket::single_stream(
        TransportKind::I2c,
        "data",
        bytes(&[
            0x29, 0x00, 0x03, 0x6B, // SHTP header: 41 bytes
            0xFB, 0x00, 0x00, 0x00, 0x00, // Base timestamp
            0x14, 0x01, 0x03, 0x00, 0x34, 0x12, 0xFE, 0xFF, 0x00, 0x80, 0x00, 0x00, 0x40, 0xE2,
            0x01, 0x00, // Raw accelerometer
            0x16, 0x02, 0x03, 0x00, 0x78, 0x56, 0xFC, 0xFF, 0x00, 0x40, 0x00, 0x00, 0x44, 0xE2,
            0x01, 0x00, // Raw magnetometer
        ]),
        PacketCompleteness::Complete,
    );

    let output = runtime.push_packet(&packet, 10_000_000);

    assert!(output.diagnostics.is_empty());
    assert_eq!(output.messages.len(), 2);
    assert_eq!(output.messages[0].message_id, "raw_accelerometer");
    assert_eq!(output.messages[1].message_id, "raw_magnetometer");
    assert!(output
        .messages
        .iter()
        .all(|message| message.message_id != "raw_gyroscope"));
}

#[test]
fn decodes_all_sh2_uncalibrated_vectors_with_bias() {
    let decoder = CustomProtocolDecoder::new(load_protocol_schema(SH2_I2C_SCHEMA).unwrap());

    for (report_id, expected_message_id, expected_unit) in [
        (0x07, "gyroscope_uncalibrated", "rad/s"),
        (0x0F, "magnetic_field_uncalibrated", "µT"),
    ] {
        let packet = TransportPacket {
            transport: TransportKind::I2c,
            streams: BTreeMap::from([(
                "data".to_owned(),
                bytes(&[
                    0x19, 0x00, 0x03, 0x6B, 0xFB, 0x00, 0x00, 0x00, 0x00, report_id, 0x07, 0x02,
                    0x00, 0x00, 0x02, 0x00, 0xFC, 0x00, 0x04, 0x00, 0x01, 0x00, 0xFF, 0x00, 0x02,
                ]),
            )]),
            attributes: BTreeMap::new(),
            completeness: PacketCompleteness::Complete,
        };

        let message = decoder.decode_packet(&packet).unwrap().unwrap();

        assert_eq!(message.message_id, expected_message_id);
        assert!(message.fields[9]
            .display
            .as_deref()
            .is_some_and(|display| display.ends_with(expected_unit)));
        assert!(message.fields[12]
            .display
            .as_deref()
            .is_some_and(|display| display.ends_with(expected_unit)));
    }
}

#[test]
fn decodes_sh2_gyro_integrated_rotation_vector_channel() {
    let decoder = CustomProtocolDecoder::new(load_protocol_schema(SH2_I2C_SCHEMA).unwrap());
    let packet = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[
                0x12, 0x00, 0x05, 0x6B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x04,
                0x00, 0xFC, 0x00, 0x08,
            ]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };

    let message = decoder.decode_packet(&packet).unwrap().unwrap();

    assert_eq!(message.message_id, "gyro_integrated_rotation_vector");
    assert_eq!(message.fields[6].display.as_deref(), Some("1"));
    assert_eq!(message.fields[7].display.as_deref(), Some("1 rad/s"));
    assert_eq!(message.fields[8].display.as_deref(), Some("-1 rad/s"));
    assert_eq!(message.fields[9].display.as_deref(), Some("2 rad/s"));
}

#[test]
fn labels_and_scales_sh2_rotation_vector_report() {
    let decoder = CustomProtocolDecoder::new(load_protocol_schema(SH2_I2C_SCHEMA).unwrap());
    let packet = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[
                0x17, 0x00, 0x03, 0x09, 0xFB, 0x00, 0x00, 0x00, 0x00, 0x05, 0x01, 0x03, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x10,
            ]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };

    let message = decoder.decode_packet(&packet).unwrap().unwrap();

    assert_eq!(message.message_id, "rotation_vector");
    assert_eq!(
        message.summary,
        "Rotation vector (orientation quaternion) · i 0 · j 0 · k 0 · real 1 · heading accuracy 1 rad · sensor accuracy high"
    );
    assert_eq!(message.fields[13].name, "heading_accuracy");
    assert_eq!(message.fields[13].display.as_deref(), Some("1 rad"));
}

#[test]
fn labels_and_scales_sh2_geomagnetic_rotation_vector_report() {
    let decoder = CustomProtocolDecoder::new(load_protocol_schema(SH2_I2C_SCHEMA).unwrap());
    let packet = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[
                0x17, 0x00, 0x03, 0x49, 0xFA, 0x00, 0x00, 0x00, 0x00, 0x09, 0x01, 0x03, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x10,
            ]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };

    let message = decoder.decode_packet(&packet).unwrap().unwrap();

    assert_eq!(message.message_id, "geomagnetic_rotation_vector");
    assert_eq!(
        message.fields[3].display.as_deref(),
        Some("timestamp rebase")
    );
    assert_eq!(
        message.summary,
        "Geomagnetic rotation vector (orientation from accelerometer + magnetometer) · i 0 · j 0 · k 0 · real 1 · heading accuracy 1 rad · sensor accuracy high"
    );
}

#[test]
fn labels_and_scales_sh2_arvr_stabilized_rotation_reports() {
    let decoder = CustomProtocolDecoder::new(load_protocol_schema(SH2_I2C_SCHEMA).unwrap());
    let rotation_packet = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[
                0x17, 0x00, 0x03, 0x6B, 0xFB, 0x00, 0x00, 0x00, 0x00, 0x28, 0x01, 0x02, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x10,
            ]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };
    let game_packet = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[
                0x15, 0x00, 0x03, 0x6C, 0xFB, 0x00, 0x00, 0x00, 0x00, 0x29, 0x02, 0x01, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40,
            ]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };

    let rotation = decoder.decode_packet(&rotation_packet).unwrap().unwrap();
    let game = decoder.decode_packet(&game_packet).unwrap().unwrap();

    assert_eq!(rotation.message_id, "arvr_stabilized_rotation_vector");
    assert_eq!(
        rotation.summary,
        "AR/VR-stabilized rotation vector (motion-corrected orientation) · i 0 · j 0 · k 0 · real 1 · heading accuracy 1 rad · sensor accuracy medium"
    );
    assert_eq!(game.message_id, "arvr_stabilized_game_rotation_vector");
    assert_eq!(
        game.summary,
        "AR/VR-stabilized game rotation vector (motion-corrected orientation without magnetic north) · i 0 · j 0 · k 0 · real 1 · sensor accuracy low"
    );
}

#[test]
fn masks_shtp_continuation_bit_out_of_packet_length() {
    let decoder = CustomProtocolDecoder::new(load_protocol_schema(SH2_I2C_SCHEMA).unwrap());
    let packet = TransportPacket {
        transport: TransportKind::I2c,
        streams: BTreeMap::from([(
            "data".to_owned(),
            bytes(&[
                0x19, 0x80, 0x03, 0x07, 0xFB, 0x00, 0x00, 0x00, 0x00, 0x15, 0x07, 0x03, 0x00, 0x34,
                0x12, 0xFE, 0xFF, 0x00, 0x80, 0x00, 0x00, 0x40, 0xE2, 0x01, 0x00,
            ]),
        )]),
        attributes: BTreeMap::new(),
        completeness: PacketCompleteness::Complete,
    };

    let message = decoder.decode_packet(&packet).unwrap().unwrap();

    assert_eq!(message.message_id, "shtp_packet");
    assert_eq!(message.summary, "SHTP input_normal · seq 7 · 25 bytes");
    assert_eq!(message.fields[0].value, DecodedValue::Integer(25));
    assert_eq!(
        message.fields[0].children[0].value,
        DecodedValue::Boolean(true)
    );
}

#[test]
fn reassembles_shtp_header_read_and_continuation_before_sh2_decode() {
    let schema = load_protocol_schema(SH2_I2C_SCHEMA).unwrap();
    let mut runtime = CustomProtocolRuntime::new(schema).unwrap();
    let header_read = TransportPacket::single_stream(
        TransportKind::I2c,
        "data",
        bytes_at(&[0x17, 0x00, 0x03, 0x48], 0),
        PacketCompleteness::Complete,
    );
    let continuation = TransportPacket::single_stream(
        TransportKind::I2c,
        "data",
        bytes_at(
            &[
                0x17, 0x80, 0x03, 0x49, 0xFA, 0x00, 0x00, 0x00, 0x00, 0x09, 0x01, 0x03, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x10,
            ],
            100,
        ),
        PacketCompleteness::Complete,
    );

    let first = runtime.push_packet(&header_read, 10_000_000);
    assert!(first.messages.is_empty());
    assert!(first.diagnostics.is_empty());

    let completed = runtime.push_packet(&continuation, 10_000_000);
    assert!(completed.diagnostics.is_empty());
    assert_eq!(completed.messages.len(), 1);
    assert_eq!(
        completed.messages[0].message_id,
        "geomagnetic_rotation_vector"
    );
    assert_eq!(
        completed.messages[0].summary,
        "Geomagnetic rotation vector (orientation from accelerometer + magnetometer) · i 0 · j 0 · k 0 · real 1 · heading accuracy 1 rad · sensor accuracy high"
    );
}

#[test]
fn loads_documented_sh2_i2c_schema() {
    let schema = load_protocol_schema(SH2_I2C_SCHEMA).unwrap();

    assert_eq!(schema.protocol.id, "ceva.bno08x.sh2-i2c");
    assert_eq!(schema.input.transport, TransportKind::I2c);
    assert_eq!(schema.messages.len(), 22);
    let FramingSchema::SegmentedTransportTransaction {
        max_frame_bytes,
        segmentation,
    } = &schema.framing
    else {
        panic!("SH-2 schema should use segmented transport framing");
    };
    assert_eq!(*max_frame_bytes, 32_767);
    let records = segmentation
        .records
        .as_ref()
        .expect("SH-2 sensor channels should decode record streams");
    assert_eq!(records.channels.iter().copied().collect::<Vec<_>>(), [3, 4]);
    assert_eq!(records.record_lengths.get(&0x14), Some(&16));
    assert_eq!(records.record_lengths.get(&0x15), Some(&16));
    assert_eq!(records.record_lengths.get(&0x16), Some(&16));
    assert!(schema.messages.iter().all(|message| {
        message
            .fields
            .iter()
            .find(|field| field.name == "packet_length")
            .is_some_and(|field| field.mask == Some(0x7FFF))
    }));
    let accelerometer = schema
        .messages
        .iter()
        .find(|message| message.id == "accelerometer")
        .unwrap();
    let acceleration_x = accelerometer
        .fields
        .iter()
        .find(|field| field.name == "acceleration_x")
        .unwrap();
    assert_eq!(acceleration_x.display.fixed_point_q, Some(8));
    assert_eq!(acceleration_x.display.unit.as_deref(), Some("m/s^2"));
}
