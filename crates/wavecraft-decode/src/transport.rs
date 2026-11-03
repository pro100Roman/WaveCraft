use crate::{ByteStatus, SampleRange, TimedByte};
use std::error::Error;
use std::fmt::{Display, Formatter};

const LOGIC_CHANNEL_COUNT: usize = u32::BITS as usize;

const UART_BAUD_RATES: [u64; 16] = [
    300, 600, 1_200, 2_400, 4_800, 9_600, 19_200, 38_400, 57_600, 115_200, 230_400, 460_800,
    921_600, 1_000_000, 2_000_000, 3_000_000,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportDecodeError {
    message: String,
}

impl TransportDecodeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for TransportDecodeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl Error for TransportDecodeError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct I2cConfig {
    pub clock_channel: usize,
    pub data_channel: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum I2cEvent {
    Start {
        sample: u64,
    },
    Byte {
        byte: TimedByte,
        index: usize,
    },
    Acknowledge {
        range: SampleRange,
        acknowledged: bool,
        byte_index: usize,
        byte_value: u8,
    },
    Stop {
        sample: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiConfig {
    pub clock_channel: usize,
    pub miso_channel: Option<usize>,
    pub mosi_channel: Option<usize>,
    pub chip_select_channel: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiTransfer {
    pub range: SampleRange,
    pub mosi: Option<u8>,
    pub miso: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UartConfig {
    pub data_channel: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UartDecode {
    pub baud_rate: u64,
    pub bytes: Vec<TimedByte>,
}

pub fn decode_i2c_samples(
    config: I2cConfig,
    samples: &[u32],
    sample_offset: u64,
) -> Result<Vec<I2cEvent>, TransportDecodeError> {
    validate_channel(config.clock_channel, "I2C clock")?;
    validate_channel(config.data_channel, "I2C data")?;
    if config.clock_channel == config.data_channel {
        return Err(TransportDecodeError::new(
            "I2C clock and data must use different channels",
        ));
    }
    let Some(&first) = samples.first() else {
        return Ok(Vec::new());
    };
    let mut events = Vec::new();
    let mut previous_clock = channel_is_high(first, config.clock_channel);
    let mut previous_data = channel_is_high(first, config.data_channel);
    let mut active = false;
    let mut byte = 0_u8;
    let mut bit_count = 0_u8;
    let mut byte_start = sample_offset;
    let mut byte_end = sample_offset;
    let mut acknowledge_start = sample_offset;
    let mut byte_index = 0_usize;

    for (local_index, sample) in samples.iter().copied().enumerate().skip(1) {
        let sample_index = sample_offset.saturating_add(local_index as u64);
        let clock = channel_is_high(sample, config.clock_channel);
        let data = channel_is_high(sample, config.data_channel);
        let start = previous_data && !data && clock;
        let stop = !previous_data && data && clock;

        if start {
            events.push(I2cEvent::Start {
                sample: sample_index,
            });
            active = true;
            byte = 0;
            bit_count = 0;
            byte_index = 0;
        } else if stop {
            events.push(I2cEvent::Stop {
                sample: sample_index,
            });
            active = false;
            byte = 0;
            bit_count = 0;
        } else if active && !previous_clock && clock {
            if bit_count == 0 {
                byte_start = sample_index;
            }
            if bit_count < 8 {
                byte = (byte << 1) | u8::from(data);
                bit_count += 1;
                if bit_count == 8 {
                    byte_end = sample_index.saturating_add(1);
                    acknowledge_start = sample_index;
                }
            } else {
                let acknowledged = !data;
                events.push(I2cEvent::Byte {
                    byte: TimedByte {
                        value: byte,
                        range: SampleRange {
                            start: byte_start,
                            end: byte_end,
                        },
                        status: ByteStatus::default(),
                    },
                    index: byte_index,
                });
                events.push(I2cEvent::Acknowledge {
                    range: SampleRange {
                        start: acknowledge_start,
                        end: sample_index.saturating_add(1),
                    },
                    acknowledged,
                    byte_index,
                    byte_value: byte,
                });
                byte = 0;
                bit_count = 0;
                byte_index += 1;
            }
        } else if active && previous_clock && !clock && bit_count == 8 {
            acknowledge_start = sample_index;
        }

        previous_clock = clock;
        previous_data = data;
    }
    Ok(events)
}

pub fn decode_spi_samples(
    config: SpiConfig,
    samples: &[u32],
    sample_offset: u64,
) -> Result<Vec<SpiTransfer>, TransportDecodeError> {
    validate_channel(config.clock_channel, "SPI clock")?;
    validate_optional_channel(config.miso_channel, "SPI MISO")?;
    validate_optional_channel(config.mosi_channel, "SPI MOSI")?;
    validate_optional_channel(config.chip_select_channel, "SPI chip select")?;
    if config.miso_channel.is_none() && config.mosi_channel.is_none() {
        return Err(TransportDecodeError::new(
            "SPI requires a MISO or MOSI channel",
        ));
    }
    validate_distinct_channels(
        config.clock_channel,
        [
            config.miso_channel,
            config.mosi_channel,
            config.chip_select_channel,
        ],
        "SPI",
    )?;
    let Some(&first) = samples.first() else {
        return Ok(Vec::new());
    };
    let mut transfers = Vec::new();
    let mut previous_clock = channel_is_high(first, config.clock_channel);
    let mut miso = 0_u8;
    let mut mosi = 0_u8;
    let mut bit_count = 0_u8;
    let mut byte_start = sample_offset;

    for (local_index, sample) in samples.iter().copied().enumerate().skip(1) {
        let sample_index = sample_offset.saturating_add(local_index as u64);
        let clock = channel_is_high(sample, config.clock_channel);
        let selected = config
            .chip_select_channel
            .map(|channel| !channel_is_high(sample, channel))
            .unwrap_or(true);
        if !selected {
            bit_count = 0;
            miso = 0;
            mosi = 0;
        } else if !previous_clock && clock {
            if bit_count == 0 {
                byte_start = sample_index;
            }
            if let Some(channel) = config.miso_channel {
                miso = (miso << 1) | u8::from(channel_is_high(sample, channel));
            }
            if let Some(channel) = config.mosi_channel {
                mosi = (mosi << 1) | u8::from(channel_is_high(sample, channel));
            }
            bit_count += 1;
            if bit_count == 8 {
                transfers.push(SpiTransfer {
                    range: SampleRange {
                        start: byte_start,
                        end: sample_index.saturating_add(1),
                    },
                    mosi: config.mosi_channel.map(|_| mosi),
                    miso: config.miso_channel.map(|_| miso),
                });
                bit_count = 0;
                miso = 0;
                mosi = 0;
            }
        }
        previous_clock = clock;
    }
    Ok(transfers)
}

pub fn decode_uart_samples(
    config: UartConfig,
    samples: &[u32],
    sample_offset: u64,
    sample_rate_hz: u64,
) -> Result<UartDecode, TransportDecodeError> {
    validate_channel(config.data_channel, "UART data")?;
    if samples.len() < 10 {
        return Ok(UartDecode {
            baud_rate: 0,
            bytes: Vec::new(),
        });
    }
    if sample_rate_hz == 0 {
        return Err(TransportDecodeError::new(
            "UART sample rate must be greater than zero",
        ));
    }
    let falling_edges = samples
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| {
            (channel_is_high(pair[0], config.data_channel)
                && !channel_is_high(pair[1], config.data_channel))
            .then_some(index + 1)
        })
        .collect::<Vec<_>>();
    let transition_runs = samples
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| {
            (channel_is_high(pair[0], config.data_channel)
                != channel_is_high(pair[1], config.data_channel))
            .then_some(index + 1)
        })
        .collect::<Vec<_>>()
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|run| *run > 1)
        .collect::<Vec<_>>();
    let mut best: Option<UartCandidate> = None;
    for baud_rate in UART_BAUD_RATES {
        let samples_per_bit = sample_rate_hz as f64 / baud_rate as f64;
        if samples_per_bit < 2.0 {
            continue;
        }
        let mut frames = Vec::new();
        let mut next_available = 0_usize;
        for &start in &falling_edges {
            if start < next_available {
                continue;
            }
            let sample_at = |bit_position: f64| {
                start.saturating_add((bit_position * samples_per_bit).round() as usize)
            };
            let stop_sample = sample_at(9.5);
            if stop_sample >= samples.len() {
                continue;
            }
            let start_is_low = !channel_is_high(samples[sample_at(0.5)], config.data_channel);
            let stop_is_high = channel_is_high(samples[stop_sample], config.data_channel);
            if !start_is_low || !stop_is_high {
                continue;
            }
            let mut value = 0_u8;
            for bit in 0..8 {
                if channel_is_high(samples[sample_at(1.5 + bit as f64)], config.data_channel) {
                    value |= 1 << bit;
                }
            }
            let end = sample_at(10.0).min(samples.len() - 1);
            frames.push((start, end, value));
            next_available = end;
        }
        if frames.is_empty() {
            continue;
        }
        let timing_error = if transition_runs.is_empty() {
            0.0
        } else {
            transition_runs
                .iter()
                .map(|run| {
                    let bit_count = *run as f64 / samples_per_bit;
                    (bit_count - bit_count.round()).abs()
                })
                .sum::<f64>()
                / transition_runs.len() as f64
        };
        let replace = best.as_ref().map_or(true, |best| {
            timing_error < best.timing_error - 0.02
                || ((timing_error - best.timing_error).abs() <= 0.02
                    && (samples_per_bit > best.samples_per_bit + 0.5
                        || ((samples_per_bit - best.samples_per_bit).abs() <= 0.5
                            && frames.len() > best.frames.len())))
        });
        if replace {
            best = Some(UartCandidate {
                timing_error,
                samples_per_bit,
                baud_rate,
                frames,
            });
        }
    }
    let Some(best) = best else {
        return Err(TransportDecodeError::new(
            "No valid 8-N-1 UART frames found at a standard baud rate",
        ));
    };
    Ok(UartDecode {
        baud_rate: best.baud_rate,
        bytes: best
            .frames
            .into_iter()
            .map(|(start, end, value)| TimedByte {
                value,
                range: SampleRange {
                    start: sample_offset.saturating_add(start as u64),
                    end: sample_offset.saturating_add(end as u64),
                },
                status: ByteStatus::default(),
            })
            .collect(),
    })
}

struct UartCandidate {
    timing_error: f64,
    samples_per_bit: f64,
    baud_rate: u64,
    frames: Vec<(usize, usize, u8)>,
}

fn channel_is_high(sample: u32, channel: usize) -> bool {
    sample & (1_u32 << channel) != 0
}

fn validate_channel(channel: usize, name: &str) -> Result<(), TransportDecodeError> {
    if channel >= LOGIC_CHANNEL_COUNT {
        return Err(TransportDecodeError::new(format!(
            "{name} channel {channel} is outside 0..{LOGIC_CHANNEL_COUNT}"
        )));
    }
    Ok(())
}

fn validate_optional_channel(
    channel: Option<usize>,
    name: &str,
) -> Result<(), TransportDecodeError> {
    if let Some(channel) = channel {
        validate_channel(channel, name)?;
    }
    Ok(())
}

fn validate_distinct_channels<const N: usize>(
    primary: usize,
    others: [Option<usize>; N],
    transport: &str,
) -> Result<(), TransportDecodeError> {
    let mut channels = vec![primary];
    for channel in others.into_iter().flatten() {
        if channels.contains(&channel) {
            return Err(TransportDecodeError::new(format!(
                "{transport} channel {channel} is mapped more than once"
            )));
        }
        channels.push(channel);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logic_word(clock: bool, data: bool, select_high: bool) -> u32 {
        u32::from(clock) | (u32::from(data) << 1) | (u32::from(select_high) << 3)
    }

    fn i2c_transaction(bytes: &[(u8, bool)]) -> Vec<u32> {
        let mut samples = vec![
            logic_word(true, true, false),
            logic_word(true, false, false),
        ];
        for (byte, acknowledged) in bytes {
            for bit in (0..8).rev() {
                let data = byte & (1 << bit) != 0;
                samples.extend([
                    logic_word(false, data, false),
                    logic_word(true, data, false),
                    logic_word(false, data, false),
                ]);
            }
            let acknowledge = !*acknowledged;
            samples.extend([
                logic_word(false, acknowledge, false),
                logic_word(true, acknowledge, false),
                logic_word(false, acknowledge, false),
            ]);
        }
        samples.extend([
            logic_word(true, false, false),
            logic_word(true, true, false),
        ]);
        samples
    }

    fn uart_8n1(bytes: &[u8], samples_per_bit: usize) -> Vec<u32> {
        let mut samples = vec![1_u32; samples_per_bit];
        for byte in bytes {
            samples.extend(std::iter::repeat(0).take(samples_per_bit));
            for bit in 0..8 {
                let value = u32::from(byte & (1 << bit) != 0);
                samples.extend(std::iter::repeat(value).take(samples_per_bit));
            }
            samples.extend(std::iter::repeat(1).take(samples_per_bit));
        }
        samples.extend(std::iter::repeat(1).take(samples_per_bit));
        samples
    }

    #[test]
    fn i2c_preserves_address_data_acknowledgement_and_ranges() {
        let events = decode_i2c_samples(
            I2cConfig {
                clock_channel: 0,
                data_channel: 1,
            },
            &i2c_transaction(&[(0xA0, true), (0x5A, false)]),
            100,
        )
        .unwrap();
        let bytes = events
            .iter()
            .filter_map(|event| match event {
                I2cEvent::Byte { byte, index } => Some((*index, *byte)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(bytes.len(), 2);
        assert_eq!((bytes[0].0, bytes[0].1.value), (0, 0xA0));
        assert_eq!((bytes[1].0, bytes[1].1.value), (1, 0x5A));
        assert!(bytes[0].1.range.start >= 100);
        assert!(bytes[1].1.range.start > bytes[0].1.range.end);
        assert!(events.iter().any(|event| matches!(
            event,
            I2cEvent::Acknowledge {
                acknowledged: false,
                byte_index: 1,
                ..
            }
        )));
    }

    #[test]
    fn spi_preserves_both_full_duplex_bytes() {
        let mut samples = vec![
            logic_word(false, false, true),
            logic_word(false, false, false),
        ];
        for bit in (0..8).rev() {
            let mosi = 0xA5 & (1 << bit) != 0;
            let miso = 0x3C & (1 << bit) != 0;
            let low = u32::from(miso) << 1 | u32::from(mosi) << 2;
            samples.extend([low, low | 1, low]);
        }
        samples.push(logic_word(false, false, true));
        let transfers = decode_spi_samples(
            SpiConfig {
                clock_channel: 0,
                miso_channel: Some(1),
                mosi_channel: Some(2),
                chip_select_channel: Some(3),
            },
            &samples,
            50,
        )
        .unwrap();
        assert_eq!(transfers.len(), 1);
        assert_eq!(transfers[0].mosi, Some(0xA5));
        assert_eq!(transfers[0].miso, Some(0x3C));
        assert!(transfers[0].range.start >= 50);
    }

    #[test]
    fn uart_auto_detects_baud_and_preserves_bytes() {
        let decoded = decode_uart_samples(
            UartConfig { data_channel: 0 },
            &uart_8n1(b"Hi", 10),
            500,
            1_152_000,
        )
        .unwrap();
        assert_eq!(decoded.baud_rate, 115_200);
        assert_eq!(
            decoded
                .bytes
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            b"Hi"
        );
        assert!(decoded.bytes[0].range.start >= 500);
    }

    #[test]
    fn rejects_invalid_or_duplicate_channel_mappings() {
        assert!(decode_i2c_samples(
            I2cConfig {
                clock_channel: 0,
                data_channel: 0,
            },
            &[0],
            0,
        )
        .is_err());
        assert!(decode_spi_samples(
            SpiConfig {
                clock_channel: 0,
                miso_channel: Some(0),
                mosi_channel: None,
                chip_select_channel: None,
            },
            &[0],
            0,
        )
        .is_err());
        assert!(decode_uart_samples(UartConfig { data_channel: 32 }, &[0; 10], 0, 1).is_err());
    }
}
