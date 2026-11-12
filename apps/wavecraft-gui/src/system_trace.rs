//! Bounded GUI state for decoded system trace events.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::{debug_probes::DebugProbeHelperProcess, WorkerSuccess};
use wavecraft_debug_probe::{
    AttachConfiguration, DebugProbeCommand, DebugProbeError, DebugProbeErrorKind, DebugProbeReply,
    ProbeSelector, RttChannelDescriptor, RttChannelDirection, RttConfiguration, RttTerminalDecoder,
    RttTerminalLine,
};
use wavecraft_systemview::{SystemViewDecoder, SYSTEMVIEW_COMMAND_START, SYSTEMVIEW_COMMAND_STOP};
use wavecraft_trace::{TraceEvent, TraceEventKind, TraceModel};

const DEFAULT_EVENT_CAPACITY: usize = 100_000;
const DEFAULT_TERMINAL_LINE_CAPACITY: usize = 20_000;
const MIN_TIMELINE_ZOOM: f32 = 1.0;
const MAX_TIMELINE_ZOOM: f32 = 64.0;
const RTT_READ_SIZE: usize = 16 * 1024;
const IDLE_RTT_POLL_INTERVAL: Duration = Duration::from_millis(2);
const SYSTEMVIEW_FILE_READ_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TraceEventView {
    #[default]
    Events,
    Logs,
    Terminal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemTraceConnectionConfiguration {
    pub(crate) probe: ProbeSelector,
    pub(crate) attach: AttachConfiguration,
    pub(crate) reset_after_attach: bool,
    pub(crate) rtt: RttConfiguration,
    pub(crate) systemview_enabled: bool,
    pub(crate) up_channel: usize,
    pub(crate) down_channel: usize,
    pub(crate) terminal_up_channel: Option<usize>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum SystemTraceConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Attached {
        channels: Vec<RttChannelDescriptor>,
        systemview_enabled: bool,
    },
    Starting {
        channels: Vec<RttChannelDescriptor>,
        recording_path: Option<PathBuf>,
    },
    Tracing {
        channels: Vec<RttChannelDescriptor>,
        recording_path: Option<PathBuf>,
    },
    Stopping {
        channels: Vec<RttChannelDescriptor>,
        recording_path: Option<PathBuf>,
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
            Self::Attached { channels, .. }
            | Self::Starting { channels, .. }
            | Self::Tracing { channels, .. }
            | Self::Stopping { channels, .. } => Some(channels),
            Self::Disconnected | Self::Connecting | Self::Disconnecting | Self::Error(_) => None,
        }
    }

    pub(crate) fn needs_fast_poll(&self) -> bool {
        matches!(
            self,
            Self::Connecting
                | Self::Attached { .. }
                | Self::Starting { .. }
                | Self::Tracing { .. }
                | Self::Stopping { .. }
                | Self::Disconnecting
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SystemTraceWorkerCommand {
    Start { recording_path: Option<PathBuf> },
    Stop,
    Disconnect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemTraceAction {
    Open,
    Record,
    Connect,
    Start,
    Stop,
    Disconnect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SystemTraceWorkerEvent {
    Attached {
        channels: Vec<RttChannelDescriptor>,
        systemview_enabled: bool,
    },
    Tracing {
        recording_path: Option<PathBuf>,
    },
    Stopped,
    RecordingSaved(PathBuf),
    Disconnected,
    Error(String),
}

pub(crate) struct SystemTraceWorkerChannels {
    pub(crate) commands: Sender<SystemTraceWorkerCommand>,
    pub(crate) events: Receiver<SystemTraceWorkerEvent>,
    pub(crate) trace_events: Receiver<TraceEvent>,
    pub(crate) terminal_lines: Receiver<RttTerminalLine>,
}

pub(crate) fn spawn_system_trace_connection(
    configuration: SystemTraceConnectionConfiguration,
) -> SystemTraceWorkerChannels {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let (trace_event_sender, trace_event_receiver) = mpsc::channel();
    let (terminal_line_sender, terminal_line_receiver) = mpsc::channel();
    thread::spawn(move || match DebugProbeHelperProcess::spawn() {
        Ok(helper) => run_connection_worker(
            helper,
            configuration,
            command_receiver,
            event_sender,
            trace_event_sender,
            terminal_line_sender,
        ),
        Err(error) => {
            let _ = event_sender.send(SystemTraceWorkerEvent::Error(error));
        }
    });
    SystemTraceWorkerChannels {
        commands: command_sender,
        events: event_receiver,
        trace_events: trace_event_receiver,
        terminal_lines: terminal_line_receiver,
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
    terminal_lines: Sender<RttTerminalLine>,
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
        .send(SystemTraceWorkerEvent::Attached {
            channels: channels.clone(),
            systemview_enabled: configuration.systemview_enabled,
        })
        .is_err()
    {
        let _ = close_connection(transport);
        return;
    }

    let mut decoder = SystemViewDecoder::new(configuration.attach.core_index as u32);
    let mut terminal_decoder = RttTerminalDecoder::new();
    let mut raw_recording: Option<(PathBuf, BufWriter<File>)> = None;
    let mut tracing = false;
    loop {
        let polling = tracing || configuration.terminal_up_channel.is_some();
        let command = if polling {
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
            Some(SystemTraceWorkerCommand::Start { recording_path })
                if !tracing && configuration.systemview_enabled =>
            {
                decoder.reset();
                raw_recording = match recording_path.as_ref() {
                    Some(path) => match create_systemview_recording(path) {
                        Ok(recording) => Some((path.clone(), recording)),
                        Err(error) => {
                            let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                            continue;
                        }
                    },
                    None => None,
                };
                match write_systemview_command(
                    &mut transport,
                    configuration.down_channel,
                    SYSTEMVIEW_COMMAND_START,
                ) {
                    Ok(()) => {
                        tracing = true;
                        let _ = events.send(SystemTraceWorkerEvent::Tracing { recording_path });
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
                        if let Err(error) =
                            finish_systemview_recording(raw_recording.take(), &events)
                        {
                            let _ = close_connection(transport);
                            let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                            return;
                        }
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
                if let Err(error) = finish_systemview_recording(raw_recording.take(), &events) {
                    let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                    let _ = close_connection(transport);
                    return;
                }
                if let Some(line) = terminal_decoder.finish() {
                    let _ = terminal_lines.send(line);
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
            Some(SystemTraceWorkerCommand::Start { .. } | SystemTraceWorkerCommand::Stop)
            | None => {}
        }

        let mut bytes_read = 0;
        if tracing {
            match read_and_decode_rtt(
                &mut transport,
                configuration.up_channel,
                &mut decoder,
                &trace_events,
                raw_recording.as_mut().map(|(_, recording)| recording),
            ) {
                Ok(count) => bytes_read += count,
                Err(error) => {
                    let _ = close_connection(transport);
                    let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                    return;
                }
            }
        }
        if let Some(channel_number) = configuration.terminal_up_channel {
            match read_rtt_terminal(
                &mut transport,
                channel_number,
                &mut terminal_decoder,
                &terminal_lines,
            ) {
                Ok(count) => bytes_read += count,
                Err(error) => {
                    let _ = close_connection(transport);
                    let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                    return;
                }
            }
        }
        if polling && bytes_read == 0 {
            thread::sleep(IDLE_RTT_POLL_INTERVAL);
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
    raw_recording: Option<&mut BufWriter<File>>,
) -> Result<usize, DebugProbeError> {
    let data = match transport.send(DebugProbeCommand::ReadRttUp {
        channel_number,
        maximum_length: RTT_READ_SIZE,
    })? {
        DebugProbeReply::RttData(data) => data,
        _ => return Err(unexpected_reply("read SystemView RTT data")),
    };
    let byte_count = data.len();
    if let Some(recording) = raw_recording {
        recording.write_all(&data).map_err(|error| {
            DebugProbeError::new(
                DebugProbeErrorKind::Transport,
                format!("failed to write raw SystemView recording: {error}"),
            )
        })?;
    }
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

fn create_systemview_recording(path: &Path) -> Result<BufWriter<File>, DebugProbeError> {
    File::create(path).map(BufWriter::new).map_err(|error| {
        DebugProbeError::new(
            DebugProbeErrorKind::Transport,
            format!(
                "failed to create raw SystemView recording {}: {error}",
                path.display()
            ),
        )
    })
}

fn finish_systemview_recording(
    recording: Option<(PathBuf, BufWriter<File>)>,
    events: &Sender<SystemTraceWorkerEvent>,
) -> Result<(), DebugProbeError> {
    let Some((path, mut recording)) = recording else {
        return Ok(());
    };
    recording.flush().map_err(|error| {
        DebugProbeError::new(
            DebugProbeErrorKind::Transport,
            format!(
                "failed to finish raw SystemView recording {}: {error}",
                path.display()
            ),
        )
    })?;
    let _ = events.send(SystemTraceWorkerEvent::RecordingSaved(path));
    Ok(())
}

fn read_rtt_terminal(
    transport: &mut impl ProbeCommandTransport,
    channel_number: usize,
    decoder: &mut RttTerminalDecoder,
    terminal_lines: &Sender<RttTerminalLine>,
) -> Result<usize, DebugProbeError> {
    let data = match transport.send(DebugProbeCommand::ReadRttUp {
        channel_number,
        maximum_length: RTT_READ_SIZE,
    })? {
        DebugProbeReply::RttData(data) => data,
        _ => return Err(unexpected_reply("read plain RTT terminal data")),
    };
    let byte_count = data.len();
    for line in decoder.push(&data) {
        terminal_lines.send(line).map_err(|_| {
            DebugProbeError::new(
                DebugProbeErrorKind::Disconnected,
                "RTT terminal receiver is no longer available",
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
    if configuration.systemview_enabled {
        let has_up = channels.iter().any(|channel| {
            channel.direction == RttChannelDirection::Up
                && channel.number == configuration.up_channel
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
    }
    if let Some(terminal_channel) = configuration.terminal_up_channel {
        if configuration.systemview_enabled && terminal_channel == configuration.up_channel {
            return Err(DebugProbeError::new(
                DebugProbeErrorKind::InvalidConfiguration,
                "plain RTT terminal and SystemView must use different up-channels",
            ));
        }
        let has_terminal = channels.iter().any(|channel| {
            channel.direction == RttChannelDirection::Up && channel.number == terminal_channel
        });
        if !has_terminal {
            return Err(DebugProbeError::new(
                DebugProbeErrorKind::InvalidConfiguration,
                format!("target does not expose RTT terminal up-channel {terminal_channel}"),
            ));
        }
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
    pub(crate) fn begin_open_system_trace(&mut self, path: PathBuf) {
        if self.pending.is_some() || !self.system_trace_connection.is_disconnected() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.status = format!("Opening SystemView trace {}…", path.display());
        self.last_error = None;
        thread::spawn(move || {
            let result = read_systemview_trace_file(&path)
                .map(|events| WorkerSuccess::SystemTraceLoaded { path, events });
            let _ = sender.send(result);
        });
    }

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
        self.system_trace_terminal_lines = Some(worker.terminal_lines);
        self.system_trace_connection = SystemTraceConnectionState::Connecting;
    }

    pub(crate) fn request_system_trace_start(&mut self) {
        self.request_system_trace_start_with_recording(None);
    }

    pub(crate) fn request_system_trace_recording(&mut self, path: PathBuf) {
        self.request_system_trace_start_with_recording(Some(path));
    }

    fn request_system_trace_start_with_recording(&mut self, recording_path: Option<PathBuf>) {
        let SystemTraceConnectionState::Attached {
            channels,
            systemview_enabled: true,
        } = &self.system_trace_connection
        else {
            return;
        };
        let channels = channels.clone();
        if self.send_system_trace_worker_command(SystemTraceWorkerCommand::Start {
            recording_path: recording_path.clone(),
        }) {
            self.system_trace.clear_trace_events();
            self.system_trace_connection = SystemTraceConnectionState::Starting {
                channels,
                recording_path,
            };
        }
    }

    pub(crate) fn request_system_trace_stop(&mut self) {
        let SystemTraceConnectionState::Tracing {
            channels,
            recording_path,
        } = &self.system_trace_connection
        else {
            return;
        };
        let channels = channels.clone();
        let recording_path = recording_path.clone();
        if self.send_system_trace_worker_command(SystemTraceWorkerCommand::Stop) {
            self.system_trace_connection = SystemTraceConnectionState::Stopping {
                channels,
                recording_path,
            };
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
                    Ok(SystemTraceWorkerEvent::Attached {
                        channels,
                        systemview_enabled,
                    }) => {
                        self.system_trace_connection = SystemTraceConnectionState::Attached {
                            channels,
                            systemview_enabled,
                        };
                    }
                    Ok(SystemTraceWorkerEvent::Tracing { recording_path }) => {
                        let channels = self
                            .system_trace_connection
                            .channels()
                            .map(<[RttChannelDescriptor]>::to_vec)
                            .unwrap_or_default();
                        self.system_trace_connection = SystemTraceConnectionState::Tracing {
                            channels,
                            recording_path,
                        };
                    }
                    Ok(SystemTraceWorkerEvent::Stopped) => {
                        let channels = self
                            .system_trace_connection
                            .channels()
                            .map(<[RttChannelDescriptor]>::to_vec)
                            .unwrap_or_default();
                        self.system_trace_connection = SystemTraceConnectionState::Attached {
                            channels,
                            systemview_enabled: true,
                        };
                    }
                    Ok(SystemTraceWorkerEvent::RecordingSaved(path)) => {
                        self.status = format!("SystemView recording saved to {}", path.display());
                        self.last_error = None;
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
    terminal_lines: VecDeque<RttTerminalLine>,
    model: TraceModel,
    event_capacity: usize,
    terminal_line_capacity: usize,
    pub(crate) selected_sequence: Option<u64>,
    pub(crate) event_view: TraceEventView,
    pub(crate) event_filter: String,
    pub(crate) follow_latest: bool,
    timeline_zoom: f32,
}

impl Default for SystemTraceDisplayState {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            terminal_lines: VecDeque::new(),
            model: TraceModel::new(),
            event_capacity: DEFAULT_EVENT_CAPACITY,
            terminal_line_capacity: DEFAULT_TERMINAL_LINE_CAPACITY,
            selected_sequence: None,
            event_view: TraceEventView::Events,
            event_filter: String::new(),
            follow_latest: true,
            timeline_zoom: MIN_TIMELINE_ZOOM,
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

    pub(crate) fn terminal_lines(&self) -> &VecDeque<RttTerminalLine> {
        &self.terminal_lines
    }

    pub(crate) fn timeline_zoom(&self) -> f32 {
        self.timeline_zoom
    }

    pub(crate) fn set_timeline_zoom(&mut self, zoom: f32) {
        self.timeline_zoom = if zoom.is_finite() {
            zoom.clamp(MIN_TIMELINE_ZOOM, MAX_TIMELINE_ZOOM)
        } else {
            MIN_TIMELINE_ZOOM
        };
    }

    pub(crate) fn scale_timeline_zoom(&mut self, factor: f32) {
        self.set_timeline_zoom(self.timeline_zoom * factor);
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

    pub(crate) fn ingest_terminal_line(&mut self, line: RttTerminalLine) {
        if self.terminal_lines.len() == self.terminal_line_capacity {
            self.terminal_lines.pop_front();
        }
        self.terminal_lines.push_back(line);
    }

    pub(crate) fn replace_trace_events(&mut self, events: impl IntoIterator<Item = TraceEvent>) {
        self.clear_trace_events();
        self.event_filter.clear();
        self.event_view = TraceEventView::Events;
        for event in events {
            self.ingest(event);
        }
    }

    #[cfg(test)]
    pub(crate) fn ingest_all(&mut self, events: impl IntoIterator<Item = TraceEvent>) {
        for event in events {
            self.ingest(event);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.clear_trace_events();
        self.terminal_lines.clear();
    }

    pub(crate) fn clear_trace_events(&mut self) {
        self.events.clear();
        self.model = TraceModel::new();
        self.selected_sequence = None;
        self.timeline_zoom = MIN_TIMELINE_ZOOM;
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

pub(crate) fn choose_systemview_trace_to_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open raw SystemView trace")
        .add_filter("SystemView data", &["SVDat", "svdat"])
        .pick_file()
}

pub(crate) fn choose_systemview_recording_path() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Record raw SystemView trace")
        .add_filter("SystemView data", &["SVDat"])
        .set_file_name("system-trace.SVDat")
        .save_file()
        .map(normalize_systemview_recording_path)
}

fn normalize_systemview_recording_path(mut path: PathBuf) -> PathBuf {
    let has_systemview_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svdat"));
    if !has_systemview_extension {
        path.set_extension("SVDat");
    }
    path
}

fn read_systemview_trace_file(path: &Path) -> Result<Vec<TraceEvent>, String> {
    let file = File::open(path).map_err(|error| {
        format!(
            "failed to open SystemView trace {}: {error}",
            path.display()
        )
    })?;
    decode_systemview_stream(BufReader::new(file)).map_err(|error| {
        format!(
            "failed to read SystemView trace {}: {error}",
            path.display()
        )
    })
}

fn decode_systemview_stream(mut input: impl Read) -> Result<Vec<TraceEvent>, String> {
    let mut decoder = SystemViewDecoder::default();
    let mut buffer = vec![0_u8; SYSTEMVIEW_FILE_READ_SIZE];
    let mut events = Vec::new();
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| format!("failed to read trace bytes: {error}"))?;
        if count == 0 {
            break;
        }
        events.extend(
            decoder
                .push(&buffer[..count])
                .map_err(|error| error.to_string())?,
        );
    }
    decoder.finish().map_err(|error| error.to_string())?;
    if events.is_empty() {
        return Err("the file contains no SystemView events".to_owned());
    }
    Ok(events)
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
            systemview_enabled: true,
            up_channel: 1,
            down_channel: 1,
            terminal_up_channel: None,
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
    fn imports_raw_systemview_event_streams() {
        let bytes = [
            10, 1, // Trace started.
            9, 7, 3, 6, b'w', b'o', b'r', b'k', b'e', b'r', 2, // Task metadata.
            4, 7, 3, // Task started.
            11, 4, // Trace stopped.
        ];

        let events = decode_systemview_stream(std::io::Cursor::new(bytes))
            .expect("raw SystemView data imports");

        assert_eq!(events.len(), 4);
        assert_eq!(events[0].kind, TraceEventKind::TraceStarted);
        assert_eq!(
            events[1].kind,
            TraceEventKind::TaskInfo {
                task_id: 7,
                priority: 3,
                name: "worker".to_owned(),
            }
        );
        assert_eq!(events[2].timestamp_ticks, 6);
        assert_eq!(events[3].kind, TraceEventKind::TraceStopped);
    }

    #[test]
    fn rejects_empty_systemview_files() {
        assert_eq!(
            decode_systemview_stream(std::io::Cursor::new([])),
            Err("the file contains no SystemView events".to_owned())
        );
    }

    #[test]
    fn checked_in_systemview_demo_exercises_trace_instruments() {
        let bytes = include_bytes!("../../../docs/examples/nrf52832-freertos.SVDat");
        let events = decode_systemview_stream(std::io::Cursor::new(bytes.as_slice()))
            .expect("checked-in demo is valid raw SystemView data");
        let mut state = SystemTraceDisplayState::default();

        state.replace_trace_events(events);

        assert!(state.events().len() >= 1_200);
        assert!(state.log_count() >= 20);
        let core = state.model().core(0).expect("demo contains core 0");
        assert_eq!(
            core.tasks().get(&2).and_then(|task| task.name.as_deref()),
            Some("sensor")
        );
        assert!(core.intervals().iter().any(|interval| matches!(
            interval.context,
            wavecraft_trace::ExecutionContext::Isr(15)
        )));
        assert!(core.intervals().iter().any(|interval| matches!(
            interval.context,
            wavecraft_trace::ExecutionContext::Timer(4)
        )));
        assert!(core.intervals().len() >= 700);
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
        state.ingest_terminal_line(RttTerminalLine::plain(0, "boot", true));
        state.selected_sequence = Some(0);

        state.clear();

        assert!(state.events().is_empty());
        assert!(state.terminal_lines().is_empty());
        assert!(state.model().cores().is_empty());
        assert_eq!(state.selected_sequence, None);
    }

    #[test]
    fn timeline_zoom_is_bounded_and_resets_with_trace_data() {
        let mut state = SystemTraceDisplayState::default();

        state.set_timeline_zoom(8.0);
        assert_eq!(state.timeline_zoom(), 8.0);
        state.scale_timeline_zoom(100.0);
        assert_eq!(state.timeline_zoom(), MAX_TIMELINE_ZOOM);
        state.set_timeline_zoom(f32::NAN);
        assert_eq!(state.timeline_zoom(), MIN_TIMELINE_ZOOM);
        state.set_timeline_zoom(4.0);

        state.clear_trace_events();

        assert_eq!(state.timeline_zoom(), MIN_TIMELINE_ZOOM);
    }

    #[test]
    fn starting_a_trace_keeps_terminal_output_collected_since_attach() {
        let mut state = SystemTraceDisplayState::default();
        state.ingest(event(0, 1, TraceEventKind::TraceStarted));
        state.ingest_terminal_line(RttTerminalLine::plain(
            0,
            "boot before SystemView start",
            true,
        ));

        state.clear_trace_events();

        assert!(state.events().is_empty());
        assert_eq!(state.terminal_lines().len(), 1);
    }

    #[test]
    fn plain_rtt_terminal_decodes_incremental_reads() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let mut transport = FakeTransport {
            replies: VecDeque::from([
                Ok(DebugProbeReply::RttData(b"boot ".to_vec())),
                Ok(DebugProbeReply::RttData(b"ready\r\n".to_vec())),
            ]),
            commands: Arc::clone(&commands),
            shutdown,
        };
        let mut decoder = RttTerminalDecoder::new();
        let (line_sender, line_receiver) = mpsc::channel();

        assert_eq!(
            read_rtt_terminal(&mut transport, 0, &mut decoder, &line_sender)
                .expect("first terminal read succeeds"),
            5
        );
        assert!(line_receiver.try_recv().is_err());
        assert_eq!(
            read_rtt_terminal(&mut transport, 0, &mut decoder, &line_sender)
                .expect("second terminal read succeeds"),
            7
        );
        assert_eq!(
            line_receiver.recv().expect("completed line is emitted"),
            RttTerminalLine::plain(0, "boot ready", true)
        );
        assert!(matches!(
            commands.lock().expect("commands are available").as_slice(),
            [
                DebugProbeCommand::ReadRttUp {
                    channel_number: 0,
                    ..
                },
                DebugProbeCommand::ReadRttUp {
                    channel_number: 0,
                    ..
                },
            ]
        ));
    }

    #[test]
    fn terminal_only_connection_needs_no_systemview_channels() {
        let mut channels = vec![RttChannelDescriptor {
            number: 0,
            direction: RttChannelDirection::Up,
            name: Some("Terminal".to_owned()),
            buffer_size: 1024,
        }];
        let mut configuration = connection_configuration();
        configuration.systemview_enabled = false;
        configuration.terminal_up_channel = Some(0);

        assert_eq!(validate_rtt_channels(&channels, &configuration), Ok(()));

        channels.push(RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Down,
            name: Some("SystemView".to_owned()),
            buffer_size: 32,
        });
        configuration.systemview_enabled = true;
        configuration.up_channel = 0;
        let error = validate_rtt_channels(&channels, &configuration)
            .expect_err("one up-channel cannot carry both streams");
        assert_eq!(error.kind, DebugProbeErrorKind::InvalidConfiguration);
        assert!(error.message.contains("different up-channels"));
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
        let (terminal_line_sender, _terminal_line_receiver) = mpsc::channel();
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
            terminal_line_sender,
        );

        assert_eq!(
            event_receiver.try_iter().collect::<Vec<_>>(),
            [
                SystemTraceWorkerEvent::Attached {
                    channels: vec![up_channel, down_channel],
                    systemview_enabled: true,
                },
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
        let (terminal_line_sender, _terminal_line_receiver) = mpsc::channel();

        run_connection_worker(
            transport,
            connection_configuration(),
            command_receiver,
            event_sender,
            trace_event_sender,
            terminal_line_sender,
        );

        assert_eq!(
            event_receiver.recv().expect("error is reported"),
            SystemTraceWorkerEvent::Error("target was not found".to_owned())
        );
        assert!(*shutdown.lock().expect("shutdown flag is available"));
    }

    #[test]
    fn tracing_writes_start_and_stop_and_decodes_incremental_rtt_data() {
        let recording_path =
            crate::app::test_support::scratch_directory("systemview-recording").join("trace.SVDat");
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
        let (terminal_line_sender, _terminal_line_receiver) = mpsc::channel();
        for command in [
            SystemTraceWorkerCommand::Start {
                recording_path: Some(recording_path.clone()),
            },
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
            terminal_line_sender,
        );

        assert_eq!(
            event_receiver.try_iter().collect::<Vec<_>>(),
            [
                SystemTraceWorkerEvent::Attached {
                    channels: vec![up_channel, down_channel],
                    systemview_enabled: true,
                },
                SystemTraceWorkerEvent::Tracing {
                    recording_path: Some(recording_path.clone()),
                },
                SystemTraceWorkerEvent::RecordingSaved(recording_path.clone()),
                SystemTraceWorkerEvent::Stopped,
                SystemTraceWorkerEvent::Disconnected,
            ]
        );
        let decoded = trace_event_receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].kind, TraceEventKind::TraceStarted);
        assert_eq!(decoded[1].kind, TraceEventKind::TaskStarted { task_id: 7 });
        assert_eq!(
            std::fs::read(&recording_path).expect("raw recording was saved"),
            [10, 3, 4, 7, 2]
        );
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

    #[test]
    fn systemview_recording_paths_use_svdat_extension() {
        assert_eq!(
            normalize_systemview_recording_path(PathBuf::from("trace")),
            PathBuf::from("trace.SVDat")
        );
        assert_eq!(
            normalize_systemview_recording_path(PathBuf::from("trace.svdat")),
            PathBuf::from("trace.svdat")
        );
    }
}
