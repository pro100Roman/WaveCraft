use crate::{
    BitTransformConfig, BitTransformer, DecodeDiagnostic, DecodeError, RadioCrcConfig,
    RadioCrcValidator, RadioPacket, RadioPacketFramer, RadioPacketFramerConfig,
    RadioPacketFramingOutput, SampleRange, Severity, TimedBitBatch,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RadioPacketPipelineConfig {
    pub bit_transform: BitTransformConfig,
    pub framing: RadioPacketFramerConfig,
    pub crc: Option<RadioCrcConfig>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RadioPacketPipelineOutput {
    pub packets: Vec<RadioPacket>,
    pub diagnostics: Vec<DecodeDiagnostic>,
}

/// Timed binary decisions → line decoding → packet framing → optional CRC.
pub struct RadioPacketPipeline {
    config: RadioPacketPipelineConfig,
    bit_transformer: BitTransformer,
    framer: RadioPacketFramer,
    crc: Option<RadioCrcValidator>,
}

impl RadioPacketPipeline {
    pub fn new(config: RadioPacketPipelineConfig) -> Result<Self, DecodeError> {
        let bit_transformer = BitTransformer::new(config.bit_transform);
        let framer = RadioPacketFramer::new(config.framing.clone())?;
        let crc = config.crc.clone().map(RadioCrcValidator::new).transpose()?;
        Ok(Self {
            config,
            bit_transformer,
            framer,
            crc,
        })
    }

    pub fn config(&self) -> &RadioPacketPipelineConfig {
        &self.config
    }

    pub fn reset(&mut self) {
        self.bit_transformer.reset();
        self.framer.reset();
    }

    pub fn push(
        &mut self,
        input: &TimedBitBatch,
    ) -> Result<RadioPacketPipelineOutput, DecodeError> {
        let transformed = self.bit_transformer.push(input)?;
        let framed = self.framer.push(&TimedBitBatch {
            discontinuity: input.discontinuity,
            bits: transformed.bits,
        })?;
        let mut output = RadioPacketPipelineOutput {
            packets: Vec::new(),
            diagnostics: transformed.diagnostics,
        };
        self.validate_packets(framed, &mut output);
        Ok(output)
    }

    pub fn finish(&mut self) -> RadioPacketPipelineOutput {
        let transformed = self.bit_transformer.finish();
        let framed = self.framer.finish();
        let mut output = RadioPacketPipelineOutput {
            packets: Vec::new(),
            diagnostics: transformed.diagnostics,
        };
        self.validate_packets(framed, &mut output);
        output
    }

    fn validate_packets(
        &self,
        framed: RadioPacketFramingOutput,
        output: &mut RadioPacketPipelineOutput,
    ) {
        output.diagnostics.extend(framed.diagnostics);
        for mut packet in framed.packets {
            let discard = if let Some(crc) = &self.crc {
                match crc.validate_packet(&mut packet) {
                    Ok(validation) => validation.discard_packet,
                    Err(error) => {
                        packet.diagnostics.push(pipeline_diagnostic(
                            "radio.crc.invalid_packet_range",
                            error.to_string(),
                            Some(packet.range),
                            Severity::Error,
                        ));
                        false
                    }
                }
            } else {
                false
            };
            if discard {
                output.diagnostics.extend(packet.diagnostics);
            } else {
                output.packets.push(packet);
            }
        }
    }
}

fn pipeline_diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    range: Option<SampleRange>,
    severity: Severity,
) -> DecodeDiagnostic {
    DecodeDiagnostic {
        code: code.into(),
        message: message.into(),
        severity,
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        calculate_crc, AdditiveWhitening, AdditiveWhiteningConfig, BitOrder, BitPolarity,
        CrcParameters, Endian, LineCode, RadioPacketLength, SyncWordConfig, TimedBit,
        WhiteningDirection,
    };

    fn byte_bits(value: u8) -> impl Iterator<Item = bool> {
        (0..8).map(move |shift| value & (1 << (7 - shift)) != 0)
    }

    fn timed_bits(values: &[bool], first_sample: u64, discontinuity: bool) -> TimedBitBatch {
        TimedBitBatch {
            discontinuity,
            bits: values
                .iter()
                .enumerate()
                .map(|(index, value)| TimedBit {
                    value: *value,
                    confidence: 0.9,
                    range: SampleRange {
                        start: first_sample + index as u64,
                        end: first_sample + index as u64 + 1,
                    },
                })
                .collect(),
        }
    }

    fn overlapping_timed_bits(
        values: &[bool],
        first_bit: usize,
        discontinuity: bool,
    ) -> TimedBitBatch {
        TimedBitBatch {
            discontinuity,
            bits: values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let start = (first_bit + index) as u64 * 2;
                    TimedBit {
                        value: *value,
                        confidence: 0.9,
                        range: SampleRange {
                            start,
                            end: start + 20,
                        },
                    }
                })
                .collect(),
        }
    }

    fn config(discard_failed: bool) -> RadioPacketPipelineConfig {
        RadioPacketPipelineConfig {
            bit_transform: BitTransformConfig {
                polarity: BitPolarity::Normal,
                line_code: LineCode::Nrz,
            },
            framing: RadioPacketFramerConfig {
                preamble: Vec::new(),
                sync: SyncWordConfig {
                    bits: vec![true, false, true, false, true, false, true, false],
                    max_bit_errors: 0,
                },
                whitening: None,
                bit_order: BitOrder::MostSignificantFirst,
                length: RadioPacketLength::Fixed { packet_bytes: 4 },
                max_search_bits: 64,
            },
            crc: Some(RadioCrcConfig {
                parameters: CrcParameters {
                    width: 8,
                    polynomial: 0x07,
                    initial: 0,
                    reflect_input: false,
                    reflect_output: false,
                    xor_output: 0,
                },
                data_start: 0,
                data_end: 3,
                expected_offset: 3,
                expected_endian: Endian::Big,
                discard_failed,
            }),
        }
    }

    fn encoded_packet(valid_crc: bool) -> Vec<bool> {
        let data = [0x10, 0x20, 0x30];
        let crc = calculate_crc(data, config(false).crc.unwrap().parameters) as u8;
        let mut bits = config(false).framing.sync.bits;
        for byte in data
            .into_iter()
            .chain([if valid_crc { crc } else { crc ^ 1 }])
        {
            bits.extend(byte_bits(byte));
        }
        bits
    }

    fn whitening_config() -> AdditiveWhiteningConfig {
        AdditiveWhiteningConfig {
            width: 7,
            polynomial: 0b100_0001,
            seed: 0b101_1011,
            direction: WhiteningDirection::ShiftRight,
            invert_output: false,
            start_bit: 0,
            end_bit: None,
            advance_excluded_bits: false,
        }
    }

    fn pn9_whitening_config() -> AdditiveWhiteningConfig {
        AdditiveWhiteningConfig {
            width: 9,
            polynomial: 0x21,
            seed: 0x1ff,
            direction: WhiteningDirection::ShiftRight,
            invert_output: false,
            start_bit: 0,
            end_bit: None,
            advance_excluded_bits: false,
        }
    }

    fn whiten_after_sync(values: &[bool], sync_bits: usize) -> Vec<bool> {
        whiten_after_sync_with(values, sync_bits, whitening_config())
    }

    fn whiten_after_sync_with(
        values: &[bool],
        sync_bits: usize,
        config: AdditiveWhiteningConfig,
    ) -> Vec<bool> {
        let mut result = values[..sync_bits].to_vec();
        let mut whitening = AdditiveWhitening::new(config).unwrap();
        result.extend(
            values[sync_bits..]
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    whitening
                        .transform(TimedBit {
                            value: *value,
                            confidence: 1.0,
                            range: SampleRange {
                                start: index as u64,
                                end: index as u64 + 1,
                            },
                        })
                        .value
                }),
        );
        result
    }

    #[test]
    fn complete_pipeline_is_independent_of_input_chunks() {
        let bits = encoded_packet(true);
        let mut whole = RadioPacketPipeline::new(config(false)).unwrap();
        let expected = whole.push(&timed_bits(&bits, 0, false)).unwrap();

        let mut chunked = RadioPacketPipeline::new(config(false)).unwrap();
        let first = chunked.push(&timed_bits(&bits[..13], 0, false)).unwrap();
        let second = chunked.push(&timed_bits(&bits[13..], 13, false)).unwrap();

        assert!(first.packets.is_empty());
        assert_eq!(second.packets, expected.packets);
        assert_eq!(
            expected.packets[0]
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0x10, 0x20, 0x30, 0x9C]
        );
        assert!(expected.packets[0].diagnostics.is_empty());
    }

    #[test]
    fn pn9_pipeline_accepts_overlapping_filtered_ranges_across_chunks() {
        let mut config = config(false);
        config.crc = None;
        config.framing.length = RadioPacketLength::Fixed { packet_bytes: 2 };
        config.framing.whitening = Some(pn9_whitening_config());
        config.framing.preamble = byte_bits(0xAA).collect();
        let acquisition_bits = config.framing.preamble.len() + config.framing.sync.bits.len();
        let mut clear = config.framing.preamble.clone();
        clear.extend(config.framing.sync.bits.clone());
        clear.extend(byte_bits(0xA5));
        clear.extend(byte_bits(0x3C));
        let encoded = whiten_after_sync_with(&clear, acquisition_bits, pn9_whitening_config());
        let split = 13;
        let mut pipeline = RadioPacketPipeline::new(config).unwrap();

        let first = pipeline
            .push(&overlapping_timed_bits(&encoded[..split], 0, false))
            .unwrap();
        let second = pipeline
            .push(&overlapping_timed_bits(&encoded[split..], split, false))
            .unwrap();

        assert!(first.packets.is_empty());
        assert_eq!(second.packets[0].preamble[0].value, 0xAA);
        assert_eq!(
            second.packets[0]
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xA5, 0x3C]
        );
    }

    #[test]
    fn crc_policy_can_retain_or_discard_a_failed_packet() {
        let bits = encoded_packet(false);
        let mut retaining = RadioPacketPipeline::new(config(false)).unwrap();
        let retained = retaining.push(&timed_bits(&bits, 0, false)).unwrap();
        let mut discarding = RadioPacketPipeline::new(config(true)).unwrap();
        let discarded = discarding.push(&timed_bits(&bits, 0, false)).unwrap();

        assert_eq!(retained.packets.len(), 1);
        assert_eq!(
            retained.packets[0].diagnostics[0].code,
            "radio.crc.mismatch"
        );
        assert!(discarded.packets.is_empty());
        assert_eq!(discarded.diagnostics[0].code, "radio.crc.mismatch");
    }

    #[test]
    fn dewhitening_happens_after_sync_and_before_crc_validation() {
        let mut pipeline_config = config(false);
        pipeline_config.framing.whitening = Some(whitening_config());
        let sync_bits = pipeline_config.framing.sync.bits.len();
        let encoded = whiten_after_sync(&encoded_packet(true), sync_bits);
        let mut pipeline = RadioPacketPipeline::new(pipeline_config).unwrap();

        let first = pipeline
            .push(&timed_bits(&encoded[..19], 0, false))
            .unwrap();
        let second = pipeline
            .push(&timed_bits(&encoded[19..], 19, false))
            .unwrap();

        assert!(first.packets.is_empty());
        assert_eq!(
            second.packets[0]
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0x10, 0x20, 0x30, 0x9C]
        );
        assert!(second.packets[0].diagnostics.is_empty());
    }
}
