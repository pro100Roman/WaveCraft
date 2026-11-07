use crate::{ByteStatus, DecodeDiagnostic, PacketCompleteness, SampleRange, Severity, TimedByte};
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

#[derive(Clone, Debug)]
pub struct I2cDecoder {
    config: I2cConfig,
    previous_sample: Option<u32>,
    next_sample_offset: Option<u64>,
    active: bool,
    transaction_start: Option<u64>,
    byte: u8,
    bit_count: u8,
    byte_start: u64,
    byte_end: u64,
    acknowledge_start: u64,
    byte_index: usize,
}

impl I2cDecoder {
    pub fn new(config: I2cConfig) -> Result<Self, TransportDecodeError> {
        validate_channel(config.clock_channel, "I2C clock")?;
        validate_channel(config.data_channel, "I2C data")?;
        if config.clock_channel == config.data_channel {
            return Err(TransportDecodeError::new(
                "I2C clock and data must use different channels",
            ));
        }
        Ok(Self {
            config,
            previous_sample: None,
            next_sample_offset: None,
            active: false,
            transaction_start: None,
            byte: 0,
            bit_count: 0,
            byte_start: 0,
            byte_end: 0,
            acknowledge_start: 0,
            byte_index: 0,
        })
    }

    pub fn push_samples(
        &mut self,
        samples: &[u32],
        sample_offset: u64,
    ) -> Result<Vec<I2cEvent>, TransportDecodeError> {
        validate_contiguous_chunk(self.next_sample_offset, sample_offset, samples.len())?;
        let mut events = Vec::new();
        for (local_index, sample) in samples.iter().copied().enumerate() {
            let sample_index = sample_offset + local_index as u64;
            let Some(previous_sample) = self.previous_sample else {
                self.previous_sample = Some(sample);
                continue;
            };
            let previous_clock = channel_is_high(previous_sample, self.config.clock_channel);
            let previous_data = channel_is_high(previous_sample, self.config.data_channel);
            let clock = channel_is_high(sample, self.config.clock_channel);
            let data = channel_is_high(sample, self.config.data_channel);
            let start = previous_data && !data && clock;
            let stop = !previous_data && data && clock;

            if start {
                events.push(I2cEvent::Start {
                    sample: sample_index,
                });
                self.active = true;
                self.transaction_start = Some(sample_index);
                self.byte = 0;
                self.bit_count = 0;
                self.byte_index = 0;
            } else if stop {
                events.push(I2cEvent::Stop {
                    sample: sample_index,
                });
                self.active = false;
                self.transaction_start = None;
                self.byte = 0;
                self.bit_count = 0;
            } else if self.active && !previous_clock && clock {
                if self.bit_count == 0 {
                    self.byte_start = sample_index;
                }
                if self.bit_count < 8 {
                    self.byte = (self.byte << 1) | u8::from(data);
                    self.bit_count += 1;
                    if self.bit_count == 8 {
                        self.byte_end = sample_index.saturating_add(1);
                        self.acknowledge_start = sample_index;
                    }
                } else {
                    let acknowledged = !data;
                    events.push(I2cEvent::Byte {
                        byte: TimedByte {
                            value: self.byte,
                            range: SampleRange {
                                start: self.byte_start,
                                end: self.byte_end,
                            },
                            status: ByteStatus::default(),
                        },
                        index: self.byte_index,
                    });
                    events.push(I2cEvent::Acknowledge {
                        range: SampleRange {
                            start: self.acknowledge_start,
                            end: sample_index.saturating_add(1),
                        },
                        acknowledged,
                        byte_index: self.byte_index,
                        byte_value: self.byte,
                    });
                    self.byte = 0;
                    self.bit_count = 0;
                    self.byte_index += 1;
                }
            } else if self.active && previous_clock && !clock && self.bit_count == 8 {
                self.acknowledge_start = sample_index;
            }

            self.previous_sample = Some(sample);
        }
        self.next_sample_offset = chunk_end(sample_offset, samples.len())?;
        Ok(events)
    }

    pub fn finish(&mut self) -> Vec<DecodeDiagnostic> {
        let diagnostics = if self.active {
            vec![incomplete_transport_diagnostic(
                "transport.i2c.incomplete",
                "I2C transaction ended before STOP",
                self.transaction_start,
                self.next_sample_offset,
            )]
        } else {
            Vec::new()
        };
        self.reset();
        diagnostics
    }

    pub fn reset(&mut self) {
        self.previous_sample = None;
        self.next_sample_offset = None;
        self.active = false;
        self.transaction_start = None;
        self.byte = 0;
        self.bit_count = 0;
        self.byte_index = 0;
    }
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

#[derive(Clone, Debug)]
pub struct SpiDecoder {
    config: SpiConfig,
    previous_sample: Option<u32>,
    next_sample_offset: Option<u64>,
    previous_selected: bool,
    transaction_index: usize,
    next_transaction_index: usize,
    transaction_start: Option<u64>,
    completed_transactions: Vec<SpiTransactionEnd>,
    miso: u8,
    mosi: u8,
    bit_count: u8,
    byte_start: u64,
}

impl SpiDecoder {
    pub fn new(config: SpiConfig) -> Result<Self, TransportDecodeError> {
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
        Ok(Self {
            config,
            previous_sample: None,
            next_sample_offset: None,
            previous_selected: false,
            transaction_index: 0,
            next_transaction_index: 0,
            transaction_start: None,
            completed_transactions: Vec::new(),
            miso: 0,
            mosi: 0,
            bit_count: 0,
            byte_start: 0,
        })
    }

    pub fn push_samples(
        &mut self,
        samples: &[u32],
        sample_offset: u64,
    ) -> Result<Vec<SpiTransfer>, TransportDecodeError> {
        validate_contiguous_chunk(self.next_sample_offset, sample_offset, samples.len())?;
        let mut transfers = Vec::new();
        for (local_index, sample) in samples.iter().copied().enumerate() {
            let sample_index = sample_offset + local_index as u64;
            let selected = self
                .config
                .chip_select_channel
                .map(|channel| !channel_is_high(sample, channel))
                .unwrap_or(true);
            let Some(previous_sample) = self.previous_sample else {
                self.previous_selected = selected;
                self.next_transaction_index = usize::from(selected);
                self.transaction_start = selected.then_some(sample_index);
                self.previous_sample = Some(sample);
                continue;
            };
            let previous_clock = channel_is_high(previous_sample, self.config.clock_channel);
            let clock = channel_is_high(sample, self.config.clock_channel);
            if !selected && self.previous_selected {
                self.completed_transactions.push(SpiTransactionEnd {
                    transaction_index: self.transaction_index,
                    completeness: if self.bit_count == 0 {
                        PacketCompleteness::Complete
                    } else {
                        PacketCompleteness::Incomplete
                    },
                });
            }
            if selected && !self.previous_selected {
                self.transaction_index = self.next_transaction_index;
                self.next_transaction_index = self.next_transaction_index.saturating_add(1);
                self.transaction_start = Some(sample_index);
            }
            if !selected {
                self.bit_count = 0;
                self.miso = 0;
                self.mosi = 0;
                self.transaction_start = None;
            } else if !previous_clock && clock {
                if self.bit_count == 0 {
                    self.byte_start = sample_index;
                }
                if let Some(channel) = self.config.miso_channel {
                    self.miso = (self.miso << 1) | u8::from(channel_is_high(sample, channel));
                }
                if let Some(channel) = self.config.mosi_channel {
                    self.mosi = (self.mosi << 1) | u8::from(channel_is_high(sample, channel));
                }
                self.bit_count += 1;
                if self.bit_count == 8 {
                    transfers.push(SpiTransfer {
                        transaction_index: self.transaction_index,
                        range: SampleRange {
                            start: self.byte_start,
                            end: sample_index.saturating_add(1),
                        },
                        mosi: self.config.mosi_channel.map(|_| self.mosi),
                        miso: self.config.miso_channel.map(|_| self.miso),
                    });
                    self.bit_count = 0;
                    self.miso = 0;
                    self.mosi = 0;
                }
            }
            self.previous_sample = Some(sample);
            self.previous_selected = selected;
        }
        self.next_sample_offset = chunk_end(sample_offset, samples.len())?;
        Ok(transfers)
    }

    pub fn take_completed_transactions(&mut self) -> Vec<SpiTransactionEnd> {
        std::mem::take(&mut self.completed_transactions)
    }

    pub fn finish(&mut self) -> Vec<DecodeDiagnostic> {
        let mut diagnostics = Vec::new();
        if self.bit_count > 0 {
            diagnostics.push(incomplete_transport_diagnostic(
                "transport.spi.incomplete_byte",
                "SPI stream ended in the middle of a byte",
                Some(self.byte_start),
                self.next_sample_offset,
            ));
        } else if self.config.chip_select_channel.is_some() && self.previous_selected {
            diagnostics.push(incomplete_transport_diagnostic(
                "transport.spi.incomplete_transaction",
                "SPI transaction ended while chip select was still asserted",
                self.transaction_start,
                self.next_sample_offset,
            ));
        }
        self.reset();
        diagnostics
    }

    pub fn reset(&mut self) {
        self.previous_sample = None;
        self.next_sample_offset = None;
        self.previous_selected = false;
        self.transaction_index = 0;
        self.next_transaction_index = 0;
        self.transaction_start = None;
        self.completed_transactions.clear();
        self.miso = 0;
        self.mosi = 0;
        self.bit_count = 0;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiTransfer {
    pub transaction_index: usize,
    pub range: SampleRange,
    pub mosi: Option<u8>,
    pub miso: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiTransactionEnd {
    pub transaction_index: usize,
    pub completeness: PacketCompleteness,
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

const UART_PROBE_SAMPLE_LIMIT: usize = 1_000_000;

#[derive(Clone, Debug)]
pub struct UartDecoder {
    config: UartConfig,
    sample_rate_hz: u64,
    next_sample_offset: Option<u64>,
    probe_offset: Option<u64>,
    probe: Vec<u32>,
    fixed: Option<FixedUartDecoder>,
}

impl UartDecoder {
    pub fn new(config: UartConfig, sample_rate_hz: u64) -> Result<Self, TransportDecodeError> {
        validate_channel(config.data_channel, "UART data")?;
        if sample_rate_hz == 0 {
            return Err(TransportDecodeError::new(
                "UART sample rate must be greater than zero",
            ));
        }
        Ok(Self {
            config,
            sample_rate_hz,
            next_sample_offset: None,
            probe_offset: None,
            probe: Vec::new(),
            fixed: None,
        })
    }

    pub fn baud_rate(&self) -> Option<u64> {
        self.fixed.as_ref().map(|decoder| decoder.baud_rate)
    }

    pub fn push_samples(
        &mut self,
        samples: &[u32],
        sample_offset: u64,
    ) -> Result<UartDecode, TransportDecodeError> {
        validate_contiguous_chunk(self.next_sample_offset, sample_offset, samples.len())?;
        self.next_sample_offset = chunk_end(sample_offset, samples.len())?;
        if let Some(decoder) = &mut self.fixed {
            return Ok(UartDecode {
                baud_rate: decoder.baud_rate,
                bytes: decoder.push_samples(samples, sample_offset),
            });
        }
        if samples.is_empty() {
            return Ok(UartDecode {
                baud_rate: 0,
                bytes: Vec::new(),
            });
        }
        self.probe_offset.get_or_insert(sample_offset);
        if self.probe.len().saturating_add(samples.len()) > UART_PROBE_SAMPLE_LIMIT {
            return Err(TransportDecodeError::new(format!(
                "UART baud rate was not detected within {UART_PROBE_SAMPLE_LIMIT} samples"
            )));
        }
        self.probe.extend_from_slice(samples);
        let Ok(candidate) = decode_uart_samples(
            self.config,
            &self.probe,
            self.probe_offset.unwrap_or(sample_offset),
            self.sample_rate_hz,
        ) else {
            return Ok(UartDecode {
                baud_rate: 0,
                bytes: Vec::new(),
            });
        };
        if candidate.bytes.len() < 2 {
            return Ok(UartDecode {
                baud_rate: 0,
                bytes: Vec::new(),
            });
        }
        let probe_offset = self.probe_offset.unwrap_or(sample_offset);
        let mut decoder =
            FixedUartDecoder::new(self.config, candidate.baud_rate, self.sample_rate_hz);
        let bytes = decoder.push_samples(&self.probe, probe_offset);
        self.probe.clear();
        self.probe_offset = None;
        self.fixed = Some(decoder);
        Ok(UartDecode {
            baud_rate: candidate.baud_rate,
            bytes,
        })
    }

    pub fn finish(&mut self) -> Result<(UartDecode, Vec<DecodeDiagnostic>), TransportDecodeError> {
        if let Some(mut decoder) = self.fixed.take() {
            let baud_rate = decoder.baud_rate;
            let (bytes, diagnostics) = decoder.finish(self.next_sample_offset);
            self.reset();
            return Ok((UartDecode { baud_rate, bytes }, diagnostics));
        }
        let decoded = if self.probe.is_empty() {
            UartDecode {
                baud_rate: 0,
                bytes: Vec::new(),
            }
        } else {
            decode_uart_samples(
                self.config,
                &self.probe,
                self.probe_offset.unwrap_or(0),
                self.sample_rate_hz,
            )?
        };
        self.reset();
        Ok((decoded, Vec::new()))
    }

    pub fn reset(&mut self) {
        self.next_sample_offset = None;
        self.probe_offset = None;
        self.probe.clear();
        self.fixed = None;
    }
}

#[derive(Clone, Debug)]
struct FixedUartDecoder {
    config: UartConfig,
    baud_rate: u64,
    samples_per_bit: f64,
    previous_high: Option<bool>,
    pending: Option<PendingUartFrame>,
}

impl FixedUartDecoder {
    fn new(config: UartConfig, baud_rate: u64, sample_rate_hz: u64) -> Self {
        Self {
            config,
            baud_rate,
            samples_per_bit: sample_rate_hz as f64 / baud_rate as f64,
            previous_high: None,
            pending: None,
        }
    }

    fn push_samples(&mut self, samples: &[u32], sample_offset: u64) -> Vec<TimedByte> {
        let mut bytes = Vec::new();
        for (local_index, sample) in samples.iter().copied().enumerate() {
            let sample_index = sample_offset + local_index as u64;
            let high = channel_is_high(sample, self.config.data_channel);
            let falling = self.previous_high == Some(true) && !high;
            let samples_per_bit = self.samples_per_bit;
            let frame_ended = self.pending.as_mut().is_some_and(|pending| {
                pending.observe(sample_index, high, samples_per_bit);
                sample_index >= pending.end_sample(samples_per_bit)
            });
            if frame_ended {
                if let Some(byte) = self
                    .pending
                    .take()
                    .and_then(|pending| pending.complete(samples_per_bit))
                {
                    bytes.push(byte);
                }
            }
            if self.pending.is_none() && falling {
                self.pending = Some(PendingUartFrame::new(sample_index));
            }
            self.previous_high = Some(high);
        }
        bytes
    }

    fn finish(&mut self, stream_end: Option<u64>) -> (Vec<TimedByte>, Vec<DecodeDiagnostic>) {
        let Some(pending) = self.pending.take() else {
            return (Vec::new(), Vec::new());
        };
        let expected_end = pending.end_sample(self.samples_per_bit);
        let actual_end = stream_end.unwrap_or(expected_end).min(expected_end);
        if let Some(byte) = pending.clone().complete_at(actual_end) {
            return (vec![byte], Vec::new());
        }
        (
            Vec::new(),
            vec![incomplete_transport_diagnostic(
                "transport.uart.incomplete_frame",
                "UART stream ended in the middle of an 8-N-1 frame",
                Some(pending.start),
                stream_end,
            )],
        )
    }
}

#[derive(Clone, Debug)]
struct PendingUartFrame {
    start: u64,
    start_high: Option<bool>,
    data_high: [Option<bool>; 8],
    stop_high: Option<bool>,
}

impl PendingUartFrame {
    fn new(start: u64) -> Self {
        Self {
            start,
            start_high: None,
            data_high: [None; 8],
            stop_high: None,
        }
    }

    fn observe(&mut self, sample: u64, high: bool, samples_per_bit: f64) {
        if sample == self.sample_at(0.5, samples_per_bit) {
            self.start_high = Some(high);
        }
        for bit in 0..8 {
            if sample == self.sample_at(1.5 + bit as f64, samples_per_bit) {
                self.data_high[bit] = Some(high);
            }
        }
        if sample == self.sample_at(9.5, samples_per_bit) {
            self.stop_high = Some(high);
        }
    }

    fn sample_at(&self, bit_position: f64, samples_per_bit: f64) -> u64 {
        self.start
            .saturating_add((bit_position * samples_per_bit).round() as u64)
    }

    fn end_sample(&self, samples_per_bit: f64) -> u64 {
        self.sample_at(10.0, samples_per_bit)
    }

    fn complete(self, samples_per_bit: f64) -> Option<TimedByte> {
        let end = self.end_sample(samples_per_bit);
        self.complete_at(end)
    }

    fn complete_at(self, end: u64) -> Option<TimedByte> {
        if self.start_high != Some(false) || self.stop_high != Some(true) {
            return None;
        }
        let mut value = 0_u8;
        for (bit, high) in self.data_high.into_iter().enumerate() {
            if high? {
                value |= 1 << bit;
            }
        }
        Some(TimedByte {
            value,
            range: SampleRange {
                start: self.start,
                end: (end > self.start).then_some(end)?,
            },
            status: ByteStatus::default(),
        })
    }
}

pub fn decode_i2c_samples(
    config: I2cConfig,
    samples: &[u32],
    sample_offset: u64,
) -> Result<Vec<I2cEvent>, TransportDecodeError> {
    I2cDecoder::new(config)?.push_samples(samples, sample_offset)
}

pub fn decode_spi_samples(
    config: SpiConfig,
    samples: &[u32],
    sample_offset: u64,
) -> Result<Vec<SpiTransfer>, TransportDecodeError> {
    SpiDecoder::new(config)?.push_samples(samples, sample_offset)
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

fn chunk_end(sample_offset: u64, sample_count: usize) -> Result<Option<u64>, TransportDecodeError> {
    let sample_count = u64::try_from(sample_count)
        .map_err(|_| TransportDecodeError::new("logic sample count does not fit in u64"))?;
    sample_offset
        .checked_add(sample_count)
        .map(Some)
        .ok_or_else(|| TransportDecodeError::new("logic sample range overflows u64"))
}

fn validate_contiguous_chunk(
    expected_offset: Option<u64>,
    sample_offset: u64,
    sample_count: usize,
) -> Result<(), TransportDecodeError> {
    let _ = chunk_end(sample_offset, sample_count)?;
    if let Some(expected_offset) = expected_offset {
        if sample_offset != expected_offset {
            return Err(TransportDecodeError::new(format!(
                "non-contiguous logic chunk starts at sample {sample_offset}; expected {expected_offset}; reset the decoder after a discontinuity"
            )));
        }
    }
    Ok(())
}

fn incomplete_transport_diagnostic(
    code: &str,
    message: &str,
    start: Option<u64>,
    end: Option<u64>,
) -> DecodeDiagnostic {
    let range = start
        .zip(end)
        .and_then(|(start, end)| (start < end).then_some(SampleRange { start, end }));
    DecodeDiagnostic {
        code: code.to_owned(),
        message: message.to_owned(),
        severity: Severity::Warning,
        range,
    }
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
        assert_eq!(transfers[0].transaction_index, 0);
        assert!(transfers[0].range.start >= 50);
    }

    #[test]
    fn spi_assigns_a_new_transaction_index_after_chip_select() {
        let mut samples = vec![logic_word(false, false, true)];
        for value in [0x12_u8, 0x34] {
            samples.push(logic_word(false, false, false));
            for bit in (0..8).rev() {
                let data = value & (1 << bit) != 0;
                let low = logic_word(false, data, false);
                samples.extend([low, low | 1, low]);
            }
            samples.push(logic_word(false, false, true));
        }

        let transfers = decode_spi_samples(
            SpiConfig {
                clock_channel: 0,
                miso_channel: None,
                mosi_channel: Some(1),
                chip_select_channel: Some(3),
            },
            &samples,
            0,
        )
        .unwrap();

        assert_eq!(transfers.len(), 2);
        assert_eq!(transfers[0].mosi, Some(0x12));
        assert_eq!(transfers[0].transaction_index, 0);
        assert_eq!(transfers[1].mosi, Some(0x34));
        assert_eq!(transfers[1].transaction_index, 1);
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

    #[test]
    fn i2c_incremental_decode_is_independent_of_chunk_boundaries() {
        let samples = i2c_transaction(&[(0xA0, true), (0x5A, false)]);
        let expected = decode_i2c_samples(
            I2cConfig {
                clock_channel: 0,
                data_channel: 1,
            },
            &samples,
            100,
        )
        .unwrap();
        let mut decoder = I2cDecoder::new(I2cConfig {
            clock_channel: 0,
            data_channel: 1,
        })
        .unwrap();
        let mut actual = Vec::new();
        for (offset, chunk) in samples.chunks(7).enumerate() {
            actual.extend(
                decoder
                    .push_samples(chunk, 100 + (offset * 7) as u64)
                    .unwrap(),
            );
        }
        assert_eq!(actual, expected);
        assert!(decoder.finish().is_empty());
    }

    #[test]
    fn spi_incremental_decode_is_independent_of_chunk_boundaries() {
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
        let config = SpiConfig {
            clock_channel: 0,
            miso_channel: Some(1),
            mosi_channel: Some(2),
            chip_select_channel: Some(3),
        };
        let expected = decode_spi_samples(config, &samples, 50).unwrap();
        let mut decoder = SpiDecoder::new(config).unwrap();
        let mut actual = Vec::new();
        let mut completed_transactions = Vec::new();
        for (offset, chunk) in samples.chunks(5).enumerate() {
            actual.extend(
                decoder
                    .push_samples(chunk, 50 + (offset * 5) as u64)
                    .unwrap(),
            );
            completed_transactions.extend(decoder.take_completed_transactions());
        }
        assert_eq!(actual, expected);
        assert_eq!(
            completed_transactions,
            [SpiTransactionEnd {
                transaction_index: 0,
                completeness: PacketCompleteness::Complete,
            }]
        );
        assert!(decoder.finish().is_empty());
    }

    #[test]
    fn uart_incremental_decode_is_independent_of_chunk_boundaries() {
        let samples = uart_8n1(b"Hi!", 10);
        let expected =
            decode_uart_samples(UartConfig { data_channel: 0 }, &samples, 500, 1_152_000).unwrap();
        let mut decoder = UartDecoder::new(UartConfig { data_channel: 0 }, 1_152_000).unwrap();
        let mut actual = Vec::new();
        let mut baud_rate = 0;
        for (offset, chunk) in samples.chunks(7).enumerate() {
            let output = decoder
                .push_samples(chunk, 500 + (offset * 7) as u64)
                .unwrap();
            baud_rate = baud_rate.max(output.baud_rate);
            actual.extend(output.bytes);
        }
        let (tail, diagnostics) = decoder.finish().unwrap();
        baud_rate = baud_rate.max(tail.baud_rate);
        actual.extend(tail.bytes);
        assert_eq!(baud_rate, expected.baud_rate);
        assert_eq!(actual, expected.bytes);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn incremental_decoder_requires_an_explicit_reset_after_discontinuity() {
        let mut decoder = I2cDecoder::new(I2cConfig {
            clock_channel: 0,
            data_channel: 1,
        })
        .unwrap();
        decoder.push_samples(&[3, 3], 10).unwrap();
        let error = decoder.push_samples(&[3], 20).unwrap_err();
        assert!(error.message().contains("reset the decoder"));
        decoder.reset();
        assert!(decoder.push_samples(&[3], 20).unwrap().is_empty());
    }

    #[test]
    fn finish_reports_an_incomplete_transport_symbol() {
        let samples = i2c_transaction(&[(0xA0, true)]);
        let mut decoder = I2cDecoder::new(I2cConfig {
            clock_channel: 0,
            data_channel: 1,
        })
        .unwrap();
        decoder
            .push_samples(&samples[..samples.len() - 2], 0)
            .unwrap();
        let diagnostics = decoder.finish();
        assert_eq!(diagnostics[0].code, "transport.i2c.incomplete");
    }

    #[test]
    fn finish_reports_an_asserted_spi_transaction() {
        let mut decoder = SpiDecoder::new(SpiConfig {
            clock_channel: 0,
            miso_channel: None,
            mosi_channel: Some(1),
            chip_select_channel: Some(3),
        })
        .unwrap();
        decoder
            .push_samples(&[logic_word(false, false, false)], 20)
            .unwrap();
        let diagnostics = decoder.finish();
        assert_eq!(diagnostics[0].code, "transport.spi.incomplete_transaction");
    }
}
