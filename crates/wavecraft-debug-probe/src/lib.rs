//! Provider-neutral debug-probe and RTT transport contracts.

use std::error::Error;
use std::fmt;
use std::io::{BufRead, Write};
use std::ops::Range;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Debug-probe implementation family.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ProbeProvider {
    JLink,
    StLink,
    BlackMagicProbe,
    CmsisDap,
    Other(String),
}

/// Version of the newline-delimited helper protocol shared with probe adapters.
pub const DEBUG_PROBE_HELPER_PROTOCOL_VERSION: u32 = 1;

/// Upper bound for one encoded helper request or response.
pub const MAX_DEBUG_PROBE_HELPER_MESSAGE_BYTES: usize = 1024 * 1024;

/// Upper bound for one memory or RTT transfer across the helper boundary.
pub const MAX_DEBUG_PROBE_TRANSFER_BYTES: usize = 64 * 1024;

/// A discoverable physical debug probe.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProbeDescriptor {
    /// Stable selector produced by the backend, including serial number when available.
    pub connection_id: String,
    pub provider: ProbeProvider,
    pub vendor: Option<String>,
    pub model: String,
    pub serial_number: Option<String>,
}

/// Exact selector for opening one discovered probe.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProbeSelector {
    pub connection_id: String,
}

impl ProbeSelector {
    pub fn connection_id(connection_id: impl Into<String>) -> Self {
        Self {
            connection_id: connection_id.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum WireProtocol {
    Swd,
    Jtag,
}

/// Target selection used when attaching without flashing or resetting it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TargetSelector {
    Auto,
    Name(String),
}

/// Non-destructive target attach settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AttachConfiguration {
    pub target: TargetSelector,
    pub wire_protocol: Option<WireProtocol>,
    pub speed_khz: Option<u32>,
    pub core_index: usize,
}

impl Default for AttachConfiguration {
    fn default() -> Self {
        Self {
            target: TargetSelector::Auto,
            wire_protocol: Some(WireProtocol::Swd),
            speed_khz: None,
            core_index: 0,
        }
    }
}

/// Where to locate the `_SEGGER_RTT` control block in target memory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RttControlBlockLocation {
    Exact(u64),
    Ranges(Vec<Range<u64>>),
    Ram,
}

/// RTT attachment settings shared by every probe backend.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RttConfiguration {
    pub control_block: RttControlBlockLocation,
    pub timeout: Duration,
}

impl Default for RttConfiguration {
    fn default() -> Self {
        Self {
            control_block: RttControlBlockLocation::Ram,
            timeout: Duration::from_secs(2),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RttChannelDirection {
    Up,
    Down,
}

/// One target-declared RTT ring buffer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RttChannelDescriptor {
    pub number: usize,
    pub direction: RttChannelDirection,
    pub name: Option<String>,
    pub buffer_size: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DebugProbeErrorKind {
    NotFound,
    Busy,
    PermissionDenied,
    InvalidConfiguration,
    Unsupported,
    Disconnected,
    Target,
    Transport,
}

/// Backend-independent error suitable for presentation in the System Trace view.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DebugProbeError {
    pub kind: DebugProbeErrorKind,
    pub message: String,
}

impl DebugProbeError {
    pub fn new(kind: DebugProbeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for DebugProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DebugProbeError {}

/// One request sent to a separately built debug-probe helper.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DebugProbeRequest {
    pub protocol_version: u32,
    pub request_id: u64,
    pub command: DebugProbeCommand,
}

impl DebugProbeRequest {
    pub fn new(request_id: u64, command: DebugProbeCommand) -> Self {
        Self {
            protocol_version: DEBUG_PROBE_HELPER_PROTOCOL_VERSION,
            request_id,
            command,
        }
    }
}

/// Stateful operation performed by the helper process.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", content = "arguments", rename_all = "snake_case")]
pub enum DebugProbeCommand {
    ListProbes,
    OpenProbe {
        selector: ProbeSelector,
    },
    Attach {
        configuration: AttachConfiguration,
    },
    ReadMemory {
        address: u64,
        length: usize,
    },
    WriteMemory {
        address: u64,
        data: Vec<u8>,
    },
    AttachRtt {
        configuration: RttConfiguration,
    },
    ReadRttUp {
        channel_number: usize,
        maximum_length: usize,
    },
    WriteRttDown {
        channel_number: usize,
        data: Vec<u8>,
    },
    CloseSession,
    Shutdown,
}

/// One response emitted for exactly one helper request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DebugProbeResponse {
    pub protocol_version: u32,
    pub request_id: u64,
    pub outcome: DebugProbeResponseOutcome,
}

impl DebugProbeResponse {
    pub fn success(request_id: u64, reply: DebugProbeReply) -> Self {
        Self {
            protocol_version: DEBUG_PROBE_HELPER_PROTOCOL_VERSION,
            request_id,
            outcome: DebugProbeResponseOutcome::Success(reply),
        }
    }

    pub fn error(request_id: u64, error: DebugProbeError) -> Self {
        Self {
            protocol_version: DEBUG_PROBE_HELPER_PROTOCOL_VERSION,
            request_id,
            outcome: DebugProbeResponseOutcome::Error(error),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "result", rename_all = "snake_case")]
pub enum DebugProbeResponseOutcome {
    Success(DebugProbeReply),
    Error(DebugProbeError),
}

/// Successful result of a helper command.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "reply", content = "value", rename_all = "snake_case")]
pub enum DebugProbeReply {
    Probes(Vec<ProbeDescriptor>),
    ProbeOpened(ProbeDescriptor),
    TargetAttached,
    MemoryData(Vec<u8>),
    MemoryWritten,
    RttAttached(Vec<RttChannelDescriptor>),
    RttData(Vec<u8>),
    RttBytesWritten(usize),
    SessionClosed,
    ShuttingDown,
}

/// Synchronous client for one persistent helper process.
///
/// Process creation and lifetime stay with the application; this type owns
/// only the helper's buffered stdout and stdin streams.
pub struct DebugProbeProtocolClient<R, W> {
    reader: R,
    writer: W,
    next_request_id: u64,
}

impl<R: BufRead, W: Write> DebugProbeProtocolClient<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_request_id: 1,
        }
    }

    pub fn send(&mut self, command: DebugProbeCommand) -> Result<DebugProbeReply, DebugProbeError> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        let encoded = serde_json::to_vec(&DebugProbeRequest::new(request_id, command))
            .map_err(|error| protocol_transport_error("failed to encode helper request", error))?;
        if encoded.len() > MAX_DEBUG_PROBE_HELPER_MESSAGE_BYTES {
            return Err(DebugProbeError::new(
                DebugProbeErrorKind::InvalidConfiguration,
                "debug-probe helper request is too large",
            ));
        }
        self.writer
            .write_all(&encoded)
            .and_then(|()| self.writer.write_all(b"\n"))
            .and_then(|()| self.writer.flush())
            .map_err(|error| protocol_transport_error("failed to send helper request", error))?;

        let encoded_response = read_bounded_protocol_message(&mut self.reader)?;
        let response = serde_json::from_slice::<DebugProbeResponse>(&encoded_response)
            .map_err(|error| protocol_transport_error("invalid helper response", error))?;
        if response.protocol_version != DEBUG_PROBE_HELPER_PROTOCOL_VERSION {
            return Err(DebugProbeError::new(
                DebugProbeErrorKind::Unsupported,
                format!(
                    "helper uses protocol version {}; expected {}",
                    response.protocol_version, DEBUG_PROBE_HELPER_PROTOCOL_VERSION
                ),
            ));
        }
        if response.request_id != request_id {
            return Err(DebugProbeError::new(
                DebugProbeErrorKind::Transport,
                format!(
                    "helper response ID {} does not match request ID {request_id}",
                    response.request_id
                ),
            ));
        }
        match response.outcome {
            DebugProbeResponseOutcome::Success(reply) => Ok(reply),
            DebugProbeResponseOutcome::Error(error) => Err(error),
        }
    }

    pub fn into_parts(self) -> (R, W) {
        (self.reader, self.writer)
    }
}

fn read_bounded_protocol_message(reader: &mut impl BufRead) -> Result<Vec<u8>, DebugProbeError> {
    let mut message = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| protocol_transport_error("failed to read helper response", error))?;
        if available.is_empty() {
            return Err(DebugProbeError::new(
                DebugProbeErrorKind::Disconnected,
                "debug-probe helper closed its response stream",
            ));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let content_length = newline.unwrap_or(consumed);
        if message.len().saturating_add(content_length) > MAX_DEBUG_PROBE_HELPER_MESSAGE_BYTES {
            return Err(DebugProbeError::new(
                DebugProbeErrorKind::Transport,
                "debug-probe helper response is too large",
            ));
        }
        message.extend_from_slice(&available[..content_length]);
        reader.consume(consumed);
        if newline.is_some() {
            if message.last() == Some(&b'\r') {
                message.pop();
            }
            return Ok(message);
        }
    }
}

fn protocol_transport_error(context: &str, error: impl fmt::Display) -> DebugProbeError {
    DebugProbeError::new(
        DebugProbeErrorKind::Transport,
        format!("{context}: {error}"),
    )
}

/// Discovers and opens probes implemented by one or more providers.
pub trait DebugProbeDriver: Send + Sync {
    fn list_probes(&self) -> Result<Vec<ProbeDescriptor>, DebugProbeError>;

    fn open_probe(
        &self,
        selector: &ProbeSelector,
    ) -> Result<Box<dyn DebugProbeHandle>, DebugProbeError>;
}

/// Exclusive ownership of an opened physical probe.
pub trait DebugProbeHandle: Send {
    fn descriptor(&self) -> &ProbeDescriptor;

    /// Attaches without resetting or flashing the target.
    fn attach(
        self: Box<Self>,
        configuration: &AttachConfiguration,
    ) -> Result<Box<dyn DebugSession>, DebugProbeError>;
}

/// Attached target session used by System Trace acquisition.
pub trait DebugSession: Send {
    fn read_memory(&mut self, address: u64, destination: &mut [u8]) -> Result<(), DebugProbeError>;

    fn write_memory(&mut self, address: u64, source: &[u8]) -> Result<(), DebugProbeError>;

    fn attach_rtt(
        &mut self,
        configuration: &RttConfiguration,
    ) -> Result<Vec<RttChannelDescriptor>, DebugProbeError>;

    /// Performs one non-blocking read from an attached target-to-host channel.
    fn read_rtt_up(
        &mut self,
        channel_number: usize,
        destination: &mut [u8],
    ) -> Result<usize, DebugProbeError>;

    fn write_rtt_down(
        &mut self,
        channel_number: usize,
        source: &[u8],
    ) -> Result<usize, DebugProbeError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeDriver {
        descriptor: ProbeDescriptor,
        up_data: Arc<Mutex<VecDeque<u8>>>,
    }

    impl DebugProbeDriver for FakeDriver {
        fn list_probes(&self) -> Result<Vec<ProbeDescriptor>, DebugProbeError> {
            Ok(vec![self.descriptor.clone()])
        }

        fn open_probe(
            &self,
            selector: &ProbeSelector,
        ) -> Result<Box<dyn DebugProbeHandle>, DebugProbeError> {
            if selector.connection_id != self.descriptor.connection_id {
                return Err(DebugProbeError::new(
                    DebugProbeErrorKind::NotFound,
                    format!("probe {} was not found", selector.connection_id),
                ));
            }
            Ok(Box::new(FakeProbe {
                descriptor: self.descriptor.clone(),
                up_data: Arc::clone(&self.up_data),
            }))
        }
    }

    struct FakeProbe {
        descriptor: ProbeDescriptor,
        up_data: Arc<Mutex<VecDeque<u8>>>,
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
                up_data: self.up_data,
                down_data: Vec::new(),
                rtt_attached: false,
            }))
        }
    }

    struct FakeSession {
        memory: Vec<u8>,
        up_data: Arc<Mutex<VecDeque<u8>>>,
        down_data: Vec<u8>,
        rtt_attached: bool,
    }

    impl DebugSession for FakeSession {
        fn read_memory(
            &mut self,
            address: u64,
            destination: &mut [u8],
        ) -> Result<(), DebugProbeError> {
            let start = usize::try_from(address).map_err(|_| invalid_address())?;
            let end = start
                .checked_add(destination.len())
                .ok_or_else(invalid_address)?;
            let source = self.memory.get(start..end).ok_or_else(invalid_address)?;
            destination.copy_from_slice(source);
            Ok(())
        }

        fn write_memory(&mut self, address: u64, source: &[u8]) -> Result<(), DebugProbeError> {
            let start = usize::try_from(address).map_err(|_| invalid_address())?;
            let end = start
                .checked_add(source.len())
                .ok_or_else(invalid_address)?;
            let destination = self
                .memory
                .get_mut(start..end)
                .ok_or_else(invalid_address)?;
            destination.copy_from_slice(source);
            Ok(())
        }

        fn attach_rtt(
            &mut self,
            _configuration: &RttConfiguration,
        ) -> Result<Vec<RttChannelDescriptor>, DebugProbeError> {
            self.rtt_attached = true;
            Ok(vec![
                RttChannelDescriptor {
                    number: 1,
                    direction: RttChannelDirection::Up,
                    name: Some("SystemView".to_owned()),
                    buffer_size: 1024,
                },
                RttChannelDescriptor {
                    number: 1,
                    direction: RttChannelDirection::Down,
                    name: Some("SystemView".to_owned()),
                    buffer_size: 32,
                },
            ])
        }

        fn read_rtt_up(
            &mut self,
            channel_number: usize,
            destination: &mut [u8],
        ) -> Result<usize, DebugProbeError> {
            self.require_rtt_channel(channel_number)?;
            let mut data = self.up_data.lock().map_err(|_| {
                DebugProbeError::new(DebugProbeErrorKind::Transport, "fake RTT lock failed")
            })?;
            let count = destination.len().min(data.len());
            for destination_byte in &mut destination[..count] {
                if let Some(source_byte) = data.pop_front() {
                    *destination_byte = source_byte;
                }
            }
            Ok(count)
        }

        fn write_rtt_down(
            &mut self,
            channel_number: usize,
            source: &[u8],
        ) -> Result<usize, DebugProbeError> {
            self.require_rtt_channel(channel_number)?;
            self.down_data.extend_from_slice(source);
            Ok(source.len())
        }
    }

    impl FakeSession {
        fn require_rtt_channel(&self, channel_number: usize) -> Result<(), DebugProbeError> {
            if self.rtt_attached && channel_number == 1 {
                Ok(())
            } else {
                Err(DebugProbeError::new(
                    DebugProbeErrorKind::InvalidConfiguration,
                    "RTT channel is not attached",
                ))
            }
        }
    }

    fn invalid_address() -> DebugProbeError {
        DebugProbeError::new(
            DebugProbeErrorKind::Target,
            "memory address is outside the fake target",
        )
    }

    fn fake_driver() -> FakeDriver {
        FakeDriver {
            descriptor: ProbeDescriptor {
                connection_id: "black-magic:ABC123".to_owned(),
                provider: ProbeProvider::BlackMagicProbe,
                vendor: Some("Black Magic Debug".to_owned()),
                model: "Black Magic Probe".to_owned(),
                serial_number: Some("ABC123".to_owned()),
            },
            up_data: Arc::new(Mutex::new(VecDeque::from(vec![1, 2, 3, 4]))),
        }
    }

    #[test]
    fn opens_only_the_exact_discovered_connection() {
        let driver = fake_driver();
        let descriptor = driver.list_probes().expect("listing succeeds").remove(0);
        let probe = driver
            .open_probe(&ProbeSelector::connection_id(&descriptor.connection_id))
            .expect("exact selector opens");
        assert_eq!(probe.descriptor(), &descriptor);

        let error = match driver.open_probe(&ProbeSelector::connection_id("missing")) {
            Ok(_) => panic!("unknown selector should fail"),
            Err(error) => error,
        };
        assert_eq!(error.kind, DebugProbeErrorKind::NotFound);
    }

    #[test]
    fn attached_session_provides_memory_and_bidirectional_rtt() {
        let driver = fake_driver();
        let probe = driver
            .open_probe(&ProbeSelector::connection_id("black-magic:ABC123"))
            .expect("probe opens");
        let mut session = probe
            .attach(&AttachConfiguration::default())
            .expect("target attaches");

        session
            .write_memory(4, &[9, 8, 7])
            .expect("memory write succeeds");
        let mut memory = [0; 3];
        session
            .read_memory(4, &mut memory)
            .expect("memory read succeeds");
        assert_eq!(memory, [9, 8, 7]);

        let channels = session
            .attach_rtt(&RttConfiguration::default())
            .expect("RTT attaches");
        assert_eq!(channels.len(), 2);
        let mut bytes = [0; 3];
        assert_eq!(
            session
                .read_rtt_up(1, &mut bytes)
                .expect("up channel reads"),
            3
        );
        assert_eq!(bytes, [1, 2, 3]);
        assert_eq!(
            session
                .write_rtt_down(1, &[0x01, 0x00])
                .expect("down channel writes"),
            2
        );
    }

    #[test]
    fn helper_requests_have_a_versioned_stable_envelope() {
        let request = DebugProbeRequest::new(7, DebugProbeCommand::ListProbes);
        let encoded = serde_json::to_string(&request).expect("request serializes");

        assert_eq!(
            encoded,
            r#"{"protocol_version":1,"request_id":7,"command":{"command":"list_probes"}}"#
        );
        assert_eq!(
            serde_json::from_str::<DebugProbeRequest>(&encoded).expect("request deserializes"),
            request
        );
    }

    #[test]
    fn helper_responses_round_trip_successes_and_errors() {
        let success = DebugProbeResponse::success(4, DebugProbeReply::RttData(vec![0, 127, 255]));
        let failure = DebugProbeResponse::error(
            5,
            DebugProbeError::new(DebugProbeErrorKind::Busy, "probe is already open"),
        );

        for response in [success, failure] {
            let encoded = serde_json::to_vec(&response).expect("response serializes");
            assert_eq!(
                serde_json::from_slice::<DebugProbeResponse>(&encoded)
                    .expect("response deserializes"),
                response
            );
        }
    }

    #[test]
    fn protocol_client_correlates_requests_and_responses() {
        let responses = [
            DebugProbeResponse::success(1, DebugProbeReply::Probes(vec![fake_driver().descriptor])),
            DebugProbeResponse::error(
                2,
                DebugProbeError::new(DebugProbeErrorKind::Busy, "probe is busy"),
            ),
        ];
        let mut input = Vec::new();
        for response in responses {
            serde_json::to_writer(&mut input, &response).expect("response serializes");
            input.push(b'\n');
        }
        let mut client = DebugProbeProtocolClient::new(Cursor::new(input), Vec::new());

        assert!(matches!(
            client.send(DebugProbeCommand::ListProbes),
            Ok(DebugProbeReply::Probes(probes)) if probes.len() == 1
        ));
        assert_eq!(
            client
                .send(DebugProbeCommand::OpenProbe {
                    selector: ProbeSelector::connection_id("black-magic:ABC123"),
                })
                .expect_err("helper error is returned"),
            DebugProbeError::new(DebugProbeErrorKind::Busy, "probe is busy")
        );

        let (_, output) = client.into_parts();
        let requests = String::from_utf8(output).expect("requests are UTF-8");
        let request_ids = requests
            .lines()
            .map(|line| {
                serde_json::from_str::<DebugProbeRequest>(line)
                    .expect("request deserializes")
                    .request_id
            })
            .collect::<Vec<_>>();
        assert_eq!(request_ids, [1, 2]);
    }

    #[test]
    fn protocol_client_rejects_a_response_for_another_request() {
        let mut input = serde_json::to_vec(&DebugProbeResponse::success(
            99,
            DebugProbeReply::SessionClosed,
        ))
        .expect("response serializes");
        input.push(b'\n');
        let mut client = DebugProbeProtocolClient::new(Cursor::new(input), Vec::new());

        let error = client
            .send(DebugProbeCommand::CloseSession)
            .expect_err("mismatched response is rejected");

        assert_eq!(error.kind, DebugProbeErrorKind::Transport);
        assert!(error.message.contains("does not match"));
    }
}
