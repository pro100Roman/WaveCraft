//! Saving and restoring capture settings through `.dsv` recording metadata.
//!
//! The metadata keys written here are part of the recording format (see
//! `docs/recording-format.md`). Renaming a key or dropping a legacy fallback
//! makes older recordings reopen with default settings.
//!
//! Most settings are described once, in a [`MetadataField`] or
//! [`ChannelField`] table entry, and that entry drives both saving and loading,
//! so a key cannot be written under one name and read under another. Settings
//! whose encoding depends on other keys are kept as hand-written code next to
//! the tables and are marked as such: the logic trigger, the radio analyzer's
//! channel list and gains, and the legacy radio analyzer flag.

use super::{
    coupling_label, logic_trigger_condition_key, normalized_channel_name,
    parse_logic_trigger_condition, AcquisitionMode, BTreeMap, Color32, DsCopeBandwidthLimit,
    DsCopeCoupling, DsCopeOperationMode, DsCopeTriggerSlope, DsCopeTriggerSource,
    DsLogicInputFilter, DsLogicOperationMode, DsLogicStopOption, LogicCaptureSettings,
    LogicTriggerMode, RadioAnalyzerChannelSettings, RadioDataMode, RadioDemodulation,
    RadioSettings, RadioWhiteningPreset, ScopeCaptureSettings, TriggerHoldoffUnit,
    WhiteningDirection, DSCOPE_SAMPLE_COUNTS, DSCOPE_SAMPLE_RATES_HZ,
    DSCOPE_TRIGGER_HOLDOFF_MAX_NS, DSCOPE_TRIGGER_LEVEL_MAX, DSCOPE_TRIGGER_LEVEL_MIN,
    DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX, DSCOPE_TRIGGER_POSITION_MAX_PERCENT,
    DSCOPE_TRIGGER_POSITION_MIN_PERCENT, DSCOPE_VERTICAL_OFFSET_MAX, DSCOPE_VERTICAL_OFFSET_MIN,
    DSCOPE_VOLTS_PER_DIV_MV, DSLOGIC_SAMPLE_ALIGNMENT, DSLOGIC_SAMPLE_RATES_HZ,
    DSLOGIC_TRIGGER_STAGE_COUNT, LOGIC_CHANNEL_COUNT, MAX_RADIO_ANALYZER_CHANNELS,
    RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS,
};

pub(crate) fn insert_channel_name_metadata(
    metadata: &mut BTreeMap<String, String>,
    logic_channel_names: &[String; LOGIC_CHANNEL_COUNT],
    scope_channel_names: &[String; 2],
) {
    for (index, name) in logic_channel_names.iter().enumerate() {
        metadata.insert(
            format!("logic.ch{index}.name"),
            normalized_channel_name(name),
        );
    }
    for (index, name) in scope_channel_names.iter().enumerate() {
        metadata.insert(
            format!("scope.ch{index}.name"),
            normalized_channel_name(name),
        );
    }
}

pub(crate) fn parse_logic_acquisition_mode(value: &str) -> Option<AcquisitionMode> {
    match value {
        "single" => Some(AcquisitionMode::Single),
        "repetitive" => Some(AcquisitionMode::Repetitive),
        "loop" => Some(AcquisitionMode::Loop),
        "run" => Some(AcquisitionMode::Run),
        _ => None,
    }
}

/// One persisted setting: the key it is stored under and how to write and read
/// it. `load` ignores values that fail to parse or fall outside the allowed
/// range, leaving the default in place.
struct MetadataField<S> {
    key: &'static str,
    save: fn(&S) -> String,
    load: fn(&mut S, &str),
}

/// A persisted setting that exists once per channel. `key` builds the key for a
/// channel index.
struct ChannelField<S> {
    key: fn(usize) -> String,
    save: fn(&S, usize) -> String,
    load: fn(&mut S, usize, &str),
}

/// A field that is stored as `Display` text and read back with `FromStr`.
macro_rules! parsed_field {
    ($key:literal, $($path:ident).+) => {
        MetadataField {
            key: $key,
            save: |settings| settings.$($path).+.to_string(),
            load: |settings, value| {
                if let Ok(parsed) = value.parse() {
                    settings.$($path).+ = parsed;
                }
            },
        }
    };
}

fn write_fields<S>(
    metadata: &mut BTreeMap<String, String>,
    settings: &S,
    fields: &[MetadataField<S>],
) {
    for field in fields {
        metadata.insert(field.key.to_owned(), (field.save)(settings));
    }
}

fn read_fields<S>(
    settings: &mut S,
    metadata: &BTreeMap<String, String>,
    fields: &[MetadataField<S>],
) {
    for field in fields {
        if let Some(value) = metadata.get(field.key) {
            (field.load)(settings, value);
        }
    }
}

fn write_channel_fields<S>(
    metadata: &mut BTreeMap<String, String>,
    settings: &S,
    channel_count: usize,
    fields: &[ChannelField<S>],
) {
    for channel in 0..channel_count {
        for field in fields {
            metadata.insert((field.key)(channel), (field.save)(settings, channel));
        }
    }
}

fn read_channel_fields<S>(
    settings: &mut S,
    metadata: &BTreeMap<String, String>,
    channel_count: usize,
    fields: &[ChannelField<S>],
) {
    for channel in 0..channel_count {
        for field in fields {
            if let Some(value) = metadata.get(&(field.key)(channel)) {
                (field.load)(settings, channel, value);
            }
        }
    }
}

fn parse_bool_into(value: &str, destination: &mut bool) {
    if let Ok(parsed) = value.parse() {
        *destination = parsed;
    }
}

/// Logic settings that are one key each. `operation_mode` must stay before
/// `sample_count`: the largest accepted sample count depends on the mode.
const LOGIC_FIELDS: &[MetadataField<LogicCaptureSettings>] = &[
    MetadataField {
        key: "logic.sample_rate_hz",
        save: |settings| settings.sample_rate_hz.to_string(),
        load: |settings, value| {
            if let Ok(sample_rate_hz) = value.parse::<u64>() {
                if DSLOGIC_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
                    settings.sample_rate_hz = sample_rate_hz;
                }
            }
        },
    },
    MetadataField {
        key: "logic.operation_mode",
        save: |settings| {
            match settings.operation_mode {
                DsLogicOperationMode::Buffer => "buffer",
                DsLogicOperationMode::Stream => "stream",
                DsLogicOperationMode::InternalTest => "internal_test",
            }
            .to_owned()
        },
        load: |settings, value| {
            settings.operation_mode = match value {
                "stream" => DsLogicOperationMode::Stream,
                "internal_test" => DsLogicOperationMode::InternalTest,
                _ => DsLogicOperationMode::Buffer,
            };
        },
    },
    MetadataField {
        key: "logic.sample_count",
        save: |settings| settings.sample_count.to_string(),
        load: |settings, value| {
            if let Ok(sample_count) = value.parse::<u64>() {
                if (DSLOGIC_SAMPLE_ALIGNMENT..=settings.maximum_sample_count())
                    .contains(&sample_count)
                {
                    settings.sample_count = sample_count;
                }
            }
        },
    },
    MetadataField {
        key: "logic.stop_option",
        save: |settings| {
            match settings.stop_option {
                DsLogicStopOption::StopImmediately => "stop_immediately",
                DsLogicStopOption::UploadCapturedData => "upload_captured_data",
            }
            .to_owned()
        },
        load: |settings, value| {
            settings.stop_option = match value {
                "stop_immediately" => DsLogicStopOption::StopImmediately,
                _ => DsLogicStopOption::UploadCapturedData,
            };
        },
    },
    MetadataField {
        key: "logic.threshold_voltage",
        save: |settings| settings.threshold_voltage.to_string(),
        load: |settings, value| {
            if let Ok(threshold_voltage) = value.parse::<f32>() {
                if threshold_voltage.is_finite() && (0.0..=5.0).contains(&threshold_voltage) {
                    settings.threshold_voltage = threshold_voltage;
                }
            }
        },
    },
    MetadataField {
        key: "logic.input_filter",
        save: |settings| {
            match settings.input_filter {
                DsLogicInputFilter::None => "none",
                DsLogicInputFilter::OneSampleClock => "one_sample_clock",
            }
            .to_owned()
        },
        load: |settings, value| {
            settings.input_filter = match value {
                "one_sample_clock" => DsLogicInputFilter::OneSampleClock,
                _ => DsLogicInputFilter::None,
            };
        },
    },
    parsed_field!("logic.rle_enabled", rle_enabled),
    parsed_field!("logic.external_clock", external_clock),
    parsed_field!("logic.falling_clock_edge", falling_clock_edge),
];

const LOGIC_CHANNEL_FIELDS: &[ChannelField<LogicCaptureSettings>] = &[
    ChannelField {
        key: |channel| format!("logic.ch{channel}.enabled"),
        save: |settings, channel| settings.enabled_channels[channel].to_string(),
        load: |settings, channel, value| {
            parse_bool_into(value, &mut settings.enabled_channels[channel]);
        },
    },
    ChannelField {
        key: |channel| format!("logic.ch{channel}.maximum_height"),
        save: |settings, channel| settings.maximum_heights[channel].to_string(),
        load: |settings, channel, value| {
            if let Ok(maximum_height) = value.parse::<u8>() {
                if (1..=5).contains(&maximum_height) {
                    settings.maximum_heights[channel] = maximum_height;
                }
            }
        },
    },
];

/// Scope settings that are one key each.
const SCOPE_FIELDS: &[MetadataField<ScopeCaptureSettings>] = &[
    MetadataField {
        key: "scope.sample_rate_hz",
        save: |settings| settings.sample_rate_hz.to_string(),
        load: |settings, value| {
            if let Ok(sample_rate_hz) = value.parse::<u64>() {
                if DSCOPE_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
                    settings.sample_rate_hz = sample_rate_hz;
                }
            }
        },
    },
    MetadataField {
        key: "scope.sample_count",
        save: |settings| settings.sample_count.to_string(),
        load: |settings, value| {
            if let Ok(sample_count) = value.parse::<u64>() {
                if DSCOPE_SAMPLE_COUNTS.contains(&sample_count) {
                    settings.sample_count = sample_count;
                }
            }
        },
    },
    MetadataField {
        key: "scope.operation_mode",
        save: |settings| {
            match settings.operation_mode {
                DsCopeOperationMode::Normal => "normal",
                DsCopeOperationMode::InternalTest => "internal_test",
            }
            .to_owned()
        },
        load: |settings, value| {
            settings.operation_mode = match value {
                "internal_test" => DsCopeOperationMode::InternalTest,
                _ => DsCopeOperationMode::Normal,
            };
        },
    },
    MetadataField {
        key: "scope.bandwidth_limit",
        save: |settings| {
            match settings.bandwidth_limit {
                DsCopeBandwidthLimit::Full => "full",
                DsCopeBandwidthLimit::Mhz20 => "20_mhz",
            }
            .to_owned()
        },
        load: |settings, value| {
            settings.bandwidth_limit = match value {
                "20_mhz" => DsCopeBandwidthLimit::Mhz20,
                _ => DsCopeBandwidthLimit::Full,
            };
        },
    },
    MetadataField {
        key: "scope.trigger.position_percent",
        save: |settings| settings.trigger.position_percent.to_string(),
        load: |settings, value| {
            if let Ok(position_percent) = value.parse::<u8>() {
                if (DSCOPE_TRIGGER_POSITION_MIN_PERCENT..=DSCOPE_TRIGGER_POSITION_MAX_PERCENT)
                    .contains(&position_percent)
                {
                    settings.trigger.position_percent = position_percent;
                }
            }
        },
    },
    MetadataField {
        key: "scope.trigger.source",
        save: |settings| {
            match settings.trigger.source {
                DsCopeTriggerSource::Auto => "auto",
                DsCopeTriggerSource::ChannelZero => "ch0",
                DsCopeTriggerSource::ChannelOne => "ch1",
                DsCopeTriggerSource::ChannelZeroAndOne => "ch0_and_ch1",
                DsCopeTriggerSource::ChannelZeroOrOne => "ch0_or_ch1",
            }
            .to_owned()
        },
        load: |settings, value| {
            settings.trigger.source = match value {
                "ch0" => DsCopeTriggerSource::ChannelZero,
                "ch1" => DsCopeTriggerSource::ChannelOne,
                "ch0_and_ch1" => DsCopeTriggerSource::ChannelZeroAndOne,
                "ch0_or_ch1" => DsCopeTriggerSource::ChannelZeroOrOne,
                _ => DsCopeTriggerSource::Auto,
            };
        },
    },
    MetadataField {
        key: "scope.trigger.auto_channel",
        save: |settings| settings.trigger.auto_channel.to_string(),
        load: |settings, value| {
            if let Ok(auto_channel) = value.parse::<u8>() {
                if auto_channel <= 1 {
                    settings.trigger.auto_channel = auto_channel;
                }
            }
        },
    },
    MetadataField {
        key: "scope.trigger.slope",
        save: |settings| {
            match settings.trigger.slope {
                DsCopeTriggerSlope::Rising => "rising",
                DsCopeTriggerSlope::Falling => "falling",
            }
            .to_owned()
        },
        load: |settings, value| {
            settings.trigger.slope = if value == "falling" {
                DsCopeTriggerSlope::Falling
            } else {
                DsCopeTriggerSlope::Rising
            };
        },
    },
    MetadataField {
        key: "scope.trigger.holdoff_ns",
        save: |settings| settings.trigger.holdoff_ns.to_string(),
        load: |settings, value| {
            if let Ok(holdoff_ns) = value.parse::<u64>() {
                settings.trigger.holdoff_ns = holdoff_ns.min(DSCOPE_TRIGGER_HOLDOFF_MAX_NS);
                settings.trigger.holdoff_unit = if settings.trigger.holdoff_ns >= 1_000_000_000 {
                    TriggerHoldoffUnit::Seconds
                } else if settings.trigger.holdoff_ns >= 1_000_000 {
                    TriggerHoldoffUnit::Milliseconds
                } else {
                    TriggerHoldoffUnit::Microseconds
                };
            }
        },
    },
    MetadataField {
        key: "scope.trigger.noise_sensitivity",
        save: |settings| settings.trigger.noise_sensitivity.to_string(),
        load: |settings, value| {
            if let Ok(noise_sensitivity) = value.parse::<u8>() {
                settings.trigger.noise_sensitivity =
                    noise_sensitivity.min(DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX);
            }
        },
    },
];

const SCOPE_CHANNEL_FIELDS: &[ChannelField<ScopeCaptureSettings>] = &[
    ChannelField {
        key: |channel| format!("scope.ch{channel}.enabled"),
        save: |settings, channel| settings.channels[channel].enabled.to_string(),
        load: |settings, channel, value| {
            parse_bool_into(value, &mut settings.channels[channel].enabled);
        },
    },
    ChannelField {
        key: |channel| format!("scope.ch{channel}.volts_per_div_mv"),
        save: |settings, channel| settings.channels[channel].volts_per_div_mv.to_string(),
        load: |settings, channel, value| {
            if let Ok(volts_per_div_mv) = value.parse::<u16>() {
                if DSCOPE_VOLTS_PER_DIV_MV.contains(&volts_per_div_mv) {
                    settings.channels[channel].volts_per_div_mv = volts_per_div_mv;
                }
            }
        },
    },
    ChannelField {
        key: |channel| format!("scope.ch{channel}.coupling"),
        save: |settings, channel| {
            coupling_label(settings.channels[channel].coupling).to_ascii_lowercase()
        },
        load: |settings, channel, value| {
            settings.channels[channel].coupling = if value.eq_ignore_ascii_case("ac") {
                DsCopeCoupling::Ac
            } else {
                DsCopeCoupling::Dc
            };
        },
    },
    ChannelField {
        key: |channel| format!("scope.ch{channel}.probe_attenuation"),
        save: |settings, channel| settings.channels[channel].probe_attenuation.to_string(),
        load: |settings, channel, value| {
            if let Ok(probe_attenuation) = value.parse::<u8>() {
                if matches!(probe_attenuation, 1 | 10 | 100) {
                    settings.channels[channel].probe_attenuation = probe_attenuation;
                }
            }
        },
    },
    ChannelField {
        key: |channel| format!("scope.ch{channel}.vertical_offset"),
        save: |settings, channel| settings.channels[channel].vertical_offset.to_string(),
        load: |settings, channel, value| {
            if let Ok(vertical_offset) = value.parse::<u16>() {
                if (DSCOPE_VERTICAL_OFFSET_MIN..=DSCOPE_VERTICAL_OFFSET_MAX)
                    .contains(&vertical_offset)
                {
                    settings.channels[channel].vertical_offset = vertical_offset;
                }
            }
        },
    },
    ChannelField {
        key: |channel| format!("scope.trigger.ch{channel}.level"),
        save: |settings, channel| settings.trigger.levels[channel].to_string(),
        load: |settings, channel, value| {
            if let Ok(level) = value.parse::<u8>() {
                settings.trigger.levels[channel] =
                    level.clamp(DSCOPE_TRIGGER_LEVEL_MIN, DSCOPE_TRIGGER_LEVEL_MAX);
            }
        },
    },
];

/// Written for readers that predate `radio.analyzer.demodulation`; on load it
/// only matters when the demodulation key is absent (see
/// [`radio_settings_from_recording_metadata`]).
const RADIO_LEGACY_ANALYZER_ENABLED_KEY: &str = "radio.analyzer.enabled";
const RADIO_DEMODULATION_KEY: &str = "radio.analyzer.demodulation";
/// Single-channel analyzer offset, written for old readers and used on load only
/// when the recording has no `radio.analyzer.channel_count`.
const RADIO_LEGACY_CHANNEL_OFFSET_KEY: &str = "radio.analyzer.channel_offset_hz";

/// Radio settings that are one key each. The analyzer channel list, the gains,
/// and the legacy keys above are handled in code, not here.
const RADIO_FIELDS: &[MetadataField<RadioSettings>] = &[
    parsed_field!("radio.center_frequency_hz", capture.center_frequency_hz),
    parsed_field!("radio.sample_rate_hz", capture.sample_rate_hz),
    parsed_field!("radio.bandwidth_hz", capture.bandwidth_hz),
    parsed_field!("radio.receive_channel", capture.receive_channel),
    parsed_field!("radio.single_duration_millis", single_duration_millis),
    parsed_field!("radio.trigger_enabled", trigger_enabled),
    MetadataField {
        key: "radio.trigger_posttrigger_micros",
        save: |settings| settings.trigger_posttrigger_micros.to_string(),
        load: |settings, value| {
            if let Ok(micros) = value.parse::<u64>() {
                settings.trigger_posttrigger_micros =
                    micros.min(RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS);
            }
        },
    },
    parsed_field!("radio.trigger_level_dbfs_tenths", trigger_level_dbfs_tenths),
    parsed_field!(
        "radio.frequency_correction_milli_ppm",
        frequency_correction_milli_ppm
    ),
    parsed_field!("radio.iq_correction_enabled", iq_correction_enabled),
    parsed_field!("radio.amplifier_enabled", amplifier_enabled),
    parsed_field!("radio.antenna_power_enabled", antenna_power_enabled),
    MetadataField {
        key: RADIO_DEMODULATION_KEY,
        save: |settings| {
            match settings.analyzer.demodulation {
                RadioDemodulation::None => "none",
                RadioDemodulation::BinaryFsk => "fsk2",
                RadioDemodulation::BinaryGfsk => "gfsk2",
            }
            .to_owned()
        },
        load: |settings, value| {
            settings.analyzer.demodulation = match value {
                "none" => RadioDemodulation::None,
                "gfsk2" => RadioDemodulation::BinaryGfsk,
                _ => RadioDemodulation::BinaryFsk,
            };
        },
    },
    parsed_field!(
        "radio.analyzer.channel_bandwidth_hz",
        analyzer.channel_bandwidth_hz
    ),
    parsed_field!(
        "radio.analyzer.output_sample_rate_hz",
        analyzer.output_sample_rate_hz
    ),
    parsed_field!("radio.analyzer.filter_tap_count", analyzer.filter_tap_count),
    parsed_field!("radio.analyzer.symbol_rate_hz", analyzer.symbol_rate_hz),
    parsed_field!("radio.analyzer.deviation_hz", analyzer.deviation_hz),
    parsed_field!("radio.analyzer.gfsk_bt_milli", analyzer.gfsk_bt_milli),
    parsed_field!("radio.analyzer.inverted", analyzer.inverted),
    parsed_field!("radio.analyzer.adaptive_timing", analyzer.adaptive_timing),
    parsed_field!(
        "radio.analyzer.timing_loop_bandwidth_milli",
        analyzer.timing_loop_bandwidth_milli
    ),
    parsed_field!(
        "radio.analyzer.max_clock_error_ppm",
        analyzer.max_clock_error_ppm
    ),
    MetadataField {
        key: "radio.analyzer.data_mode",
        save: |settings| {
            match settings.analyzer.data_mode {
                RadioDataMode::Raw => "raw",
                RadioDataMode::Packets => "packets",
            }
            .to_owned()
        },
        load: |settings, value| {
            settings.analyzer.data_mode = match value {
                "packets" => RadioDataMode::Packets,
                _ => RadioDataMode::Raw,
            };
        },
    },
    parsed_field!("radio.analyzer.preamble_byte", analyzer.preamble_byte),
    parsed_field!("radio.analyzer.preamble_bytes", analyzer.preamble_bytes),
    parsed_field!("radio.analyzer.sync_word", analyzer.sync_word),
    parsed_field!("radio.analyzer.sync_word_bytes", analyzer.sync_word_bytes),
    parsed_field!(
        "radio.analyzer.maximum_preamble_bit_errors",
        analyzer.maximum_preamble_bit_errors
    ),
    parsed_field!(
        "radio.analyzer.minimum_bit_confidence_milli",
        analyzer.minimum_bit_confidence_milli
    ),
    parsed_field!(
        "radio.analyzer.maximum_uncertain_bits",
        analyzer.maximum_uncertain_bits
    ),
    parsed_field!(
        "radio.analyzer.whitening_enabled",
        analyzer.whitening_enabled
    ),
    MetadataField {
        key: "radio.analyzer.whitening_preset",
        save: |settings| {
            settings
                .analyzer
                .whitening_preset
                .metadata_value()
                .to_owned()
        },
        load: |settings, value| {
            settings.analyzer.whitening_preset = match value {
                "pn9_x9_x5" => RadioWhiteningPreset::Pn9X9X5,
                _ => RadioWhiteningPreset::Custom,
            };
        },
    },
    parsed_field!("radio.analyzer.whitening_width", analyzer.whitening_width),
    parsed_field!(
        "radio.analyzer.whitening_polynomial",
        analyzer.whitening_polynomial
    ),
    parsed_field!("radio.analyzer.whitening_seed", analyzer.whitening_seed),
    MetadataField {
        key: "radio.analyzer.whitening_direction",
        save: |settings| {
            match settings.analyzer.whitening_direction {
                WhiteningDirection::ShiftRight => "right",
                WhiteningDirection::ShiftLeft => "left",
            }
            .to_owned()
        },
        load: |settings, value| {
            settings.analyzer.whitening_direction = match value {
                "left" => WhiteningDirection::ShiftLeft,
                _ => WhiteningDirection::ShiftRight,
            };
        },
    },
    parsed_field!(
        "radio.analyzer.whitening_invert_output",
        analyzer.whitening_invert_output
    ),
    parsed_field!(
        "radio.analyzer.whitening_start_bit",
        analyzer.whitening_start_bit
    ),
    MetadataField {
        key: "radio.analyzer.whitening_end_bit",
        save: |settings| {
            settings
                .analyzer
                .whitening_end_bit
                .map_or_else(|| "none".to_owned(), |value| value.to_string())
        },
        load: |settings, value| {
            settings.analyzer.whitening_end_bit = (value != "none")
                .then(|| value.parse::<usize>().ok())
                .flatten();
        },
    },
    parsed_field!(
        "radio.analyzer.whitening_advance_excluded_bits",
        analyzer.whitening_advance_excluded_bits
    ),
];

pub(crate) fn saved_logic_settings_metadata(
    settings: LogicCaptureSettings,
    acquisition_mode: AcquisitionMode,
    repetitive_interval_seconds: f32,
    smart_timing_enabled: bool,
    channel_names: &[String; LOGIC_CHANNEL_COUNT],
) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    write_fields(&mut metadata, &settings, LOGIC_FIELDS);
    write_channel_fields(
        &mut metadata,
        &settings,
        LOGIC_CHANNEL_COUNT,
        LOGIC_CHANNEL_FIELDS,
    );
    write_logic_trigger(&mut metadata, &settings);
    // Not part of `LogicCaptureSettings`; restored by the caller.
    metadata.insert(
        "logic.smart_timing_enabled".to_owned(),
        smart_timing_enabled.to_string(),
    );
    metadata.insert(
        "logic.acquisition_mode".to_owned(),
        match acquisition_mode {
            AcquisitionMode::Single => "single",
            AcquisitionMode::Repetitive => "repetitive",
            AcquisitionMode::Loop => "loop",
            AcquisitionMode::Run => "run",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.repetitive_interval_seconds".to_owned(),
        repetitive_interval_seconds.to_string(),
    );
    for (channel, channel_name) in channel_names.iter().enumerate() {
        metadata.insert(
            format!("logic.ch{channel}.name"),
            normalized_channel_name(channel_name),
        );
    }
    metadata
}

/// Hand-written: the trigger keys depend on each other (mode, stage count,
/// per-stage conditions), so they are not table-driven. Keep this in step with
/// [`read_logic_trigger`].
fn write_logic_trigger(metadata: &mut BTreeMap<String, String>, settings: &LogicCaptureSettings) {
    metadata.insert(
        "logic.trigger.enabled".to_owned(),
        settings.trigger_enabled.to_string(),
    );
    metadata.insert(
        "logic.trigger.mode".to_owned(),
        match settings.trigger_mode {
            LogicTriggerMode::Simple => "simple",
            LogicTriggerMode::Smart => "smart",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.trigger.stage_count".to_owned(),
        settings.smart_trigger_stage_count.to_string(),
    );
    for channel in 0..LOGIC_CHANNEL_COUNT {
        if let Some(condition) = settings.trigger_conditions[channel] {
            metadata.insert(
                format!("logic.trigger.ch{channel}.condition"),
                logic_trigger_condition_key(condition).to_owned(),
            );
        }
        for stage in 0..DSLOGIC_TRIGGER_STAGE_COUNT {
            if let Some(condition) = settings.smart_trigger_conditions[stage][channel] {
                metadata.insert(
                    format!("logic.trigger.stage{stage}.ch{channel}.condition"),
                    logic_trigger_condition_key(condition).to_owned(),
                );
            }
        }
    }
}

pub(crate) fn saved_scope_settings_metadata(
    settings: ScopeCaptureSettings,
    auto_sample_rate: bool,
    smart_timing_enabled: bool,
    channel_names: &[String; 2],
    channel_colors: [Color32; 2],
) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    write_fields(&mut metadata, &settings, SCOPE_FIELDS);
    write_channel_fields(&mut metadata, &settings, 2, SCOPE_CHANNEL_FIELDS);
    // Not part of `ScopeCaptureSettings`; restored by the caller.
    metadata.insert(
        "scope.auto_sample_rate".to_owned(),
        auto_sample_rate.to_string(),
    );
    metadata.insert(
        "scope.smart_timing_enabled".to_owned(),
        smart_timing_enabled.to_string(),
    );
    for index in 0..2 {
        metadata.insert(
            format!("scope.ch{index}.name"),
            normalized_channel_name(&channel_names[index]),
        );
        metadata.insert(
            format!("scope.ch{index}.color"),
            color_string(channel_colors[index]),
        );
    }
    metadata
}

/// The analyzer channel list: how many there are, and the offset, name and, if
/// it has one of its own, the sync word of each.
pub(crate) fn insert_radio_analyzer_channels_metadata(
    metadata: &mut BTreeMap<String, String>,
    channels: &[RadioAnalyzerChannelSettings],
) {
    metadata.insert(
        "radio.analyzer.channel_count".to_owned(),
        channels.len().to_string(),
    );
    for (index, channel) in channels.iter().enumerate() {
        metadata.insert(
            format!("radio.analyzer.channel.{index}.offset_hz"),
            channel.frequency_offset_hz.to_string(),
        );
        metadata.insert(
            format!("radio.analyzer.channel.{index}.name"),
            channel.name.clone(),
        );
        let sync_word = channel.sync_word.trim();
        if !sync_word.is_empty() {
            metadata.insert(
                format!("radio.analyzer.channel.{index}.sync_word"),
                sync_word.to_owned(),
            );
        }
    }
}

pub(crate) fn saved_radio_settings_metadata(settings: &RadioSettings) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    write_fields(&mut metadata, settings, RADIO_FIELDS);
    // Derived keys kept for readers that predate the demodulation setting and
    // the analyzer channel list.
    metadata.insert(
        RADIO_LEGACY_ANALYZER_ENABLED_KEY.to_owned(),
        settings.analyzer.is_enabled().to_string(),
    );
    metadata.insert(
        RADIO_LEGACY_CHANNEL_OFFSET_KEY.to_owned(),
        settings
            .analyzer
            .channels
            .first()
            .map_or(0, |channel| channel.frequency_offset_hz)
            .to_string(),
    );
    insert_radio_analyzer_channels_metadata(&mut metadata, &settings.analyzer.channels);
    for (id, gain_db) in &settings.gains_db {
        metadata.insert(format!("radio.gain.{id}_db"), gain_db.to_string());
    }
    metadata
}

fn color_string(color: Color32) -> String {
    let [red, green, blue, alpha] = color.to_array();
    format!("{red:02x}{green:02x}{blue:02x}{alpha:02x}")
}

pub(crate) fn parse_color(value: &str) -> Option<Color32> {
    if value.len() != 8 {
        return None;
    }
    let red = u8::from_str_radix(&value[0..2], 16).ok()?;
    let green = u8::from_str_radix(&value[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&value[4..6], 16).ok()?;
    let alpha = u8::from_str_radix(&value[6..8], 16).ok()?;
    Some(Color32::from_rgba_unmultiplied(red, green, blue, alpha))
}

pub(crate) fn recording_metadata_value<T: std::str::FromStr>(
    metadata: &BTreeMap<String, String>,
    key: &str,
) -> Option<T> {
    metadata.get(key)?.parse().ok()
}

pub(crate) fn channel_names_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> ([String; LOGIC_CHANNEL_COUNT], [String; 2]) {
    let logic_names = std::array::from_fn(|index| {
        metadata
            .get(&format!("logic.ch{index}.name"))
            .map(|name| normalized_channel_name(name))
            .unwrap_or_default()
    });
    let scope_names = std::array::from_fn(|index| {
        metadata
            .get(&format!("scope.ch{index}.name"))
            .map(|name| normalized_channel_name(name))
            .unwrap_or_default()
    });
    (logic_names, scope_names)
}

pub(crate) fn logic_settings_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> LogicCaptureSettings {
    let mut settings = LogicCaptureSettings::default();
    read_fields(&mut settings, metadata, LOGIC_FIELDS);
    // Older recordings stored one height for every channel; per-channel keys
    // below take precedence.
    if let Some(maximum_height) = recording_metadata_value::<u8>(metadata, "logic.maximum_height") {
        if (1..=5).contains(&maximum_height) {
            settings.maximum_heights = [maximum_height; LOGIC_CHANNEL_COUNT];
        }
    }
    read_channel_fields(
        &mut settings,
        metadata,
        LOGIC_CHANNEL_COUNT,
        LOGIC_CHANNEL_FIELDS,
    );
    read_logic_trigger(&mut settings, metadata);
    settings
}

/// Hand-written counterpart of [`write_logic_trigger`]. It runs after the
/// channel and operation-mode fields because trigger validity depends on them.
fn read_logic_trigger(settings: &mut LogicCaptureSettings, metadata: &BTreeMap<String, String>) {
    if recording_metadata_value::<bool>(metadata, "logic.trigger.enabled") == Some(true) {
        if metadata.get("logic.trigger.mode").map(String::as_str) == Some("smart") {
            let stage_count = recording_metadata_value::<u8>(metadata, "logic.trigger.stage_count")
                .map(|count| usize::from(count).clamp(2, DSLOGIC_TRIGGER_STAGE_COUNT));
            if let Some(stage_count) = stage_count {
                let mut stages = [[None; LOGIC_CHANNEL_COUNT]; DSLOGIC_TRIGGER_STAGE_COUNT];
                for (stage_index, stage) in stages.iter_mut().enumerate().take(stage_count) {
                    for (channel, destination) in stage.iter_mut().enumerate() {
                        if let Some(condition) = metadata
                            .get(&format!(
                                "logic.trigger.stage{stage_index}.ch{channel}.condition"
                            ))
                            .and_then(|value| parse_logic_trigger_condition(value))
                        {
                            *destination = Some(condition);
                        }
                    }
                }
                let stages_are_valid = stages[..stage_count].iter().all(|stage| {
                    stage.iter().enumerate().any(|(channel, condition)| {
                        condition.is_some() && settings.enabled_channels[channel]
                    }) && stage.iter().enumerate().all(|(channel, condition)| {
                        condition.is_none() || settings.enabled_channels[channel]
                    })
                });
                if stages_are_valid && settings.operation_mode == DsLogicOperationMode::Buffer {
                    settings.trigger_mode = LogicTriggerMode::Smart;
                    settings.smart_trigger_stage_count = stage_count as u8;
                    settings.smart_trigger_conditions = stages;
                    settings.trigger_enabled = true;
                }
            }
        } else {
            let mut trigger_conditions = [None; LOGIC_CHANNEL_COUNT];
            if let (Some(channel), Some(condition)) = (
                recording_metadata_value::<u8>(metadata, "logic.trigger.channel"),
                metadata
                    .get("logic.trigger.condition")
                    .and_then(|value| parse_logic_trigger_condition(value)),
            ) {
                if let Some(destination) = trigger_conditions.get_mut(usize::from(channel)) {
                    *destination = Some(condition);
                }
            }
            for (channel, destination) in trigger_conditions.iter_mut().enumerate() {
                if let Some(condition) = metadata
                    .get(&format!("logic.trigger.ch{channel}.condition"))
                    .and_then(|value| parse_logic_trigger_condition(value))
                {
                    *destination = Some(condition);
                }
            }
            let conditions_are_valid =
                trigger_conditions
                    .iter()
                    .enumerate()
                    .any(|(channel, condition)| {
                        condition.is_some() && settings.enabled_channels[channel]
                    })
                    && trigger_conditions
                        .iter()
                        .enumerate()
                        .all(|(channel, condition)| {
                            condition.is_none() || settings.enabled_channels[channel]
                        });
            if conditions_are_valid && settings.operation_mode != DsLogicOperationMode::InternalTest
            {
                settings.trigger_mode = LogicTriggerMode::Simple;
                settings.trigger_conditions = trigger_conditions;
                settings.trigger_enabled = true;
            }
        }
    }
    if metadata.get("logic.trigger.mode").map(String::as_str) == Some("smart") {
        settings.trigger_conditions = std::array::from_fn(|channel| {
            metadata
                .get(&format!("logic.trigger.ch{channel}.condition"))
                .and_then(|value| parse_logic_trigger_condition(value))
        });
    } else {
        if let Some(stage_count) =
            recording_metadata_value::<u8>(metadata, "logic.trigger.stage_count")
        {
            settings.smart_trigger_stage_count =
                stage_count.clamp(2, DSLOGIC_TRIGGER_STAGE_COUNT as u8);
        }
        settings.smart_trigger_conditions = std::array::from_fn(|stage| {
            std::array::from_fn(|channel| {
                metadata
                    .get(&format!("logic.trigger.stage{stage}.ch{channel}.condition"))
                    .and_then(|value| parse_logic_trigger_condition(value))
            })
        });
    }
}

pub(crate) fn scope_settings_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> ScopeCaptureSettings {
    let mut settings = ScopeCaptureSettings::default();
    read_fields(&mut settings, metadata, SCOPE_FIELDS);
    read_channel_fields(&mut settings, metadata, 2, SCOPE_CHANNEL_FIELDS);
    settings
        .trigger
        .clamp_to_channels(settings.channels.map(|channel| channel.enabled));
    settings
}

pub(crate) fn radio_settings_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> RadioSettings {
    let mut settings = RadioSettings::default();
    read_fields(&mut settings, metadata, RADIO_FIELDS);
    // Older recordings only had an on/off flag: `false` always disables the
    // analyzer, and `true` without a demodulation key meant GFSK.
    let legacy_analyzer_enabled =
        recording_metadata_value::<bool>(metadata, RADIO_LEGACY_ANALYZER_ENABLED_KEY);
    if legacy_analyzer_enabled == Some(false) {
        settings.analyzer.demodulation = RadioDemodulation::None;
    } else if !metadata.contains_key(RADIO_DEMODULATION_KEY) {
        settings.analyzer.demodulation = if legacy_analyzer_enabled == Some(true) {
            RadioDemodulation::BinaryGfsk
        } else {
            RadioDemodulation::None
        };
    }
    if let Some(channel_count) =
        recording_metadata_value::<usize>(metadata, "radio.analyzer.channel_count")
    {
        let channels = (0..channel_count.min(MAX_RADIO_ANALYZER_CHANNELS))
            .filter_map(|index| {
                let frequency_offset_hz = recording_metadata_value(
                    metadata,
                    &format!("radio.analyzer.channel.{index}.offset_hz"),
                )?;
                let name = metadata
                    .get(&format!("radio.analyzer.channel.{index}.name"))
                    .cloned()
                    .unwrap_or_else(|| format!("Channel {}", index + 1));
                let sync_word = metadata
                    .get(&format!("radio.analyzer.channel.{index}.sync_word"))
                    .cloned()
                    .unwrap_or_default();
                Some(RadioAnalyzerChannelSettings {
                    name,
                    frequency_offset_hz,
                    sync_word,
                })
            })
            .collect::<Vec<_>>();
        if !channels.is_empty() {
            settings.analyzer.channels = channels;
        }
    } else if let Some(value) = recording_metadata_value(metadata, RADIO_LEGACY_CHANNEL_OFFSET_KEY)
    {
        settings.analyzer.channels = vec![RadioAnalyzerChannelSettings::numbered(0, value)];
    }
    for (key, value) in metadata {
        let Some(id) = key
            .strip_prefix("radio.gain.")
            .and_then(|key| key.strip_suffix("_db"))
        else {
            continue;
        };
        if let Ok(value) = value.parse::<i32>() {
            settings.gains_db.insert(id.to_owned(), value);
        }
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use std::collections::HashSet;

    fn channel_keys<S>(fields: &[ChannelField<S>], channel_count: usize) -> Vec<String> {
        fields
            .iter()
            .flat_map(|field| (0..channel_count).map(|channel| (field.key)(channel)))
            .collect()
    }

    fn field_keys<S>(fields: &[MetadataField<S>]) -> Vec<String> {
        fields.iter().map(|field| field.key.to_owned()).collect()
    }

    fn assert_unique(label: &str, keys: &[String]) {
        let mut seen = HashSet::new();
        for key in keys {
            assert!(seen.insert(key), "{label} table lists {key} more than once");
        }
    }

    /// A key listed twice would make one entry silently overwrite the other on
    /// save, and both would read the same stored value on load.
    #[test]
    fn table_keys_are_unique_within_each_instrument() {
        let mut logic = field_keys(LOGIC_FIELDS);
        logic.extend(channel_keys(LOGIC_CHANNEL_FIELDS, LOGIC_CHANNEL_COUNT));
        assert_unique("logic", &logic);

        let mut scope = field_keys(SCOPE_FIELDS);
        scope.extend(channel_keys(SCOPE_CHANNEL_FIELDS, 2));
        assert_unique("scope", &scope);

        let mut radio = field_keys(RADIO_FIELDS);
        radio.push(RADIO_LEGACY_ANALYZER_ENABLED_KEY.to_owned());
        radio.push(RADIO_LEGACY_CHANNEL_OFFSET_KEY.to_owned());
        assert_unique("radio", &radio);
    }

    #[test]
    fn table_keys_stay_inside_their_instrument_namespace() {
        for key in field_keys(LOGIC_FIELDS)
            .into_iter()
            .chain(channel_keys(LOGIC_CHANNEL_FIELDS, LOGIC_CHANNEL_COUNT))
        {
            assert!(key.starts_with("logic."), "{key}");
        }
        for key in field_keys(SCOPE_FIELDS)
            .into_iter()
            .chain(channel_keys(SCOPE_CHANNEL_FIELDS, 2))
        {
            assert!(key.starts_with("scope."), "{key}");
        }
        for key in field_keys(RADIO_FIELDS) {
            assert!(key.starts_with("radio."), "{key}");
        }
    }

    #[test]
    fn saved_logic_settings_round_trip_independently() {
        let mut settings = LogicCaptureSettings {
            sample_rate_hz: 100_000_000,
            sample_count: 2_000_000,
            enabled_channels: std::array::from_fn(|channel| channel < 8),
            maximum_heights: std::array::from_fn(|channel| (channel % 5 + 1) as u8),
            operation_mode: DsLogicOperationMode::Buffer,
            stop_option: DsLogicStopOption::StopImmediately,
            threshold_voltage: 2.5,
            input_filter: DsLogicInputFilter::OneSampleClock,
            rle_enabled: true,
            external_clock: true,
            falling_clock_edge: true,
            trigger_enabled: true,
            trigger_mode: LogicTriggerMode::Smart,
            trigger_conditions: [None; LOGIC_CHANNEL_COUNT],
            smart_trigger_stage_count: 3,
            smart_trigger_conditions: [[None; LOGIC_CHANNEL_COUNT]; DSLOGIC_TRIGGER_STAGE_COUNT],
        };
        settings.smart_trigger_conditions[0][0] = Some(DsLogicTriggerCondition::RisingEdge);
        settings.smart_trigger_conditions[1][1] = Some(DsLogicTriggerCondition::High);
        settings.smart_trigger_conditions[2][2] = Some(DsLogicTriggerCondition::FallingEdge);
        let channel_names = std::array::from_fn(|channel| format!("L{channel}"));

        let metadata = saved_logic_settings_metadata(
            settings,
            AcquisitionMode::Repetitive,
            2.5,
            false,
            &channel_names,
        );

        assert_eq!(logic_settings_from_recording_metadata(&metadata), settings);
        assert_eq!(metadata["logic.acquisition_mode"], "repetitive".to_owned());
        assert_eq!(metadata["logic.repetitive_interval_seconds"], "2.5");
        assert_eq!(metadata["logic.smart_timing_enabled"], "false");
        assert_eq!(
            channel_names_from_recording_metadata(&metadata).0,
            channel_names
        );
    }

    #[test]
    fn saved_scope_settings_round_trip_independently() {
        let settings = ScopeCaptureSettings {
            sample_rate_hz: 500_000_000,
            sample_count: 1_000_000,
            channels: [
                ScopeChannelSettings {
                    enabled: true,
                    volts_per_div_mv: 500,
                    coupling: DsCopeCoupling::Ac,
                    probe_attenuation: 10,
                    vertical_offset: 120,
                },
                ScopeChannelSettings {
                    enabled: true,
                    volts_per_div_mv: 2_000,
                    coupling: DsCopeCoupling::Dc,
                    probe_attenuation: 100,
                    vertical_offset: 140,
                },
            ],
            trigger: ScopeTriggerSettings {
                position_percent: 70,
                source: DsCopeTriggerSource::ChannelZeroOrOne,
                auto_channel: 1,
                slope: DsCopeTriggerSlope::Falling,
                holdoff_ns: 2_000_000,
                holdoff_unit: TriggerHoldoffUnit::Milliseconds,
                noise_sensitivity: 4,
                levels: [100, 140],
            },
            operation_mode: DsCopeOperationMode::InternalTest,
            bandwidth_limit: DsCopeBandwidthLimit::Mhz20,
        };
        let names = ["Input".to_owned(), "Reference".to_owned()];
        let colors = [Color32::RED, Color32::BLUE];

        let metadata = saved_scope_settings_metadata(settings, false, true, &names, colors);

        assert_eq!(scope_settings_from_recording_metadata(&metadata), settings);
        assert_eq!(metadata["scope.auto_sample_rate"], "false");
        assert_eq!(metadata["scope.smart_timing_enabled"], "true");
        assert_eq!(channel_names_from_recording_metadata(&metadata).1, names);
        assert_eq!(parse_color(&metadata["scope.ch0.color"]), Some(colors[0]));
        assert_eq!(parse_color(&metadata["scope.ch1.color"]), Some(colors[1]));
    }

    /// Every field is listed and set away from its default, so a field added to
    /// `RadioSettings` or `RadioAnalyzerSettings` fails to compile here until it is
    /// given a value, and a field that is not saved or restored fails the
    /// comparison instead of silently reopening with its default.
    #[test]
    fn saved_rf_settings_round_trip_every_field() {
        let settings = RadioSettings {
            capture: RadioCaptureSettings {
                center_frequency_hz: 915_000_000,
                sample_rate_hz: 10_000_000,
                bandwidth_hz: 8_000_000,
                receive_channel: 1,
                // Derived from `single_duration_millis` when a Single capture
                // starts, so it is deliberately not saved.
                sample_limit: None,
            },
            single_duration_millis: 2_500,
            trigger_enabled: true,
            trigger_posttrigger_micros: 25_000,
            trigger_level_dbfs_tenths: -350,
            gains_db: BTreeMap::from([("lna".to_owned(), 24), ("vga".to_owned(), 20)]),
            frequency_correction_milli_ppm: 1_250,
            iq_correction_enabled: false,
            amplifier_enabled: true,
            antenna_power_enabled: true,
            analyzer: RadioAnalyzerSettings {
                demodulation: RadioDemodulation::BinaryGfsk,
                channels: vec![
                    RadioAnalyzerChannelSettings {
                        name: "Weather".to_owned(),
                        frequency_offset_hz: 125_000,
                        sync_word: "91 d3".to_owned(),
                    },
                    RadioAnalyzerChannelSettings {
                        name: "Control".to_owned(),
                        frequency_offset_hz: -250_000,
                        sync_word: String::new(),
                    },
                ],
                channel_bandwidth_hz: 250_000,
                output_sample_rate_hz: 600_000,
                filter_tap_count: 129,
                symbol_rate_hz: 38_400,
                deviation_hz: 50_000,
                gfsk_bt_milli: 300,
                inverted: true,
                adaptive_timing: false,
                timing_loop_bandwidth_milli: 50,
                max_clock_error_ppm: 5_000,
                data_mode: RadioDataMode::Packets,
                preamble_byte: 0x55,
                preamble_bytes: 6,
                sync_word: 0x2dd4,
                sync_word_bytes: 2,
                maximum_preamble_bit_errors: 3,
                minimum_bit_confidence_milli: 250,
                maximum_uncertain_bits: 2,
                whitening_enabled: true,
                whitening_preset: RadioWhiteningPreset::Pn9X9X5,
                whitening_width: 9,
                whitening_polynomial: 0x11,
                whitening_seed: 0x1ff,
                whitening_direction: WhiteningDirection::ShiftLeft,
                whitening_invert_output: true,
                whitening_start_bit: 8,
                whitening_end_bit: Some(160),
                whitening_advance_excluded_bits: true,
            },
        };

        let metadata = saved_radio_settings_metadata(&settings);

        assert_eq!(radio_settings_from_recording_metadata(&metadata), settings);
    }

    #[test]
    fn saved_rf_settings_round_trip_independently() {
        let mut settings = RadioSettings::default();
        settings.capture.center_frequency_hz = 915_000_000;
        settings.capture.sample_rate_hz = 10_000_000;
        settings.capture.bandwidth_hz = 8_000_000;
        settings.single_duration_millis = 2_500;
        settings.trigger_enabled = true;
        settings.trigger_posttrigger_micros = 25_000;
        settings.trigger_level_dbfs_tenths = -350;
        settings.frequency_correction_milli_ppm = 1_250;
        settings.iq_correction_enabled = false;
        settings.amplifier_enabled = true;
        settings.antenna_power_enabled = true;
        settings.gains_db.insert("lna".to_owned(), 24);
        settings.gains_db.insert("vga".to_owned(), 20);
        settings.analyzer.demodulation = RadioDemodulation::BinaryFsk;
        settings.analyzer.channels = vec![
            RadioAnalyzerChannelSettings {
                name: "Weather".to_owned(),
                frequency_offset_hz: 125_000,
                sync_word: "2d d4".to_owned(),
            },
            RadioAnalyzerChannelSettings {
                name: "Control".to_owned(),
                frequency_offset_hz: -250_000,
                sync_word: String::new(),
            },
        ];
        settings.analyzer.sync_word = 0xd391;
        settings.analyzer.sync_word_bytes = 2;
        settings.analyzer.data_mode = RadioDataMode::Packets;
        settings.analyzer.whitening_enabled = true;
        settings.analyzer.whitening_width = 9;
        settings.analyzer.whitening_polynomial = 0x11;
        settings.analyzer.whitening_seed = 0x1ff;
        settings.analyzer.whitening_direction = WhiteningDirection::ShiftLeft;
        settings.analyzer.whitening_invert_output = true;
        settings.analyzer.whitening_start_bit = 8;
        settings.analyzer.whitening_end_bit = Some(160);
        settings.analyzer.whitening_advance_excluded_bits = true;

        let metadata = saved_radio_settings_metadata(&settings);

        assert_eq!(radio_settings_from_recording_metadata(&metadata), settings);
    }

    #[test]
    fn radio_analyzer_settings_round_trip_from_recording_metadata() {
        let metadata = BTreeMap::from([
            ("radio.analyzer.enabled".to_owned(), "true".to_owned()),
            (
                "radio.analyzer.whitening_preset".to_owned(),
                "pn9_x9_x5".to_owned(),
            ),
            ("radio.analyzer.demodulation".to_owned(), "gfsk2".to_owned()),
            (
                "radio.analyzer.channel_offset_hz".to_owned(),
                "-125000".to_owned(),
            ),
            (
                "radio.analyzer.channel_bandwidth_hz".to_owned(),
                "250000".to_owned(),
            ),
            (
                "radio.analyzer.output_sample_rate_hz".to_owned(),
                "1000000".to_owned(),
            ),
            (
                "radio.analyzer.filter_tap_count".to_owned(),
                "257".to_owned(),
            ),
            (
                "radio.analyzer.symbol_rate_hz".to_owned(),
                "100000".to_owned(),
            ),
            ("radio.analyzer.deviation_hz".to_owned(), "50000".to_owned()),
            ("radio.analyzer.gfsk_bt_milli".to_owned(), "300".to_owned()),
            ("radio.analyzer.inverted".to_owned(), "true".to_owned()),
            (
                "radio.analyzer.adaptive_timing".to_owned(),
                "false".to_owned(),
            ),
            (
                "radio.analyzer.timing_loop_bandwidth_milli".to_owned(),
                "50".to_owned(),
            ),
            (
                "radio.analyzer.max_clock_error_ppm".to_owned(),
                "20000".to_owned(),
            ),
            ("radio.analyzer.data_mode".to_owned(), "packets".to_owned()),
            ("radio.analyzer.preamble_byte".to_owned(), "85".to_owned()),
            ("radio.analyzer.preamble_bytes".to_owned(), "12".to_owned()),
            ("radio.analyzer.sync_word".to_owned(), "11732".to_owned()),
            ("radio.analyzer.sync_word_bytes".to_owned(), "2".to_owned()),
            (
                "radio.analyzer.maximum_preamble_bit_errors".to_owned(),
                "2".to_owned(),
            ),
            (
                "radio.analyzer.minimum_bit_confidence_milli".to_owned(),
                "650".to_owned(),
            ),
            (
                "radio.analyzer.maximum_uncertain_bits".to_owned(),
                "3".to_owned(),
            ),
            (
                "radio.analyzer.whitening_enabled".to_owned(),
                "true".to_owned(),
            ),
            ("radio.analyzer.whitening_width".to_owned(), "9".to_owned()),
            (
                "radio.analyzer.whitening_polynomial".to_owned(),
                "17".to_owned(),
            ),
            ("radio.analyzer.whitening_seed".to_owned(), "511".to_owned()),
            (
                "radio.analyzer.whitening_direction".to_owned(),
                "left".to_owned(),
            ),
            (
                "radio.analyzer.whitening_invert_output".to_owned(),
                "true".to_owned(),
            ),
            (
                "radio.analyzer.whitening_start_bit".to_owned(),
                "8".to_owned(),
            ),
            (
                "radio.analyzer.whitening_end_bit".to_owned(),
                "160".to_owned(),
            ),
            (
                "radio.analyzer.whitening_advance_excluded_bits".to_owned(),
                "true".to_owned(),
            ),
        ]);

        let settings = radio_settings_from_recording_metadata(&metadata).analyzer;

        assert!(settings.is_enabled());
        assert_eq!(settings.demodulation, RadioDemodulation::BinaryGfsk);
        assert_eq!(settings.channels[0].name, "Channel 1");
        assert_eq!(settings.channels[0].frequency_offset_hz, -125_000);
        assert_eq!(settings.channel_bandwidth_hz, 250_000);
        assert_eq!(settings.output_sample_rate_hz, 1_000_000);
        assert_eq!(settings.filter_tap_count, 257);
        assert_eq!(settings.symbol_rate_hz, 100_000);
        assert_eq!(settings.deviation_hz, 50_000);
        assert_eq!(settings.gfsk_bt_milli, 300);
        assert!(settings.inverted);
        assert!(!settings.adaptive_timing);
        assert_eq!(settings.timing_loop_bandwidth_milli, 50);
        assert_eq!(settings.max_clock_error_ppm, 20_000);
        assert_eq!(settings.preamble_byte, 0x55);
        assert_eq!(settings.preamble_bytes, 12);
        assert_eq!(settings.sync_word, 0x2dd4);
        assert_eq!(settings.sync_word_bytes, 2);
        assert_eq!(settings.maximum_preamble_bit_errors, 2);
        assert_eq!(settings.minimum_bit_confidence_milli, 650);
        assert_eq!(settings.maximum_uncertain_bits, 3);
        assert_eq!(settings.data_mode, RadioDataMode::Packets);
        assert!(settings.whitening_enabled);
        assert_eq!(settings.whitening_preset, RadioWhiteningPreset::Pn9X9X5);
        assert_eq!(settings.whitening_width, 9);
        assert_eq!(settings.whitening_polynomial, 0x11);
        assert_eq!(settings.whitening_seed, 0x1ff);
        assert_eq!(settings.whitening_direction, WhiteningDirection::ShiftLeft);
        assert!(settings.whitening_invert_output);
        assert_eq!(settings.whitening_start_bit, 8);
        assert_eq!(settings.whitening_end_bit, Some(160));
        assert!(settings.whitening_advance_excluded_bits);
    }

    #[test]
    fn legacy_disabled_radio_analyzer_loads_as_none() {
        let metadata = BTreeMap::from([
            ("radio.analyzer.enabled".to_owned(), "false".to_owned()),
            ("radio.analyzer.demodulation".to_owned(), "gfsk2".to_owned()),
        ]);

        let settings = radio_settings_from_recording_metadata(&metadata).analyzer;

        assert_eq!(settings.demodulation, RadioDemodulation::None);
        assert!(!settings.is_enabled());
        assert!(settings.signal_profiles().unwrap().is_empty());
    }

    #[test]
    fn recording_metadata_restores_smart_trigger_stages() {
        let mut metadata = BTreeMap::new();
        metadata.insert("logic.operation_mode".to_owned(), "buffer".to_owned());
        metadata.insert("logic.trigger.enabled".to_owned(), "true".to_owned());
        metadata.insert("logic.trigger.mode".to_owned(), "smart".to_owned());
        metadata.insert("logic.trigger.stage_count".to_owned(), "2".to_owned());
        metadata.insert(
            "logic.trigger.stage0.ch0.condition".to_owned(),
            "rising_edge".to_owned(),
        );
        metadata.insert(
            "logic.trigger.stage0.ch1.condition".to_owned(),
            "high".to_owned(),
        );
        metadata.insert(
            "logic.trigger.stage1.ch2.condition".to_owned(),
            "falling_edge".to_owned(),
        );

        let settings = logic_settings_from_recording_metadata(&metadata);

        assert!(settings.trigger_enabled);
        assert_eq!(settings.trigger_mode, LogicTriggerMode::Smart);
        assert_eq!(settings.smart_trigger_stage_count, 2);
        assert_eq!(
            settings.smart_trigger_conditions[0][0],
            Some(DsLogicTriggerCondition::RisingEdge)
        );
        assert_eq!(
            settings.smart_trigger_conditions[0][1],
            Some(DsLogicTriggerCondition::High)
        );
        assert_eq!(
            settings.smart_trigger_conditions[1][2],
            Some(DsLogicTriggerCondition::FallingEdge)
        );
    }

    #[test]
    fn recordings_without_channel_names_use_hardware_identifiers() {
        let (logic_names, scope_names) = channel_names_from_recording_metadata(&BTreeMap::new());

        assert!(logic_names.iter().all(String::is_empty));
        assert!(scope_names.iter().all(String::is_empty));
        assert_eq!(channel_label("D", 0, &logic_names[0]), "D0");
        assert_eq!(scope_channel_label(1, &scope_names), "CH1");
    }

    #[test]
    fn recording_metadata_saves_normalized_channel_names() {
        let mut logic_names = empty_logic_channel_names();
        logic_names[0] = "  Clock  ".to_owned();
        let mut scope_names = empty_scope_channel_names();
        scope_names[1] = "Reference".to_owned();
        let mut metadata = BTreeMap::new();

        insert_channel_name_metadata(&mut metadata, &logic_names, &scope_names);

        assert_eq!(
            metadata.get("logic.ch0.name").map(String::as_str),
            Some("Clock")
        );
        assert_eq!(metadata.get("logic.ch1.name").map(String::as_str), Some(""));
        assert_eq!(
            metadata.get("scope.ch1.name").map(String::as_str),
            Some("Reference")
        );
    }
}
