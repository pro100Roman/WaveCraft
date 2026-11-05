use crate::{DecodeDiagnostic, DecodeError, Endian, RadioPacket, SampleRange, Severity};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CrcParameters {
    /// Supported widths are 8, 16, 24, and 32 bits.
    pub width: u8,
    /// Polynomial without the implicit top bit, in normal (non-reflected) form.
    pub polynomial: u32,
    pub initial: u32,
    pub reflect_input: bool,
    pub reflect_output: bool,
    pub xor_output: u32,
}

impl CrcParameters {
    pub fn validate(&self) -> Result<(), DecodeError> {
        if !matches!(self.width, 8 | 16 | 24 | 32) {
            return Err(DecodeError::new(
                "radio CRC width must be 8, 16, 24, or 32 bits",
            ));
        }
        let mask = crc_mask(self.width);
        if self.polynomial == 0 || self.polynomial & !mask != 0 {
            return Err(DecodeError::new(
                "radio CRC polynomial must be nonzero and fit its width",
            ));
        }
        if self.initial & !mask != 0 || self.xor_output & !mask != 0 {
            return Err(DecodeError::new(
                "radio CRC initial and final XOR values must fit its width",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RadioCrcConfig {
    pub parameters: CrcParameters,
    /// First covered packet byte, inclusive.
    pub data_start: usize,
    /// Last covered packet byte, exclusive.
    pub data_end: usize,
    /// First byte containing the transmitted CRC.
    pub expected_offset: usize,
    pub expected_endian: Endian,
    /// Failed packets remain populated; this flag tells a downstream pipeline
    /// whether its policy requests dropping them.
    pub discard_failed: bool,
}

impl RadioCrcConfig {
    pub fn validate(&self) -> Result<(), DecodeError> {
        self.parameters.validate()?;
        if self.data_start >= self.data_end {
            return Err(DecodeError::new(
                "radio CRC covered byte range must be nonempty",
            ));
        }
        let expected_end = self
            .expected_offset
            .checked_add(self.crc_byte_count())
            .ok_or_else(|| DecodeError::new("radio CRC field range overflows"))?;
        if self.data_start < expected_end && self.expected_offset < self.data_end {
            return Err(DecodeError::new(
                "radio CRC covered bytes must not include the transmitted CRC field",
            ));
        }
        Ok(())
    }

    pub fn crc_byte_count(&self) -> usize {
        usize::from(self.parameters.width / 8)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CrcValidation {
    pub expected: u32,
    pub calculated: u32,
    pub valid: bool,
    pub discard_packet: bool,
}

pub struct RadioCrcValidator {
    config: RadioCrcConfig,
}

impl RadioCrcValidator {
    pub fn new(config: RadioCrcConfig) -> Result<Self, DecodeError> {
        config.validate()?;
        Ok(Self { config })
    }

    pub fn config(&self) -> &RadioCrcConfig {
        &self.config
    }

    /// Validates one packet and attaches a stable mismatch diagnostic.
    ///
    /// The packet is never cleared. `discard_packet` in the result communicates
    /// an explicit downstream policy while retaining failed bytes for RF debug.
    pub fn validate_packet(&self, packet: &mut RadioPacket) -> Result<CrcValidation, DecodeError> {
        if self.config.data_end > packet.bytes.len() {
            return Err(DecodeError::new(
                "radio CRC covered range lies outside the packet",
            ));
        }
        let expected_end = self
            .config
            .expected_offset
            .checked_add(self.config.crc_byte_count())
            .ok_or_else(|| DecodeError::new("radio CRC field range overflows"))?;
        if expected_end > packet.bytes.len() {
            return Err(DecodeError::new("radio CRC field lies outside the packet"));
        }
        let expected = decode_crc_value(
            &packet.bytes[self.config.expected_offset..expected_end],
            self.config.expected_endian,
        );
        let calculated = calculate_crc(
            packet.bytes[self.config.data_start..self.config.data_end]
                .iter()
                .map(|byte| byte.value),
            self.config.parameters,
        );
        let valid = expected == calculated;
        if !valid {
            let first = packet.bytes[self.config.expected_offset].range;
            let last = packet.bytes[expected_end - 1].range;
            packet.diagnostics.push(DecodeDiagnostic {
                code: "radio.crc.mismatch".to_owned(),
                message: format!(
                    "radio CRC mismatch: expected 0x{expected:X}, calculated 0x{calculated:X}"
                ),
                severity: Severity::Error,
                range: Some(SampleRange {
                    start: first.start,
                    end: last.end,
                }),
            });
        }
        Ok(CrcValidation {
            expected,
            calculated,
            valid,
            discard_packet: !valid && self.config.discard_failed,
        })
    }
}

pub fn calculate_crc(bytes: impl IntoIterator<Item = u8>, parameters: CrcParameters) -> u32 {
    let mask = crc_mask(parameters.width);
    let mut crc = parameters.initial & mask;
    if parameters.reflect_input {
        let reflected_polynomial = reflect_width(parameters.polynomial, parameters.width);
        for byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ reflected_polynomial
                } else {
                    crc >> 1
                };
            }
        }
    } else {
        let top_bit = 1_u32 << (parameters.width - 1);
        for byte in bytes {
            crc ^= u32::from(byte) << (parameters.width - 8);
            for _ in 0..8 {
                crc = if crc & top_bit != 0 {
                    (crc << 1) ^ parameters.polynomial
                } else {
                    crc << 1
                };
                crc &= mask;
            }
        }
    }
    if parameters.reflect_output != parameters.reflect_input {
        crc = reflect_width(crc, parameters.width);
    }
    (crc ^ parameters.xor_output) & mask
}

fn crc_mask(width: u8) -> u32 {
    if width == 32 {
        u32::MAX
    } else {
        (1_u32 << width) - 1
    }
}

fn reflect_width(value: u32, width: u8) -> u32 {
    value.reverse_bits() >> (32 - width)
}

fn decode_crc_value(bytes: &[crate::TimedByte], endian: Endian) -> u32 {
    match endian {
        Endian::Little => bytes
            .iter()
            .rev()
            .fold(0_u32, |value, byte| (value << 8) | u32::from(byte.value)),
        Endian::Big => bytes
            .iter()
            .fold(0_u32, |value, byte| (value << 8) | u32::from(byte.value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ByteStatus, RadioPacket, TimedByte};

    fn packet(values: &[u8]) -> RadioPacket {
        RadioPacket {
            preamble: Vec::new(),
            bytes: values
                .iter()
                .enumerate()
                .map(|(index, value)| TimedByte {
                    value: *value,
                    range: SampleRange {
                        start: index as u64 * 8,
                        end: index as u64 * 8 + 8,
                    },
                    status: ByteStatus::default(),
                })
                .collect(),
            range: SampleRange {
                start: 0,
                end: values.len() as u64 * 8,
            },
            confidence: 1.0,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn calculates_standard_crc_check_values() {
        assert_eq!(
            calculate_crc(
                *b"123456789",
                CrcParameters {
                    width: 8,
                    polynomial: 0x07,
                    initial: 0,
                    reflect_input: false,
                    reflect_output: false,
                    xor_output: 0,
                }
            ),
            0xF4
        );
        assert_eq!(
            calculate_crc(
                *b"123456789",
                CrcParameters {
                    width: 16,
                    polynomial: 0x1021,
                    initial: 0xFFFF,
                    reflect_input: false,
                    reflect_output: false,
                    xor_output: 0,
                }
            ),
            0x29B1
        );
        assert_eq!(
            calculate_crc(
                *b"123456789",
                CrcParameters {
                    width: 32,
                    polynomial: 0x04C11DB7,
                    initial: 0xFFFF_FFFF,
                    reflect_input: true,
                    reflect_output: true,
                    xor_output: 0xFFFF_FFFF,
                }
            ),
            0xCBF4_3926
        );
    }

    #[test]
    fn validates_crc_and_preserves_failed_packet_with_diagnostic() {
        let validator = RadioCrcValidator::new(RadioCrcConfig {
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
            discard_failed: true,
        })
        .unwrap();
        let calculated = calculate_crc([0x10, 0x20, 0x30], validator.config().parameters) as u8;
        let mut valid_packet = packet(&[0x10, 0x20, 0x30, calculated]);
        let mut failed_packet = packet(&[0x10, 0x20, 0x30, calculated ^ 1]);

        let valid = validator.validate_packet(&mut valid_packet).unwrap();
        let failed = validator.validate_packet(&mut failed_packet).unwrap();

        assert!(valid.valid);
        assert!(!valid.discard_packet);
        assert!(valid_packet.diagnostics.is_empty());
        assert!(!failed.valid);
        assert!(failed.discard_packet);
        assert_eq!(failed_packet.bytes.len(), 4);
        assert_eq!(failed_packet.diagnostics[0].code, "radio.crc.mismatch");
        assert_eq!(
            failed_packet.diagnostics[0].range,
            Some(SampleRange { start: 24, end: 32 })
        );
    }

    #[test]
    fn rejects_overlapping_data_and_crc_ranges() {
        assert!(RadioCrcValidator::new(RadioCrcConfig {
            parameters: CrcParameters {
                width: 16,
                polynomial: 0x1021,
                initial: 0,
                reflect_input: false,
                reflect_output: false,
                xor_output: 0,
            },
            data_start: 0,
            data_end: 4,
            expected_offset: 3,
            expected_endian: Endian::Big,
            discard_failed: false,
        })
        .is_err());
    }
}
