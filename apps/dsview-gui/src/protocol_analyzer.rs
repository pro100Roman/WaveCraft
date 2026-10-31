use std::collections::HashSet;

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
pub struct ProtocolAnalyzerInstance {
    pub instance_id: u64,
    pub protocol_id: &'static str,
    pub channel_mappings: Vec<Option<usize>>,
    pub data_format: ProtocolDataFormat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolAnalyzerEditor {
    pub analyzer: ProtocolAnalyzerInstance,
    pub target_index: Option<usize>,
}

#[derive(Debug, Default)]
pub struct ProtocolAnalyzerSettings {
    pub search: String,
    pub picker_open: bool,
    pub analyzers: Vec<ProtocolAnalyzerInstance>,
    pub editor: Option<ProtocolAnalyzerEditor>,
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
}
