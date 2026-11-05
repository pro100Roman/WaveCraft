use crate::{DecodeError, TimedBit};

/// Direction in which the additive-whitening LFSR advances.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WhiteningDirection {
    /// Emit the least-significant state bit, shift right, and insert feedback
    /// at the most-significant bit.
    ShiftRight,
    /// Emit the most-significant state bit, shift left, and insert feedback
    /// at the least-significant bit.
    ShiftLeft,
}

/// Configuration for an additive (PN XOR) whitening sequence.
///
/// `polynomial` is a tap mask over the current `width`-bit LFSR state. The
/// parity of the tapped state bits becomes the newly inserted feedback bit.
/// The emitted whitening bit is read before the register advances.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdditiveWhiteningConfig {
    pub width: u8,
    pub polynomial: u64,
    pub seed: u64,
    pub direction: WhiteningDirection,
    /// Invert each emitted PN bit before XORing it with an on-air bit.
    pub invert_output: bool,
    /// First packet bit covered by whitening, relative to the end of sync.
    pub start_bit: usize,
    /// Exclusive end of the covered range. `None` covers the rest of packet.
    pub end_bit: Option<usize>,
    /// Advance the LFSR over packet bits outside the covered range.
    pub advance_excluded_bits: bool,
}

impl AdditiveWhiteningConfig {
    pub fn validate(self) -> Result<(), DecodeError> {
        if !(1..=64).contains(&self.width) {
            return Err(DecodeError::new(
                "radio whitening LFSR width must be between 1 and 64 bits",
            ));
        }
        let mask = width_mask(self.width);
        if self.polynomial == 0 || self.polynomial & !mask != 0 {
            return Err(DecodeError::new(
                "radio whitening polynomial must be nonzero and fit the LFSR width",
            ));
        }
        if self.seed == 0 || self.seed & !mask != 0 {
            return Err(DecodeError::new(
                "radio whitening seed must be nonzero and fit the LFSR width",
            ));
        }
        if self.end_bit.is_some_and(|end| end <= self.start_bit) {
            return Err(DecodeError::new(
                "radio whitening end bit must be greater than its start bit",
            ));
        }
        Ok(())
    }
}

/// Stateful additive whitening transform.
///
/// XOR whitening is symmetric, so the same transform can produce test vectors
/// on transmit and dewhiten received bits. Timing and confidence are retained.
#[derive(Clone, Debug)]
pub struct AdditiveWhitening {
    config: AdditiveWhiteningConfig,
    state: u64,
    bit_index: usize,
}

impl AdditiveWhitening {
    pub fn new(config: AdditiveWhiteningConfig) -> Result<Self, DecodeError> {
        config.validate()?;
        Ok(Self {
            state: config.seed,
            bit_index: 0,
            config,
        })
    }

    pub fn config(&self) -> AdditiveWhiteningConfig {
        self.config
    }

    /// Restart the PN sequence and packet-relative coverage at the configured
    /// seed. Packet framing calls this immediately after accepting sync.
    pub fn reset(&mut self) {
        self.state = self.config.seed;
        self.bit_index = 0;
    }

    pub fn transform(&mut self, mut bit: TimedBit) -> TimedBit {
        let covered = self.bit_index >= self.config.start_bit
            && self.config.end_bit.map_or(true, |end| self.bit_index < end);
        let whitening_bit = self.output_bit() ^ self.config.invert_output;
        if covered {
            bit.value ^= whitening_bit;
        }
        if covered || self.config.advance_excluded_bits {
            self.advance();
        }
        self.bit_index = self.bit_index.saturating_add(1);
        bit
    }

    fn output_bit(&self) -> bool {
        match self.config.direction {
            WhiteningDirection::ShiftRight => self.state & 1 != 0,
            WhiteningDirection::ShiftLeft => self.state & (1_u64 << (self.config.width - 1)) != 0,
        }
    }

    fn advance(&mut self) {
        let feedback = (self.state & self.config.polynomial).count_ones() & 1 != 0;
        match self.config.direction {
            WhiteningDirection::ShiftRight => {
                self.state = (self.state >> 1) | (u64::from(feedback) << (self.config.width - 1));
            }
            WhiteningDirection::ShiftLeft => {
                self.state =
                    ((self.state << 1) & width_mask(self.config.width)) | u64::from(feedback);
            }
        }
    }
}

fn width_mask(width: u8) -> u64 {
    if width == 64 {
        u64::MAX
    } else {
        (1_u64 << width) - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SampleRange;

    fn config() -> AdditiveWhiteningConfig {
        AdditiveWhiteningConfig {
            width: 4,
            polynomial: 0b1001,
            seed: 0b0001,
            direction: WhiteningDirection::ShiftRight,
            invert_output: false,
            start_bit: 0,
            end_bit: None,
            advance_excluded_bits: false,
        }
    }

    fn bit(value: bool, index: u64) -> TimedBit {
        TimedBit {
            value,
            confidence: 0.75,
            range: SampleRange {
                start: index * 2,
                end: index * 2 + 2,
            },
        }
    }

    #[test]
    fn xor_sequence_is_deterministic_and_preserves_metadata() {
        let mut whitening = AdditiveWhitening::new(config()).unwrap();
        let output = (0..8)
            .map(|index| whitening.transform(bit(false, index)))
            .collect::<Vec<_>>();

        assert_eq!(
            output.iter().map(|bit| bit.value).collect::<Vec<_>>(),
            [true, false, false, false, true, true, true, true]
        );
        assert_eq!(output[3].confidence, 0.75);
        assert_eq!(output[3].range, SampleRange { start: 6, end: 8 });
    }

    #[test]
    fn reset_repeats_the_sequence() {
        let mut whitening = AdditiveWhitening::new(config()).unwrap();
        let first = whitening.transform(bit(false, 0)).value;
        whitening.transform(bit(false, 1));
        whitening.reset();

        assert_eq!(whitening.transform(bit(false, 2)).value, first);
    }

    #[test]
    fn coverage_can_skip_a_clear_header_without_advancing() {
        let mut scoped = config();
        scoped.start_bit = 2;
        scoped.end_bit = Some(4);
        let mut whitening = AdditiveWhitening::new(scoped).unwrap();
        let output = (0..5)
            .map(|index| whitening.transform(bit(false, index)).value)
            .collect::<Vec<_>>();

        assert_eq!(output, [false, false, true, false, false]);
    }

    #[test]
    fn rejects_invalid_lfsr_and_scope_configuration() {
        for invalid in [
            AdditiveWhiteningConfig {
                width: 0,
                ..config()
            },
            AdditiveWhiteningConfig {
                polynomial: 0,
                ..config()
            },
            AdditiveWhiteningConfig {
                seed: 0,
                ..config()
            },
            AdditiveWhiteningConfig {
                start_bit: 4,
                end_bit: Some(4),
                ..config()
            },
        ] {
            assert!(AdditiveWhitening::new(invalid).is_err());
        }
    }
}
