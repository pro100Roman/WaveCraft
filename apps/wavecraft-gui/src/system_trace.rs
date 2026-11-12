//! Bounded GUI state for decoded system trace events.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use crate::debug_probes::DebugProbeHelperProcess;
use wavecraft_debug_probe::{
    AttachConfiguration, DebugProbeCommand, DebugProbeError, DebugProbeErrorKind, DebugProbeReply,
    ProbeSelector, RttChannelDescriptor, RttConfiguration,
};
use wavecraft_trace::{TraceEvent, TraceEventKind, TraceModel};

const DEFAULT_EVENT_CAPACITY: usize = 100_000;

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
    pub(crate) rtt: RttConfiguration,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum SystemTraceConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Attached {
        channels: Vec<RttChannelDescriptor>,
    },
    Disconnecting,
    Error(String),
}

impl SystemTraceConnectionState {
    pub(crate) fn is_disconnected(&self) -> bool {
        matches!(self, Self::Disconnected | Self::Error(_))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemTraceWorkerCommand {
    Disconnect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemTraceAction {
    Connect,
    Disconnect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SystemTraceWorkerEvent {
    Attached(Vec<RttChannelDescriptor>),
    Disconnected,
    Error(String),
}

pub(crate) struct SystemTraceWorkerChannels {
    pub(crate) commands: Sender<SystemTraceWorkerCommand>,
    pub(crate) events: Receiver<SystemTraceWorkerEvent>,
}

pub(crate) fn spawn_system_trace_connection(
    configuration: SystemTraceConnectionConfiguration,
) -> SystemTraceWorkerChannels {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    thread::spawn(move || match DebugProbeHelperProcess::spawn() {
        Ok(helper) => run_connection_worker(helper, configuration, command_receiver, event_sender),
        Err(error) => {
            let _ = event_sender.send(SystemTraceWorkerEvent::Error(error));
        }
    });
    SystemTraceWorkerChannels {
        commands: command_sender,
        events: event_receiver,
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
        .send(SystemTraceWorkerEvent::Attached(channels))
        .is_err()
    {
        let _ = close_connection(transport);
        return;
    }

    match commands.recv() {
        Ok(SystemTraceWorkerCommand::Disconnect) | Err(_) => match close_connection(transport) {
            Ok(()) => {
                let _ = events.send(SystemTraceWorkerEvent::Disconnected);
            }
            Err(error) => {
                let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
            }
        },
    }
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
    match transport.send(DebugProbeCommand::AttachRtt {
        configuration: configuration.rtt.clone(),
    })? {
        DebugProbeReply::RttAttached(channels) => Ok(channels),
        _ => Err(unexpected_reply("attach RTT")),
    }
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
        self.system_trace_connection = SystemTraceConnectionState::Connecting;
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

    pub(crate) fn poll_system_trace_connection(&mut self) {
        let mut finished = false;
        if let Some(events) = &self.system_trace_worker_events {
            loop {
                match events.try_recv() {
                    Ok(SystemTraceWorkerEvent::Attached(channels)) => {
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
            rtt: RttConfiguration::default(),
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
    fn connection_worker_opens_attaches_rtt_and_disconnects() {
        let channel = RttChannelDescriptor {
            number: 1,
            direction: wavecraft_debug_probe::RttChannelDirection::Up,
            name: Some("SystemView".to_owned()),
            buffer_size: 1024,
        };
        let commands = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let transport = FakeTransport {
            replies: VecDeque::from([
                Ok(DebugProbeReply::ProbeOpened(probe_descriptor())),
                Ok(DebugProbeReply::TargetAttached),
                Ok(DebugProbeReply::RttAttached(vec![channel.clone()])),
                Ok(DebugProbeReply::SessionClosed),
            ]),
            commands: Arc::clone(&commands),
            shutdown: Arc::clone(&shutdown),
        };
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        command_sender
            .send(SystemTraceWorkerCommand::Disconnect)
            .expect("disconnect is queued");

        run_connection_worker(
            transport,
            connection_configuration(),
            command_receiver,
            event_sender,
        );

        assert_eq!(
            event_receiver.try_iter().collect::<Vec<_>>(),
            [
                SystemTraceWorkerEvent::Attached(vec![channel]),
                SystemTraceWorkerEvent::Disconnected,
            ]
        );
        assert!(matches!(
            commands.lock().expect("commands are available").as_slice(),
            [
                DebugProbeCommand::OpenProbe { .. },
                DebugProbeCommand::Attach { .. },
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

        run_connection_worker(
            transport,
            connection_configuration(),
            command_receiver,
            event_sender,
        );

        assert_eq!(
            event_receiver.recv().expect("error is reported"),
            SystemTraceWorkerEvent::Error("target was not found".to_owned())
        );
        assert!(*shutdown.lock().expect("shutdown flag is available"));
    }
}
