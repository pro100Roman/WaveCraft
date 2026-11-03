use crate::{
    ByteStreamFramer, CustomProtocolDecoder, DecodeDiagnostic, DecodeError, DecodedMessage,
    FramedBytes, FramingOutput, FramingSchema, ProtocolSchema, SampleRange, Severity, TimedByte,
    TransportPacket,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProtocolRuntimeOutput {
    pub messages: Vec<DecodedMessage>,
    pub diagnostics: Vec<DecodeDiagnostic>,
}

#[derive(Clone, Debug)]
pub struct CustomProtocolRuntime {
    schema: ProtocolSchema,
    decoder: CustomProtocolDecoder,
    framer: Option<ByteStreamFramer>,
    previous_byte_end: Option<u64>,
}

impl CustomProtocolRuntime {
    pub fn new(schema: ProtocolSchema) -> Result<Self, DecodeError> {
        let framer = if matches!(schema.framing, FramingSchema::TransportTransaction { .. }) {
            None
        } else {
            Some(ByteStreamFramer::new(schema.framing.clone())?)
        };
        Ok(Self {
            decoder: CustomProtocolDecoder::new(schema.clone()),
            schema,
            framer,
            previous_byte_end: None,
        })
    }

    pub fn schema(&self) -> &ProtocolSchema {
        &self.schema
    }

    pub fn push_bytes(
        &mut self,
        bytes: &[TimedByte],
        sample_rate_hz: u64,
    ) -> Result<ProtocolRuntimeOutput, DecodeError> {
        let Some(framer) = &mut self.framer else {
            return Err(DecodeError::new(
                "transport_transaction framing requires complete transport packets",
            ));
        };
        let mut framing_output = FramingOutput::default();
        if matches!(self.schema.framing, FramingSchema::Gap { .. }) {
            if sample_rate_hz == 0 {
                return Err(DecodeError::new(
                    "gap framing requires a sample rate greater than zero",
                ));
            }
            for byte in bytes {
                if let Some(previous_end) = self.previous_byte_end {
                    let gap_samples = byte.range.start.saturating_sub(previous_end);
                    let gap_us = gap_samples
                        .saturating_mul(1_000_000)
                        .checked_div(sample_rate_hz)
                        .unwrap_or(0);
                    append_framing_output(&mut framing_output, framer.push_gap(gap_us));
                }
                append_framing_output(
                    &mut framing_output,
                    framer.push_bytes(std::slice::from_ref(byte))?,
                );
                self.previous_byte_end = Some(byte.range.end);
            }
        } else {
            append_framing_output(&mut framing_output, framer.push_bytes(bytes)?);
            if let Some(byte) = bytes.last() {
                self.previous_byte_end = Some(byte.range.end);
            }
        }
        Ok(self.decode_framing_output(framing_output, false))
    }

    pub fn push_packet(
        &mut self,
        packet: &TransportPacket,
        sample_rate_hz: u64,
    ) -> ProtocolRuntimeOutput {
        if packet.transport != self.schema.input.transport {
            return ProtocolRuntimeOutput {
                messages: Vec::new(),
                diagnostics: vec![runtime_diagnostic(
                    "protocol.wrong_transport",
                    format!(
                        "protocol expects {:?} input but received {:?}",
                        self.schema.input.transport, packet.transport
                    ),
                    None,
                )],
            };
        }
        if self.framer.is_none() {
            let mut output = ProtocolRuntimeOutput::default();
            self.decode_packet(
                packet,
                &mut output,
                packet.completeness == crate::PacketCompleteness::Incomplete,
            );
            return output;
        }
        let Some(bytes) = packet.streams.get(&self.schema.input.stream) else {
            return ProtocolRuntimeOutput {
                messages: Vec::new(),
                diagnostics: vec![runtime_diagnostic(
                    "protocol.missing_stream",
                    format!(
                        "transport packet does not contain stream {:?}",
                        self.schema.input.stream
                    ),
                    None,
                )],
            };
        };
        self.push_bytes(bytes, sample_rate_hz)
            .unwrap_or_else(|error| ProtocolRuntimeOutput {
                messages: Vec::new(),
                diagnostics: vec![runtime_diagnostic(
                    "protocol.runtime_error",
                    error.to_string(),
                    timed_bytes_range(bytes),
                )],
            })
    }

    pub fn finish(&mut self) -> ProtocolRuntimeOutput {
        let Some(frame) = self.framer.as_mut().and_then(ByteStreamFramer::finish) else {
            self.previous_byte_end = None;
            return ProtocolRuntimeOutput::default();
        };
        let mut output = ProtocolRuntimeOutput::default();
        self.decode_frame(frame, &mut output, true);
        self.previous_byte_end = None;
        output
    }

    pub fn reset(&mut self) {
        self.framer = if matches!(
            self.schema.framing,
            FramingSchema::TransportTransaction { .. }
        ) {
            None
        } else {
            Some(
                ByteStreamFramer::new(self.schema.framing.clone())
                    .expect("validated framing remains valid"),
            )
        };
        self.previous_byte_end = None;
    }

    fn decode_framing_output(
        &self,
        framing_output: FramingOutput,
        finishing: bool,
    ) -> ProtocolRuntimeOutput {
        let mut output = ProtocolRuntimeOutput {
            messages: Vec::new(),
            diagnostics: framing_output.diagnostics,
        };
        for frame in framing_output.frames {
            self.decode_frame(frame, &mut output, finishing);
        }
        output
    }

    fn decode_frame(
        &self,
        frame: FramedBytes,
        output: &mut ProtocolRuntimeOutput,
        finishing: bool,
    ) {
        let range = timed_bytes_range(&frame.bytes);
        match self.decoder.decode_framed_bytes(frame) {
            Ok(Some(message)) => {
                output.messages.push(message);
                if finishing {
                    output.diagnostics.push(DecodeDiagnostic {
                        code: "protocol.incomplete_frame".to_owned(),
                        message: "protocol stream ended before the frame boundary".to_owned(),
                        severity: Severity::Warning,
                        range,
                    });
                }
            }
            Ok(None) => {
                if finishing {
                    output.diagnostics.push(DecodeDiagnostic {
                        code: "protocol.incomplete_frame".to_owned(),
                        message: "protocol stream ended before the frame boundary".to_owned(),
                        severity: Severity::Warning,
                        range,
                    });
                }
            }
            Err(error) => {
                let mut diagnostic = runtime_diagnostic(
                    if finishing {
                        "protocol.incomplete_frame"
                    } else {
                        "protocol.decode_failed"
                    },
                    error.to_string(),
                    range,
                );
                if finishing {
                    diagnostic.severity = Severity::Warning;
                }
                output.diagnostics.push(diagnostic);
            }
        }
    }

    fn decode_packet(
        &self,
        packet: &TransportPacket,
        output: &mut ProtocolRuntimeOutput,
        finishing: bool,
    ) {
        let range = packet
            .streams
            .get(&self.schema.input.stream)
            .and_then(|bytes| timed_bytes_range(bytes));
        match self.decoder.decode_packet(packet) {
            Ok(Some(message)) => {
                output.messages.push(message);
                if finishing {
                    output.diagnostics.push(DecodeDiagnostic {
                        code: "protocol.incomplete_packet".to_owned(),
                        message: "transport stream ended before the packet boundary".to_owned(),
                        severity: Severity::Warning,
                        range,
                    });
                }
            }
            Ok(None) => {
                if finishing {
                    output.diagnostics.push(DecodeDiagnostic {
                        code: "protocol.incomplete_packet".to_owned(),
                        message: "transport stream ended before the packet boundary".to_owned(),
                        severity: Severity::Warning,
                        range,
                    });
                }
            }
            Err(error) => {
                let mut diagnostic = runtime_diagnostic(
                    if finishing {
                        "protocol.incomplete_packet"
                    } else {
                        "protocol.decode_failed"
                    },
                    error.to_string(),
                    range,
                );
                if finishing {
                    diagnostic.severity = Severity::Warning;
                }
                output.diagnostics.push(diagnostic);
            }
        }
    }
}

fn append_framing_output(target: &mut FramingOutput, mut source: FramingOutput) {
    target.frames.append(&mut source.frames);
    target.diagnostics.append(&mut source.diagnostics);
}

fn timed_bytes_range(bytes: &[TimedByte]) -> Option<SampleRange> {
    bytes.first().zip(bytes.last()).and_then(|(first, last)| {
        (first.range.start < last.range.end).then_some(SampleRange {
            start: first.range.start,
            end: last.range.end,
        })
    })
}

fn runtime_diagnostic(
    code: &str,
    message: impl Into<String>,
    range: Option<SampleRange>,
) -> DecodeDiagnostic {
    DecodeDiagnostic {
        code: code.to_owned(),
        message: message.into(),
        severity: Severity::Error,
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{load_protocol_schema, ByteStatus};

    const SCHEMA: &str = r#"
schema: wavecraft.protocol/v1
protocol:
  id: example.stream
  name: Stream
  version: 1
input:
  transport: uart
  stream: data
framing:
  strategy: fixed_length
  bytes: 3
messages:
  - id: value
    name: Value
    default: true
    fields:
      - name: first
        type: u8
      - name: second
        type: u8
      - name: third
        type: u8
    summary: "{first}:{second}:{third}"
"#;
    const GAP_SCHEMA: &str = r#"
schema: wavecraft.protocol/v1
protocol:
  id: example.gap-stream
  name: Gap stream
  version: 1
input:
  transport: uart
  stream: data
framing:
  strategy: gap
  minimum_us: 50
  max_frame_bytes: 16
messages:
  - id: value
    name: Value
    default: true
    fields:
      - name: value
        type: u8
    summary: "{value}"
"#;
    const TRANSACTION_SCHEMA: &str = r#"
schema: wavecraft.protocol/v1
protocol:
  id: example.transaction
  name: Transaction
  version: 1
input:
  transport: i2c
  stream: data
framing:
  strategy: transport_transaction
  max_frame_bytes: 16
messages:
  - id: value
    name: Value
    default: true
    fields:
      - name: value
        type: u8
    summary: "{value}"
"#;

    fn timed(values: &[u8], first_sample: u64) -> Vec<TimedByte> {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| TimedByte {
                value: *value,
                range: SampleRange {
                    start: first_sample + index as u64 * 10,
                    end: first_sample + index as u64 * 10 + 8,
                },
                status: ByteStatus::default(),
            })
            .collect()
    }

    #[test]
    fn protocol_runtime_preserves_frames_across_input_chunks() {
        let schema = load_protocol_schema(SCHEMA).unwrap();
        let mut runtime = CustomProtocolRuntime::new(schema).unwrap();
        assert!(runtime
            .push_bytes(&timed(&[1], 0), 1)
            .unwrap()
            .messages
            .is_empty());
        let output = runtime.push_bytes(&timed(&[2, 3], 10), 1).unwrap();
        assert_eq!(output.messages.len(), 1);
        assert_eq!(output.messages[0].summary, "1:2:3");
        assert!(output.diagnostics.is_empty());
    }

    #[test]
    fn protocol_runtime_flushes_an_incomplete_tail_as_a_diagnostic() {
        let schema = load_protocol_schema(SCHEMA).unwrap();
        let mut runtime = CustomProtocolRuntime::new(schema).unwrap();
        runtime.push_bytes(&timed(&[1, 2], 0), 1).unwrap();
        let output = runtime.finish();
        assert!(output.messages.is_empty());
        assert_eq!(output.diagnostics[0].code, "protocol.incomplete_frame");
        assert_eq!(
            output.diagnostics[0].range,
            Some(SampleRange { start: 0, end: 18 })
        );
    }

    #[test]
    fn reset_drops_partial_state_at_a_discontinuity() {
        let schema = load_protocol_schema(SCHEMA).unwrap();
        let mut runtime = CustomProtocolRuntime::new(schema).unwrap();
        runtime.push_bytes(&timed(&[1, 2], 0), 1).unwrap();
        runtime.reset();
        assert!(runtime
            .push_bytes(&timed(&[3], 100), 1)
            .unwrap()
            .messages
            .is_empty());
        assert!(runtime.finish().diagnostics.len() == 1);
    }

    #[test]
    fn gap_framing_preserves_the_previous_byte_across_batches() {
        let schema = load_protocol_schema(GAP_SCHEMA).unwrap();
        let mut runtime = CustomProtocolRuntime::new(schema).unwrap();
        assert!(runtime
            .push_bytes(&timed(&[1], 0), 1_000_000)
            .unwrap()
            .messages
            .is_empty());
        let output = runtime.push_bytes(&timed(&[2], 100), 1_000_000).unwrap();
        assert_eq!(output.messages[0].summary, "1");

        let tail = runtime.finish();
        assert_eq!(tail.messages[0].summary, "2");
        assert_eq!(tail.diagnostics[0].code, "protocol.incomplete_frame");
    }

    #[test]
    fn incomplete_transport_packet_keeps_message_and_reports_diagnostic() {
        let schema = load_protocol_schema(TRANSACTION_SCHEMA).unwrap();
        let mut runtime = CustomProtocolRuntime::new(schema).unwrap();
        let packet = TransportPacket::single_stream(
            crate::TransportKind::I2c,
            "data",
            timed(&[7], 20),
            crate::PacketCompleteness::Incomplete,
        );
        let output = runtime.push_packet(&packet, 1_000_000);
        assert_eq!(output.messages[0].summary, "7");
        assert_eq!(output.diagnostics[0].code, "protocol.incomplete_packet");
    }
}
