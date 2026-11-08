//! Radio receiver capture settings: tuning and gains, the signal analyzer
//! configuration, and the translation to capture and runtime requests.

use crate::{
    recommended_fsk_channel_bandwidth_hz, AdditiveWhiteningConfig, BTreeMap, BinaryFskProfile,
    BinaryGfskProfile, BitOrder, BitTransformConfig, ChannelizerConfig, DemodulationProfile,
    DiscoveredRadio, FskPolarity, FskTiming, HashSet, RadioCapabilities, RadioCaptureSettings,
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
    /// One decoder configuration per analyzer channel, in channel order: the
    /// channels differ in their sync word.
    pub(crate) decoder_configs: Vec<RadioDataDecoderConfig>,
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

/// `channel_bandwidth_hz` value that selects the recommended bandwidth.
pub(crate) const AUTOMATIC_CHANNEL_BANDWIDTH_HZ: u64 = 0;
/// `filter_tap_count` value that selects the length from the input rate.
pub(crate) const AUTOMATIC_FILTER_TAP_COUNT: usize = 0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RadioAnalyzerChannelSettings {
    pub(crate) name: String,
    pub(crate) frequency_offset_hz: i64,
    /// Sync word of this channel as hexadecimal text, one to eight bytes. Empty
    /// uses the general sync word of [`RadioAnalyzerSettings`]. Kept as the
    /// text typed so that a half-typed word is not lost; see
    /// [`SyncWordSetting::parse`].
    pub(crate) sync_word: String,
}

impl RadioAnalyzerChannelSettings {
    pub(crate) fn numbered(index: usize, frequency_offset_hz: i64) -> Self {
        Self {
            name: format!("Channel {}", index + 1),
            frequency_offset_hz,
            sync_word: String::new(),
        }
    }
}

/// A sync word and its length in bytes, as in [`RawRadioConfig`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SyncWordSetting {
    pub(crate) word: u64,
    pub(crate) bytes: u8,
}

impl SyncWordSetting {
    /// Reads a sync word typed as hexadecimal digits, big-endian as on the air
    /// (`91 d3` is the bytes 0x91, 0xd3). Spaces and a leading `0x` are
    /// accepted. Empty text is no sync word.
    pub(crate) fn parse(text: &str) -> Result<Option<Self>, String> {
        let digits = text.split_whitespace().collect::<String>();
        let digits = digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
            .unwrap_or(&digits);
        if digits.is_empty() {
            return Ok(None);
        }
        if !digits.chars().all(|digit| digit.is_ascii_hexdigit()) {
            return Err("sync word must be hexadecimal, for example 91 d3".to_owned());
        }
        if digits.len() % 2 != 0 {
            return Err("sync word must be whole bytes: two hex digits per byte".to_owned());
        }
        if digits.len() > 16 {
            return Err("sync word must be at most 8 bytes".to_owned());
        }
        let word = u64::from_str_radix(digits, 16)
            .map_err(|_| "sync word must be hexadecimal, for example 91 d3".to_owned())?;
        Ok(Some(Self {
            word,
            bytes: (digits.len() / 2) as u8,
        }))
    }

    /// The word as bytes in hexadecimal, such as `91 d3`.
    pub(crate) fn text(self) -> String {
        (0..self.bytes)
            .map(|index| {
                let shift = u32::from(self.bytes - index - 1) * 8;
                format!("{:02x}", (self.word >> shift) & 0xff)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RadioAnalyzerSettings {
    pub(crate) demodulation: RadioDemodulation,
    pub(crate) channels: Vec<RadioAnalyzerChannelSettings>,
    /// Channel bandwidth, or [`AUTOMATIC_CHANNEL_BANDWIDTH_HZ`] to derive it
    /// from the bit rate and deviation (see
    /// [`Self::effective_channel_bandwidth_hz`]).
    pub(crate) channel_bandwidth_hz: u64,
    pub(crate) output_sample_rate_hz: u64,
    /// Odd FIR length, or [`AUTOMATIC_FILTER_TAP_COUNT`] to size it from the
    /// capture rate and the channel bandwidth.
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
            channel_bandwidth_hz: AUTOMATIC_CHANNEL_BANDWIDTH_HZ,
            output_sample_rate_hz: 512_000,
            filter_tap_count: AUTOMATIC_FILTER_TAP_COUNT,
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
    /// Channel bandwidth in use: the configured one, or for
    /// [`AUTOMATIC_CHANNEL_BANDWIDTH_HZ`] the recommended bandwidth for the bit rate and deviation, limited to the
    /// processing rate that the channel is resampled to.
    pub(crate) fn effective_channel_bandwidth_hz(&self) -> u64 {
        if self.channel_bandwidth_hz != AUTOMATIC_CHANNEL_BANDWIDTH_HZ {
            return self.channel_bandwidth_hz;
        }
        recommended_fsk_channel_bandwidth_hz(self.symbol_rate_hz, self.deviation_hz)
            .clamp(1, self.output_sample_rate_hz.max(1))
    }

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

    /// The sync word shared by the channels that have none of their own.
    pub(crate) fn general_sync_word(&self) -> SyncWordSetting {
        SyncWordSetting {
            word: self.sync_word,
            bytes: self.sync_word_bytes,
        }
    }

    /// The sync word of channel `index`: its own if one is set, otherwise the
    /// general one.
    pub(crate) fn sync_word_for_channel(&self, index: usize) -> Result<SyncWordSetting, String> {
        let own = match self.channels.get(index) {
            Some(channel) => SyncWordSetting::parse(&channel.sync_word)
                .map_err(|error| format!("{}: {error}", self.channel_name(index)))?,
            None => None,
        };
        Ok(own.unwrap_or_else(|| self.general_sync_word()))
    }

    /// Acceptable bit errors in the acquisition pattern, kept below the length
    /// of a sync word: a short sync word of one channel cannot take the limit
    /// set for a longer general one.
    fn bit_errors_for(&self, sync: SyncWordSetting) -> usize {
        if sync.bytes == 0 {
            return self.maximum_preamble_bit_errors;
        }
        self.maximum_preamble_bit_errors
            .min((usize::from(sync.bytes) * 8).saturating_sub(1))
    }

    fn raw_config_with(&self, sync: SyncWordSetting) -> RawRadioConfig {
        RawRadioConfig {
            preamble_byte: self.preamble_byte,
            preamble_bytes: self.preamble_bytes,
            sync_word: sync.word,
            sync_word_bytes: sync.bytes,
            maximum_preamble_bit_errors: self.bit_errors_for(sync),
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

    /// The decoder of the channels that have no sync word of their own.
    #[cfg(test)]
    pub(crate) fn decoder_config(&self) -> Result<RadioDataDecoderConfig, String> {
        self.decoder_config_with(self.general_sync_word())
    }

    /// One decoder configuration per channel, each with its own sync word or
    /// the general one.
    pub(crate) fn decoder_configs(&self) -> Result<Vec<RadioDataDecoderConfig>, String> {
        (0..self.channels.len())
            .map(|index| {
                self.decoder_config_with(self.sync_word_for_channel(index)?)
                    .map_err(|error| format!("{}: {error}", self.channel_name(index)))
            })
            .collect()
    }

    fn decoder_config_with(&self, sync: SyncWordSetting) -> Result<RadioDataDecoderConfig, String> {
        match self.data_mode {
            RadioDataMode::Raw => {
                let config = self.raw_config_with(sync);
                config.validate().map_err(|error| error.to_string())?;
                Ok(RadioDataDecoderConfig::Raw(config))
            }
            RadioDataMode::Packets => {
                if sync.bytes == 0 {
                    return Err("packet decoding requires a sync word".to_owned());
                }
                let preamble_bits = std::iter::repeat(self.preamble_byte)
                    .take(self.preamble_bytes)
                    .flat_map(|byte| {
                        (0..8).map(move |bit_index| byte & (1 << (7 - bit_index)) != 0)
                    })
                    .collect();
                let sync_bits = (0..sync.bytes)
                    .flat_map(|byte_index| {
                        let shift = u32::from(sync.bytes - byte_index - 1) * 8;
                        let byte = ((sync.word >> shift) & 0xff) as u8;
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
                            max_bit_errors: self.bit_errors_for(sync),
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
        let bandwidth_hz = self.effective_channel_bandwidth_hz();
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
                        bandwidth_hz,
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    #[test]
    fn radio_settings_follow_selected_device_capabilities() {
        let capabilities = wavecraft_hackrf::hackrf_one_capabilities();
        let mut settings = RadioSettings::default();
        settings.capture.center_frequency_hz = 100;
        settings.capture.sample_rate_hz = 21_000_000;
        settings.capture.bandwidth_hz = 8_100_000;
        settings.capture.receive_channel = 7;
        settings.gains_db.insert("lna".to_owned(), 39);
        settings.gains_db.insert("stale".to_owned(), 10);

        normalize_radio_settings(&mut settings, &capabilities);

        assert_eq!(settings.capture.center_frequency_hz, 1_000_000);
        assert_eq!(settings.capture.sample_rate_hz, 20_000_000);
        assert_eq!(settings.capture.bandwidth_hz, 8_000_000);
        assert_eq!(settings.capture.receive_channel, 0);
        assert_eq!(settings.gains_db.get("lna"), Some(&40));
        assert_eq!(settings.gains_db.get("vga"), Some(&16));
        assert!(!settings.gains_db.contains_key("stale"));
        settings
            .capture
            .validate(&capabilities)
            .expect("normalized radio settings should validate");
    }

    #[test]
    fn radio_defaults_start_with_signal_analysis_disabled() {
        let settings = RadioSettings::default();

        assert_eq!(settings.capture.center_frequency_hz, 868_300_000);
        assert_eq!(settings.capture.sample_rate_hz, 8_000_000);
        assert_eq!(settings.capture.bandwidth_hz, 1_750_000);
        assert_eq!(settings.analyzer.demodulation, RadioDemodulation::None);
        assert!(!settings.analyzer.is_enabled());
        assert_eq!(settings.analyzer.symbol_rate_hz, 19_200);
        assert_eq!(settings.analyzer.deviation_hz, 35_000);
        assert_eq!(settings.analyzer.channels[0].name, "Channel 1");
        assert_eq!(settings.analyzer.channels[0].frequency_offset_hz, -200_000);
        assert_eq!(settings.analyzer.output_sample_rate_hz, 512_000);
        assert_eq!(
            settings.analyzer.channel_bandwidth_hz,
            AUTOMATIC_CHANNEL_BANDWIDTH_HZ
        );
        assert_eq!(
            settings.analyzer.filter_tap_count,
            AUTOMATIC_FILTER_TAP_COUNT
        );
        // Carson's rule for 19.2 kbit/s and 35 kHz deviation plus the margin.
        assert_eq!(settings.analyzer.effective_channel_bandwidth_hz(), 120_000);
        assert_eq!(settings.analyzer.maximum_preamble_bit_errors, 1);
        assert_eq!(settings.analyzer.minimum_bit_confidence_milli, 0);
        assert_eq!(settings.analyzer.maximum_uncertain_bits, 0);
        assert_eq!(settings.trigger_posttrigger_micros, 2_000);
        settings
            .capture
            .validate(&wavecraft_hackrf::hackrf_one_capabilities())
            .expect("default radio settings should be supported by HackRF One");
    }

    #[test]
    fn radio_single_duration_derives_an_exact_bounded_sample_limit() {
        assert_eq!(
            radio_single_sample_limit(10_000_000, 1_500).unwrap(),
            15_000_000
        );
        assert_eq!(radio_single_sample_limit(3, 1).unwrap(), 1);
        assert!(radio_single_sample_limit(10_000_000, 0).is_err());
    }

    #[test]
    fn radio_single_trigger_derives_fractional_millisecond_posttrigger_samples() {
        let mut settings = RadioSettings::default();
        settings.capture.sample_rate_hz = 10_000_000;
        settings.trigger_posttrigger_micros = 125;

        let config = radio_single_trigger_config(&settings).unwrap();

        assert_eq!(config.posttrigger_samples, 1_250);
        assert_eq!(config.fft_length, RADIO_SPECTRUM_FFT_LENGTH);
    }

    #[test]
    fn radio_trigger_level_stays_editable_during_run_and_while_single_is_armed() {
        assert!(radio_trigger_level_runtime_controls_are_enabled(
            true, false
        ));
        assert!(radio_trigger_level_runtime_controls_are_enabled(
            false, true
        ));
        assert!(!radio_trigger_level_runtime_controls_are_enabled(
            false, false
        ));
        assert!(radio_trigger_level_controls_are_enabled(false, true));
        assert!(radio_trigger_level_controls_are_enabled(true, false));
        assert!(!radio_trigger_level_controls_are_enabled(false, false));
    }

    #[test]
    fn radio_runtime_settings_differ_only_for_live_controls() {
        let capabilities = wavecraft_hackrf::hackrf_one_capabilities();
        let mut initial = RadioSettings::default();
        normalize_radio_settings(&mut initial, &capabilities);
        let mut edited = initial.clone();
        edited.capture.sample_rate_hz = 12_000_000;
        assert!(!radio_runtime_settings_differ(&edited, &initial));

        let mut correction_only = initial.clone();
        correction_only.frequency_correction_milli_ppm = -8_250;
        assert!(radio_runtime_settings_differ(&correction_only, &initial));

        edited.capture.center_frequency_hz = 433_920_000;
        edited.capture.bandwidth_hz = 8_000_000;
        edited.gains_db.insert("lna".to_owned(), 24);
        edited.gains_db.insert("vga".to_owned(), 20);
        edited.amplifier_enabled = true;
        edited.frequency_correction_milli_ppm = 12_500;
        assert!(radio_runtime_settings_differ(&edited, &initial));
    }

    #[test]
    fn radio_analyzer_settings_build_a_tagged_signal_profile() {
        let mut settings = RadioAnalyzerSettings::default();
        assert!(settings.signal_profiles().unwrap().is_empty());

        settings.demodulation = RadioDemodulation::BinaryGfsk;
        settings.channels = vec![
            RadioAnalyzerChannelSettings::numbered(0, 125_000),
            RadioAnalyzerChannelSettings::numbered(1, -250_000),
        ];
        settings.gfsk_bt_milli = 500;
        let profiles = settings.signal_profiles().unwrap();
        assert_eq!(profiles.len(), 2);
        let profile = profiles[0].clone();

        assert_eq!(profile.channel.frequency_offset_hz, 125_000);
        assert_eq!(profiles[1].channel.frequency_offset_hz, -250_000);
        let DemodulationProfile::BinaryGfsk(gfsk) = profile.demodulation else {
            panic!("GFSK settings should build the GFSK profile variant");
        };
        assert_eq!(gfsk.symbol_rate_hz, 19_200);
        assert_eq!(gfsk.deviation_hz, 35_000);
        assert_eq!(gfsk.bt, 0.5);
        assert!(matches!(gfsk.timing, FskTiming::Adaptive { .. }));
    }

    #[test]
    fn automatic_channel_bandwidth_follows_the_signal_and_manual_values_win() {
        let mut settings = RadioAnalyzerSettings {
            demodulation: RadioDemodulation::BinaryGfsk,
            ..RadioAnalyzerSettings::default()
        };
        let profile =
            |settings: &RadioAnalyzerSettings| settings.signal_profiles().unwrap().remove(0);

        // 19.2 kbit/s with 35 kHz deviation: Carson 89.2 kHz plus 2 x 15 kHz.
        let automatic = profile(&settings);
        assert_eq!(automatic.channel.bandwidth_hz, 120_000);
        assert_eq!(
            automatic.channel.filter_tap_count,
            AUTOMATIC_FILTER_TAP_COUNT
        );

        settings.deviation_hz = 50_000;
        settings.symbol_rate_hz = 100_000;
        assert_eq!(profile(&settings).channel.bandwidth_hz, 230_000);

        // The channel cannot be wider than the rate it is resampled to.
        settings.output_sample_rate_hz = 200_000;
        assert_eq!(profile(&settings).channel.bandwidth_hz, 200_000);

        settings.channel_bandwidth_hz = 150_000;
        settings.filter_tap_count = 129;
        let manual = profile(&settings);
        assert_eq!(manual.channel.bandwidth_hz, 150_000);
        assert_eq!(manual.channel.filter_tap_count, 129);
    }

    #[test]
    fn radio_analyzer_supports_up_to_one_hundred_unique_channels() {
        let mut settings = RadioAnalyzerSettings {
            demodulation: RadioDemodulation::BinaryGfsk,
            channels: (0..MAX_RADIO_ANALYZER_CHANNELS)
                .map(|index| RadioAnalyzerChannelSettings::numbered(index, index as i64 * 1_000))
                .collect(),
            ..RadioAnalyzerSettings::default()
        };

        assert_eq!(
            settings.signal_profiles().unwrap().len(),
            MAX_RADIO_ANALYZER_CHANNELS
        );

        settings
            .channels
            .push(RadioAnalyzerChannelSettings::numbered(
                MAX_RADIO_ANALYZER_CHANNELS,
                200_000,
            ));
        assert!(settings
            .signal_profiles()
            .unwrap_err()
            .contains("between 1 and 100"));

        settings.channels = vec![
            RadioAnalyzerChannelSettings::numbered(0, 100_000),
            RadioAnalyzerChannelSettings::numbered(1, 100_000),
        ];
        assert!(settings
            .signal_profiles()
            .unwrap_err()
            .contains("must be unique"));
    }

    #[test]
    fn radio_analyzer_settings_build_a_post_sync_whitening_pipeline() {
        let mut settings = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Packets,
            sync_word: 0xd391,
            sync_word_bytes: 2,
            maximum_preamble_bit_errors: 1,
            whitening_enabled: true,
            whitening_width: 9,
            whitening_polynomial: 0x11,
            whitening_seed: 0x1ff,
            whitening_direction: WhiteningDirection::ShiftLeft,
            whitening_invert_output: true,
            whitening_start_bit: 8,
            whitening_end_bit: Some(160),
            whitening_advance_excluded_bits: true,
            ..RadioAnalyzerSettings::default()
        };

        let RadioDataDecoderConfig::Packets(config) = settings.decoder_config().unwrap() else {
            panic!("packet settings should build a packet decoder");
        };
        assert_eq!(
            config.framing.preamble,
            [true, false, true, false, true, false, true, false].repeat(4)
        );
        assert_eq!(
            config.framing.sync.bits,
            [
                true, true, false, true, false, false, true, true, true, false, false, true, false,
                false, false, true
            ]
        );
        assert_eq!(config.framing.sync.max_bit_errors, 1);
        assert_eq!(config.framing.length, RadioPacketLength::UntilEndOfStream);
        assert_eq!(
            config.framing.whitening,
            Some(AdditiveWhiteningConfig {
                width: 9,
                polynomial: 0x11,
                seed: 0x1ff,
                direction: WhiteningDirection::ShiftLeft,
                invert_output: true,
                start_bit: 8,
                end_bit: Some(160),
                advance_excluded_bits: true,
            })
        );

        settings.sync_word_bytes = 0;
        assert_eq!(
            settings.decoder_config().unwrap_err(),
            "packet decoding requires a sync word"
        );
    }

    #[test]
    fn radio_analyzer_allows_zero_preamble_length() {
        let raw = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Raw,
            preamble_bytes: 0,
            sync_word: 0xd391,
            sync_word_bytes: 2,
            ..RadioAnalyzerSettings::default()
        };
        assert!(matches!(
            raw.decoder_config().unwrap(),
            RadioDataDecoderConfig::Raw(RawRadioConfig {
                preamble_bytes: 0,
                sync_word: 0xd391,
                sync_word_bytes: 2,
                ..
            })
        ));

        let packets = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Packets,
            preamble_bytes: 0,
            sync_word: 0xd391,
            sync_word_bytes: 2,
            ..RadioAnalyzerSettings::default()
        };
        let RadioDataDecoderConfig::Packets(config) = packets.decoder_config().unwrap() else {
            panic!("packet settings should build a packet decoder");
        };
        assert!(config.framing.preamble.is_empty());

        let raw_without_sync = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Raw,
            preamble_bytes: 0,
            sync_word_bytes: 0,
            ..RadioAnalyzerSettings::default()
        };
        assert!(matches!(
            raw_without_sync.decoder_config().unwrap(),
            RadioDataDecoderConfig::Raw(RawRadioConfig {
                preamble_bytes: 0,
                sync_word_bytes: 0,
                ..
            })
        ));
    }

    #[test]
    fn pn9_whitening_preset_uses_sx127x_compatible_parameters() {
        let settings = RadioAnalyzerSettings {
            data_mode: RadioDataMode::Packets,
            sync_word: 0xd391,
            sync_word_bytes: 2,
            whitening_enabled: true,
            whitening_preset: RadioWhiteningPreset::Pn9X9X5,
            // Named presets must ignore retained custom values.
            whitening_width: 3,
            whitening_polynomial: 1,
            whitening_seed: 1,
            whitening_direction: WhiteningDirection::ShiftLeft,
            whitening_invert_output: true,
            whitening_start_bit: 8,
            whitening_end_bit: Some(16),
            whitening_advance_excluded_bits: true,
            ..RadioAnalyzerSettings::default()
        };

        let RadioDataDecoderConfig::Packets(config) = settings.decoder_config().unwrap() else {
            panic!("packet settings should build a packet decoder");
        };
        assert_eq!(
            config.framing.whitening,
            Some(AdditiveWhiteningConfig {
                width: 9,
                polynomial: 0x21,
                seed: 0x1ff,
                direction: WhiteningDirection::ShiftRight,
                invert_output: false,
                start_bit: 0,
                end_bit: None,
                advance_excluded_bits: false,
            })
        );
    }

    #[test]
    fn a_sync_word_is_read_from_hexadecimal_text() {
        let parse = |text| SyncWordSetting::parse(text);

        assert_eq!(parse(""), Ok(None));
        assert_eq!(parse("   "), Ok(None));
        assert_eq!(
            parse("91 d3"),
            Ok(Some(SyncWordSetting {
                word: 0x91d3,
                bytes: 2
            }))
        );
        assert_eq!(parse("91d3"), parse("0x91D3"));
        assert_eq!(parse(" 9 1 d 3 "), parse("91d3"));
        // A leading zero byte is part of the word.
        assert_eq!(
            parse("00 2d"),
            Ok(Some(SyncWordSetting {
                word: 0x2d,
                bytes: 2
            }))
        );
        assert_eq!(parse("0123456789abcdef").unwrap().unwrap().bytes, 8);
        assert!(parse("91d").unwrap_err().contains("whole bytes"));
        assert!(parse("91 g3").unwrap_err().contains("hexadecimal"));
        assert!(parse("0123456789abcdef01")
            .unwrap_err()
            .contains("at most 8 bytes"));
    }

    #[test]
    fn a_sync_word_is_shown_as_bytes_in_hexadecimal() {
        let word = SyncWordSetting::parse("0x00 2D d4").unwrap().unwrap();

        assert_eq!(word.text(), "00 2d d4");
        assert_eq!(SyncWordSetting::parse(&word.text()), Ok(Some(word)));
    }

    fn two_channels(own: &str) -> RadioAnalyzerSettings {
        let mut channels = vec![
            RadioAnalyzerChannelSettings::numbered(0, -200_000),
            RadioAnalyzerChannelSettings::numbered(1, 200_000),
        ];
        channels[1].sync_word = own.to_owned();
        RadioAnalyzerSettings {
            demodulation: RadioDemodulation::BinaryGfsk,
            channels,
            sync_word: 0x91d3,
            sync_word_bytes: 2,
            ..RadioAnalyzerSettings::default()
        }
    }

    fn raw_sync_words(configs: Vec<RadioDataDecoderConfig>) -> Vec<(u64, u8)> {
        configs
            .into_iter()
            .map(|config| match config {
                RadioDataDecoderConfig::Raw(raw) => (raw.sync_word, raw.sync_word_bytes),
                RadioDataDecoderConfig::Packets(_) => panic!("raw settings build raw decoders"),
            })
            .collect()
    }

    #[test]
    fn a_channel_without_a_sync_word_uses_the_general_one() {
        let settings = two_channels("");

        assert_eq!(
            raw_sync_words(settings.decoder_configs().unwrap()),
            [(0x91d3, 2), (0x91d3, 2)]
        );
    }

    #[test]
    fn a_channel_with_a_sync_word_uses_its_own_and_the_others_keep_the_general_one() {
        let settings = two_channels("2d d4 aa");

        assert_eq!(
            raw_sync_words(settings.decoder_configs().unwrap()),
            [(0x91d3, 2), (0x2dd4aa, 3)]
        );
    }

    #[test]
    fn a_channel_can_have_a_sync_word_when_there_is_no_general_one() {
        let mut settings = two_channels("2dd4");
        settings.sync_word = 0;
        settings.sync_word_bytes = 0;

        assert_eq!(
            raw_sync_words(settings.decoder_configs().unwrap()),
            [(0, 0), (0x2dd4, 2)]
        );
    }

    #[test]
    fn a_channel_sync_word_reaches_the_packet_decoder_of_that_channel_only() {
        let mut settings = two_channels("2d d4");
        settings.data_mode = RadioDataMode::Packets;

        let sync_bits = settings
            .decoder_configs()
            .unwrap()
            .into_iter()
            .map(|config| match config {
                RadioDataDecoderConfig::Packets(config) => config.framing.sync.bits,
                RadioDataDecoderConfig::Raw(_) => panic!("packet settings build packet decoders"),
            })
            .collect::<Vec<_>>();
        assert_eq!(sync_bits.len(), 2);
        assert_eq!(sync_bits[0].len(), 16);
        assert_ne!(sync_bits[0], sync_bits[1]);
        // 0x2d = 0010_1101, 0xd4 = 1101_0100.
        assert_eq!(
            sync_bits[1],
            [
                false, false, true, false, true, true, false, true, true, true, false, true, false,
                true, false, false
            ]
        );
    }

    #[test]
    fn packet_decoding_asks_for_a_sync_word_of_each_channel_that_has_none() {
        let mut settings = two_channels("2d d4");
        settings.data_mode = RadioDataMode::Packets;
        settings.sync_word_bytes = 0;

        let error = settings.decoder_configs().unwrap_err();

        assert_eq!(error, "Channel 1: packet decoding requires a sync word");
    }

    #[test]
    fn a_bad_channel_sync_word_is_reported_with_the_channel_name() {
        let mut settings = two_channels("2d d");
        settings.channels[1].name = "Downlink".to_owned();

        let error = settings.decoder_configs().unwrap_err();

        assert!(error.starts_with("Downlink: "), "{error}");
        assert!(error.contains("whole bytes"), "{error}");
    }

    #[test]
    fn the_bit_error_limit_stays_below_the_length_of_a_short_channel_sync_word() {
        let mut settings = two_channels("2d");
        settings.maximum_preamble_bit_errors = 12;

        let limits = settings
            .decoder_configs()
            .unwrap()
            .into_iter()
            .map(|config| match config {
                RadioDataDecoderConfig::Raw(raw) => raw.maximum_preamble_bit_errors,
                RadioDataDecoderConfig::Packets(_) => panic!("raw settings build raw decoders"),
            })
            .collect::<Vec<_>>();

        // The general word has 16 bits, the one byte of channel two only 8.
        assert_eq!(limits, [12, 7]);
    }

    #[test]
    fn stepped_radio_domains_choose_the_nearest_supported_value() {
        let domain = ValueDomain::Stepped {
            range: 10..=30,
            step: 8,
        };

        assert_eq!(nearest_domain_value(&domain, 9), 10);
        assert_eq!(nearest_domain_value(&domain, 15), 18);
        assert_eq!(nearest_domain_value(&domain, 23), 26);
        assert_eq!(nearest_domain_value(&domain, 100), 26);
    }
}
