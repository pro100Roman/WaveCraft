//! Persistent newline-delimited JSON helper server.

use std::io::{self, BufRead, Write};

use wavecraft_debug_probe::{
    DEBUG_PROBE_HELPER_PROTOCOL_VERSION, DebugProbeCommand, DebugProbeDriver, DebugProbeError,
    DebugProbeErrorKind, DebugProbeHandle, DebugProbeReply, DebugProbeRequest, DebugProbeResponse,
    DebugSession, MAX_DEBUG_PROBE_HELPER_MESSAGE_BYTES, MAX_DEBUG_PROBE_TRANSFER_BYTES,
};

/// Runs one stateful helper session until shutdown or end-of-input.
pub fn serve(
    driver: impl DebugProbeDriver,
    mut reader: impl BufRead,
    mut writer: impl Write,
) -> io::Result<()> {
    let mut server = HelperServer {
        driver,
        state: HelperState::Idle,
    };
    loop {
        let message = match read_message(&mut reader)? {
            ReadMessage::EndOfInput => return Ok(()),
            ReadMessage::TooLong => {
                write_response(
                    &mut writer,
                    &DebugProbeResponse::error(
                        0,
                        protocol_error("debug-probe helper request is too large"),
                    ),
                )?;
                continue;
            }
            ReadMessage::Message(message) => message,
        };
        let request = match serde_json::from_slice::<DebugProbeRequest>(&message) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut writer,
                    &DebugProbeResponse::error(
                        0,
                        protocol_error(format!("invalid debug-probe helper request: {error}")),
                    ),
                )?;
                continue;
            }
        };
        let request_id = request.request_id;
        let (response, shutdown) =
            if request.protocol_version != DEBUG_PROBE_HELPER_PROTOCOL_VERSION {
                (
                    DebugProbeResponse::error(
                        request_id,
                        DebugProbeError::new(
                            DebugProbeErrorKind::Unsupported,
                            format!(
                                "unsupported debug-probe helper protocol version {}; expected {}",
                                request.protocol_version, DEBUG_PROBE_HELPER_PROTOCOL_VERSION
                            ),
                        ),
                    ),
                    false,
                )
            } else {
                let shutdown = request.command == DebugProbeCommand::Shutdown;
                let response = match server.execute(request.command) {
                    Ok(reply) => DebugProbeResponse::success(request_id, reply),
                    Err(error) => DebugProbeResponse::error(request_id, error),
                };
                (response, shutdown)
            };
        write_response(&mut writer, &response)?;
        if shutdown {
            return Ok(());
        }
    }
}

struct HelperServer<D> {
    driver: D,
    state: HelperState,
}

enum HelperState {
    Idle,
    Probe(Box<dyn DebugProbeHandle>),
    Session(Box<dyn DebugSession>),
}

impl<D: DebugProbeDriver> HelperServer<D> {
    fn execute(&mut self, command: DebugProbeCommand) -> Result<DebugProbeReply, DebugProbeError> {
        match command {
            DebugProbeCommand::ListProbes => self.driver.list_probes().map(DebugProbeReply::Probes),
            DebugProbeCommand::OpenProbe { selector } => {
                if !matches!(self.state, HelperState::Idle) {
                    return Err(invalid_state(
                        "close the current probe before opening another",
                    ));
                }
                let probe = self.driver.open_probe(&selector)?;
                let descriptor = probe.descriptor().clone();
                self.state = HelperState::Probe(probe);
                Ok(DebugProbeReply::ProbeOpened(descriptor))
            }
            DebugProbeCommand::Attach { configuration } => {
                let state = std::mem::replace(&mut self.state, HelperState::Idle);
                let HelperState::Probe(probe) = state else {
                    self.state = state;
                    return Err(invalid_state("open a probe before attaching to a target"));
                };
                let session = probe.attach(&configuration)?;
                self.state = HelperState::Session(session);
                Ok(DebugProbeReply::TargetAttached)
            }
            DebugProbeCommand::ReadMemory { address, length } => {
                validate_transfer_length(length)?;
                let session = self.session_mut()?;
                let mut data = vec![0; length];
                session.read_memory(address, &mut data)?;
                Ok(DebugProbeReply::MemoryData(data))
            }
            DebugProbeCommand::WriteMemory { address, data } => {
                validate_transfer_length(data.len())?;
                self.session_mut()?.write_memory(address, &data)?;
                Ok(DebugProbeReply::MemoryWritten)
            }
            DebugProbeCommand::AttachRtt { configuration } => self
                .session_mut()?
                .attach_rtt(&configuration)
                .map(DebugProbeReply::RttAttached),
            DebugProbeCommand::ReadRttUp {
                channel_number,
                maximum_length,
            } => {
                validate_transfer_length(maximum_length)?;
                let session = self.session_mut()?;
                let mut data = vec![0; maximum_length];
                let count = session.read_rtt_up(channel_number, &mut data)?;
                if count > data.len() {
                    return Err(DebugProbeError::new(
                        DebugProbeErrorKind::Transport,
                        "debug-probe backend returned an invalid RTT byte count",
                    ));
                }
                data.truncate(count);
                Ok(DebugProbeReply::RttData(data))
            }
            DebugProbeCommand::WriteRttDown {
                channel_number,
                data,
            } => {
                validate_transfer_length(data.len())?;
                self.session_mut()?
                    .write_rtt_down(channel_number, &data)
                    .map(DebugProbeReply::RttBytesWritten)
            }
            DebugProbeCommand::CloseSession => {
                self.state = HelperState::Idle;
                Ok(DebugProbeReply::SessionClosed)
            }
            DebugProbeCommand::Shutdown => {
                self.state = HelperState::Idle;
                Ok(DebugProbeReply::ShuttingDown)
            }
        }
    }

    fn session_mut(&mut self) -> Result<&mut dyn DebugSession, DebugProbeError> {
        match &mut self.state {
            HelperState::Session(session) => Ok(session.as_mut()),
            HelperState::Idle | HelperState::Probe(_) => {
                Err(invalid_state("attach to a target before using the session"))
            }
        }
    }
}

fn validate_transfer_length(length: usize) -> Result<(), DebugProbeError> {
    if (1..=MAX_DEBUG_PROBE_TRANSFER_BYTES).contains(&length) {
        Ok(())
    } else {
        Err(DebugProbeError::new(
            DebugProbeErrorKind::InvalidConfiguration,
            format!(
                "debug-probe transfer length must be between 1 and {MAX_DEBUG_PROBE_TRANSFER_BYTES} bytes"
            ),
        ))
    }
}

enum ReadMessage {
    EndOfInput,
    Message(Vec<u8>),
    TooLong,
}

fn read_message(reader: &mut impl BufRead) -> io::Result<ReadMessage> {
    let mut message = Vec::new();
    let mut too_long = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if too_long {
                Ok(ReadMessage::TooLong)
            } else if message.is_empty() {
                Ok(ReadMessage::EndOfInput)
            } else {
                trim_carriage_return(&mut message);
                Ok(ReadMessage::Message(message))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let content_length = newline.unwrap_or(consumed);
        if !too_long {
            if message.len().saturating_add(content_length) > MAX_DEBUG_PROBE_HELPER_MESSAGE_BYTES {
                too_long = true;
                message.clear();
            } else {
                message.extend_from_slice(&available[..content_length]);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            if too_long {
                return Ok(ReadMessage::TooLong);
            }
            trim_carriage_return(&mut message);
            return Ok(ReadMessage::Message(message));
        }
    }
}

fn trim_carriage_return(message: &mut Vec<u8>) {
    if message.last() == Some(&b'\r') {
        message.pop();
    }
}

fn write_response(writer: &mut impl Write, response: &DebugProbeResponse) -> io::Result<()> {
    let encoded = serde_json::to_vec(response).map_err(io::Error::other)?;
    if encoded.len() > MAX_DEBUG_PROBE_HELPER_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "debug-probe helper response is too large",
        ));
    }
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn invalid_state(message: &str) -> DebugProbeError {
    DebugProbeError::new(DebugProbeErrorKind::InvalidConfiguration, message)
}

fn protocol_error(message: impl Into<String>) -> DebugProbeError {
    DebugProbeError::new(DebugProbeErrorKind::InvalidConfiguration, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use wavecraft_debug_probe::{
        AttachConfiguration, DebugProbeHandle, ProbeDescriptor, ProbeProvider, ProbeSelector,
        RttChannelDescriptor, RttChannelDirection, RttConfiguration,
    };

    #[derive(Clone, Copy)]
    struct FakeDriver;

    impl DebugProbeDriver for FakeDriver {
        fn list_probes(&self) -> Result<Vec<ProbeDescriptor>, DebugProbeError> {
            Ok(vec![descriptor()])
        }

        fn open_probe(
            &self,
            selector: &ProbeSelector,
        ) -> Result<Box<dyn DebugProbeHandle>, DebugProbeError> {
            if selector.connection_id == descriptor().connection_id {
                Ok(Box::new(FakeProbe {
                    descriptor: descriptor(),
                }))
            } else {
                Err(DebugProbeError::new(
                    DebugProbeErrorKind::NotFound,
                    "fake probe was not found",
                ))
            }
        }
    }

    struct FakeProbe {
        descriptor: ProbeDescriptor,
    }

    impl DebugProbeHandle for FakeProbe {
        fn descriptor(&self) -> &ProbeDescriptor {
            &self.descriptor
        }

        fn attach(
            self: Box<Self>,
            _configuration: &AttachConfiguration,
        ) -> Result<Box<dyn DebugSession>, DebugProbeError> {
            Ok(Box::new(FakeSession {
                memory: vec![0; 16],
                rtt_attached: false,
            }))
        }
    }

    struct FakeSession {
        memory: Vec<u8>,
        rtt_attached: bool,
    }

    impl DebugSession for FakeSession {
        fn read_memory(
            &mut self,
            address: u64,
            destination: &mut [u8],
        ) -> Result<(), DebugProbeError> {
            let start = usize::try_from(address).map_err(|_| target_error())?;
            let end = start
                .checked_add(destination.len())
                .ok_or_else(target_error)?;
            let source = self.memory.get(start..end).ok_or_else(target_error)?;
            destination.copy_from_slice(source);
            Ok(())
        }

        fn write_memory(&mut self, address: u64, source: &[u8]) -> Result<(), DebugProbeError> {
            let start = usize::try_from(address).map_err(|_| target_error())?;
            let end = start.checked_add(source.len()).ok_or_else(target_error)?;
            let destination = self.memory.get_mut(start..end).ok_or_else(target_error)?;
            destination.copy_from_slice(source);
            Ok(())
        }

        fn attach_rtt(
            &mut self,
            _configuration: &RttConfiguration,
        ) -> Result<Vec<RttChannelDescriptor>, DebugProbeError> {
            self.rtt_attached = true;
            Ok(vec![RttChannelDescriptor {
                number: 1,
                direction: RttChannelDirection::Up,
                name: Some("SystemView".to_owned()),
                buffer_size: 1024,
            }])
        }

        fn read_rtt_up(
            &mut self,
            channel_number: usize,
            destination: &mut [u8],
        ) -> Result<usize, DebugProbeError> {
            require_rtt(self.rtt_attached, channel_number)?;
            let source = [1, 2, 3];
            let count = source.len().min(destination.len());
            destination[..count].copy_from_slice(&source[..count]);
            Ok(count)
        }

        fn write_rtt_down(
            &mut self,
            channel_number: usize,
            source: &[u8],
        ) -> Result<usize, DebugProbeError> {
            require_rtt(self.rtt_attached, channel_number)?;
            Ok(source.len())
        }
    }

    fn descriptor() -> ProbeDescriptor {
        ProbeDescriptor {
            connection_id: "1366:0105:1234".to_owned(),
            provider: ProbeProvider::JLink,
            vendor: Some("SEGGER".to_owned()),
            model: "J-Link".to_owned(),
            serial_number: Some("1234".to_owned()),
        }
    }

    fn target_error() -> DebugProbeError {
        DebugProbeError::new(
            DebugProbeErrorKind::Target,
            "address is outside fake memory",
        )
    }

    fn require_rtt(attached: bool, channel_number: usize) -> Result<(), DebugProbeError> {
        if attached && channel_number == 1 {
            Ok(())
        } else {
            Err(DebugProbeError::new(
                DebugProbeErrorKind::InvalidConfiguration,
                "fake RTT channel is not attached",
            ))
        }
    }

    fn run_requests(
        commands: impl IntoIterator<Item = DebugProbeCommand>,
    ) -> Vec<DebugProbeResponse> {
        let mut input = Vec::new();
        for (index, command) in commands.into_iter().enumerate() {
            serde_json::to_writer(
                &mut input,
                &DebugProbeRequest::new(index as u64 + 1, command),
            )
            .expect("request serializes");
            input.push(b'\n');
        }
        let mut output = Vec::new();
        serve(FakeDriver, Cursor::new(input), &mut output).expect("helper serves requests");
        String::from_utf8(output)
            .expect("responses are UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("response deserializes"))
            .collect()
    }

    #[test]
    fn serves_a_complete_probe_and_rtt_session() {
        let responses = run_requests([
            DebugProbeCommand::ListProbes,
            DebugProbeCommand::OpenProbe {
                selector: ProbeSelector::connection_id("1366:0105:1234"),
            },
            DebugProbeCommand::Attach {
                configuration: AttachConfiguration::default(),
            },
            DebugProbeCommand::WriteMemory {
                address: 4,
                data: vec![9, 8, 7],
            },
            DebugProbeCommand::ReadMemory {
                address: 4,
                length: 3,
            },
            DebugProbeCommand::AttachRtt {
                configuration: RttConfiguration::default(),
            },
            DebugProbeCommand::ReadRttUp {
                channel_number: 1,
                maximum_length: 16,
            },
            DebugProbeCommand::WriteRttDown {
                channel_number: 1,
                data: vec![0x01, 0x00],
            },
            DebugProbeCommand::CloseSession,
            DebugProbeCommand::Shutdown,
        ]);

        assert_eq!(responses.len(), 10);
        assert_eq!(
            responses[0].outcome,
            wavecraft_debug_probe::DebugProbeResponseOutcome::Success(DebugProbeReply::Probes(
                vec![descriptor()]
            ))
        );
        assert_eq!(
            responses[4].outcome,
            wavecraft_debug_probe::DebugProbeResponseOutcome::Success(DebugProbeReply::MemoryData(
                vec![9, 8, 7]
            ))
        );
        assert_eq!(
            responses[6].outcome,
            wavecraft_debug_probe::DebugProbeResponseOutcome::Success(DebugProbeReply::RttData(
                vec![1, 2, 3]
            ))
        );
        assert_eq!(
            responses[7].outcome,
            wavecraft_debug_probe::DebugProbeResponseOutcome::Success(
                DebugProbeReply::RttBytesWritten(2)
            )
        );
        assert_eq!(
            responses[9].outcome,
            wavecraft_debug_probe::DebugProbeResponseOutcome::Success(
                DebugProbeReply::ShuttingDown
            )
        );
    }

    #[test]
    fn rejects_session_operations_before_attach() {
        let responses = run_requests([
            DebugProbeCommand::ReadMemory {
                address: 0,
                length: 1,
            },
            DebugProbeCommand::Shutdown,
        ]);

        assert!(matches!(
            &responses[0].outcome,
            wavecraft_debug_probe::DebugProbeResponseOutcome::Error(error)
                if error.kind == DebugProbeErrorKind::InvalidConfiguration
        ));
    }

    #[test]
    fn rejects_and_drains_an_oversized_message() {
        let mut input = vec![b' '; MAX_DEBUG_PROBE_HELPER_MESSAGE_BYTES + 1];
        input.push(b'\n');
        serde_json::to_writer(
            &mut input,
            &DebugProbeRequest::new(1, DebugProbeCommand::Shutdown),
        )
        .expect("shutdown serializes");
        input.push(b'\n');
        let mut output = Vec::new();

        serve(FakeDriver, Cursor::new(input), &mut output).expect("helper stays synchronized");

        let responses = String::from_utf8(output).expect("responses are UTF-8");
        assert_eq!(responses.lines().count(), 2);
        assert!(responses.contains("too large"));
        assert!(responses.contains("shutting_down"));
    }
}
