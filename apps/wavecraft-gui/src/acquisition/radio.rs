//! Radio receiver capture settings: tuning and gains, the signal analyzer
//! configuration, and the translation to capture and runtime requests.

use crate::{
    AdditiveWhiteningConfig, BTreeMap, BinaryFskProfile, BinaryGfskProfile, BitOrder,
    BitTransformConfig, ChannelizerConfig, DemodulationProfile, DiscoveredRadio, FskPolarity,
    FskTiming, HackRfRuntimeSettings, HashSet, RadioCapabilities, RadioCaptureSettings,
    RadioDataDecoderConfig, RadioPacketFramerConfig, RadioPacketLength, RadioPacketPipelineConfig,
    RadioProviderSettings, RadioSingleTriggerConfig, RawRadioConfig, SignalAnalyzerProfile,
    SyncWordConfig, ValueDomain, WhiteningDirection, MAX_RADIO_ANALYZER_CHANNELS,
    RADIO_SPECTRUM_FFT_LENGTH, RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS,
};

pub(crate) struct RadioCaptureRequest {
    pub(crate) device: DiscoveredRadio,
    pub(crate) settings: RadioCaptureSettings,
    pub(crate) provider_settings: RadioProviderSettings,
    pub(crate) iq_correction_enabled: bool,
    pub(crate) analyzer_profiles: Vec<SignalAnalyzerProfile>,
    pub(crate) analyzer_channel_names: Vec<String>,
    pub(crate) decoder_config: RadioDataDecoderConfig,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RadioSettings {
    pub(crate) capture: RadioCaptureSettings,
    pub(crate) single_duration_millis: u64,
    pub(crate) trigger_enabled: bool,
    pub(crate) trigger_posttrigger_micros: u64,
    pub(crate) trigger_level_dbfs_tenths: i16,
    pub(crate) gains_db: BTreeMap<String, i32>,
    pub(crate) frequency_correction_milli_ppm: i32,
    pub(crate) iq_correction_enabled: bool,
    pub(crate) amplifier_enabled: bool,
    pub(crate) antenna_power_enabled: bool,
    pub(crate) analyzer: RadioAnalyzerSettings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RadioDemodulation {
    None,
    BinaryFsk,
    BinaryGfsk,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RadioDataMode {
    Raw,
    Packets,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RadioWhiteningPreset {
    Custom,
    Pn9X9X5,
}

impl RadioWhiteningPreset {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Custom => "Custom",
            Self::Pn9X9X5 => "PN9",
        }
    }

    pub(crate) fn metadata_value(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Pn9X9X5 => "pn9_x9_x5",
        }
    }

    pub(crate) fn description(self) -> Option<&'static str> {
        match self {
            Self::Custom => None,
            Self::Pn9X9X5 => Some("PN9 XORs packet data after the sync word using x⁹ + x⁵ + 1, resets to seed 0x1FF at each packet, and is compatible with standard SX127x FSK whitening"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RadioAnalyzerChannelSettings {
    pub(crate) name: String,
    pub(crate) frequency_offset_hz: i64,
}

impl RadioAnalyzerChannelSettings {
    pub(crate) fn numbered(index: usize, frequency_offset_hz: i64) -> Self {
        Self {
            name: format!("Channel {}", index + 1),
            frequency_offset_hz,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RadioAnalyzerSettings {
    pub(crate) demodulation: RadioDemodulation,
    pub(crate) channels: Vec<RadioAnalyzerChannelSettings>,
    pub(crate) channel_bandwidth_hz: u64,
    pub(crate) output_sample_rate_hz: u64,
    pub(crate) filter_tap_count: usize,
    pub(crate) symbol_rate_hz: u64,
    pub(crate) deviation_hz: u64,
    pub(crate) gfsk_bt_milli: u32,
    pub(crate) inverted: bool,
    pub(crate) adaptive_timing: bool,
    pub(crate) timing_loop_bandwidth_milli: u32,
    pub(crate) max_clock_error_ppm: u32,
    pub(crate) data_mode: RadioDataMode,
    pub(crate) preamble_byte: u8,
    pub(crate) preamble_bytes: usize,
    pub(crate) sync_word: u64,
    pub(crate) sync_word_bytes: u8,
    pub(crate) maximum_preamble_bit_errors: usize,
    pub(crate) minimum_bit_confidence_milli: u16,
    pub(crate) maximum_uncertain_bits: usize,
    pub(crate) whitening_enabled: bool,
    pub(crate) whitening_preset: RadioWhiteningPreset,
    pub(crate) whitening_width: u8,
    pub(crate) whitening_polynomial: u64,
    pub(crate) whitening_seed: u64,
    pub(crate) whitening_direction: WhiteningDirection,
    pub(crate) whitening_invert_output: bool,
    pub(crate) whitening_start_bit: usize,
    pub(crate) whitening_end_bit: Option<usize>,
    pub(crate) whitening_advance_excluded_bits: bool,
}

impl Default for RadioAnalyzerSettings {
    fn default() -> Self {
        Self {
            demodulation: RadioDemodulation::None,
            // Capture both observed 868.1/868.5 MHz channels around 868.3 MHz;
            // retain 868.1 MHz as the first channel if analysis is enabled.
            channels: vec![RadioAnalyzerChannelSettings::numbered(0, -200_000)],
            channel_bandwidth_hz: 200_000,
            output_sample_rate_hz: 512_000,
            filter_tap_count: 257,
            symbol_rate_hz: 19_200,
            deviation_hz: 35_000,
            gfsk_bt_milli: 500,
            inverted: false,
            adaptive_timing: true,
            timing_loop_bandwidth_milli: 100,
            max_clock_error_ppm: 10_000,
            data_mode: RadioDataMode::Raw,
            preamble_byte: 0xaa,
            preamble_bytes: 4,
            sync_word: 0,
            sync_word_bytes: 0,
            maximum_preamble_bit_errors: 1,
            minimum_bit_confidence_milli: 0,
            maximum_uncertain_bits: 0,
            whitening_enabled: false,
            whitening_preset: RadioWhiteningPreset::Custom,
            whitening_width: 7,
            whitening_polynomial: 0b100_0001,
            whitening_seed: 0b101_1011,
            whitening_direction: WhiteningDirection::ShiftRight,
            whitening_invert_output: false,
            whitening_start_bit: 0,
            whitening_end_bit: None,
            whitening_advance_excluded_bits: false,
        }
    }
}

impl RadioAnalyzerSettings {
    pub(crate) fn is_enabled(&self) -> bool {
        self.demodulation != RadioDemodulation::None
    }

    pub(crate) fn channel_name(&self, index: usize) -> String {
        self.channels
            .get(index)
            .map(|channel| channel.name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Channel {}", index + 1))
    }

    pub(crate) fn raw_config(&self) -> RawRadioConfig {
        RawRadioConfig {
            preamble_byte: self.preamble_byte,
            preamble_bytes: self.preamble_bytes,
            sync_word: self.sync_word,
            sync_word_bytes: self.sync_word_bytes,
            maximum_preamble_bit_errors: self.maximum_preamble_bit_errors,
            bit_order: BitOrder::MostSignificantFirst,
            minimum_bit_confidence_milli: self.minimum_bit_confidence_milli,
            maximum_uncertain_bits: self.maximum_uncertain_bits,
        }
    }

    pub(crate) fn whitening_config(&self) -> AdditiveWhiteningConfig {
        match self.whitening_preset {
            RadioWhiteningPreset::Custom => AdditiveWhiteningConfig {
                width: self.whitening_width,
                polynomial: self.whitening_polynomial,
                seed: self.whitening_seed,
                direction: self.whitening_direction,
                invert_output: self.whitening_invert_output,
                start_bit: self.whitening_start_bit,
                end_bit: self.whitening_end_bit,
                advance_excluded_bits: self.whitening_advance_excluded_bits,
            },
            RadioWhiteningPreset::Pn9X9X5 => AdditiveWhiteningConfig {
                width: 9,
                polynomial: 0x21,
                seed: 0x1ff,
                direction: WhiteningDirection::ShiftRight,
                invert_output: false,
                start_bit: 0,
                end_bit: None,
                advance_excluded_bits: false,
            },
        }
    }

    pub(crate) fn decoder_config(&self) -> Result<RadioDataDecoderConfig, String> {
        match self.data_mode {
            RadioDataMode::Raw => {
                let config = self.raw_config();
                config.validate().map_err(|error| error.to_string())?;
                Ok(RadioDataDecoderConfig::Raw(config))
            }
            RadioDataMode::Packets => {
                if self.sync_word_bytes == 0 {
                    return Err("packet decoding requires a sync word".to_owned());
                }
                let preamble_bits = std::iter::repeat(self.preamble_byte)
                    .take(self.preamble_bytes)
                    .flat_map(|byte| {
                        (0..8).map(move |bit_index| byte & (1 << (7 - bit_index)) != 0)
                    })
                    .collect();
                let sync_bits = (0..self.sync_word_bytes)
                    .flat_map(|byte_index| {
                        let shift = u32::from(self.sync_word_bytes - byte_index - 1) * 8;
                        let byte = ((self.sync_word >> shift) & 0xff) as u8;
                        (0..8).map(move |bit_index| byte & (1 << (7 - bit_index)) != 0)
                    })
                    .collect();
                let whitening = self.whitening_config();
                let whitening = self.whitening_enabled.then_some(whitening);
                let config = RadioPacketPipelineConfig {
                    bit_transform: BitTransformConfig::default(),
                    framing: RadioPacketFramerConfig {
                        preamble: preamble_bits,
                        sync: SyncWordConfig {
                            bits: sync_bits,
                            max_bit_errors: self.maximum_preamble_bit_errors,
                        },
                        whitening,
                        bit_order: BitOrder::MostSignificantFirst,
                        length: RadioPacketLength::UntilEndOfStream,
                        max_search_bits: 1_048_576,
                    },
                    crc: None,
                };
                wavecraft_decode::RadioPacketPipeline::new(config.clone())
                    .map_err(|error| error.to_string())?;
                Ok(RadioDataDecoderConfig::Packets(config))
            }
        }
    }

    pub(crate) fn signal_profiles(&self) -> Result<Vec<SignalAnalyzerProfile>, String> {
        if !self.is_enabled() {
            return Ok(Vec::new());
        }
        if self.channels.is_empty() || self.channels.len() > MAX_RADIO_ANALYZER_CHANNELS {
            return Err(format!(
                "signal analyzer requires between 1 and {MAX_RADIO_ANALYZER_CHANNELS} channels"
            ));
        }
        let mut unique_offsets = HashSet::with_capacity(self.channels.len());
        if self
            .channels
            .iter()
            .any(|channel| !unique_offsets.insert(channel.frequency_offset_hz))
        {
            return Err("signal analyzer channel offsets must be unique".to_owned());
        }
        let timing = if self.adaptive_timing {
            FskTiming::Adaptive {
                loop_bandwidth: f64::from(self.timing_loop_bandwidth_milli) / 1_000.0,
                max_clock_error_ppm: self.max_clock_error_ppm,
            }
        } else {
            FskTiming::Fixed
        };
        let polarity = if self.inverted {
            FskPolarity::Inverted
        } else {
            FskPolarity::Normal
        };
        let demodulation = match self.demodulation {
            RadioDemodulation::None => return Ok(Vec::new()),
            RadioDemodulation::BinaryFsk => DemodulationProfile::BinaryFsk(BinaryFskProfile {
                symbol_rate_hz: self.symbol_rate_hz,
                deviation_hz: self.deviation_hz,
                polarity,
                timing,
            }),
            RadioDemodulation::BinaryGfsk => DemodulationProfile::BinaryGfsk(BinaryGfskProfile {
                symbol_rate_hz: self.symbol_rate_hz,
                deviation_hz: self.deviation_hz,
                bt: f64::from(self.gfsk_bt_milli) / 1_000.0,
                polarity,
                timing,
            }),
        };
        self.channels
            .iter()
            .map(|channel| {
                let profile = SignalAnalyzerProfile {
                    channel: ChannelizerConfig {
                        frequency_offset_hz: channel.frequency_offset_hz,
                        bandwidth_hz: self.channel_bandwidth_hz,
                        output_sample_rate_hz: self.output_sample_rate_hz,
                        filter_tap_count: self.filter_tap_count,
                    },
                    demodulation: demodulation.clone(),
                };
                profile.validate().map_err(|error| error.to_string())?;
                Ok(profile)
            })
            .collect()
    }
}

impl Default for RadioSettings {
    fn default() -> Self {
        Self {
            capture: RadioCaptureSettings {
                center_frequency_hz: 868_300_000,
                sample_rate_hz: 8_000_000,
                bandwidth_hz: 1_750_000,
                receive_channel: 0,
                sample_limit: None,
            },
            single_duration_millis: 1_000,
            trigger_enabled: false,
            trigger_posttrigger_micros: 2_000,
            trigger_level_dbfs_tenths: -500,
            gains_db: BTreeMap::new(),
            frequency_correction_milli_ppm: 0,
            iq_correction_enabled: true,
            amplifier_enabled: false,
            antenna_power_enabled: false,
            analyzer: RadioAnalyzerSettings::default(),
        }
    }
}

pub(crate) fn nearest_domain_value(domain: &ValueDomain, requested: u64) -> u64 {
    match domain {
        ValueDomain::Discrete(values) => values
            .iter()
            .copied()
            .min_by_key(|value| value.abs_diff(requested))
            .unwrap_or(requested),
        ValueDomain::Stepped { range, step } => {
            if *step == 0 {
                return requested.clamp(*range.start(), *range.end());
            }
            let maximum = range.start().saturating_add(
                range
                    .end()
                    .saturating_sub(*range.start())
                    .saturating_div(*step)
                    .saturating_mul(*step),
            );
            let clamped = requested.clamp(*range.start(), maximum);
            let offset = clamped.saturating_sub(*range.start());
            let lower = range
                .start()
                .saturating_add((offset / step).saturating_mul(*step));
            let upper = lower.saturating_add(*step).min(maximum);
            if clamped.abs_diff(lower) <= clamped.abs_diff(upper) {
                lower
            } else {
                upper
            }
        }
    }
}

pub(crate) fn normalize_radio_settings(
    settings: &mut RadioSettings,
    capabilities: &RadioCapabilities,
) {
    settings.capture.center_frequency_hz = nearest_domain_value(
        &capabilities.frequency_hz,
        settings.capture.center_frequency_hz,
    );
    settings.capture.sample_rate_hz = nearest_domain_value(
        &capabilities.sample_rates_hz,
        settings.capture.sample_rate_hz,
    );
    settings.capture.bandwidth_hz =
        nearest_domain_value(&capabilities.bandwidths_hz, settings.capture.bandwidth_hz);
    settings.capture.receive_channel = settings
        .capture
        .receive_channel
        .min(capabilities.receive_channels.saturating_sub(1));

    settings.gains_db.retain(|id, value| {
        let Some(control) = capabilities
            .gain_controls
            .iter()
            .find(|control| control.id == *id)
        else {
            return false;
        };
        *value = normalize_gain_value(
            *value,
            *control.range_db.start(),
            *control.range_db.end(),
            control.step_db,
        );
        true
    });
    for control in &capabilities.gain_controls {
        settings
            .gains_db
            .entry(control.id.clone())
            .or_insert(control.default_db);
    }
}

pub(crate) fn radio_gain_u32(settings: &RadioSettings, id: &str) -> Result<Option<u32>, String> {
    settings
        .gains_db
        .get(id)
        .copied()
        .map(|value| {
            u32::try_from(value).map_err(|_| format!("radio gain {id} cannot be negative"))
        })
        .transpose()
}

pub(crate) fn radio_single_sample_limit(
    sample_rate_hz: u64,
    duration_millis: u64,
) -> Result<u64, String> {
    if sample_rate_hz == 0 || duration_millis == 0 {
        return Err("RF Single duration and sample rate must be greater than zero".to_owned());
    }
    radio_duration_sample_count(sample_rate_hz, duration_millis)
}

pub(crate) fn radio_duration_sample_count(
    sample_rate_hz: u64,
    duration_millis: u64,
) -> Result<u64, String> {
    if sample_rate_hz == 0 {
        return Err("RF sample rate must be greater than zero".to_owned());
    }
    let samples = u128::from(sample_rate_hz)
        .saturating_mul(u128::from(duration_millis))
        .div_ceil(1_000);
    u64::try_from(samples).map_err(|_| "RF Single capture is too long".to_owned())
}

pub(crate) fn radio_microsecond_sample_count(
    sample_rate_hz: u64,
    duration_micros: u64,
) -> Result<u64, String> {
    if sample_rate_hz == 0 {
        return Err("RF sample rate must be greater than zero".to_owned());
    }
    let samples = u128::from(sample_rate_hz)
        .saturating_mul(u128::from(duration_micros))
        .div_ceil(1_000_000);
    u64::try_from(samples).map_err(|_| "RF post-trigger interval is too long".to_owned())
}

pub(crate) fn radio_single_trigger_config(
    settings: &RadioSettings,
) -> Result<RadioSingleTriggerConfig, String> {
    if settings.trigger_posttrigger_micros > RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS {
        return Err(format!(
            "RF post-trigger duration must not exceed {} ms",
            RADIO_TRIGGER_POSTTRIGGER_MAX_MICROS / 1_000
        ));
    }
    Ok(RadioSingleTriggerConfig {
        threshold_dbfs: f32::from(settings.trigger_level_dbfs_tenths) / 10.0,
        posttrigger_samples: radio_microsecond_sample_count(
            settings.capture.sample_rate_hz,
            settings.trigger_posttrigger_micros,
        )?,
        fft_length: RADIO_SPECTRUM_FFT_LENGTH,
        iq_correction_enabled: settings.iq_correction_enabled,
    })
}

pub(crate) fn radio_trigger_level_controls_are_enabled(
    capture_settings_enabled: bool,
    trigger_runtime_enabled: bool,
) -> bool {
    capture_settings_enabled || trigger_runtime_enabled
}

pub(crate) fn radio_trigger_level_runtime_controls_are_enabled(
    run_runtime_enabled: bool,
    single_trigger_runtime_enabled: bool,
) -> bool {
    run_runtime_enabled || single_trigger_runtime_enabled
}

pub(crate) fn hackrf_runtime_settings(
    settings: &RadioSettings,
) -> Result<HackRfRuntimeSettings, String> {
    Ok(HackRfRuntimeSettings {
        center_frequency_hz: settings.capture.center_frequency_hz,
        bandwidth_hz: settings.capture.bandwidth_hz,
        lna_gain_db: radio_gain_u32(settings, "lna")?,
        vga_gain_db: radio_gain_u32(settings, "vga")?,
        amplifier_enabled: settings.amplifier_enabled,
        frequency_correction_milli_ppm: settings.frequency_correction_milli_ppm,
    })
}

pub(crate) fn radio_runtime_settings_differ(
    current: &RadioSettings,
    previous: &RadioSettings,
) -> bool {
    current.capture.center_frequency_hz != previous.capture.center_frequency_hz
        || current.capture.bandwidth_hz != previous.capture.bandwidth_hz
        || current.gains_db != previous.gains_db
        || current.amplifier_enabled != previous.amplifier_enabled
        || current.frequency_correction_milli_ppm != previous.frequency_correction_milli_ppm
}

pub(crate) fn normalize_gain_value(value: i32, minimum: i32, maximum: i32, step: u32) -> i32 {
    let clamped = value.clamp(minimum, maximum);
    let step = i64::from(step);
    if step == 0 {
        return clamped;
    }
    let offset = i64::from(clamped) - i64::from(minimum);
    let rounded_steps = (offset + step / 2) / step;
    (i64::from(minimum) + rounded_steps * step).min(i64::from(maximum)) as i32
}
