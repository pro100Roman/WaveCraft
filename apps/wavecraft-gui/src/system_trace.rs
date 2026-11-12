//! Bounded GUI state for decoded system trace events.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::debug_probes::DebugProbeHelperProcess;
use wavecraft_debug_probe::{
    AttachConfiguration, DebugProbeCommand, DebugProbeError, DebugProbeErrorKind, DebugProbeReply,
    ProbeSelector, RttChannelDescriptor, RttChannelDirection, RttConfiguration,
};
use wavecraft_systemview::{SystemViewDecoder, SYSTEMVIEW_COMMAND_START, SYSTEMVIEW_COMMAND_STOP};
use wavecraft_trace::{TraceEvent, TraceEventKind, TraceModel};

const DEFAULT_EVENT_CAPACITY: usize = 100_000;
const RTT_READ_SIZE: usize = 16 * 1024;
const IDLE_RTT_POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TraceEventView {
    #[default]
    Events,
    Logs,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemTraceConnectionConfiguration {
    pub(crate) probe: ProbeSelector,
    pub(crate) attach: AttachConfiguration,
    pub(crate) reset_after_attach: bool,
    pub(crate) rtt: RttConfiguration,
    pub(crate) up_channel: usize,
    pub(crate) down_channel: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum SystemTraceConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Attached {
        channels: Vec<RttChannelDescriptor>,
    },
    Starting {
        channels: Vec<RttChannelDescriptor>,
    },
    Tracing {
        channels: Vec<RttChannelDescriptor>,
    },
    Stopping {
        channels: Vec<RttChannelDescriptor>,
    },
    Disconnecting,
    Error(String),
}

impl SystemTraceConnectionState {
    pub(crate) fn is_disconnected(&self) -> bool {
        matches!(self, Self::Disconnected | Self::Error(_))
    }

    fn channels(&self) -> Option<&[RttChannelDescriptor]> {
        match self {
            Self::Attached { channels }
            | Self::Starting { channels }
            | Self::Tracing { channels }
            | Self::Stopping { channels } => Some(channels),
            Self::Disconnected | Self::Connecting | Self::Disconnecting | Self::Error(_) => None,
        }
    }

    pub(crate) fn needs_fast_poll(&self) -> bool {
        matches!(
            self,
            Self::Connecting
                | Self::Starting { .. }
                | Self::Tracing { .. }
                | Self::Stopping { .. }
                | Self::Disconnecting
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemTraceWorkerCommand {
    Start,
    Stop,
    Disconnect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemTraceAction {
    Connect,
    Start,
    Stop,
    Disconnect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SystemTraceWorkerEvent {
    Attached(Vec<RttChannelDescriptor>),
    Tracing,
    Stopped,
    Disconnected,
    Error(String),
}

pub(crate) struct SystemTraceWorkerChannels {
    pub(crate) commands: Sender<SystemTraceWorkerCommand>,
    pub(crate) events: Receiver<SystemTraceWorkerEvent>,
    pub(crate) trace_events: Receiver<TraceEvent>,
}

pub(crate) fn spawn_system_trace_connection(
    configuration: SystemTraceConnectionConfiguration,
) -> SystemTraceWorkerChannels {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let (trace_event_sender, trace_event_receiver) = mpsc::channel();
    thread::spawn(move || match DebugProbeHelperProcess::spawn() {
        Ok(helper) => run_connection_worker(
            helper,
            configuration,
            command_receiver,
            event_sender,
            trace_event_sender,
        ),
        Err(error) => {
            let _ = event_sender.send(SystemTraceWorkerEvent::Error(error));
        }
    });
    SystemTraceWorkerChannels {
        commands: command_sender,
        events: event_receiver,
        trace_events: trace_event_receiver,
    }
}

trait ProbeCommandTransport: Sized {
    fn send(&mut self, command: DebugProbeCommand) -> Result<DebugProbeReply, DebugProbeError>;
    fn shutdown(self) -> Result<(), DebugProbeError>;
}

impl ProbeCommandTransport for DebugProbeHelperProcess {
    fn send(&mut self, command: DebugProbeCommand) -> Result<DebugProbeReply, DebugProbeError> {
        Self::send(self, command)
    }

    fn shutdown(self) -> Result<(), DebugProbeError> {
        Self::shutdown(self)
    }
}

fn run_connection_worker(
    mut transport: impl ProbeCommandTransport,
    configuration: SystemTraceConnectionConfiguration,
    commands: Receiver<SystemTraceWorkerCommand>,
    events: Sender<SystemTraceWorkerEvent>,
    trace_events: Sender<TraceEvent>,
) {
    let connection = establish_connection(&mut transport, &configuration);
    let channels = match connection {
        Ok(channels) => channels,
        Err(error) => {
            let _ = transport.shutdown();
            let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
            return;
        }
    };
    if events
        .send(SystemTraceWorkerEvent::Attached(channels.clone()))
        .is_err()
    {
        let _ = close_connection(transport);
        return;
    }

    let mut decoder = SystemViewDecoder::new(configuration.attach.core_index as u32);
    let mut tracing = false;
    loop {
        let command = if tracing {
            match commands.try_recv() {
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(SystemTraceWorkerCommand::Disconnect),
            }
        } else {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => Some(SystemTraceWorkerCommand::Disconnect),
            }
        };

        match command {
            Some(SystemTraceWorkerCommand::Start) if !tracing => {
                decoder.reset();
                match write_systemview_command(
                    &mut transport,
                    configuration.down_channel,
                    SYSTEMVIEW_COMMAND_START,
                ) {
                    Ok(()) => {
                        tracing = true;
                        let _ = events.send(SystemTraceWorkerEvent::Tracing);
                    }
                    Err(error) => {
                        let _ = close_connection(transport);
                        let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                        return;
                    }
                }
            }
            Some(SystemTraceWorkerCommand::Stop) if tracing => {
                match write_systemview_command(
                    &mut transport,
                    configuration.down_channel,
                    SYSTEMVIEW_COMMAND_STOP,
                ) {
                    Ok(()) => {
                        tracing = false;
                        let _ = events.send(SystemTraceWorkerEvent::Stopped);
                    }
                    Err(error) => {
                        let _ = close_connection(transport);
                        let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                        return;
                    }
                }
            }
            Some(SystemTraceWorkerCommand::Disconnect) => {
                if tracing {
                    let _ = write_systemview_command(
                        &mut transport,
                        configuration.down_channel,
                        SYSTEMVIEW_COMMAND_STOP,
                    );
                }
                match close_connection(transport) {
                    Ok(()) => {
                        let _ = events.send(SystemTraceWorkerEvent::Disconnected);
                    }
                    Err(error) => {
                        let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                    }
                }
                return;
            }
            Some(SystemTraceWorkerCommand::Start | SystemTraceWorkerCommand::Stop) | None => {}
        }

        if tracing {
            match read_and_decode_rtt(
                &mut transport,
                configuration.up_channel,
                &mut decoder,
                &trace_events,
            ) {
                Ok(0) => thread::sleep(IDLE_RTT_POLL_INTERVAL),
                Ok(_) => {}
                Err(error) => {
                    let _ = close_connection(transport);
                    let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                    return;
                }
            }
        }
    }
}

fn write_systemview_command(
    transport: &mut impl ProbeCommandTransport,
    channel_number: usize,
    command: u8,
) -> Result<(), DebugProbeError> {
    match transport.send(DebugProbeCommand::WriteRttDown {
        channel_number,
        data: vec![command],
    })? {
        DebugProbeReply::RttBytesWritten(1) => Ok(()),
        DebugProbeReply::RttBytesWritten(count) => Err(DebugProbeError::new(
            DebugProbeErrorKind::Transport,
            format!("SystemView command wrote {count} bytes instead of 1"),
        )),
        _ => Err(unexpected_reply("write a SystemView command")),
    }
}

fn read_and_decode_rtt(
    transport: &mut impl ProbeCommandTransport,
    channel_number: usize,
    decoder: &mut SystemViewDecoder,
    trace_events: &Sender<TraceEvent>,
) -> Result<usize, DebugProbeError> {
    let data = match transport.send(DebugProbeCommand::ReadRttUp {
        channel_number,
        maximum_length: RTT_READ_SIZE,
    })? {
        DebugProbeReply::RttData(data) => data,
        _ => return Err(unexpected_reply("read SystemView RTT data")),
    };
    let byte_count = data.len();
    let decoded = decoder.push(&data).map_err(|error| {
        DebugProbeError::new(
            DebugProbeErrorKind::Transport,
            format!("failed to decode SystemView stream: {error}"),
        )
    })?;
    for event in decoded {
        trace_events.send(event).map_err(|_| {
            DebugProbeError::new(
                DebugProbeErrorKind::Disconnected,
                "System Trace event receiver is no longer available",
            )
        })?;
    }
    Ok(byte_count)
}

fn establish_connection(
    transport: &mut impl ProbeCommandTransport,
    configuration: &SystemTraceConnectionConfiguration,
) -> Result<Vec<RttChannelDescriptor>, DebugProbeError> {
    expect_reply(
        transport.send(DebugProbeCommand::OpenProbe {
            selector: configuration.probe.clone(),
        })?,
        |reply| matches!(reply, DebugProbeReply::ProbeOpened(_)),
        "open probe",
    )?;
    expect_reply(
        transport.send(DebugProbeCommand::Attach {
            configuration: configuration.attach.clone(),
        })?,
        |reply| reply == &DebugProbeReply::TargetAttached,
        "attach target",
    )?;
    if configuration.reset_after_attach {
        expect_reply(
            transport.send(DebugProbeCommand::ResetTarget)?,
            |reply| reply == &DebugProbeReply::TargetReset,
            "reset target",
        )?;
    }
    match transport.send(DebugProbeCommand::AttachRtt {
        configuration: configuration.rtt.clone(),
    })? {
        DebugProbeReply::RttAttached(channels) => {
            validate_rtt_channels(&channels, configuration)?;
            Ok(channels)
        }
        _ => Err(unexpected_reply("attach RTT")),
    }
}

fn validate_rtt_channels(
    channels: &[RttChannelDescriptor],
    configuration: &SystemTraceConnectionConfiguration,
) -> Result<(), DebugProbeError> {
    let has_up = channels.iter().any(|channel| {
        channel.direction == RttChannelDirection::Up && channel.number == configuration.up_channel
    });
    let has_down = channels.iter().any(|channel| {
        channel.direction == RttChannelDirection::Down
            && channel.number == configuration.down_channel
    });
    if !has_up || !has_down {
        return Err(DebugProbeError::new(
            DebugProbeErrorKind::InvalidConfiguration,
            format!(
                "target does not expose configured SystemView RTT up/down channels {}/{}",
                configuration.up_channel, configuration.down_channel
            ),
        ));
    }
    Ok(())
}

fn close_connection(mut transport: impl ProbeCommandTransport) -> Result<(), DebugProbeError> {
    expect_reply(
        transport.send(DebugProbeCommand::CloseSession)?,
        |reply| reply == &DebugProbeReply::SessionClosed,
        "close probe session",
    )?;
    transport.shutdown()
}

fn expect_reply(
    reply: DebugProbeReply,
    expected: impl FnOnce(&DebugProbeReply) -> bool,
    operation: &str,
) -> Result<(), DebugProbeError> {
    if expected(&reply) {
        Ok(())
    } else {
        Err(unexpected_reply(operation))
    }
}

fn unexpected_reply(operation: &str) -> DebugProbeError {
    DebugProbeError::new(
        DebugProbeErrorKind::Transport,
        format!("debug-probe helper returned an unexpected reply while trying to {operation}"),
    )
}

impl crate::WaveCraftApp {
    pub(crate) fn begin_system_trace_connection(
        &mut self,
        configuration: SystemTraceConnectionConfiguration,
    ) {
        if !self.system_trace_connection.is_disconnected() {
            return;
        }
        let worker = spawn_system_trace_connection(configuration);
        self.system_trace_worker_commands = Some(worker.commands);
        self.system_trace_worker_events = Some(worker.events);
        self.system_trace_events = Some(worker.trace_events);
        self.system_trace_connection = SystemTraceConnectionState::Connecting;
    }

    pub(crate) fn request_system_trace_start(&mut self) {
        let SystemTraceConnectionState::Attached { channels } = &self.system_trace_connection
        else {
            return;
        };
        let channels = channels.clone();
        if self.send_system_trace_worker_command(SystemTraceWorkerCommand::Start) {
            self.system_trace.clear();
            self.system_trace_connection = SystemTraceConnectionState::Starting { channels };
        }
    }

    pub(crate) fn request_system_trace_stop(&mut self) {
        let SystemTraceConnectionState::Tracing { channels } = &self.system_trace_connection else {
            return;
        };
        let channels = channels.clone();
        if self.send_system_trace_worker_command(SystemTraceWorkerCommand::Stop) {
            self.system_trace_connection = SystemTraceConnectionState::Stopping { channels };
        }
    }

    pub(crate) fn request_system_trace_disconnect(&mut self) {
        let Some(commands) = &self.system_trace_worker_commands else {
            self.system_trace_connection = SystemTraceConnectionState::Disconnected;
            return;
        };
        match commands.send(SystemTraceWorkerCommand::Disconnect) {
            Ok(()) => self.system_trace_connection = SystemTraceConnectionState::Disconnecting,
            Err(_) => {
                self.system_trace_connection = SystemTraceConnectionState::Error(
                    "debug-probe worker is no longer available".to_owned(),
                );
                self.system_trace_worker_commands = None;
                self.system_trace_worker_events = None;
            }
        }
    }

    fn send_system_trace_worker_command(&mut self, command: SystemTraceWorkerCommand) -> bool {
        let sent = self
            .system_trace_worker_commands
            .as_ref()
            .is_some_and(|commands| commands.send(command).is_ok());
        if !sent {
            self.system_trace_connection = SystemTraceConnectionState::Error(
                "debug-probe worker is no longer available".to_owned(),
            );
            self.system_trace_worker_commands = None;
            self.system_trace_worker_events = None;
        }
        sent
    }

    pub(crate) fn poll_system_trace_connection(&mut self) {
        let mut finished = false;
        if let Some(events) = &self.system_trace_worker_events {
            loop {
                match events.try_recv() {
                    Ok(SystemTraceWorkerEvent::Attached(channels)) => {
                        self.system_trace_connection =
                            SystemTraceConnectionState::Attached { channels };
                    }
                    Ok(SystemTraceWorkerEvent::Tracing) => {
                        let channels = self
                            .system_trace_connection
                            .channels()
                            .map(<[RttChannelDescriptor]>::to_vec)
                            .unwrap_or_default();
                        self.system_trace_connection =
                            SystemTraceConnectionState::Tracing { channels };
                    }
                    Ok(SystemTraceWorkerEvent::Stopped) => {
                        let channels = self
                            .system_trace_connection
                            .channels()
                            .map(<[RttChannelDescriptor]>::to_vec)
                            .unwrap_or_default();
                        self.system_trace_connection =
                            SystemTraceConnectionState::Attached { channels };
                    }
                    Ok(SystemTraceWorkerEvent::Disconnected) => {
                        self.system_trace_connection = SystemTraceConnectionState::Disconnected;
                        finished = true;
                    }
                    Ok(SystemTraceWorkerEvent::Error(error)) => {
                        self.system_trace_connection = SystemTraceConnectionState::Error(error);
                        finished = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        if matches!(
                            self.system_trace_connection,
                            SystemTraceConnectionState::Connecting
                                | SystemTraceConnectionState::Starting { .. }
                                | SystemTraceConnectionState::Tracing { .. }
                                | SystemTraceConnectionState::Stopping { .. }
                                | SystemTraceConnectionState::Disconnecting
                        ) {
                            self.system_trace_connection = SystemTraceConnectionState::Error(
                                "debug-probe worker ended unexpectedly".to_owned(),
                            );
                        }
                        finished = true;
                        break;
                    }
                }
            }
        }
        if finished {
            self.system_trace_worker_commands = None;
            self.system_trace_worker_events = None;
        }
    }
}

/// Display state is intentionally independent of a debug-probe backend. A
/// future capture worker can feed this type events from any supported probe.
#[derive(Debug)]
pub(crate) struct SystemTraceDisplayState {
    events: VecDeque<TraceEvent>,
    model: TraceModel,
    event_capacity: usize,
    pub(crate) selected_sequence: Option<u64>,
    pub(crate) event_view: TraceEventView,
    pub(crate) event_filter: String,
    pub(crate) follow_latest: bool,
}

impl Default for SystemTraceDisplayState {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            model: TraceModel::new(),
            event_capacity: DEFAULT_EVENT_CAPACITY,
            selected_sequence: None,
            event_view: TraceEventView::Events,
            event_filter: String::new(),
            follow_latest: true,
        }
    }
}

impl SystemTraceDisplayState {
    pub(crate) fn events(&self) -> &VecDeque<TraceEvent> {
        &self.events
    }

    pub(crate) fn model(&self) -> &TraceModel {
        &self.model
    }

    pub(crate) fn ingest(&mut self, event: TraceEvent) {
        self.model.apply(&event);
        if self.events.len() == self.event_capacity
            && self
                .events
                .pop_front()
                .is_some_and(|discarded| Some(discarded.sequence) == self.selected_sequence)
        {
            self.selected_sequence = None;
        }
        self.events.push_back(event);
    }

    #[cfg(test)]
    pub(crate) fn ingest_all(&mut self, events: impl IntoIterator<Item = TraceEvent>) {
        for event in events {
            self.ingest(event);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.events.clear();
        self.model = TraceModel::new();
        self.selected_sequence = None;
    }

    pub(crate) fn time_bounds(&self) -> Option<(u64, u64)> {
        let mut timestamps = self.events.iter().map(|event| event.timestamp_ticks);
        let first = timestamps.next()?;
        Some(
            timestamps.fold((first, first), |(minimum, maximum), timestamp| {
                (minimum.min(timestamp), maximum.max(timestamp))
            }),
        )
    }

    pub(crate) fn dropped_event_count(&self) -> u64 {
        self.model()
            .cores()
            .values()
            .map(|core| core.dropped_events())
            .sum()
    }

    pub(crate) fn selected_event(&self) -> Option<&TraceEvent> {
        let selected_sequence = self.selected_sequence?;
        self.events
            .iter()
            .find(|event| event.sequence == selected_sequence)
    }

    pub(crate) fn log_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event.kind, TraceEventKind::FormattedPrint { .. }))
            .count()
    }

    #[cfg(test)]
    fn with_capacity(event_capacity: usize) -> Self {
        Self {
            event_capacity,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use wavecraft_debug_probe::{ProbeDescriptor, ProbeProvider, TargetSelector, WireProtocol};

    struct FakeTransport {
        replies: VecDeque<Result<DebugProbeReply, DebugProbeError>>,
        commands: Arc<Mutex<Vec<DebugProbeCommand>>>,
        shutdown: Arc<Mutex<bool>>,
    }

    impl ProbeCommandTransport for FakeTransport {
        fn send(&mut self, command: DebugProbeCommand) -> Result<DebugProbeReply, DebugProbeError> {
            self.commands
                .lock()
                .expect("command log is available")
                .push(command);
            self.replies
                .pop_front()
                .expect("fake reply exists for every command")
        }

        fn shutdown(self) -> Result<(), DebugProbeError> {
            *self.shutdown.lock().expect("shutdown flag is available") = true;
            Ok(())
        }
    }

    fn connection_configuration() -> SystemTraceConnectionConfiguration {
        SystemTraceConnectionConfiguration {
            probe: ProbeSelector::connection_id("1366:0105:1234"),
            attach: AttachConfiguration {
                target: TargetSelector::Name("STM32F407VGTx".to_owned()),
                wire_protocol: Some(WireProtocol::Swd),
                speed_khz: Some(4_000),
                core_index: 0,
            },
            reset_after_attach: false,
            rtt: RttConfiguration::default(),
            up_channel: 1,
            down_channel: 1,
        }
    }

    fn probe_descriptor() -> ProbeDescriptor {
        ProbeDescriptor {
            connection_id: "1366:0105:1234".to_owned(),
            provider: ProbeProvider::JLink,
            vendor: Some("SEGGER".to_owned()),
            model: "J-Link".to_owned(),
            serial_number: Some("1234".to_owned()),
        }
    }

    fn event(sequence: u64, timestamp_ticks: u64, kind: TraceEventKind) -> TraceEvent {
        TraceEvent {
            sequence,
            core_id: 0,
            timestamp_ticks,
            kind,
        }
    }

    #[test]
    fn bounds_display_events_without_losing_reconstructed_state() {
        let mut state = SystemTraceDisplayState::with_capacity(2);
        state.ingest_all([
            event(
                0,
                1,
                TraceEventKind::TaskInfo {
                    task_id: 7,
                    priority: 3,
                    name: "worker".to_owned(),
                },
            ),
            event(1, 2, TraceEventKind::TaskStarted { task_id: 7 }),
            event(2, 5, TraceEventKind::TaskStopped),
        ]);

        assert_eq!(
            state
                .events()
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            state
                .model()
                .core(0)
                .and_then(|core| core.tasks().get(&7))
                .and_then(|task| task.name.as_deref()),
            Some("worker")
        );
    }

    #[test]
    fn reports_time_bounds_logs_and_transport_loss() {
        let mut state = SystemTraceDisplayState::default();
        state.ingest_all([
            event(
                0,
                20,
                TraceEventKind::FormattedPrint {
                    format: "temperature=%u".to_owned(),
                    options: 0,
                    arguments: vec![42],
                },
            ),
            event(1, 10, TraceEventKind::Overflow { dropped_events: 8 }),
        ]);

        assert_eq!(state.time_bounds(), Some((10, 20)));
        assert_eq!(state.log_count(), 1);
        assert_eq!(state.dropped_event_count(), 8);
    }

    #[test]
    fn clear_starts_a_fresh_trace_session() {
        let mut state = SystemTraceDisplayState::default();
        state.ingest(event(0, 1, TraceEventKind::TaskStarted { task_id: 1 }));
        state.selected_sequence = Some(0);

        state.clear();

        assert!(state.events().is_empty());
        assert!(state.model().cores().is_empty());
        assert_eq!(state.selected_sequence, None);
    }

    #[test]
    fn connection_worker_optionally_resets_before_rtt_attach() {
        let up_channel = RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Up,
            name: Some("SystemView".to_owned()),
            buffer_size: 1024,
        };
        let down_channel = RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Down,
            name: Some("SystemView".to_owned()),
            buffer_size: 8,
        };
        let commands = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let transport = FakeTransport {
            replies: VecDeque::from([
                Ok(DebugProbeReply::ProbeOpened(probe_descriptor())),
                Ok(DebugProbeReply::TargetAttached),
                Ok(DebugProbeReply::TargetReset),
                Ok(DebugProbeReply::RttAttached(vec![
                    up_channel.clone(),
                    down_channel.clone(),
                ])),
                Ok(DebugProbeReply::SessionClosed),
            ]),
            commands: Arc::clone(&commands),
            shutdown: Arc::clone(&shutdown),
        };
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let (trace_event_sender, _trace_event_receiver) = mpsc::channel();
        command_sender
            .send(SystemTraceWorkerCommand::Disconnect)
            .expect("disconnect is queued");

        let mut configuration = connection_configuration();
        configuration.reset_after_attach = true;
        run_connection_worker(
            transport,
            configuration,
            command_receiver,
            event_sender,
            trace_event_sender,
        );

        assert_eq!(
            event_receiver.try_iter().collect::<Vec<_>>(),
            [
                SystemTraceWorkerEvent::Attached(vec![up_channel, down_channel]),
                SystemTraceWorkerEvent::Disconnected,
            ]
        );
        assert!(matches!(
            commands.lock().expect("commands are available").as_slice(),
            [
                DebugProbeCommand::OpenProbe { .. },
                DebugProbeCommand::Attach { .. },
                DebugProbeCommand::ResetTarget,
                DebugProbeCommand::AttachRtt { .. },
                DebugProbeCommand::CloseSession,
            ]
        ));
        assert!(*shutdown.lock().expect("shutdown flag is available"));
    }

    #[test]
    fn connection_worker_reports_attach_errors_and_stops_the_helper() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let transport = FakeTransport {
            replies: VecDeque::from([Err(DebugProbeError::new(
                DebugProbeErrorKind::Target,
                "target was not found",
            ))]),
            commands,
            shutdown: Arc::clone(&shutdown),
        };
        let (_command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let (trace_event_sender, _trace_event_receiver) = mpsc::channel();

        run_connection_worker(
            transport,
            connection_configuration(),
            command_receiver,
            event_sender,
            trace_event_sender,
        );

        assert_eq!(
            event_receiver.recv().expect("error is reported"),
            SystemTraceWorkerEvent::Error("target was not found".to_owned())
        );
        assert!(*shutdown.lock().expect("shutdown flag is available"));
    }

    #[test]
    fn tracing_writes_start_and_stop_and_decodes_incremental_rtt_data() {
        let up_channel = RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Up,
            name: Some("SystemView".to_owned()),
            buffer_size: 1024,
        };
        let down_channel = RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Down,
            name: Some("SystemView".to_owned()),
            buffer_size: 8,
        };
        let commands = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let transport = FakeTransport {
            replies: VecDeque::from([
                Ok(DebugProbeReply::ProbeOpened(probe_descriptor())),
                Ok(DebugProbeReply::TargetAttached),
                Ok(DebugProbeReply::RttAttached(vec![
                    up_channel.clone(),
                    down_channel.clone(),
                ])),
                Ok(DebugProbeReply::RttBytesWritten(1)),
                Ok(DebugProbeReply::RttData(vec![10, 3, 4, 7, 2])),
                Ok(DebugProbeReply::RttBytesWritten(1)),
                Ok(DebugProbeReply::SessionClosed),
            ]),
            commands: Arc::clone(&commands),
            shutdown: Arc::clone(&shutdown),
        };
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let (trace_event_sender, trace_event_receiver) = mpsc::channel();
        for command in [
            SystemTraceWorkerCommand::Start,
            SystemTraceWorkerCommand::Stop,
            SystemTraceWorkerCommand::Disconnect,
        ] {
            command_sender.send(command).expect("command is queued");
        }

        run_connection_worker(
            transport,
            connection_configuration(),
            command_receiver,
            event_sender,
            trace_event_sender,
        );

        assert_eq!(
            event_receiver.try_iter().collect::<Vec<_>>(),
            [
                SystemTraceWorkerEvent::Attached(vec![up_channel, down_channel]),
                SystemTraceWorkerEvent::Tracing,
                SystemTraceWorkerEvent::Stopped,
                SystemTraceWorkerEvent::Disconnected,
            ]
        );
        let decoded = trace_event_receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].kind, TraceEventKind::TraceStarted);
        assert_eq!(decoded[1].kind, TraceEventKind::TaskStarted { task_id: 7 });
        let commands = commands.lock().expect("commands are available");
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DebugProbeCommand::WriteRttDown { data, .. }
                    if data == &[SYSTEMVIEW_COMMAND_START]
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DebugProbeCommand::WriteRttDown { data, .. }
                    if data == &[SYSTEMVIEW_COMMAND_STOP]
            )
        }));
        assert!(*shutdown.lock().expect("shutdown flag is available"));
    }
}
