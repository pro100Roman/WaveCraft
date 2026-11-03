use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use wavecraft_decode::{
    decode_i2c_samples, decode_spi_samples, decode_uart_samples, load_protocol_schema,
    ByteStreamFramer, CustomProtocolDecoder, DecodedField, DecodedMessage, DecodedValue,
    FramedBytes, FramingSchema, I2cConfig, I2cEvent, PacketCompleteness, ProtocolSchema, Severity,
    SpiConfig, TimedByte, TransportKind, TransportPacket, TransportValue, UartConfig,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub long_name: &'static str,
    pub input: &'static str,
    pub output: &'static str,
}

impl ProtocolDefinition {
    const fn new(
        id: &'static str,
        name: &'static str,
        long_name: &'static str,
        input: &'static str,
        output: &'static str,
    ) -> Self {
        Self {
            id,
            name,
            long_name,
            input,
            output,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolChannel {
    pub name: &'static str,
    pub required: bool,
}

impl ProtocolChannel {
    const fn required(name: &'static str) -> Self {
        Self {
            name,
            required: true,
        }
    }

    const fn optional(name: &'static str) -> Self {
        Self {
            name,
            required: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProtocolDataFormat {
    #[default]
    Hexadecimal,
    Decimal,
    Binary,
    Ascii,
}

impl ProtocolDataFormat {
    pub const ALL: [Self; 4] = [Self::Hexadecimal, Self::Decimal, Self::Binary, Self::Ascii];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Hexadecimal => "Hex",
            Self::Decimal => "Decimal",
            Self::Binary => "Binary",
            Self::Ascii => "ASCII",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolDecodedEvent {
    pub start_sample: u64,
    pub end_sample: u64,
    pub text: String,
    pub detail: String,
    pub warning: bool,
    pub kind: ProtocolDecodedKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolDecodedKind {
    I2cAddress { address: u8, read: bool },
    Data { value: u8 },
    Acknowledge { acknowledged: bool },
    Spi { mosi: Option<u8>, miso: Option<u8> },
    Uart { value: u8 },
    Custom,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolDecodeOutcome {
    Events(Vec<ProtocolDecodedEvent>),
    Unavailable(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolAnalyzerInstance {
    pub instance_id: u64,
    pub protocol_id: &'static str,
    pub channel_mappings: Vec<Option<usize>>,
    pub data_format: ProtocolDataFormat,
    pub custom_protocol: Option<CustomProtocolAnalyzer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomProtocolAnalyzer {
    pub source_path: PathBuf,
    pub source: String,
    pub schema: ProtocolSchema,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolAnalyzerEditor {
    pub analyzer: ProtocolAnalyzerInstance,
    pub target_index: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct ProtocolAnalyzerSettings {
    pub search: String,
    pub picker_open: bool,
    pub analyzers: Vec<ProtocolAnalyzerInstance>,
    pub editor: Option<ProtocolAnalyzerEditor>,
    pub custom_load_error: Option<String>,
    next_instance_id: u64,
}

impl ProtocolAnalyzerSettings {
    fn new_analyzer(&mut self, protocol_id: &'static str) -> Option<ProtocolAnalyzerInstance> {
        let stack = protocol_stack(protocol_id)?;
        let channels = protocol_channels(stack[0].id);
        let channel_mappings = channels
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                (index < 16 && (channel.required || index < 4)).then_some(index)
            })
            .collect();
        let instance_id = self.next_instance_id;
        self.next_instance_id = self.next_instance_id.saturating_add(1);
        Some(ProtocolAnalyzerInstance {
            instance_id,
            protocol_id,
            channel_mappings,
            data_format: ProtocolDataFormat::default(),
            custom_protocol: None,
        })
    }

    pub fn begin_add(&mut self, protocol_id: &'static str) -> bool {
        let Some(analyzer) = self.new_analyzer(protocol_id) else {
            return false;
        };
        self.editor = Some(ProtocolAnalyzerEditor {
            analyzer,
            target_index: None,
        });
        self.picker_open = false;
        true
    }

    pub fn begin_add_custom_yaml(
        &mut self,
        source_path: PathBuf,
        source: String,
    ) -> Result<(), String> {
        let schema = load_protocol_schema(&source).map_err(|error| error.to_string())?;
        let protocol_id = match schema.input.transport {
            TransportKind::Uart => "1:uart",
            TransportKind::Spi => "1:spi",
            TransportKind::I2c => "1:i2c",
        };
        let mut analyzer = self
            .new_analyzer(protocol_id)
            .ok_or_else(|| format!("base decoder {protocol_id:?} is unavailable"))?;
        analyzer.custom_protocol = Some(CustomProtocolAnalyzer {
            source_path,
            source,
            schema,
        });
        self.editor = Some(ProtocolAnalyzerEditor {
            analyzer,
            target_index: None,
        });
        self.picker_open = false;
        self.custom_load_error = None;
        Ok(())
    }

    pub fn begin_edit(&mut self, index: usize) -> bool {
        let Some(analyzer) = self.analyzers.get(index).cloned() else {
            return false;
        };
        self.editor = Some(ProtocolAnalyzerEditor {
            analyzer,
            target_index: Some(index),
        });
        true
    }

    pub fn save_editor(&mut self, editor: ProtocolAnalyzerEditor) {
        if let Some(index) = editor.target_index {
            if let Some(analyzer) = self.analyzers.get_mut(index) {
                *analyzer = editor.analyzer;
            }
        } else {
            self.analyzers.push(editor.analyzer);
        }
        self.editor = None;
    }

    pub fn cancel_editor(&mut self) {
        self.editor = None;
    }

    pub fn clear(&mut self) {
        self.analyzers.clear();
    }
}

pub fn protocol_by_id(id: &str) -> Option<&'static ProtocolDefinition> {
    PROTOCOLS.iter().find(|protocol| protocol.id == id)
}

pub fn analyzer_name(analyzer: &ProtocolAnalyzerInstance) -> &str {
    analyzer
        .custom_protocol
        .as_ref()
        .map(|custom| custom.schema.protocol.name.as_str())
        .or_else(|| protocol_by_id(analyzer.protocol_id).map(|protocol| protocol.name))
        .unwrap_or(analyzer.protocol_id)
}

pub fn analyzer_long_name(analyzer: &ProtocolAnalyzerInstance) -> &str {
    analyzer
        .custom_protocol
        .as_ref()
        .and_then(|custom| custom.schema.protocol.description.as_deref())
        .or_else(|| protocol_by_id(analyzer.protocol_id).map(|protocol| protocol.long_name))
        .unwrap_or(analyzer.protocol_id)
}

pub fn analyzer_decoder_chain(analyzer: &ProtocolAnalyzerInstance) -> Vec<&str> {
    let mut chain = protocol_stack(analyzer.protocol_id)
        .unwrap_or_default()
        .into_iter()
        .map(|protocol| protocol.name)
        .collect::<Vec<_>>();
    if let Some(custom) = &analyzer.custom_protocol {
        chain.push(custom.schema.protocol.name.as_str());
    }
    chain
}

pub fn analyzer_channels(analyzer: &ProtocolAnalyzerInstance) -> Vec<ProtocolChannel> {
    let base_id = protocol_stack(analyzer.protocol_id)
        .and_then(|stack| stack.first().map(|protocol| protocol.id))
        .unwrap_or(analyzer.protocol_id);
    let mut channels = protocol_channels(base_id);
    if let Some(custom) = &analyzer.custom_protocol {
        match (
            custom.schema.input.transport,
            custom.schema.input.stream.as_str(),
        ) {
            (TransportKind::Spi, "miso") if channels.len() > 1 => channels[1].required = true,
            (TransportKind::Spi, "mosi") if channels.len() > 2 => channels[2].required = true,
            _ => {}
        }
    }
    channels
}

pub fn matching_protocols(query: &str) -> Vec<&'static ProtocolDefinition> {
    let query = query.trim().to_lowercase();
    let mut protocols = PROTOCOLS
        .iter()
        .filter(|protocol| {
            query.is_empty()
                || protocol.id.to_lowercase().contains(&query)
                || protocol.name.to_lowercase().contains(&query)
                || protocol.long_name.to_lowercase().contains(&query)
        })
        .collect::<Vec<_>>();
    protocols.sort_by_key(|protocol| protocol.name.to_lowercase());
    protocols
}

pub fn protocol_stack(id: &str) -> Option<Vec<&'static ProtocolDefinition>> {
    let mut stack = vec![protocol_by_id(id)?];
    let mut seen = HashSet::from([id]);
    while stack.last()?.input != "logic" {
        let input = stack.last()?.input;
        let base = matching_protocols("").into_iter().find(|candidate| {
            candidate.output == input
                && (!candidate.id.starts_with("0:") || candidate.output == candidate.id)
        })?;
        if !seen.insert(base.id) {
            return None;
        }
        stack.push(base);
    }
    stack.reverse();
    Some(stack)
}

pub fn protocol_channels(id: &str) -> Vec<ProtocolChannel> {
    use ProtocolChannel as Ch;
    let channels: &[ProtocolChannel] = match id {
        "0:i2c" | "1:i2c" => &[Ch::required("SCL"), Ch::required("SDA")],
        "0:spi" | "1:spi" => &[
            Ch::required("CLK"),
            Ch::optional("MISO"),
            Ch::optional("MOSI"),
            Ch::optional("CS#"),
        ],
        "0:uart" | "1:uart" => &[Ch::required("RX/TX")],
        "ac97" => &[
            Ch::required("SYNC"),
            Ch::required("BIT_CLK"),
            Ch::optional("SDATA_OUT"),
            Ch::optional("SDATA_IN"),
            Ch::optional("RESET#"),
        ],
        "am230x" => &[Ch::required("SDA")],
        "aud" => &[
            Ch::required("AUDCK"),
            Ch::required("nAUDSYNC"),
            Ch::required("AUDATA3"),
            Ch::required("AUDATA2"),
            Ch::required("AUDATA1"),
            Ch::required("AUDATA0"),
        ],
        "avr_pdi" => &[Ch::required("RESET"), Ch::required("DATA")],
        "C2" => &[Ch::required("C2CK"), Ch::required("C2D")],
        "caliper" => &[Ch::required("CLK"), Ch::required("DATA")],
        "can" => &[Ch::required("CAN")],
        "can-fd" => &[Ch::required("CAN FD")],
        "cec" => &[Ch::required("CEC")],
        "cjtag" => &[Ch::required("TCKC"), Ch::required("TMSC")],
        "cjtag_oscan1" | "jtag" => &[
            Ch::required("TDI"),
            Ch::required("TDO"),
            Ch::required("TCK"),
            Ch::required("TMS"),
            Ch::optional("TRST#"),
            Ch::optional("SRST#"),
            Ch::optional("RTCK"),
        ],
        "counter" => &[Ch::required("Data"), Ch::optional("Reset")],
        "dali" => &[Ch::required("DALI")],
        "dcf77" => &[Ch::required("DATA")],
        "dmx512" => &[Ch::required("DMX data")],
        "dsi" => &[Ch::required("DSI")],
        "em4100" | "em4305" | "miller" | "morse" | "pwm" | "pxx1" | "rc_encode"
        | "sae_j1850_vpw" | "spdif" | "swim" | "t55xx" | "timing" => &[Ch::required("Data")],
        "eth_auto_negotiation" => &[Ch::required("TX+")],
        "example" => &[Ch::required("c1"), Ch::optional("c2")],
        "flexray" => &[Ch::required("Channel")],
        "fsi" => &[Ch::required("DATA"), Ch::required("CLOCK")],
        "gpib" => &GPIB_CHANNELS,
        "graycode" => &GRAY_CHANNELS,
        "guess_bitrate" => &[Ch::required("Data")],
        "i2s" => &[Ch::required("SCK"), Ch::required("WS"), Ch::required("SD")],
        "iec" => &[
            Ch::required("DATA"),
            Ch::required("CLK"),
            Ch::required("ATN"),
            Ch::optional("SRQ"),
        ],
        "ieee488" => &IEEE488_CHANNELS,
        "ir_irmp" | "ir_nec" | "ir_rc5" | "ir_rc6" | "ir_sirc" => &[Ch::required("IR")],
        "jitter" => &[Ch::required("Clock"), Ch::required("Resulting signal")],
        "lfast" => &[Ch::required("Data")],
        "lpc" => &LPC_CHANNELS,
        "maple_bus" => &[Ch::required("SDCKA"), Ch::required("SDCKB")],
        "mcs48" => &MCS48_CHANNELS,
        "mdio" => &[Ch::required("MDC"), Ch::required("MDIO")],
        "microwire" => &[
            Ch::required("CS"),
            Ch::required("SK"),
            Ch::required("SI"),
            Ch::required("SO"),
        ],
        "MIPI_DSI" => &[Ch::required("D0N"), Ch::required("D0P")],
        "mipi_rffe" => &[Ch::required("SCLK"), Ch::required("SDATA")],
        "numbers_and_state" => &NUMBER_CHANNELS,
        "onewire_link" => &[Ch::required("OWR")],
        "ook" => &[Ch::required("Data")],
        "parallel" => &PARALLEL_CHANNELS,
        "pjdl" => &[Ch::required("DATA")],
        "ps2" => &[Ch::required("Clock"), Ch::required("Data")],
        "qi" => &[Ch::required("Qi")],
        "qspi" | "smart_qspi" => &[
            Ch::required("CLK"),
            Ch::required("IO0"),
            Ch::optional("IO1"),
            Ch::optional("IO2"),
            Ch::optional("IO3"),
            Ch::optional("CS#"),
        ],
        "rgb_led_ws281x" => &[Ch::required("DIN")],
        "sda2506" => &[
            Ch::required("CLK"),
            Ch::required("DATA"),
            Ch::required("CE#"),
        ],
        "sdcard_sd" => &[
            Ch::required("CMD"),
            Ch::required("CLK"),
            Ch::optional("DAT0"),
            Ch::optional("DAT1"),
            Ch::optional("DAT2"),
            Ch::optional("DAT3"),
        ],
        "sdq" => &[Ch::required("SDQ")],
        "seven_segment" => &[
            Ch::required("A"),
            Ch::required("B"),
            Ch::required("C"),
            Ch::required("D"),
            Ch::required("E"),
            Ch::required("F"),
            Ch::required("G"),
            Ch::optional("DP"),
        ],
        "signature" => &[
            Ch::required("START"),
            Ch::required("STOP"),
            Ch::required("CLOCK"),
            Ch::required("DATA"),
        ],
        "sle44xx" => &[
            Ch::required("RST"),
            Ch::required("CLK"),
            Ch::required("I/O"),
        ],
        "st7735" => &[
            Ch::required("CS#"),
            Ch::required("CLK"),
            Ch::required("MOSI"),
            Ch::required("DC"),
        ],
        "st7789" => &[
            Ch::required("CSX"),
            Ch::required("DCX"),
            Ch::required("SDO"),
            Ch::required("WRX"),
        ],
        "stepper_motor" => &[Ch::required("Step"), Ch::required("Direction")],
        "swd" => &[Ch::required("SWCLK"), Ch::required("SWDIO")],
        "tdm_audio" => &[
            Ch::required("Bitclk"),
            Ch::required("Framesync"),
            Ch::required("Data"),
        ],
        "tlc5620" => &[
            Ch::required("CLK"),
            Ch::required("DATA"),
            Ch::optional("LOAD"),
            Ch::optional("LDAC"),
        ],
        "usb_power_delivery" => &[Ch::required("CC1"), Ch::optional("CC2")],
        "usb_signalling" => &[Ch::required("D+"), Ch::required("D-")],
        "wiegand" => &[Ch::required("D0"), Ch::required("D1")],
        "xy2-100" => &[
            Ch::required("CLK"),
            Ch::required("SYNC"),
            Ch::required("DATA"),
            Ch::optional("STAT"),
        ],
        "z80" => &Z80_CHANNELS,
        _ => &[],
    };
    channels.to_vec()
}

pub fn decode_protocol_samples(
    analyzer: &ProtocolAnalyzerInstance,
    samples: &[u32],
    sample_offset: u64,
    sample_rate_hz: u64,
) -> ProtocolDecodeOutcome {
    if let Some(custom) = &analyzer.custom_protocol {
        return decode_custom_protocol_samples(
            analyzer,
            custom,
            samples,
            sample_offset,
            sample_rate_hz,
        );
    }
    let Some(base) = protocol_stack(analyzer.protocol_id).and_then(|stack| stack.first().copied())
    else {
        return ProtocolDecodeOutcome::Unavailable("Decoder stack is incomplete".to_owned());
    };
    match base.id {
        "0:i2c" | "1:i2c" => decode_i2c(analyzer, samples, sample_offset),
        "0:spi" | "1:spi" => decode_spi(analyzer, samples, sample_offset),
        "0:uart" | "1:uart" => decode_uart(analyzer, samples, sample_offset, sample_rate_hz),
        _ => ProtocolDecodeOutcome::Unavailable(format!(
            "{} decoding is not implemented yet",
            base.name.trim_start_matches(|character: char| {
                character.is_ascii_digit() || character == ':'
            })
        )),
    }
}

fn mapped_channel(analyzer: &ProtocolAnalyzerInstance, index: usize) -> Option<usize> {
    analyzer.channel_mappings.get(index).copied().flatten()
}

fn format_protocol_byte(value: u8, format: ProtocolDataFormat) -> String {
    match format {
        ProtocolDataFormat::Hexadecimal => format!("{value:02X}"),
        ProtocolDataFormat::Decimal => value.to_string(),
        ProtocolDataFormat::Binary => format!("{value:08b}"),
        ProtocolDataFormat::Ascii if value.is_ascii_graphic() || value == b' ' => {
            format!("'{}'", char::from(value))
        }
        ProtocolDataFormat::Ascii => format!("{value:02X}"),
    }
}

fn decode_custom_protocol_samples(
    analyzer: &ProtocolAnalyzerInstance,
    custom: &CustomProtocolAnalyzer,
    samples: &[u32],
    sample_offset: u64,
    sample_rate_hz: u64,
) -> ProtocolDecodeOutcome {
    let decoder = CustomProtocolDecoder::new(custom.schema.clone());
    let result = match custom.schema.input.transport {
        TransportKind::Uart => {
            let Some(data_channel) = mapped_channel(analyzer, 0) else {
                return ProtocolDecodeOutcome::Unavailable("Map the RX/TX channel".to_owned());
            };
            let decoded = match decode_uart_samples(
                UartConfig { data_channel },
                samples,
                sample_offset,
                sample_rate_hz,
            ) {
                Ok(decoded) => decoded,
                Err(error) => return ProtocolDecodeOutcome::Unavailable(error.to_string()),
            };
            decode_custom_byte_stream(&decoder, &custom.schema, &decoded.bytes, sample_rate_hz)
        }
        TransportKind::Spi => decode_custom_spi(analyzer, &decoder, samples, sample_offset),
        TransportKind::I2c => decode_custom_i2c(analyzer, &decoder, samples, sample_offset),
    };
    match result {
        Ok(messages) => {
            ProtocolDecodeOutcome::Events(messages.into_iter().map(custom_message_event).collect())
        }
        Err(error) => ProtocolDecodeOutcome::Unavailable(error),
    }
}

fn decode_custom_byte_stream(
    decoder: &CustomProtocolDecoder,
    schema: &ProtocolSchema,
    bytes: &[TimedByte],
    sample_rate_hz: u64,
) -> Result<Vec<DecodedMessage>, String> {
    let mut framer =
        ByteStreamFramer::new(schema.framing.clone()).map_err(|error| error.to_string())?;
    let mut frames = Vec::new();
    if matches!(schema.framing, FramingSchema::Gap { .. }) {
        let mut previous_end = None;
        for byte in bytes {
            if let Some(previous_end) = previous_end {
                let gap_samples = byte.range.start.saturating_sub(previous_end);
                let gap_us = gap_samples
                    .saturating_mul(1_000_000)
                    .checked_div(sample_rate_hz)
                    .unwrap_or(0);
                frames.extend(framer.push_gap(gap_us).frames);
            }
            frames.extend(
                framer
                    .push_bytes(std::slice::from_ref(byte))
                    .map_err(|error| error.to_string())?
                    .frames,
            );
            previous_end = Some(byte.range.end);
        }
    } else {
        frames.extend(
            framer
                .push_bytes(bytes)
                .map_err(|error| error.to_string())?
                .frames,
        );
    }
    decode_custom_frames(decoder, frames)
}

fn decode_custom_frames(
    decoder: &CustomProtocolDecoder,
    frames: Vec<FramedBytes>,
) -> Result<Vec<DecodedMessage>, String> {
    let mut messages = Vec::new();
    for frame in frames {
        if let Some(message) = decoder
            .decode_framed_bytes(frame)
            .map_err(|error| error.to_string())?
        {
            messages.push(message);
        }
    }
    Ok(messages)
}

fn decode_custom_spi(
    analyzer: &ProtocolAnalyzerInstance,
    decoder: &CustomProtocolDecoder,
    samples: &[u32],
    sample_offset: u64,
) -> Result<Vec<DecodedMessage>, String> {
    let Some(clock_channel) = mapped_channel(analyzer, 0) else {
        return Err("Map the CLK channel".to_owned());
    };
    let transfers = decode_spi_samples(
        SpiConfig {
            clock_channel,
            miso_channel: mapped_channel(analyzer, 1),
            mosi_channel: mapped_channel(analyzer, 2),
            chip_select_channel: mapped_channel(analyzer, 3),
        },
        samples,
        sample_offset,
    )
    .map_err(|error| error.to_string())?;
    if matches!(
        decoder.schema().framing,
        FramingSchema::TransportTransaction { .. }
    ) {
        let mut transactions = BTreeMap::<usize, Vec<TimedByte>>::new();
        for transfer in transfers {
            let value = match decoder.schema().input.stream.as_str() {
                "mosi" => transfer.mosi,
                "miso" => transfer.miso,
                _ => None,
            };
            if let Some(value) = value {
                transactions
                    .entry(transfer.transaction_index)
                    .or_default()
                    .push(TimedByte {
                        value,
                        range: transfer.range,
                        status: Default::default(),
                    });
            }
        }
        decode_custom_transactions(decoder, transactions.into_values())
    } else {
        let bytes = transfers
            .into_iter()
            .filter_map(|transfer| {
                let value = match decoder.schema().input.stream.as_str() {
                    "mosi" => transfer.mosi,
                    "miso" => transfer.miso,
                    _ => None,
                }?;
                Some(TimedByte {
                    value,
                    range: transfer.range,
                    status: Default::default(),
                })
            })
            .collect::<Vec<_>>();
        decode_custom_byte_stream(decoder, decoder.schema(), &bytes, 0)
    }
}

fn decode_custom_i2c(
    analyzer: &ProtocolAnalyzerInstance,
    decoder: &CustomProtocolDecoder,
    samples: &[u32],
    sample_offset: u64,
) -> Result<Vec<DecodedMessage>, String> {
    let (Some(clock_channel), Some(data_channel)) =
        (mapped_channel(analyzer, 0), mapped_channel(analyzer, 1))
    else {
        return Err("Map both SCL and SDA channels".to_owned());
    };
    let events = decode_i2c_samples(
        I2cConfig {
            clock_channel,
            data_channel,
        },
        samples,
        sample_offset,
    )
    .map_err(|error| error.to_string())?;
    let mut transactions = Vec::<(Vec<TimedByte>, BTreeMap<String, TransportValue>)>::new();
    let mut bytes = Vec::new();
    let mut attributes = BTreeMap::new();
    let mut active = false;
    for event in events {
        match event {
            I2cEvent::Start { .. } => {
                if active && (!bytes.is_empty() || !attributes.is_empty()) {
                    transactions
                        .push((std::mem::take(&mut bytes), std::mem::take(&mut attributes)));
                }
                active = true;
            }
            I2cEvent::Byte { byte, index: 0 } => {
                attributes.insert(
                    "address".to_owned(),
                    TransportValue::Integer(i64::from(byte.value >> 1)),
                );
                attributes.insert(
                    "read".to_owned(),
                    TransportValue::Boolean(byte.value & 1 != 0),
                );
            }
            I2cEvent::Byte { byte, .. } if active => bytes.push(byte),
            I2cEvent::Stop { .. } if active => {
                transactions.push((std::mem::take(&mut bytes), std::mem::take(&mut attributes)));
                active = false;
            }
            I2cEvent::Acknowledge { .. } | I2cEvent::Byte { .. } | I2cEvent::Stop { .. } => {}
        }
    }
    if active && (!bytes.is_empty() || !attributes.is_empty()) {
        transactions.push((bytes, attributes));
    }
    if matches!(
        decoder.schema().framing,
        FramingSchema::TransportTransaction { .. }
    ) {
        let mut messages = Vec::new();
        for (bytes, attributes) in transactions {
            if bytes.is_empty() {
                continue;
            }
            let mut packet = TransportPacket::single_stream(
                TransportKind::I2c,
                "data",
                bytes,
                PacketCompleteness::Complete,
            );
            packet.attributes = attributes;
            if let Some(message) = decoder
                .decode_packet(&packet)
                .map_err(|error| error.to_string())?
            {
                messages.push(message);
            }
        }
        Ok(messages)
    } else {
        let bytes = transactions
            .into_iter()
            .flat_map(|(bytes, _)| bytes)
            .collect::<Vec<_>>();
        decode_custom_byte_stream(decoder, decoder.schema(), &bytes, 0)
    }
}

fn decode_custom_transactions(
    decoder: &CustomProtocolDecoder,
    transactions: impl IntoIterator<Item = Vec<TimedByte>>,
) -> Result<Vec<DecodedMessage>, String> {
    let mut messages = Vec::new();
    for bytes in transactions {
        if bytes.is_empty() {
            continue;
        }
        let packet = TransportPacket::single_stream(
            decoder.schema().input.transport,
            decoder.schema().input.stream.clone(),
            bytes,
            PacketCompleteness::Complete,
        );
        if let Some(message) = decoder
            .decode_packet(&packet)
            .map_err(|error| error.to_string())?
        {
            messages.push(message);
        }
    }
    Ok(messages)
}

fn custom_message_event(message: DecodedMessage) -> ProtocolDecodedEvent {
    let mut details = message
        .fields
        .iter()
        .filter(|field| !field.hidden)
        .map(format_custom_field)
        .collect::<Vec<_>>();
    details.extend(
        message
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.clone()),
    );
    ProtocolDecodedEvent {
        start_sample: message.range.start,
        end_sample: message.range.end,
        text: message.summary,
        detail: details.join(" · "),
        warning: message.severity >= Severity::Warning,
        kind: ProtocolDecodedKind::Custom,
    }
}

fn format_custom_field(field: &DecodedField) -> String {
    let value = field.display.clone().unwrap_or_else(|| match &field.value {
        DecodedValue::Integer(value) => value.to_string(),
        DecodedValue::Bytes(bytes) => bytes
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" "),
        DecodedValue::String(value) => value.clone(),
        DecodedValue::Boolean(value) => value.to_string(),
    });
    format!("{}={value}", field.name)
}

fn decode_i2c(
    analyzer: &ProtocolAnalyzerInstance,
    samples: &[u32],
    sample_offset: u64,
) -> ProtocolDecodeOutcome {
    let (Some(clock_channel), Some(data_channel)) =
        (mapped_channel(analyzer, 0), mapped_channel(analyzer, 1))
    else {
        return ProtocolDecodeOutcome::Unavailable("Map both SCL and SDA channels".to_owned());
    };
    let transport_events = match decode_i2c_samples(
        I2cConfig {
            clock_channel,
            data_channel,
        },
        samples,
        sample_offset,
    ) {
        Ok(events) => events,
        Err(error) => return ProtocolDecodeOutcome::Unavailable(error.to_string()),
    };
    ProtocolDecodeOutcome::Events(
        transport_events
            .into_iter()
            .filter_map(|event| match event {
                I2cEvent::Byte { byte, index } => {
                    let (text, detail, kind) = if index == 0 {
                        let address = byte.value >> 1;
                        let read = byte.value & 1 != 0;
                        let direction = if read { "R" } else { "W" };
                        (
                            format!(
                                "ADDR {} {direction}",
                                format_protocol_byte(address, analyzer.data_format)
                            ),
                            format!(
                                "I²C address 0x{address:02X} ({address}) · {} · raw byte 0x{:02X}",
                                if read { "read" } else { "write" },
                                byte.value
                            ),
                            ProtocolDecodedKind::I2cAddress { address, read },
                        )
                    } else {
                        (
                            format_protocol_byte(byte.value, analyzer.data_format),
                            format!("I²C data 0x{:02X} ({})", byte.value, byte.value),
                            ProtocolDecodedKind::Data { value: byte.value },
                        )
                    };
                    Some(ProtocolDecodedEvent {
                        start_sample: byte.range.start,
                        end_sample: byte.range.end,
                        text,
                        detail,
                        warning: false,
                        kind,
                    })
                }
                I2cEvent::Acknowledge {
                    range,
                    acknowledged,
                    byte_index,
                    byte_value,
                } => Some(ProtocolDecodedEvent {
                    start_sample: range.start,
                    end_sample: range.end,
                    text: if acknowledged { "ACK" } else { "NACK" }.to_owned(),
                    detail: format!(
                        "I²C {} for {} byte 0x{byte_value:02X}",
                        if acknowledged {
                            "acknowledge"
                        } else {
                            "not acknowledged"
                        },
                        if byte_index == 0 { "address" } else { "data" }
                    ),
                    warning: !acknowledged,
                    kind: ProtocolDecodedKind::Acknowledge { acknowledged },
                }),
                I2cEvent::Start { .. } | I2cEvent::Stop { .. } => None,
            })
            .collect(),
    )
}

fn decode_spi(
    analyzer: &ProtocolAnalyzerInstance,
    samples: &[u32],
    sample_offset: u64,
) -> ProtocolDecodeOutcome {
    let Some(clock_channel) = mapped_channel(analyzer, 0) else {
        return ProtocolDecodeOutcome::Unavailable("Map the CLK channel".to_owned());
    };
    let miso_channel = mapped_channel(analyzer, 1);
    let mosi_channel = mapped_channel(analyzer, 2);
    let chip_select_channel = mapped_channel(analyzer, 3);
    if miso_channel.is_none() && mosi_channel.is_none() {
        return ProtocolDecodeOutcome::Unavailable("Map MISO or MOSI".to_owned());
    }
    let transfers = match decode_spi_samples(
        SpiConfig {
            clock_channel,
            miso_channel,
            mosi_channel,
            chip_select_channel,
        },
        samples,
        sample_offset,
    ) {
        Ok(transfers) => transfers,
        Err(error) => return ProtocolDecodeOutcome::Unavailable(error.to_string()),
    };
    ProtocolDecodeOutcome::Events(
        transfers
            .into_iter()
            .map(|transfer| {
                let text = match (transfer.mosi, transfer.miso) {
                    (Some(mosi), Some(miso)) => format!(
                        "MOSI {} · MISO {}",
                        format_protocol_byte(mosi, analyzer.data_format),
                        format_protocol_byte(miso, analyzer.data_format)
                    ),
                    (Some(mosi), None) => format_protocol_byte(mosi, analyzer.data_format),
                    (None, Some(miso)) => format_protocol_byte(miso, analyzer.data_format),
                    (None, None) => unreachable!("SPI configuration requires a data channel"),
                };
                ProtocolDecodedEvent {
                    start_sample: transfer.range.start,
                    end_sample: transfer.range.end,
                    detail: format!(
                        "SPI byte · MOSI {} · MISO {}",
                        transfer.mosi.map_or_else(
                            || "not mapped".to_owned(),
                            |value| format!("0x{value:02X}")
                        ),
                        transfer.miso.map_or_else(
                            || "not mapped".to_owned(),
                            |value| format!("0x{value:02X}")
                        )
                    ),
                    text,
                    warning: false,
                    kind: ProtocolDecodedKind::Spi {
                        mosi: transfer.mosi,
                        miso: transfer.miso,
                    },
                }
            })
            .collect(),
    )
}

fn decode_uart(
    analyzer: &ProtocolAnalyzerInstance,
    samples: &[u32],
    sample_offset: u64,
    sample_rate_hz: u64,
) -> ProtocolDecodeOutcome {
    let Some(channel) = mapped_channel(analyzer, 0) else {
        return ProtocolDecodeOutcome::Unavailable("Map the RX/TX channel".to_owned());
    };
    let decoded = match decode_uart_samples(
        UartConfig {
            data_channel: channel,
        },
        samples,
        sample_offset,
        sample_rate_hz,
    ) {
        Ok(decoded) => decoded,
        Err(error) => return ProtocolDecodeOutcome::Unavailable(error.to_string()),
    };
    let baud_rate = decoded.baud_rate;
    ProtocolDecodeOutcome::Events(
        decoded
            .bytes
            .into_iter()
            .map(|byte| ProtocolDecodedEvent {
                start_sample: byte.range.start,
                end_sample: byte.range.end,
                text: format_protocol_byte(byte.value, analyzer.data_format),
                detail: format!(
                    "UART 8-N-1 · {baud_rate} baud · 0x{:02X} ({})",
                    byte.value, byte.value
                ),
                warning: false,
                kind: ProtocolDecodedKind::Uart { value: byte.value },
            })
            .collect(),
    )
}

const GPIB_CHANNELS: [ProtocolChannel; 16] = [
    ProtocolChannel::required("DIO1"),
    ProtocolChannel::required("DIO2"),
    ProtocolChannel::required("DIO3"),
    ProtocolChannel::required("DIO4"),
    ProtocolChannel::required("DIO5"),
    ProtocolChannel::required("DIO6"),
    ProtocolChannel::required("DIO7"),
    ProtocolChannel::required("DIO8"),
    ProtocolChannel::required("EOI"),
    ProtocolChannel::required("DAV"),
    ProtocolChannel::required("NRFD"),
    ProtocolChannel::required("NDAC"),
    ProtocolChannel::required("IFC"),
    ProtocolChannel::required("SRQ"),
    ProtocolChannel::required("ATN"),
    ProtocolChannel::required("REN"),
];

const IEEE488_CHANNELS: [ProtocolChannel; 17] = [
    ProtocolChannel::required("DIO1/DATA"),
    ProtocolChannel::optional("DIO2"),
    ProtocolChannel::optional("DIO3"),
    ProtocolChannel::optional("DIO4"),
    ProtocolChannel::optional("DIO5"),
    ProtocolChannel::optional("DIO6"),
    ProtocolChannel::optional("DIO7"),
    ProtocolChannel::optional("DIO8"),
    ProtocolChannel::optional("EOI"),
    ProtocolChannel::optional("DAV"),
    ProtocolChannel::optional("NRFD"),
    ProtocolChannel::optional("NDAC"),
    ProtocolChannel::optional("IFC"),
    ProtocolChannel::optional("SRQ"),
    ProtocolChannel::optional("ATN"),
    ProtocolChannel::optional("REN"),
    ProtocolChannel::optional("CLK"),
];

const LPC_CHANNELS: [ProtocolChannel; 13] = [
    ProtocolChannel::required("LFRAME#"),
    ProtocolChannel::required("LCLK"),
    ProtocolChannel::required("LAD[0]"),
    ProtocolChannel::required("LAD[1]"),
    ProtocolChannel::required("LAD[2]"),
    ProtocolChannel::required("LAD[3]"),
    ProtocolChannel::optional("LRESET#"),
    ProtocolChannel::optional("LDRQ#"),
    ProtocolChannel::optional("SERIRQ"),
    ProtocolChannel::optional("CLKRUN#"),
    ProtocolChannel::optional("LPME#"),
    ProtocolChannel::optional("LPCPD#"),
    ProtocolChannel::optional("LSMI#"),
];

const GRAY_CHANNELS: [ProtocolChannel; 8] = [
    ProtocolChannel::optional("D0"),
    ProtocolChannel::optional("D1"),
    ProtocolChannel::optional("D2"),
    ProtocolChannel::optional("D3"),
    ProtocolChannel::optional("D4"),
    ProtocolChannel::optional("D5"),
    ProtocolChannel::optional("D6"),
    ProtocolChannel::optional("D7"),
];

const NUMBER_CHANNELS: [ProtocolChannel; 17] = [
    ProtocolChannel::optional("Clock"),
    ProtocolChannel::optional("Bit0"),
    ProtocolChannel::optional("Bit1"),
    ProtocolChannel::optional("Bit2"),
    ProtocolChannel::optional("Bit3"),
    ProtocolChannel::optional("Bit4"),
    ProtocolChannel::optional("Bit5"),
    ProtocolChannel::optional("Bit6"),
    ProtocolChannel::optional("Bit7"),
    ProtocolChannel::optional("Bit8"),
    ProtocolChannel::optional("Bit9"),
    ProtocolChannel::optional("Bit10"),
    ProtocolChannel::optional("Bit11"),
    ProtocolChannel::optional("Bit12"),
    ProtocolChannel::optional("Bit13"),
    ProtocolChannel::optional("Bit14"),
    ProtocolChannel::optional("Bit15"),
];

const MCS48_CHANNELS: [ProtocolChannel; 15] = [
    ProtocolChannel::required("ALE"),
    ProtocolChannel::required("/PSEN"),
    ProtocolChannel::required("D0"),
    ProtocolChannel::required("D1"),
    ProtocolChannel::required("D2"),
    ProtocolChannel::required("D3"),
    ProtocolChannel::required("D4"),
    ProtocolChannel::required("D5"),
    ProtocolChannel::required("D6"),
    ProtocolChannel::required("D7"),
    ProtocolChannel::required("A8"),
    ProtocolChannel::required("A9"),
    ProtocolChannel::required("A10"),
    ProtocolChannel::required("A11"),
    ProtocolChannel::optional("A12"),
];

const PARALLEL_CHANNELS: [ProtocolChannel; 33] = [
    ProtocolChannel::optional("CLK"),
    ProtocolChannel::optional("D0"),
    ProtocolChannel::optional("D1"),
    ProtocolChannel::optional("D2"),
    ProtocolChannel::optional("D3"),
    ProtocolChannel::optional("D4"),
    ProtocolChannel::optional("D5"),
    ProtocolChannel::optional("D6"),
    ProtocolChannel::optional("D7"),
    ProtocolChannel::optional("D8"),
    ProtocolChannel::optional("D9"),
    ProtocolChannel::optional("D10"),
    ProtocolChannel::optional("D11"),
    ProtocolChannel::optional("D12"),
    ProtocolChannel::optional("D13"),
    ProtocolChannel::optional("D14"),
    ProtocolChannel::optional("D15"),
    ProtocolChannel::optional("D16"),
    ProtocolChannel::optional("D17"),
    ProtocolChannel::optional("D18"),
    ProtocolChannel::optional("D19"),
    ProtocolChannel::optional("D20"),
    ProtocolChannel::optional("D21"),
    ProtocolChannel::optional("D22"),
    ProtocolChannel::optional("D23"),
    ProtocolChannel::optional("D24"),
    ProtocolChannel::optional("D25"),
    ProtocolChannel::optional("D26"),
    ProtocolChannel::optional("D27"),
    ProtocolChannel::optional("D28"),
    ProtocolChannel::optional("D29"),
    ProtocolChannel::optional("D30"),
    ProtocolChannel::optional("D31"),
];

const Z80_CHANNELS: [ProtocolChannel; 29] = [
    ProtocolChannel::required("D0"),
    ProtocolChannel::required("D1"),
    ProtocolChannel::required("D2"),
    ProtocolChannel::required("D3"),
    ProtocolChannel::required("D4"),
    ProtocolChannel::required("D5"),
    ProtocolChannel::required("D6"),
    ProtocolChannel::required("D7"),
    ProtocolChannel::required("/M1"),
    ProtocolChannel::required("/RD"),
    ProtocolChannel::required("/WR"),
    ProtocolChannel::optional("/MREQ"),
    ProtocolChannel::optional("/IORQ"),
    ProtocolChannel::optional("A0"),
    ProtocolChannel::optional("A1"),
    ProtocolChannel::optional("A2"),
    ProtocolChannel::optional("A3"),
    ProtocolChannel::optional("A4"),
    ProtocolChannel::optional("A5"),
    ProtocolChannel::optional("A6"),
    ProtocolChannel::optional("A7"),
    ProtocolChannel::optional("A8"),
    ProtocolChannel::optional("A9"),
    ProtocolChannel::optional("A10"),
    ProtocolChannel::optional("A11"),
    ProtocolChannel::optional("A12"),
    ProtocolChannel::optional("A13"),
    ProtocolChannel::optional("A14"),
    ProtocolChannel::optional("A15"),
];

pub const PROTOCOLS: [ProtocolDefinition; 150] = [
    ProtocolDefinition::new("0:i2c", "0:I²C", "Inter-Integrated Circuit", "logic", "i2c"),
    ProtocolDefinition::new(
        "0:spi",
        "0:SPI",
        "Serial Peripheral Interface",
        "logic",
        "spi",
    ),
    ProtocolDefinition::new(
        "0:uart",
        "0:UART",
        "Universal Asynchronous Receiver/Transmitter",
        "logic",
        "",
    ),
    ProtocolDefinition::new("1:i2c", "1:I²C", "Inter-Integrated Circuit", "logic", "i2c"),
    ProtocolDefinition::new(
        "1:spi",
        "1:SPI",
        "Serial Peripheral Interface",
        "logic",
        "spi",
    ),
    ProtocolDefinition::new(
        "1:uart",
        "1:UART",
        "Universal Asynchronous Receiver/Transmitter",
        "logic",
        "uart",
    ),
    ProtocolDefinition::new("a7105", "A7105", "AMICCOM A7105", "spi", ""),
    ProtocolDefinition::new("ac97", "AC '97", "Audio Codec '97", "logic", ""),
    ProtocolDefinition::new("ad5626", "AD5626", "Analog Devices AD5626", "spi", ""),
    ProtocolDefinition::new("ad79x0", "AD79x0", "Analog Devices AD79x0", "spi", ""),
    ProtocolDefinition::new("ade77xx", "ADE77xx", "Analog Devices ADE77xx", "spi", ""),
    ProtocolDefinition::new("adf435x", "ADF435x", "Analog Devices ADF4350/1", "spi", ""),
    ProtocolDefinition::new("adns5020", "ADNS-5020", "Avago ADNS-5020", "spi", ""),
    ProtocolDefinition::new("adxl345", "ADXL345", "Analog Devices ADXL345", "spi", ""),
    ProtocolDefinition::new("am230x", "AM230x", "Aosong AM230x/DHTxx/RHTxx", "logic", ""),
    ProtocolDefinition::new(
        "amulet_ascii",
        "Amulet ASCII",
        "Amulet LCD ASCII",
        "uart",
        "",
    ),
    ProtocolDefinition::new(
        "arm_etmv3",
        "ARM ETMv3",
        "ARM Embedded Trace Macroblock v3",
        "uart",
        "",
    ),
    ProtocolDefinition::new(
        "arm_itm",
        "ARM ITM",
        "ARM Instrumentation Trace Macroblock",
        "uart",
        "",
    ),
    ProtocolDefinition::new(
        "arm_tpiu",
        "ARM TPIU",
        "ARM Trace Port Interface Unit",
        "uart",
        "uart",
    ),
    ProtocolDefinition::new("atsha204a", "ATSHA204A", "Microchip ATSHA204A", "i2c", ""),
    ProtocolDefinition::new("aud", "AUD", "Advanced User Debugger", "logic", ""),
    ProtocolDefinition::new("avr_isp", "AVR ISP", "AVR In-System Programming", "spi", ""),
    ProtocolDefinition::new(
        "avr_pdi",
        "AVR PDI",
        "Atmel Program and Debug Interface",
        "logic",
        "",
    ),
    ProtocolDefinition::new("C2", "C2 interface", "Silabs C2 Interface", "logic", "C2"),
    ProtocolDefinition::new("caliper", "Caliper", "Digital calipers", "logic", ""),
    ProtocolDefinition::new("can", "CAN", "Controller Area Network", "logic", "can"),
    ProtocolDefinition::new(
        "can-fd",
        "CAN-FD",
        "Controller Area Network with Flexible Data rate",
        "logic",
        "can",
    ),
    ProtocolDefinition::new("cc1101", "CC1101", "Texas Instruments CC1101", "spi", ""),
    ProtocolDefinition::new("cec", "CEC", "HDMI-CEC", "logic", ""),
    ProtocolDefinition::new(
        "cfp",
        "CFP",
        "100 Gigabit C form-factor pluggable",
        "mdio",
        "",
    ),
    ProtocolDefinition::new(
        "cjtag",
        "cJTAG",
        "Compact Joint Test Action Group (IEEE 1149.7)",
        "logic",
        "jtag",
    ),
    ProtocolDefinition::new(
        "cjtag_oscan1",
        "cJTAG OScan1",
        "Compact Joint Test Action Group (IEEE 1149.7)",
        "logic",
        "jtag",
    ),
    ProtocolDefinition::new("counter", "Counter", "Edge counter", "logic", ""),
    ProtocolDefinition::new(
        "dali",
        "DALI",
        "Digital Addressable Lighting Interface",
        "logic",
        "",
    ),
    ProtocolDefinition::new("dcf77", "DCF77", "DCF77 time protocol", "logic", ""),
    ProtocolDefinition::new("dmx512", "DMX512", "Digital MultipleX 512", "logic", ""),
    ProtocolDefinition::new("ds1307", "DS1307", "Dallas DS1307", "i2c", ""),
    ProtocolDefinition::new("ds2408", "DS2408", "Maxim DS2408", "onewire_network", ""),
    ProtocolDefinition::new("ds243x", "DS243x", "Maxim DS2432/3", "onewire_network", ""),
    ProtocolDefinition::new(
        "ds28ea00",
        "DS28EA00",
        "Maxim DS28EA00 1-Wire digital thermometer",
        "onewire_network",
        "",
    ),
    ProtocolDefinition::new("dsi", "DSI", "Digital Serial Interface", "logic", ""),
    ProtocolDefinition::new(
        "edid",
        "EDID",
        "Extended Display Identification Data",
        "i2c",
        "",
    ),
    ProtocolDefinition::new("eeprom24xx", "24xx EEPROM", "24xx I²C EEPROM", "i2c", ""),
    ProtocolDefinition::new(
        "eeprom93xx",
        "93xx EEPROM",
        "93xx Microwire EEPROM",
        "microwire",
        "",
    ),
    ProtocolDefinition::new("em4100", "EM4100", "RFID EM4100", "logic", ""),
    ProtocolDefinition::new("em4305", "EM4305", "RFID EM4205/EM4305", "logic", ""),
    ProtocolDefinition::new("enc28j60", "ENC28J60", "Microchip ENC28J60", "spi", ""),
    ProtocolDefinition::new(
        "eth_auto_negotiation",
        "ETH_AN",
        "ETH Auto Negotiation",
        "logic",
        "eth_an",
    ),
    ProtocolDefinition::new("example", "Example", "example-lala", "logic", "test"),
    ProtocolDefinition::new("flexray", "FlexRay", "FlexRay", "logic", ""),
    ProtocolDefinition::new("fsi", "FSI", "Flexible Service Interface", "logic", ""),
    ProtocolDefinition::new("gpib", "GPIB", "General Purpose Interface Bus", "logic", ""),
    ProtocolDefinition::new(
        "graycode",
        "Gray code",
        "Gray code and rotary encoder",
        "logic",
        "",
    ),
    ProtocolDefinition::new(
        "guess_bitrate",
        "Guess bitrate",
        "Guess bitrate/baudrate",
        "logic",
        "",
    ),
    ProtocolDefinition::new("hdcp", "HDCP", "HDCP over HDMI", "i2c", "hdcp"),
    ProtocolDefinition::new("i2cdemux", "I²C demux", "I²C demultiplexer", "i2c", ""),
    ProtocolDefinition::new("i2cfilter", "I²C filter", "I²C filter", "i2c", "i2c"),
    ProtocolDefinition::new("i2s", "I²S", "Integrated Interchip Sound", "logic", "i2s"),
    ProtocolDefinition::new("iec", "IEC", "Commodore IEC bus", "logic", ""),
    ProtocolDefinition::new(
        "ieee488",
        "IEEE-488",
        "IEEE-488 GPIB/HPIB/IEC",
        "logic",
        "ieee488",
    ),
    ProtocolDefinition::new("ir_irmp", "IR IRMP", "IR IRMP", "logic", ""),
    ProtocolDefinition::new("ir_nec", "IR NEC", "IR NEC", "logic", ""),
    ProtocolDefinition::new("ir_rc5", "IR RC-5", "IR RC-5", "logic", ""),
    ProtocolDefinition::new("ir_rc6", "IR RC-6", "IR RC-6", "logic", ""),
    ProtocolDefinition::new("ir_sirc", "IR SIRC", "Sony IR (SIRC)", "logic", ""),
    ProtocolDefinition::new("jitter", "Jitter", "Timing jitter calculation", "logic", ""),
    ProtocolDefinition::new(
        "jtag",
        "JTAG",
        "Joint Test Action Group (IEEE 1149.1)",
        "logic",
        "jtag",
    ),
    ProtocolDefinition::new(
        "jtag_ejtag",
        "JTAG / EJTAG",
        "Joint Test Action Group / EJTAG (MIPS)",
        "jtag",
        "",
    ),
    ProtocolDefinition::new(
        "jtag_stm32",
        "JTAG / STM32",
        "Joint Test Action Group / ST STM32",
        "jtag",
        "",
    ),
    ProtocolDefinition::new("lfast", "LFAST", "NXP LFAST interface", "logic", "lfast"),
    ProtocolDefinition::new("lin", "LIN", "Local Interconnect Network", "uart", ""),
    ProtocolDefinition::new("lm75", "LM75", "National LM75", "i2c", ""),
    ProtocolDefinition::new("lpc", "LPC", "Low Pin Count", "logic", ""),
    ProtocolDefinition::new("ltc242x", "LTC242x", "Linear Technology LTC242x", "spi", ""),
    ProtocolDefinition::new("ltc26x7", "LTC26x7", "Linear Technology LTC26x7", "i2c", ""),
    ProtocolDefinition::new("maple_bus", "Maple bus", "SEGA Maple bus", "logic", ""),
    ProtocolDefinition::new("max7219", "MAX7219", "Maxim MAX7219/MAX7221", "spi", ""),
    ProtocolDefinition::new("mcs48", "MCS-48", "Intel MCS-48", "logic", ""),
    ProtocolDefinition::new(
        "mdio",
        "MDIO",
        "Management Data Input/Output",
        "logic",
        "mdio",
    ),
    ProtocolDefinition::new("microwire", "Microwire", "Microwire", "logic", "microwire"),
    ProtocolDefinition::new(
        "midi",
        "MIDI",
        "Musical Instrument Digital Interface",
        "uart",
        "",
    ),
    ProtocolDefinition::new("miller", "Miller", "Miller encoding", "logic", ""),
    ProtocolDefinition::new(
        "MIPI_DSI",
        "MIPI_DSI",
        "MIPI Display Serial Interface",
        "logic",
        "mipi_dsi",
    ),
    ProtocolDefinition::new(
        "mipi_rffe",
        "MIPI_RFFE",
        "RF Front-End Control Interface",
        "logic",
        "mipi_rffe",
    ),
    ProtocolDefinition::new("mlx90614", "MLX90614", "Melexis MLX90614", "i2c", ""),
    ProtocolDefinition::new(
        "modbus",
        "Modbus",
        "Modbus RTU over RS232/RS485",
        "uart",
        "modbus",
    ),
    ProtocolDefinition::new("morse", "Morse", "Morse code", "logic", ""),
    ProtocolDefinition::new("mrf24j40", "MRF24J40", "Microchip MRF24J40", "spi", ""),
    ProtocolDefinition::new("mxc6225xu", "MXC6225XU", "MEMSIC MXC6225XU", "i2c", ""),
    ProtocolDefinition::new(
        "nes_gamepad",
        "NES gamepad",
        "Nintendo Entertainment System gamepad",
        "spi",
        "",
    ),
    ProtocolDefinition::new(
        "nrf24l01",
        "nRF24L01(+)",
        "Nordic Semiconductor nRF24L01(+)",
        "spi",
        "",
    ),
    ProtocolDefinition::new("nrf905", "nRF905", "Nordic Semiconductor nRF905", "spi", ""),
    ProtocolDefinition::new(
        "numbers_and_state",
        "Numbers and State",
        "Interpret bit patters as numbers or state enums",
        "logic",
        "numbers_and_state",
    ),
    ProtocolDefinition::new("nunchuk", "Nunchuk", "Nintendo Wii Nunchuk", "i2c", ""),
    ProtocolDefinition::new(
        "onewire_link",
        "OneWire link layer",
        "1-Wire serial communication bus (link layer)",
        "logic",
        "onewire_link",
    ),
    ProtocolDefinition::new(
        "onewire_network",
        "1-Wire network layer",
        "1-Wire serial communication bus (network layer)",
        "onewire_link",
        "onewire_network",
    ),
    ProtocolDefinition::new("ook", "OOK", "On-off keying", "logic", "ook"),
    ProtocolDefinition::new("ook_oregon", "Oregon", "Oregon Scientific", "ook", ""),
    ProtocolDefinition::new(
        "ook_vis",
        "OOK visualisation",
        "On-off keying visualisation",
        "ook",
        "ook",
    ),
    ProtocolDefinition::new("pan1321", "PAN1321", "Panasonic PAN1321", "uart", ""),
    ProtocolDefinition::new(
        "parallel",
        "Parallel",
        "Parallel sync bus",
        "logic",
        "parallel",
    ),
    ProtocolDefinition::new("pca9571", "PCA9571", "NXP PCA9571", "i2c", ""),
    ProtocolDefinition::new(
        "pjdl",
        "PJDL",
        "Padded Jittering Data Link",
        "logic",
        "pjon_link",
    ),
    ProtocolDefinition::new("pjon", "PJON", "PJON", "pjon_link", ""),
    ProtocolDefinition::new("ps2", "PS/2", "PS/2", "logic", ""),
    ProtocolDefinition::new("pwm", "PWM", "Pulse-width modulation", "logic", ""),
    ProtocolDefinition::new("pxx1", "PXX1", "PXX1 modulation", "logic", ""),
    ProtocolDefinition::new("qi", "Qi", "Qi charger protocol", "logic", ""),
    ProtocolDefinition::new(
        "qspi",
        "QSPI",
        "Quad Serial Peripheral Interface",
        "logic",
        "spi",
    ),
    ProtocolDefinition::new(
        "rc_encode",
        "RC encode",
        "Remote control encoder",
        "logic",
        "",
    ),
    ProtocolDefinition::new("rfm12", "RFM12", "HopeRF RFM12", "spi", ""),
    ProtocolDefinition::new(
        "rgb_led_spi",
        "RGB LED (SPI)",
        "RGB LED string decoder (SPI)",
        "spi",
        "",
    ),
    ProtocolDefinition::new(
        "rgb_led_ws281x",
        "RGB LED WS2812+",
        "RGB LED color decoder",
        "logic",
        "",
    ),
    ProtocolDefinition::new("rtc8564", "RTC-8564", "Epson RTC-8564 JE/NB", "i2c", ""),
    ProtocolDefinition::new(
        "sae_j1850_vpw",
        "SAE J1850 VPW",
        "SAE J1850 VPW.",
        "logic",
        "",
    ),
    ProtocolDefinition::new(
        "sbus_futaba",
        "SBUS (Futaba)",
        "Futaba SBUS (Serial bus)",
        "uart",
        "sbus_futaba",
    ),
    ProtocolDefinition::new("sda2506", "SDA2506", "Siemens SDA 2506-5", "logic", ""),
    ProtocolDefinition::new(
        "sdcard_sd",
        "SD card (SD mode)",
        "Secure Digital card (SD mode)",
        "logic",
        "",
    ),
    ProtocolDefinition::new(
        "sdcard_spi",
        "SD card (SPI mode)",
        "Secure Digital card (SPI mode)",
        "spi",
        "",
    ),
    ProtocolDefinition::new("sdq", "SDQ", "Texas Instruments SDQ", "logic", ""),
    ProtocolDefinition::new(
        "seven_segment",
        "Segment-7",
        "7-segment display",
        "logic",
        "",
    ),
    ProtocolDefinition::new("signature", "Signature", "Signature analysis", "logic", ""),
    ProtocolDefinition::new("sipi", "SIPI (Zipwire)", "NXP SIPI interface", "lfast", ""),
    ProtocolDefinition::new("sle44xx", "SLE 44xx", "SLE44xx memory card", "logic", ""),
    ProtocolDefinition::new(
        "smart_qspi",
        "Smart QSPI",
        "Quad Serial Peripheral Interface",
        "logic",
        "spi",
    ),
    ProtocolDefinition::new(
        "spdif",
        "S/PDIF",
        "Sony/Philips Digital Interface Format",
        "logic",
        "",
    ),
    ProtocolDefinition::new("spi_tpm", "SPI TPM", "SPI TPM transactions", "spi", ""),
    ProtocolDefinition::new("spiflash", "SPI flash", "SPI flash chips", "spi", ""),
    ProtocolDefinition::new(
        "ssi32",
        "SSI32",
        "Synchronous Serial Interface (32bit)",
        "spi",
        "",
    ),
    ProtocolDefinition::new(
        "st25r39xx_spi",
        "ST25R39xx (SPI mode)",
        "STMicroelectronics ST25R39xx",
        "spi",
        "",
    ),
    ProtocolDefinition::new("st7735", "ST7735", "Sitronix ST7735", "logic", ""),
    ProtocolDefinition::new("st7789", "ST7789", "Sitronix ST7789", "logic", ""),
    ProtocolDefinition::new(
        "stepper_motor",
        "Stepper motor",
        "Stepper motor position / speed",
        "logic",
        "",
    ),
    ProtocolDefinition::new("swd", "SWD", "Serial Wire Debug", "logic", "swd"),
    ProtocolDefinition::new("swim", "SWIM", "STM8 SWIM bus", "logic", ""),
    ProtocolDefinition::new("t55xx", "T55xx", "RFID T55xx", "logic", ""),
    ProtocolDefinition::new(
        "tca6408a",
        "TI TCA6408A",
        "Texas Instruments TCA6408A",
        "i2c",
        "",
    ),
    ProtocolDefinition::new(
        "tdm_audio",
        "TDM audio",
        "Time division multiplex audio",
        "logic",
        "",
    ),
    ProtocolDefinition::new(
        "timing",
        "Timing",
        "Timing calculation with frequency and averaging",
        "logic",
        "",
    ),
    ProtocolDefinition::new(
        "tlc5620",
        "TI TLC5620",
        "Texas Instruments TLC5620",
        "logic",
        "",
    ),
    ProtocolDefinition::new(
        "ufcs",
        "UFCS",
        "Universal Fast Charging Specification",
        "uart",
        "",
    ),
    ProtocolDefinition::new(
        "usb_packet",
        "USB packet",
        "Universal Serial Bus (LS/FS) packet",
        "usb_signalling",
        "usb_packet",
    ),
    ProtocolDefinition::new(
        "usb_power_delivery",
        "USB PD",
        "USB Power Delivery",
        "logic",
        "usb_pd",
    ),
    ProtocolDefinition::new(
        "usb_request",
        "USB request",
        "Universal Serial Bus (LS/FS) transaction/request",
        "usb_packet",
        "usb_request",
    ),
    ProtocolDefinition::new(
        "usb_signalling",
        "USB signalling",
        "Universal Serial Bus (LS/FS) signalling",
        "logic",
        "usb_signalling",
    ),
    ProtocolDefinition::new("wiegand", "Wiegand", "Wiegand interface", "logic", ""),
    ProtocolDefinition::new("x2444m", "X2444M/P", "Xicor X2444M/P", "spi", ""),
    ProtocolDefinition::new(
        "xfp",
        "XFP",
        "10 Gigabit Small Form Factor Pluggable Module (XFP)",
        "i2c",
        "",
    ),
    ProtocolDefinition::new(
        "xy2-100",
        "XY2-100",
        "XY2-100(E) and XY-200(E) galvanometer protocol",
        "logic",
        "",
    ),
    ProtocolDefinition::new("z80", "Z80", "Zilog Z80 CPU", "logic", ""),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn logic_word(clock: bool, data: bool, chip_select: bool) -> u32 {
        u32::from(clock)
            | (u32::from(data) << 1)
            | (u32::from(data) << 2)
            | (u32::from(chip_select) << 3)
    }

    fn i2c_transaction(bytes: &[(u8, bool)]) -> Vec<u32> {
        let mut samples = vec![logic_word(true, true, true), logic_word(true, false, true)];
        for &(byte, acknowledged) in bytes {
            for bit in (0..8).rev() {
                let data = byte & (1 << bit) != 0;
                samples.extend([
                    logic_word(false, data, true),
                    logic_word(true, data, true),
                    logic_word(false, data, true),
                ]);
            }
            let acknowledge_level = !acknowledged;
            samples.extend([
                logic_word(false, acknowledge_level, true),
                logic_word(true, acknowledge_level, true),
                logic_word(false, acknowledge_level, true),
            ]);
        }
        samples.extend([
            logic_word(false, false, true),
            logic_word(true, false, true),
            logic_word(true, true, true),
        ]);
        samples
    }

    fn analyzer(
        protocol_id: &'static str,
        mappings: Vec<Option<usize>>,
    ) -> ProtocolAnalyzerInstance {
        ProtocolAnalyzerInstance {
            instance_id: 0,
            protocol_id,
            channel_mappings: mappings,
            data_format: ProtocolDataFormat::Hexadecimal,
            custom_protocol: None,
        }
    }

    fn uart_8n1(bytes: &[u8], samples_per_bit: usize) -> Vec<u32> {
        let mut samples = vec![1; samples_per_bit * 2];
        for byte in bytes {
            samples.extend(std::iter::repeat(0).take(samples_per_bit));
            for bit in 0..8 {
                let level = u32::from(byte & (1 << bit) != 0);
                samples.extend(std::iter::repeat(level).take(samples_per_bit));
            }
            samples.extend(std::iter::repeat(1).take(samples_per_bit * 2));
        }
        samples
    }

    fn crc8_smbus(bytes: &[u8]) -> u8 {
        let mut crc = 0_u8;
        for byte in bytes {
            crc ^= byte;
            for _ in 0..8 {
                crc = if crc & 0x80 != 0 {
                    (crc << 1) ^ 0x07
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    #[test]
    fn catalog_matches_original_decoder_count_and_has_unique_ids() {
        assert_eq!(PROTOCOLS.len(), 150);
        let ids = PROTOCOLS
            .iter()
            .map(|protocol| protocol.id)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), PROTOCOLS.len());
    }

    #[test]
    fn every_original_protocol_resolves_to_raw_logic() {
        for protocol in PROTOCOLS {
            let stack = protocol_stack(protocol.id)
                .unwrap_or_else(|| panic!("{} should have a complete decoder stack", protocol.id));
            assert_eq!(stack[0].input, "logic", "{}", protocol.id);
            assert_eq!(stack.last().unwrap().id, protocol.id);
        }
    }

    #[test]
    fn stacked_spi_protocol_uses_original_primary_spi_decoder() {
        let stack = protocol_stack("spiflash").unwrap();
        assert_eq!(
            stack.iter().map(|item| item.id).collect::<Vec<_>>(),
            ["1:spi", "spiflash"]
        );
    }

    #[test]
    fn adding_protocol_assigns_required_logic_channels() {
        let mut settings = ProtocolAnalyzerSettings::default();
        assert!(settings.begin_add("i2cdemux"));
        let editor = settings.editor.clone().unwrap();
        settings.save_editor(editor);
        assert_eq!(
            protocol_stack(settings.analyzers[0].protocol_id).unwrap()[0].id,
            "1:i2c"
        );
        assert_eq!(settings.analyzers[0].channel_mappings, [Some(0), Some(1)]);
    }

    #[test]
    fn adding_through_editor_is_transactional() {
        let mut settings = ProtocolAnalyzerSettings::default();
        assert!(settings.begin_add("spiflash"));
        assert!(settings.analyzers.is_empty());
        let editor = settings.editor.clone().unwrap();
        settings.save_editor(editor);
        assert_eq!(settings.analyzers.len(), 1);

        assert!(settings.begin_edit(0));
        settings.editor.as_mut().unwrap().analyzer.data_format = ProtocolDataFormat::Ascii;
        settings.cancel_editor();
        assert_eq!(
            settings.analyzers[0].data_format,
            ProtocolDataFormat::Hexadecimal
        );
    }

    #[test]
    fn i2c_decoder_separates_address_data_and_acknowledgements() {
        let outcome = decode_protocol_samples(
            &analyzer("1:i2c", vec![Some(0), Some(1)]),
            &i2c_transaction(&[(0xa0, true), (0x5a, true)]),
            100,
            1_000_000,
        );
        let ProtocolDecodeOutcome::Events(events) = outcome else {
            panic!("I²C should be decoded");
        };

        assert_eq!(events.len(), 4);
        assert_eq!(events[0].text, "ADDR 50 W");
        assert!(events[0].detail.contains("address 0x50"));
        assert!(events[0].start_sample >= 100);
        assert!(events[0].end_sample > events[0].start_sample);
        assert!(!events[0].warning);
        assert_eq!(events[1].text, "ACK");
        assert!(events[1].start_sample >= events[0].end_sample);
        assert_eq!(events[2].text, "5A");
        assert!(events[2].detail.contains("data 0x5A"));
        assert_eq!(events[3].text, "ACK");
    }

    #[test]
    fn i2c_decoder_marks_unacknowledged_bytes() {
        let outcome = decode_protocol_samples(
            &analyzer("0:i2c", vec![Some(0), Some(1)]),
            &i2c_transaction(&[(0x43, false)]),
            0,
            1_000_000,
        );
        let ProtocolDecodeOutcome::Events(events) = outcome else {
            panic!("I²C should be decoded");
        };

        assert_eq!(events[0].text, "ADDR 21 R");
        assert!(!events[0].warning);
        assert_eq!(events[1].text, "NACK");
        assert!(events[1].warning);
    }

    #[test]
    fn spi_decoder_emits_mosi_and_miso_bytes() {
        let mut samples = vec![
            logic_word(false, false, true),
            logic_word(false, false, false),
        ];
        for bit in (0..8).rev() {
            let mosi = 0xa5 & (1 << bit) != 0;
            let miso = 0x3c & (1 << bit) != 0;
            let low = u32::from(miso) << 1 | u32::from(mosi) << 2;
            samples.extend([low, low | 1, low]);
        }
        samples.push(logic_word(false, false, true));
        let outcome = decode_protocol_samples(
            &analyzer("1:spi", vec![Some(0), Some(1), Some(2), Some(3)]),
            &samples,
            0,
            1_000_000,
        );
        let ProtocolDecodeOutcome::Events(events) = outcome else {
            panic!("SPI should be decoded");
        };

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text, "MOSI A5 · MISO 3C");
    }

    #[test]
    fn uart_decoder_emits_ascii_text_bytes() {
        let mut uart = analyzer("1:uart", vec![Some(0)]);
        uart.data_format = ProtocolDataFormat::Ascii;
        let outcome = decode_protocol_samples(&uart, &uart_8n1(b"Hi", 10), 500, 1_152_000);
        let ProtocolDecodeOutcome::Events(events) = outcome else {
            panic!("UART should be decoded");
        };

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].text, "'H'");
        assert_eq!(events[1].text, "'i'");
        assert!(events[0].detail.contains("115200 baud"));
        assert!(matches!(
            events[0].kind,
            ProtocolDecodedKind::Uart { value: b'H' }
        ));
    }

    #[test]
    fn custom_uart_yaml_decodes_into_generic_gui_event() {
        let source =
            include_str!("../../../docs/examples/motor-controller.wavecraft-protocol.yaml");
        let mut settings = ProtocolAnalyzerSettings::default();
        settings
            .begin_add_custom_yaml(
                PathBuf::from("motor-controller.wavecraft-protocol.yaml"),
                source.to_owned(),
            )
            .unwrap();
        let editor = settings.editor.clone().unwrap();
        settings.save_editor(editor);

        let mut frame = vec![0xaa, 0x55, 0x05, 0x10, 0x02, 0xd0, 0x07, 0x03];
        frame.push(crc8_smbus(&frame[2..]));
        let outcome = decode_protocol_samples(
            &settings.analyzers[0],
            &uart_8n1(&frame, 10),
            500,
            1_152_000,
        );
        let ProtocolDecodeOutcome::Events(events) = outcome else {
            panic!("custom UART protocol should be decoded");
        };

        assert_eq!(analyzer_name(&settings.analyzers[0]), "Motor Controller");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text, "Motor 2: 2000 rpm");
        assert!(events[0].detail.contains("speed_rpm=2000 rpm"));
        assert!(!events[0].warning);
        assert_eq!(events[0].kind, ProtocolDecodedKind::Custom);
        assert!(events[0].start_sample >= 500);
    }
}
