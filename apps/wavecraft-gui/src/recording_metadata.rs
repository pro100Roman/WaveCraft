//! Saving and restoring capture settings through `.dsv` recording metadata.
//!
//! The metadata keys written here are part of the recording format (see
//! `docs/recording-format.md`). Renaming a key or dropping a legacy fallback
//! makes older recordings reopen with default settings.

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

pub(crate) fn saved_logic_settings_metadata(
    settings: LogicCaptureSettings,
    acquisition_mode: AcquisitionMode,
    repetitive_interval_seconds: f32,
    smart_timing_enabled: bool,
    channel_names: &[String; LOGIC_CHANNEL_COUNT],
) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "logic.sample_rate_hz".to_owned(),
        settings.sample_rate_hz.to_string(),
    );
    metadata.insert(
        "logic.sample_count".to_owned(),
        settings.sample_count.to_string(),
    );
    metadata.insert(
        "logic.operation_mode".to_owned(),
        match settings.operation_mode {
            DsLogicOperationMode::Buffer => "buffer",
            DsLogicOperationMode::Stream => "stream",
            DsLogicOperationMode::InternalTest => "internal_test",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.stop_option".to_owned(),
        match settings.stop_option {
            DsLogicStopOption::StopImmediately => "stop_immediately",
            DsLogicStopOption::UploadCapturedData => "upload_captured_data",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.threshold_voltage".to_owned(),
        settings.threshold_voltage.to_string(),
    );
    metadata.insert(
        "logic.input_filter".to_owned(),
        match settings.input_filter {
            DsLogicInputFilter::None => "none",
            DsLogicInputFilter::OneSampleClock => "one_sample_clock",
        }
        .to_owned(),
    );
    for (key, value) in [
        ("logic.rle_enabled", settings.rle_enabled),
        ("logic.external_clock", settings.external_clock),
        ("logic.falling_clock_edge", settings.falling_clock_edge),
        ("logic.trigger.enabled", settings.trigger_enabled),
        ("logic.smart_timing_enabled", smart_timing_enabled),
    ] {
        metadata.insert(key.to_owned(), value.to_string());
    }
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
    for (channel, channel_name) in channel_names.iter().enumerate() {
        metadata.insert(
            format!("logic.ch{channel}.enabled"),
            settings.enabled_channels[channel].to_string(),
        );
        metadata.insert(
            format!("logic.ch{channel}.maximum_height"),
            settings.maximum_heights[channel].to_string(),
        );
        metadata.insert(
            format!("logic.ch{channel}.name"),
            normalized_channel_name(channel_name),
        );
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
    metadata
}

pub(crate) fn saved_scope_settings_metadata(
    settings: ScopeCaptureSettings,
    auto_sample_rate: bool,
    smart_timing_enabled: bool,
    channel_names: &[String; 2],
    channel_colors: [Color32; 2],
) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "scope.sample_rate_hz".to_owned(),
        settings.sample_rate_hz.to_string(),
    );
    metadata.insert(
        "scope.sample_count".to_owned(),
        settings.sample_count.to_string(),
    );
    metadata.insert(
        "scope.auto_sample_rate".to_owned(),
        auto_sample_rate.to_string(),
    );
    metadata.insert(
        "scope.smart_timing_enabled".to_owned(),
        smart_timing_enabled.to_string(),
    );
    metadata.insert(
        "scope.operation_mode".to_owned(),
        match settings.operation_mode {
            DsCopeOperationMode::Normal => "normal",
            DsCopeOperationMode::InternalTest => "internal_test",
        }
        .to_owned(),
    );
    metadata.insert(
        "scope.bandwidth_limit".to_owned(),
        match settings.bandwidth_limit {
            DsCopeBandwidthLimit::Full => "full",
            DsCopeBandwidthLimit::Mhz20 => "20_mhz",
        }
        .to_owned(),
    );
    let trigger = settings.trigger;
    metadata.insert(
        "scope.trigger.position_percent".to_owned(),
        trigger.position_percent.to_string(),
    );
    metadata.insert(
        "scope.trigger.source".to_owned(),
        match trigger.source {
            DsCopeTriggerSource::Auto => "auto",
            DsCopeTriggerSource::ChannelZero => "ch0",
            DsCopeTriggerSource::ChannelOne => "ch1",
            DsCopeTriggerSource::ChannelZeroAndOne => "ch0_and_ch1",
            DsCopeTriggerSource::ChannelZeroOrOne => "ch0_or_ch1",
        }
        .to_owned(),
    );
    metadata.insert(
        "scope.trigger.auto_channel".to_owned(),
        trigger.auto_channel.to_string(),
    );
    metadata.insert(
        "scope.trigger.slope".to_owned(),
        match trigger.slope {
            DsCopeTriggerSlope::Rising => "rising",
            DsCopeTriggerSlope::Falling => "falling",
        }
        .to_owned(),
    );
    metadata.insert(
        "scope.trigger.holdoff_ns".to_owned(),
        trigger.holdoff_ns.to_string(),
    );
    metadata.insert(
        "scope.trigger.noise_sensitivity".to_owned(),
        trigger.noise_sensitivity.to_string(),
    );
    for (index, channel) in settings.channels.iter().enumerate() {
        metadata.insert(
            format!("scope.ch{index}.enabled"),
            channel.enabled.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.volts_per_div_mv"),
            channel.volts_per_div_mv.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.coupling"),
            coupling_label(channel.coupling).to_ascii_lowercase(),
        );
        metadata.insert(
            format!("scope.ch{index}.probe_attenuation"),
            channel.probe_attenuation.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.vertical_offset"),
            channel.vertical_offset.to_string(),
        );
        metadata.insert(
            format!("scope.trigger.ch{index}.level"),
            trigger.levels[index].to_string(),
        );
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

pub(crate) fn saved_radio_settings_metadata(settings: &RadioSettings) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    for (key, value) in [
        (
            "radio.center_frequency_hz",
            settings.capture.center_frequency_hz.to_string(),
        ),
        (
            "radio.sample_rate_hz",
            settings.capture.sample_rate_hz.to_string(),
        ),
        (
            "radio.bandwidth_hz",
            settings.capture.bandwidth_hz.to_string(),
        ),
        (
            "radio.receive_channel",
            settings.capture.receive_channel.to_string(),
        ),
        (
            "radio.single_duration_millis",
            settings.single_duration_millis.to_string(),
        ),
        (
            "radio.trigger_enabled",
            settings.trigger_enabled.to_string(),
        ),
        (
            "radio.trigger_posttrigger_micros",
            settings.trigger_posttrigger_micros.to_string(),
        ),
        (
            "radio.trigger_level_dbfs_tenths",
            settings.trigger_level_dbfs_tenths.to_string(),
        ),
        (
            "radio.frequency_correction_milli_ppm",
            settings.frequency_correction_milli_ppm.to_string(),
        ),
        (
            "radio.iq_correction_enabled",
            settings.iq_correction_enabled.to_string(),
        ),
        (
            "radio.amplifier_enabled",
            settings.amplifier_enabled.to_string(),
        ),
        (
            "radio.antenna_power_enabled",
            settings.antenna_power_enabled.to_string(),
        ),
        (
            "radio.analyzer.enabled",
            settings.analyzer.is_enabled().to_string(),
        ),
        (
            "radio.analyzer.demodulation",
            match settings.analyzer.demodulation {
                RadioDemodulation::None => "none",
                RadioDemodulation::BinaryFsk => "fsk2",
                RadioDemodulation::BinaryGfsk => "gfsk2",
            }
            .to_owned(),
        ),
        (
            "radio.analyzer.channel_offset_hz",
            settings
                .analyzer
                .channels
                .first()
                .map_or(0, |channel| channel.frequency_offset_hz)
                .to_string(),
        ),
        (
            "radio.analyzer.channel_bandwidth_hz",
            settings.analyzer.channel_bandwidth_hz.to_string(),
        ),
        (
            "radio.analyzer.output_sample_rate_hz",
            settings.analyzer.output_sample_rate_hz.to_string(),
        ),
        (
            "radio.analyzer.filter_tap_count",
            settings.analyzer.filter_tap_count.to_string(),
        ),
        (
            "radio.analyzer.symbol_rate_hz",
            settings.analyzer.symbol_rate_hz.to_string(),
        ),
        (
            "radio.analyzer.deviation_hz",
            settings.analyzer.deviation_hz.to_string(),
        ),
        (
            "radio.analyzer.gfsk_bt_milli",
            settings.analyzer.gfsk_bt_milli.to_string(),
        ),
        (
            "radio.analyzer.inverted",
            settings.analyzer.inverted.to_string(),
        ),
        (
            "radio.analyzer.adaptive_timing",
            settings.analyzer.adaptive_timing.to_string(),
        ),
        (
            "radio.analyzer.timing_loop_bandwidth_milli",
            settings.analyzer.timing_loop_bandwidth_milli.to_string(),
        ),
        (
            "radio.analyzer.max_clock_error_ppm",
            settings.analyzer.max_clock_error_ppm.to_string(),
        ),
        (
            "radio.analyzer.data_mode",
            match settings.analyzer.data_mode {
                RadioDataMode::Raw => "raw",
                RadioDataMode::Packets => "packets",
            }
            .to_owned(),
        ),
        (
            "radio.analyzer.preamble_byte",
            settings.analyzer.preamble_byte.to_string(),
        ),
        (
            "radio.analyzer.preamble_bytes",
            settings.analyzer.preamble_bytes.to_string(),
        ),
        (
            "radio.analyzer.sync_word",
            settings.analyzer.sync_word.to_string(),
        ),
        (
            "radio.analyzer.sync_word_bytes",
            settings.analyzer.sync_word_bytes.to_string(),
        ),
        (
            "radio.analyzer.maximum_preamble_bit_errors",
            settings.analyzer.maximum_preamble_bit_errors.to_string(),
        ),
        (
            "radio.analyzer.minimum_bit_confidence_milli",
            settings.analyzer.minimum_bit_confidence_milli.to_string(),
        ),
        (
            "radio.analyzer.maximum_uncertain_bits",
            settings.analyzer.maximum_uncertain_bits.to_string(),
        ),
        (
            "radio.analyzer.whitening_enabled",
            settings.analyzer.whitening_enabled.to_string(),
        ),
        (
            "radio.analyzer.whitening_preset",
            settings
                .analyzer
                .whitening_preset
                .metadata_value()
                .to_owned(),
        ),
        (
            "radio.analyzer.whitening_width",
            settings.analyzer.whitening_width.to_string(),
        ),
        (
            "radio.analyzer.whitening_polynomial",
            settings.analyzer.whitening_polynomial.to_string(),
        ),
        (
            "radio.analyzer.whitening_seed",
            settings.analyzer.whitening_seed.to_string(),
        ),
        (
            "radio.analyzer.whitening_direction",
            match settings.analyzer.whitening_direction {
                WhiteningDirection::ShiftRight => "right",
                WhiteningDirection::ShiftLeft => "left",
            }
            .to_owned(),
        ),
        (
            "radio.analyzer.whitening_invert_output",
            settings.analyzer.whitening_invert_output.to_string(),
        ),
        (
            "radio.analyzer.whitening_start_bit",
            settings.analyzer.whitening_start_bit.to_string(),
        ),
        (
            "radio.analyzer.whitening_end_bit",
            settings
                .analyzer
                .whitening_end_bit
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        ),
        (
            "radio.analyzer.whitening_advance_excluded_bits",
            settings
                .analyzer
                .whitening_advance_excluded_bits
                .to_string(),
        ),
    ] {
        metadata.insert(key.to_owned(), value);
    }
    metadata.insert(
        "radio.analyzer.channel_count".to_owned(),
        settings.analyzer.channels.len().to_string(),
    );
    for (index, channel) in settings.analyzer.channels.iter().enumerate() {
        metadata.insert(
            format!("radio.analyzer.channel.{index}.offset_hz"),
            channel.frequency_offset_hz.to_string(),
        );
        metadata.insert(
            format!("radio.analyzer.channel.{index}.name"),
            channel.name.clone(),
        );
    }
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
    if let Some(sample_rate_hz) = recording_metadata_value::<u64>(metadata, "logic.sample_rate_hz")
    {
        if DSLOGIC_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            settings.sample_rate_hz = sample_rate_hz;
        }
    }
    if let Some(operation_mode) = metadata.get("logic.operation_mode") {
        settings.operation_mode = match operation_mode.as_str() {
            "stream" => DsLogicOperationMode::Stream,
            "internal_test" => DsLogicOperationMode::InternalTest,
            _ => DsLogicOperationMode::Buffer,
        };
    }
    if let Some(sample_count) = recording_metadata_value::<u64>(metadata, "logic.sample_count") {
        if (DSLOGIC_SAMPLE_ALIGNMENT..=settings.maximum_sample_count()).contains(&sample_count) {
            settings.sample_count = sample_count;
        }
    }
    for (channel, enabled) in settings.enabled_channels.iter_mut().enumerate() {
        if let Some(recorded) =
            recording_metadata_value::<bool>(metadata, &format!("logic.ch{channel}.enabled"))
        {
            *enabled = recorded;
        }
    }
    if let Some(stop_option) = metadata.get("logic.stop_option") {
        settings.stop_option = match stop_option.as_str() {
            "stop_immediately" => DsLogicStopOption::StopImmediately,
            _ => DsLogicStopOption::UploadCapturedData,
        };
    }
    if let Some(threshold_voltage) =
        recording_metadata_value::<f32>(metadata, "logic.threshold_voltage")
    {
        if threshold_voltage.is_finite() && (0.0..=5.0).contains(&threshold_voltage) {
            settings.threshold_voltage = threshold_voltage;
        }
    }
    if let Some(input_filter) = metadata.get("logic.input_filter") {
        settings.input_filter = match input_filter.as_str() {
            "one_sample_clock" => DsLogicInputFilter::OneSampleClock,
            _ => DsLogicInputFilter::None,
        };
    }
    if let Some(maximum_height) = recording_metadata_value::<u8>(metadata, "logic.maximum_height") {
        if (1..=5).contains(&maximum_height) {
            settings.maximum_heights = [maximum_height; LOGIC_CHANNEL_COUNT];
        }
    }
    for (channel, maximum_height) in settings.maximum_heights.iter_mut().enumerate() {
        if let Some(recorded) =
            recording_metadata_value::<u8>(metadata, &format!("logic.ch{channel}.maximum_height"))
        {
            if (1..=5).contains(&recorded) {
                *maximum_height = recorded;
            }
        }
    }
    if let Some(rle_enabled) = recording_metadata_value(metadata, "logic.rle_enabled") {
        settings.rle_enabled = rle_enabled;
    }
    if let Some(external_clock) = recording_metadata_value(metadata, "logic.external_clock") {
        settings.external_clock = external_clock;
    }
    if let Some(falling_clock_edge) = recording_metadata_value(metadata, "logic.falling_clock_edge")
    {
        settings.falling_clock_edge = falling_clock_edge;
    }
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
    settings
}

pub(crate) fn scope_settings_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> ScopeCaptureSettings {
    let mut settings = ScopeCaptureSettings::default();
    if let Some(operation_mode) = metadata.get("scope.operation_mode") {
        settings.operation_mode = match operation_mode.as_str() {
            "internal_test" => DsCopeOperationMode::InternalTest,
            _ => DsCopeOperationMode::Normal,
        };
    }
    if let Some(bandwidth_limit) = metadata.get("scope.bandwidth_limit") {
        settings.bandwidth_limit = match bandwidth_limit.as_str() {
            "20_mhz" => DsCopeBandwidthLimit::Mhz20,
            _ => DsCopeBandwidthLimit::Full,
        };
    }
    if let Some(sample_rate_hz) = recording_metadata_value::<u64>(metadata, "scope.sample_rate_hz")
    {
        if DSCOPE_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            settings.sample_rate_hz = sample_rate_hz;
        }
    }
    if let Some(sample_count) = recording_metadata_value::<u64>(metadata, "scope.sample_count") {
        if DSCOPE_SAMPLE_COUNTS.contains(&sample_count) {
            settings.sample_count = sample_count;
        }
    }
    for (index, channel) in settings.channels.iter_mut().enumerate() {
        if let Some(enabled) =
            recording_metadata_value::<bool>(metadata, &format!("scope.ch{index}.enabled"))
        {
            channel.enabled = enabled;
        }
        if let Some(volts_per_div_mv) =
            recording_metadata_value::<u16>(metadata, &format!("scope.ch{index}.volts_per_div_mv"))
        {
            if DSCOPE_VOLTS_PER_DIV_MV.contains(&volts_per_div_mv) {
                channel.volts_per_div_mv = volts_per_div_mv;
            }
        }
        if let Some(coupling) = metadata.get(&format!("scope.ch{index}.coupling")) {
            channel.coupling = if coupling.eq_ignore_ascii_case("ac") {
                DsCopeCoupling::Ac
            } else {
                DsCopeCoupling::Dc
            };
        }
        if let Some(probe_attenuation) =
            recording_metadata_value::<u8>(metadata, &format!("scope.ch{index}.probe_attenuation"))
        {
            if matches!(probe_attenuation, 1 | 10 | 100) {
                channel.probe_attenuation = probe_attenuation;
            }
        }
        if let Some(vertical_offset) =
            recording_metadata_value::<u16>(metadata, &format!("scope.ch{index}.vertical_offset"))
        {
            if (DSCOPE_VERTICAL_OFFSET_MIN..=DSCOPE_VERTICAL_OFFSET_MAX).contains(&vertical_offset)
            {
                channel.vertical_offset = vertical_offset;
            }
        }
    }

    if let Some(position_percent) =
        recording_metadata_value::<u8>(metadata, "scope.trigger.position_percent")
    {
        if (DSCOPE_TRIGGER_POSITION_MIN_PERCENT..=DSCOPE_TRIGGER_POSITION_MAX_PERCENT)
            .contains(&position_percent)
        {
            settings.trigger.position_percent = position_percent;
        }
    }
    if let Some(source) = metadata.get("scope.trigger.source") {
        settings.trigger.source = match source.as_str() {
            "ch0" => DsCopeTriggerSource::ChannelZero,
            "ch1" => DsCopeTriggerSource::ChannelOne,
            "ch0_and_ch1" => DsCopeTriggerSource::ChannelZeroAndOne,
            "ch0_or_ch1" => DsCopeTriggerSource::ChannelZeroOrOne,
            _ => DsCopeTriggerSource::Auto,
        };
    }
    if let Some(auto_channel) =
        recording_metadata_value::<u8>(metadata, "scope.trigger.auto_channel")
    {
        if auto_channel <= 1 {
            settings.trigger.auto_channel = auto_channel;
        }
    }
    if let Some(slope) = metadata.get("scope.trigger.slope") {
        settings.trigger.slope = if slope == "falling" {
            DsCopeTriggerSlope::Falling
        } else {
            DsCopeTriggerSlope::Rising
        };
    }
    if let Some(holdoff_ns) = recording_metadata_value::<u64>(metadata, "scope.trigger.holdoff_ns")
    {
        settings.trigger.holdoff_ns = holdoff_ns.min(DSCOPE_TRIGGER_HOLDOFF_MAX_NS);
        settings.trigger.holdoff_unit = if settings.trigger.holdoff_ns >= 1_000_000_000 {
            TriggerHoldoffUnit::Seconds
        } else if settings.trigger.holdoff_ns >= 1_000_000 {
            TriggerHoldoffUnit::Milliseconds
        } else {
            TriggerHoldoffUnit::Microseconds
        };
    }
    if let Some(noise_sensitivity) =
        recording_metadata_value::<u8>(metadata, "scope.trigger.noise_sensitivity")
    {
        settings.trigger.noise_sensitivity =
            noise_sensitivity.min(DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX);
    }
    for (index, level) in settings.trigger.levels.iter_mut().enumerate() {
        if let Some(recorded_level) =
            recording_metadata_value::<u8>(metadata, &format!("scope.trigger.ch{index}.level"))
        {
            *level = recorded_level.clamp(DSCOPE_TRIGGER_LEVEL_MIN, DSCOPE_TRIGGER_LEVEL_MAX);
        }
    }
    settings
        .trigger
        .clamp_to_channels(settings.channels.map(|channel| channel.enabled));
    settings
}

pub(crate) fn radio_settings_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> RadioSettings {
    let mut settings = RadioSettings::default();
    if let Some(value) = recording_metadata_value(metadata, "radio.center_frequency_hz") {
        settings.capture.center_frequency_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.sample_rate_hz") {
        settings.capture.sample_rate_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.bandwidth_hz") {
        settings.capture.bandwidth_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.receive_channel") {
        settings.capture.receive_channel = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.single_duration_millis") {
        settings.single_duration_millis = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.trigger_enabled") {
        settings.trigger_enabled = value;
    }
    if let Some(value) =
        recording_metadata_value::<u64>(metadata, "radio.trigger_posttrigger_micros")
    {
        settings.trigger_posttrigger_micros = value.min(RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS);
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.trigger_level_dbfs_tenths") {
        settings.trigger_level_dbfs_tenths = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.frequency_correction_milli_ppm")
    {
        settings.frequency_correction_milli_ppm = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.amplifier_enabled") {
        settings.amplifier_enabled = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.iq_correction_enabled") {
        settings.iq_correction_enabled = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.antenna_power_enabled") {
        settings.antenna_power_enabled = value;
    }
    let legacy_analyzer_enabled =
        recording_metadata_value::<bool>(metadata, "radio.analyzer.enabled");
    settings.analyzer.demodulation = if legacy_analyzer_enabled == Some(false) {
        RadioDemodulation::None
    } else if let Some(value) = metadata.get("radio.analyzer.demodulation") {
        match value.as_str() {
            "none" => RadioDemodulation::None,
            "gfsk2" => RadioDemodulation::BinaryGfsk,
            _ => RadioDemodulation::BinaryFsk,
        }
    } else if legacy_analyzer_enabled == Some(true) {
        RadioDemodulation::BinaryGfsk
    } else {
        RadioDemodulation::None
    };
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
                Some(RadioAnalyzerChannelSettings {
                    name,
                    frequency_offset_hz,
                })
            })
            .collect::<Vec<_>>();
        if !channels.is_empty() {
            settings.analyzer.channels = channels;
        }
    } else if let Some(value) =
        recording_metadata_value(metadata, "radio.analyzer.channel_offset_hz")
    {
        settings.analyzer.channels = vec![RadioAnalyzerChannelSettings::numbered(0, value)];
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.channel_bandwidth_hz") {
        settings.analyzer.channel_bandwidth_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.output_sample_rate_hz")
    {
        settings.analyzer.output_sample_rate_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.filter_tap_count") {
        settings.analyzer.filter_tap_count = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.symbol_rate_hz") {
        settings.analyzer.symbol_rate_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.deviation_hz") {
        settings.analyzer.deviation_hz = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.gfsk_bt_milli") {
        settings.analyzer.gfsk_bt_milli = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.inverted") {
        settings.analyzer.inverted = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.adaptive_timing") {
        settings.analyzer.adaptive_timing = value;
    }
    if let Some(value) =
        recording_metadata_value(metadata, "radio.analyzer.timing_loop_bandwidth_milli")
    {
        settings.analyzer.timing_loop_bandwidth_milli = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.max_clock_error_ppm") {
        settings.analyzer.max_clock_error_ppm = value;
    }
    if let Some(value) = metadata.get("radio.analyzer.data_mode") {
        settings.analyzer.data_mode = match value.as_str() {
            "packets" => RadioDataMode::Packets,
            _ => RadioDataMode::Raw,
        };
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.preamble_byte") {
        settings.analyzer.preamble_byte = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.preamble_bytes") {
        settings.analyzer.preamble_bytes = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.sync_word") {
        settings.analyzer.sync_word = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.sync_word_bytes") {
        settings.analyzer.sync_word_bytes = value;
    }
    if let Some(value) =
        recording_metadata_value(metadata, "radio.analyzer.maximum_preamble_bit_errors")
    {
        settings.analyzer.maximum_preamble_bit_errors = value;
    }
    if let Some(value) =
        recording_metadata_value(metadata, "radio.analyzer.minimum_bit_confidence_milli")
    {
        settings.analyzer.minimum_bit_confidence_milli = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.maximum_uncertain_bits")
    {
        settings.analyzer.maximum_uncertain_bits = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.whitening_enabled") {
        settings.analyzer.whitening_enabled = value;
    }
    if let Some(value) = metadata.get("radio.analyzer.whitening_preset") {
        settings.analyzer.whitening_preset = match value.as_str() {
            "pn9_x9_x5" => RadioWhiteningPreset::Pn9X9X5,
            _ => RadioWhiteningPreset::Custom,
        };
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.whitening_width") {
        settings.analyzer.whitening_width = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.whitening_polynomial") {
        settings.analyzer.whitening_polynomial = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.whitening_seed") {
        settings.analyzer.whitening_seed = value;
    }
    if let Some(value) = metadata.get("radio.analyzer.whitening_direction") {
        settings.analyzer.whitening_direction = match value.as_str() {
            "left" => WhiteningDirection::ShiftLeft,
            _ => WhiteningDirection::ShiftRight,
        };
    }
    if let Some(value) =
        recording_metadata_value(metadata, "radio.analyzer.whitening_invert_output")
    {
        settings.analyzer.whitening_invert_output = value;
    }
    if let Some(value) = recording_metadata_value(metadata, "radio.analyzer.whitening_start_bit") {
        settings.analyzer.whitening_start_bit = value;
    }
    if let Some(value) = metadata.get("radio.analyzer.whitening_end_bit") {
        settings.analyzer.whitening_end_bit = (value != "none")
            .then(|| value.parse::<usize>().ok())
            .flatten();
    }
    if let Some(value) =
        recording_metadata_value(metadata, "radio.analyzer.whitening_advance_excluded_bits")
    {
        settings.analyzer.whitening_advance_excluded_bits = value;
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
