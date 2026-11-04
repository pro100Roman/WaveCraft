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
        calculate_crc, BitOrder, BitPolarity, CrcParameters, Endian, LineCode, RadioPacketLength,
        SyncWordConfig, TimedBit,
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

    fn config(discard_failed: bool) -> RadioPacketPipelineConfig {
        RadioPacketPipelineConfig {
            bit_transform: BitTransformConfig {
                polarity: BitPolarity::Normal,
                line_code: LineCode::Nrz,
            },
            framing: RadioPacketFramerConfig {
                sync: SyncWordConfig {
                    bits: vec![true, false, true, false, true, false, true, false],
                    max_bit_errors: 0,
                },
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
}
