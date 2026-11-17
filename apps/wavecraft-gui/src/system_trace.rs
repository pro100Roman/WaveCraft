//! Bounded GUI state for decoded system trace events.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::{debug_probes::DebugProbeHelperProcess, WorkerSuccess};
use wavecraft_debug_probe::{
    AttachConfiguration, DebugProbeCommand, DebugProbeError, DebugProbeErrorKind, DebugProbeReply,
    ProbeProvider, ProbeSelector, RttChannelDescriptor, RttChannelDirection, RttConfiguration,
    RttTerminalDecoder, RttTerminalLine, TargetSelector,
};
use wavecraft_systemview::{
    SystemViewDecoder, SystemViewDescription, SystemViewElf, SYSTEMVIEW_COMMAND_START,
    SYSTEMVIEW_COMMAND_STOP,
};
use wavecraft_trace::{ExecutionContext, TraceEvent, TraceEventKind, TraceModel};

const DEFAULT_EVENT_CAPACITY: usize = 20_000;
const DEFAULT_TERMINAL_LINE_CAPACITY: usize = 20_000;
const DEFAULT_PROBE_LOG_CAPACITY: usize = 10_000;
const DEFAULT_TIMELINE_VIEW_HEIGHT: f32 = 260.0;
const DEFAULT_EVENT_VIEW_HEIGHT: f32 = 230.0;
const DEFAULT_TIMELINE_LABEL_WIDTH: f32 = 150.0;
const MIN_TIMELINE_LABEL_WIDTH: f32 = 80.0;
const MAX_TIMELINE_LABEL_WIDTH: f32 = 600.0;
const MIN_TRACE_INSTRUMENT_HEIGHT: f32 = 140.0;
const MAX_TRACE_INSTRUMENT_HEIGHT: f32 = 900.0;
const RTT_READ_SIZE: usize = 16 * 1024;
// probe-rs uses a 1024-byte Black Magic remote buffer. Memory reads are hex
// encoded, leaving 504 payload bytes per remote command. Four payloads balance
// target read-pointer latency against the control-block overhead of every RTT
// read. The helper streams consecutive RTT drains so it can advance the target
// read pointer continuously while the GUI decodes each completed chunk.
const BLACK_MAGIC_REMOTE_READ_PAYLOAD_SIZE: usize = 504;
const NETWORK_BMP_RTT_READ_SIZE: usize = 4 * BLACK_MAGIC_REMOTE_READ_PAYLOAD_SIZE;
const NETWORK_BMP_SELECTOR_PREFIX: &str = "1d50:6018:";
const MAX_SYSTEMVIEW_RTT_READS_PER_POLL: usize = 4;
// 32 * 2016 bytes stays below the 64-KiB helper request limit while keeping a
// saturated network BMP busy. Each completed read is streamed back immediately.
const NETWORK_BMP_RTT_READ_BATCH: usize = 32;
const MAX_SYSTEMVIEW_STOP_DRAIN_BUFFER_PASSES: usize = 4;
const TRACE_EVENT_QUEUE_CAPACITY: usize = 64 * 1024;
const TERMINAL_LINE_QUEUE_CAPACITY: usize = 2 * 1024;
const IDLE_RTT_POLL_INTERVAL: Duration = Duration::from_millis(2);
const RTT_CHANNEL_DISCOVERY_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const PROBE_OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const TARGET_ATTACH_TIMEOUT: Duration = Duration::from_secs(10);
const TARGET_RESET_TIMEOUT: Duration = Duration::from_secs(10);
const RTT_ATTACH_TIMEOUT_MARGIN: Duration = Duration::from_secs(2);
const SYSTEMVIEW_FILE_READ_SIZE: usize = 64 * 1024;
static NEXT_TEMPORARY_TRACE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TraceEventView {
    #[default]
    Events,
    Logs,
    Terminal,
    ProbeLog,
    CpuLoad,
    ContextStatistics,
}

impl TraceEventView {
    pub(crate) const ALL: [Self; 6] = [
        Self::Events,
        Self::Logs,
        Self::Terminal,
        Self::ProbeLog,
        Self::CpuLoad,
        Self::ContextStatistics,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TraceInstrument {
    Timeline,
    Analysis,
}

impl TraceInstrument {
    pub(crate) const ALL: [Self; 2] = [Self::Timeline, Self::Analysis];
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TraceTimeDisplay {
    #[default]
    Target,
    RecordingRelative,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TraceEventCategory {
    Tasks,
    Isrs,
    Timers,
    Logs,
    Markers,
    DataSamples,
    ApiCalls,
    System,
}

impl TraceEventCategory {
    pub(crate) const ALL: [Self; 8] = [
        Self::Tasks,
        Self::Isrs,
        Self::Timers,
        Self::Logs,
        Self::Markers,
        Self::DataSamples,
        Self::ApiCalls,
        Self::System,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Tasks => 0,
            Self::Isrs => 1,
            Self::Timers => 2,
            Self::Logs => 3,
            Self::Markers => 4,
            Self::DataSamples => 5,
            Self::ApiCalls => 6,
            Self::System => 7,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TraceEventCategoryFilter {
    enabled: [bool; TraceEventCategory::ALL.len()],
}

impl Default for TraceEventCategoryFilter {
    fn default() -> Self {
        Self {
            enabled: [true; TraceEventCategory::ALL.len()],
        }
    }
}

impl TraceEventCategoryFilter {
    pub(crate) fn is_enabled(self, category: TraceEventCategory) -> bool {
        self.enabled[category.index()]
    }

    pub(crate) fn set_enabled(&mut self, category: TraceEventCategory, enabled: bool) {
        self.enabled[category.index()] = enabled;
    }

    pub(crate) fn show_all(&mut self) {
        self.enabled.fill(true);
    }

    pub(crate) fn show_only(&mut self, category: TraceEventCategory) {
        self.enabled.fill(false);
        self.set_enabled(category, true);
    }

    pub(crate) fn enabled_count(self) -> usize {
        self.enabled.into_iter().filter(|enabled| *enabled).count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EventFilterCacheKey {
    pub(crate) query: String,
    pub(crate) view: TraceEventView,
    pub(crate) categories: TraceEventCategoryFilter,
    pub(crate) search_generation: u64,
}

#[derive(Debug, Default)]
pub(crate) struct EventFilterCache {
    pub(crate) key: Option<EventFilterCacheKey>,
    pub(crate) matching_sequences: Arc<Vec<u64>>,
    pub(crate) last_scanned_sequence: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProbeLogLevel {
    Debug,
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProbeLogMessage {
    pub(crate) level: ProbeLogLevel,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProbeLogEntry {
    pub(crate) sequence: u64,
    pub(crate) level: ProbeLogLevel,
    pub(crate) message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TimelineTimingEdge {
    pub(crate) core_id: u32,
    pub(crate) context: ExecutionContext,
    pub(crate) timestamp_ticks: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TimelineTimingMeasurement {
    pub(crate) first: TimelineTimingEdge,
    pub(crate) second: TimelineTimingEdge,
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
    Save,
    Connect,
    Start,
    Stop,
    Disconnect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SystemTraceWorkerEvent {
    ProbeLog(ProbeLogMessage),
    Attached {
        channels: Vec<RttChannelDescriptor>,
        systemview_enabled: bool,
    },
    Tracing {
        recording_path: Option<PathBuf>,
        history_path: PathBuf,
        history_is_temporary: bool,
    },
    Stopped,
    RecordingSaved(PathBuf),
    Disconnected,
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LiveTraceUpdate {
    Event(TraceEvent),
    DisplayGap {
        sequence: u64,
        core_id: u32,
        timestamp_ticks: u64,
        skipped_events: u64,
    },
}

pub(crate) struct SystemTraceWorkerChannels {
    pub(crate) commands: Sender<SystemTraceWorkerCommand>,
    pub(crate) events: Receiver<SystemTraceWorkerEvent>,
    pub(crate) trace_events: Receiver<LiveTraceUpdate>,
    pub(crate) terminal_lines: Receiver<RttTerminalLine>,
}

pub(crate) fn spawn_system_trace_connection(
    configuration: SystemTraceConnectionConfiguration,
) -> SystemTraceWorkerChannels {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let (trace_event_sender, trace_event_receiver) = mpsc::sync_channel(TRACE_EVENT_QUEUE_CAPACITY);
    let (terminal_line_sender, terminal_line_receiver) =
        mpsc::sync_channel(TERMINAL_LINE_QUEUE_CAPACITY);
    thread::spawn(move || match DebugProbeHelperProcess::spawn() {
        Ok(helper) => {
            send_probe_log(
                &event_sender,
                ProbeLogLevel::Success,
                "probe-rs helper started",
            );
            run_connection_worker(
                helper,
                configuration,
                command_receiver,
                event_sender,
                trace_event_sender,
                terminal_line_sender,
            );
        }
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
    fn send_with_timeout(
        &mut self,
        command: DebugProbeCommand,
        _timeout: Duration,
    ) -> Result<DebugProbeReply, DebugProbeError> {
        self.send(command)
    }
    fn send_streaming(
        &mut self,
        command: DebugProbeCommand,
        publish: &mut dyn FnMut(DebugProbeReply) -> Result<(), DebugProbeError>,
    ) -> Result<(), DebugProbeError> {
        publish(self.send(command)?)
    }
    fn shutdown(self) -> Result<(), DebugProbeError>;
}

impl ProbeCommandTransport for DebugProbeHelperProcess {
    fn send(&mut self, command: DebugProbeCommand) -> Result<DebugProbeReply, DebugProbeError> {
        Self::send(self, command)
    }

    fn send_streaming(
        &mut self,
        command: DebugProbeCommand,
        publish: &mut dyn FnMut(DebugProbeReply) -> Result<(), DebugProbeError>,
    ) -> Result<(), DebugProbeError> {
        Self::send_streaming(self, command, publish)
    }

    fn send_with_timeout(
        &mut self,
        command: DebugProbeCommand,
        timeout: Duration,
    ) -> Result<DebugProbeReply, DebugProbeError> {
        Self::send_with_timeout(self, command, timeout)
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
    trace_events: SyncSender<LiveTraceUpdate>,
    terminal_lines: SyncSender<RttTerminalLine>,
) {
    let connection = establish_connection(&mut transport, &configuration, &events);
    let channels = match connection {
        Ok(channels) => channels,
        Err(error) => {
            let _ = transport.shutdown();
            let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
            return;
        }
    };
    if !configuration.systemview_enabled {
        send_probe_log(
            &events,
            ProbeLogLevel::Warning,
            "SystemView binary trace is disabled; session is RTT-terminal only",
        );
    }
    let systemview_read_size =
        systemview_rtt_read_size(&channels, configuration.up_channel, &configuration.probe);
    let systemview_read_batch = systemview_rtt_read_batch(&configuration.probe);
    let systemview_stop_drain_read_limit =
        systemview_stop_drain_read_limit(&channels, configuration.up_channel, systemview_read_size);
    if configuration.systemview_enabled && is_network_bmp(&configuration.probe) {
        send_probe_log(
            &events,
            ProbeLogLevel::Info,
            format!(
                "network BMP RTT optimization active: up to {systemview_read_batch} × {systemview_read_size}-byte reads per streamed helper request"
            ),
        );
    }
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
    let mut trace_events = TraceEventPublisher::new(trace_events);
    let mut terminal_decoder = RttTerminalDecoder::new();
    let mut terminal_lines = TerminalLinePublisher::new(terminal_lines);
    let mut raw_recording: Option<ActiveSystemViewRecording> = None;
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
                send_probe_log(
                    &events,
                    ProbeLogLevel::Info,
                    format!(
                        "starting SystemView trace on RTT channels up {}/down {}",
                        configuration.up_channel, configuration.down_channel
                    ),
                );
                decoder.reset();
                raw_recording = match create_systemview_recording(recording_path.as_deref()) {
                    Ok(recording) => Some(recording),
                    Err(error) => {
                        let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                        continue;
                    }
                };
                match write_systemview_command(
                    &mut transport,
                    configuration.down_channel,
                    SYSTEMVIEW_COMMAND_START,
                ) {
                    Ok(()) => {
                        tracing = true;
                        send_probe_log(&events, ProbeLogLevel::Success, "SystemView trace started");
                        let Some(recording) = raw_recording.as_ref() else {
                            let _ = events.send(SystemTraceWorkerEvent::Error(
                                "SystemView history recording was not created".to_owned(),
                            ));
                            return;
                        };
                        let _ = events.send(SystemTraceWorkerEvent::Tracing {
                            recording_path,
                            history_path: recording.path.clone(),
                            history_is_temporary: recording.temporary,
                        });
                    }
                    Err(error) => {
                        let _ = close_connection(transport);
                        let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                        return;
                    }
                }
            }
            Some(SystemTraceWorkerCommand::Stop) if tracing => {
                send_probe_log(&events, ProbeLogLevel::Info, "stopping SystemView trace");
                match write_systemview_command(
                    &mut transport,
                    configuration.down_channel,
                    SYSTEMVIEW_COMMAND_STOP,
                ) {
                    Ok(()) => {
                        tracing = false;
                        if let Err(error) = drain_stopped_systemview(
                            &mut transport,
                            configuration.up_channel,
                            systemview_read_size,
                            systemview_stop_drain_read_limit,
                            &mut decoder,
                            &mut trace_events,
                            raw_recording
                                .as_mut()
                                .map(|recording| &mut recording.writer),
                        ) {
                            let _ = close_connection(transport);
                            let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                            return;
                        }
                        if let Err(error) =
                            finish_systemview_recording(raw_recording.take(), &events)
                        {
                            let _ = close_connection(transport);
                            let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                            return;
                        }
                        send_probe_log(&events, ProbeLogLevel::Success, "SystemView trace stopped");
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
                send_probe_log(&events, ProbeLogLevel::Info, "closing debug-probe session");
                let drain_failed = tracing
                    && write_systemview_command(
                        &mut transport,
                        configuration.down_channel,
                        SYSTEMVIEW_COMMAND_STOP,
                    )
                    .and_then(|()| {
                        drain_stopped_systemview(
                            &mut transport,
                            configuration.up_channel,
                            systemview_read_size,
                            systemview_stop_drain_read_limit,
                            &mut decoder,
                            &mut trace_events,
                            raw_recording
                                .as_mut()
                                .map(|recording| &mut recording.writer),
                        )
                    })
                    .is_err();
                if drain_failed {
                    send_probe_log(
                        &events,
                        ProbeLogLevel::Warning,
                        "SystemView stream could not be fully drained before disconnect",
                    );
                }
                if let Err(error) = finish_systemview_recording(raw_recording.take(), &events) {
                    let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                    let _ = close_connection(transport);
                    return;
                }
                if let Some(line) = terminal_decoder.finish() {
                    let _ = terminal_lines.publish(line);
                }
                match close_connection(transport) {
                    Ok(()) => {
                        send_probe_log(
                            &events,
                            ProbeLogLevel::Success,
                            "debug-probe session closed",
                        );
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
            let reads_per_poll = MAX_SYSTEMVIEW_RTT_READS_PER_POLL.div_ceil(systemview_read_batch);
            for _ in 0..reads_per_poll {
                match read_and_decode_rtt(
                    &mut transport,
                    configuration.up_channel,
                    systemview_read_size,
                    systemview_read_batch,
                    &mut decoder,
                    &mut trace_events,
                    raw_recording
                        .as_mut()
                        .map(|recording| &mut recording.writer),
                ) {
                    Ok(count) => {
                        bytes_read += count;
                        if count < systemview_read_size.saturating_mul(systemview_read_batch) {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = close_connection(transport);
                        let _ = events.send(SystemTraceWorkerEvent::Error(error.to_string()));
                        return;
                    }
                }
            }
        }
        if let Some(channel_number) = configuration.terminal_up_channel {
            match read_rtt_terminal(
                &mut transport,
                channel_number,
                &mut terminal_decoder,
                &mut terminal_lines,
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

fn systemview_rtt_read_size(
    channels: &[RttChannelDescriptor],
    channel_number: usize,
    probe: &ProbeSelector,
) -> usize {
    let maximum_read_size = if is_network_bmp(probe) {
        NETWORK_BMP_RTT_READ_SIZE
    } else {
        RTT_READ_SIZE
    };
    channels
        .iter()
        .find(|channel| {
            channel.direction == RttChannelDirection::Up && channel.number == channel_number
        })
        .map_or(maximum_read_size, |channel| {
            // An RTT ring always keeps one byte unused to distinguish a full
            // buffer from an empty one. Request its usable capacity so a full
            // read can trigger another immediate drain pass.
            channel
                .buffer_size
                .saturating_sub(1)
                .clamp(1, maximum_read_size)
        })
}

fn is_network_bmp(probe: &ProbeSelector) -> bool {
    probe
        .connection_id
        .strip_prefix(NETWORK_BMP_SELECTOR_PREFIX)
        .is_some_and(|endpoint| endpoint.parse::<std::net::SocketAddr>().is_ok())
}

fn systemview_rtt_read_batch(probe: &ProbeSelector) -> usize {
    if is_network_bmp(probe) {
        NETWORK_BMP_RTT_READ_BATCH
    } else {
        1
    }
}

fn systemview_stop_drain_read_limit(
    channels: &[RttChannelDescriptor],
    channel_number: usize,
    maximum_length: usize,
) -> usize {
    let buffer_capacity = channels
        .iter()
        .find(|channel| {
            channel.direction == RttChannelDirection::Up && channel.number == channel_number
        })
        .map_or(maximum_length, |channel| {
            channel.buffer_size.saturating_sub(1).max(1)
        });
    buffer_capacity
        .div_ceil(maximum_length.max(1))
        .saturating_mul(MAX_SYSTEMVIEW_STOP_DRAIN_BUFFER_PASSES)
        .saturating_add(1)
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
    maximum_length: usize,
    maximum_reads: usize,
    decoder: &mut SystemViewDecoder,
    trace_events: &mut TraceEventPublisher,
    mut raw_recording: Option<&mut BufWriter<File>>,
) -> Result<usize, DebugProbeError> {
    let mut byte_count = 0_usize;
    transport.send_streaming(
        DebugProbeCommand::ReadRttUp {
            channel_number,
            maximum_length,
            maximum_reads,
        },
        &mut |reply| {
            let DebugProbeReply::RttData { data, .. } = reply else {
                return Err(unexpected_reply("read SystemView RTT data"));
            };
            byte_count = byte_count.saturating_add(data.len());
            if let Some(recording) = raw_recording.as_deref_mut() {
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
                trace_events.publish(event)?;
            }
            Ok(())
        },
    )?;
    Ok(byte_count)
}

fn drain_stopped_systemview(
    transport: &mut impl ProbeCommandTransport,
    channel_number: usize,
    maximum_length: usize,
    read_limit: usize,
    decoder: &mut SystemViewDecoder,
    trace_events: &mut TraceEventPublisher,
    mut raw_recording: Option<&mut BufWriter<File>>,
) -> Result<(), DebugProbeError> {
    thread::sleep(IDLE_RTT_POLL_INTERVAL);
    for _ in 0..read_limit {
        let count = read_and_decode_rtt(
            transport,
            channel_number,
            maximum_length,
            1,
            decoder,
            trace_events,
            raw_recording.as_deref_mut(),
        )?;
        if count == 0 {
            return decoder.finish().map_err(|error| {
                DebugProbeError::new(
                    DebugProbeErrorKind::Transport,
                    format!("SystemView stream ended with an incomplete packet: {error}"),
                )
            });
        }
    }
    Err(DebugProbeError::new(
        DebugProbeErrorKind::Transport,
        "SystemView RTT channel did not become empty after stop",
    ))
}

struct TraceEventPublisher {
    sender: SyncSender<LiveTraceUpdate>,
    dropped_events: u64,
}

impl TraceEventPublisher {
    fn new(sender: SyncSender<LiveTraceUpdate>) -> Self {
        Self {
            sender,
            dropped_events: 0,
        }
    }

    fn publish(&mut self, event: TraceEvent) -> Result<(), DebugProbeError> {
        let update = if self.dropped_events == 0 {
            LiveTraceUpdate::Event(event)
        } else {
            let skipped_events = self.dropped_events.saturating_add(1);
            LiveTraceUpdate::DisplayGap {
                sequence: event.sequence,
                core_id: event.core_id,
                timestamp_ticks: event.timestamp_ticks,
                skipped_events,
            }
        };
        match self.sender.try_send(update) {
            Ok(()) => {
                self.dropped_events = 0;
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                self.dropped_events = self.dropped_events.saturating_add(1);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(DebugProbeError::new(
                DebugProbeErrorKind::Disconnected,
                "System Trace event receiver is no longer available",
            )),
        }
    }
}

struct ActiveSystemViewRecording {
    path: PathBuf,
    temporary: bool,
    writer: BufWriter<File>,
}

fn create_systemview_recording(
    requested_path: Option<&Path>,
) -> Result<ActiveSystemViewRecording, DebugProbeError> {
    if let Some(path) = requested_path {
        let file = File::create(path).map_err(|error| recording_create_error(path, error))?;
        return Ok(ActiveSystemViewRecording {
            path: path.to_owned(),
            temporary: false,
            writer: BufWriter::new(file),
        });
    }

    for _ in 0..100 {
        let id = NEXT_TEMPORARY_TRACE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wavecraft-systemview-{}-{id}.SVDat",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                return Ok(ActiveSystemViewRecording {
                    path,
                    temporary: true,
                    writer: BufWriter::new(file),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(recording_create_error(&path, error)),
        }
    }
    Err(DebugProbeError::new(
        DebugProbeErrorKind::Transport,
        "failed to allocate a unique temporary SystemView history file",
    ))
}

fn recording_create_error(path: &Path, error: std::io::Error) -> DebugProbeError {
    DebugProbeError::new(
        DebugProbeErrorKind::Transport,
        format!(
            "failed to create raw SystemView recording {}: {error}",
            path.display()
        ),
    )
}

fn finish_systemview_recording(
    recording: Option<ActiveSystemViewRecording>,
    events: &Sender<SystemTraceWorkerEvent>,
) -> Result<(), DebugProbeError> {
    let Some(mut recording) = recording else {
        return Ok(());
    };
    recording.writer.flush().map_err(|error| {
        DebugProbeError::new(
            DebugProbeErrorKind::Transport,
            format!(
                "failed to finish raw SystemView recording {}: {error}",
                recording.path.display()
            ),
        )
    })?;
    if recording.temporary {
        send_probe_log(
            events,
            ProbeLogLevel::Debug,
            format!(
                "complete SystemView history retained in {}",
                recording.path.display()
            ),
        );
    } else {
        send_probe_log(
            events,
            ProbeLogLevel::Success,
            format!("SystemView recording saved to {}", recording.path.display()),
        );
        let _ = events.send(SystemTraceWorkerEvent::RecordingSaved(recording.path));
    }
    Ok(())
}

fn read_rtt_terminal(
    transport: &mut impl ProbeCommandTransport,
    channel_number: usize,
    decoder: &mut RttTerminalDecoder,
    terminal_lines: &mut TerminalLinePublisher,
) -> Result<usize, DebugProbeError> {
    let data = match transport.send(DebugProbeCommand::ReadRttUp {
        channel_number,
        maximum_length: RTT_READ_SIZE,
        maximum_reads: 1,
    })? {
        DebugProbeReply::RttData { data, .. } => data,
        _ => return Err(unexpected_reply("read plain RTT terminal data")),
    };
    let byte_count = data.len();
    for line in decoder.push(&data) {
        terminal_lines.publish(line)?;
    }
    Ok(byte_count)
}

struct TerminalLinePublisher {
    sender: SyncSender<RttTerminalLine>,
    dropped_lines: u64,
}

impl TerminalLinePublisher {
    fn new(sender: SyncSender<RttTerminalLine>) -> Self {
        Self {
            sender,
            dropped_lines: 0,
        }
    }

    fn publish(&mut self, line: RttTerminalLine) -> Result<(), DebugProbeError> {
        let line = if self.dropped_lines == 0 {
            line
        } else {
            let dropped_lines = self.dropped_lines.saturating_add(1);
            RttTerminalLine::plain(
                line.sequence,
                format!("[WaveCraft: dropped {dropped_lines} RTT terminal lines]"),
                true,
            )
        };
        match self.sender.try_send(line) {
            Ok(()) => {
                self.dropped_lines = 0;
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                self.dropped_lines = self.dropped_lines.saturating_add(1);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(DebugProbeError::new(
                DebugProbeErrorKind::Disconnected,
                "RTT terminal receiver is no longer available",
            )),
        }
    }
}

fn establish_connection(
    transport: &mut impl ProbeCommandTransport,
    configuration: &SystemTraceConnectionConfiguration,
    events: &Sender<SystemTraceWorkerEvent>,
) -> Result<Vec<RttChannelDescriptor>, DebugProbeError> {
    validate_rtt_channel_selection(configuration)?;
    send_probe_log(
        events,
        ProbeLogLevel::Info,
        format!("opening debug probe {}", configuration.probe.connection_id),
    );
    let opened_probe = match transport.send_with_timeout(
        DebugProbeCommand::OpenProbe {
            selector: configuration.probe.clone(),
        },
        PROBE_OPEN_TIMEOUT,
    )? {
        DebugProbeReply::ProbeOpened(probe) => probe,
        _ => return Err(unexpected_reply("open probe")),
    };
    send_probe_log(events, ProbeLogLevel::Success, "debug probe opened");
    if opened_probe.provider == ProbeProvider::JLink
        && matches!(configuration.attach.target, TargetSelector::Auto)
    {
        return Err(DebugProbeError::new(
            DebugProbeErrorKind::InvalidConfiguration,
            "select the target MCU explicitly when using J-Link; automatic target detection is not reliable for this probe",
        ));
    }
    send_probe_log(
        events,
        ProbeLogLevel::Info,
        format!(
            "attaching to target {:?}, protocol {:?}, speed {:?} kHz, core {}",
            configuration.attach.target,
            configuration.attach.wire_protocol,
            configuration.attach.speed_khz,
            configuration.attach.core_index
        ),
    );
    expect_reply(
        transport.send_with_timeout(
            DebugProbeCommand::Attach {
                configuration: configuration.attach.clone(),
            },
            TARGET_ATTACH_TIMEOUT,
        )?,
        |reply| reply == &DebugProbeReply::TargetAttached,
        "attach target",
    )?;
    send_probe_log(events, ProbeLogLevel::Success, "target attached");
    if configuration.reset_after_attach {
        send_probe_log(events, ProbeLogLevel::Info, "resetting target after attach");
        expect_reply(
            transport.send_with_timeout(DebugProbeCommand::ResetTarget, TARGET_RESET_TIMEOUT)?,
            |reply| reply == &DebugProbeReply::TargetReset,
            "reset target",
        )?;
        send_probe_log(events, ProbeLogLevel::Success, "target reset completed");
    }
    send_probe_log(
        events,
        ProbeLogLevel::Info,
        format!(
            "searching for RTT control block at {:?} (timeout {:?})",
            configuration.rtt.control_block, configuration.rtt.timeout
        ),
    );
    let discovery_started = std::time::Instant::now();
    let mut waiting_for_channels_logged = false;
    loop {
        let mut rtt_configuration = configuration.rtt.clone();
        rtt_configuration.timeout = configuration
            .rtt
            .timeout
            .saturating_sub(discovery_started.elapsed());
        let helper_timeout = rtt_configuration
            .timeout
            .saturating_add(RTT_ATTACH_TIMEOUT_MARGIN);
        let channels = match transport.send_with_timeout(
            DebugProbeCommand::AttachRtt {
                configuration: rtt_configuration,
            },
            helper_timeout,
        )? {
            DebugProbeReply::RttAttached(channels) => channels,
            _ => return Err(unexpected_reply("attach RTT")),
        };
        match validate_rtt_channels(&channels, configuration) {
            Ok(()) => {
                send_probe_log(
                    events,
                    ProbeLogLevel::Success,
                    format!("RTT attached with {} channel(s)", channels.len()),
                );
                for channel in &channels {
                    send_probe_log(
                        events,
                        ProbeLogLevel::Debug,
                        format!(
                            "RTT {:?} channel {}{}",
                            channel.direction,
                            channel.number,
                            channel
                                .name
                                .as_deref()
                                .map(|name| format!(" ({name})"))
                                .unwrap_or_default()
                        ),
                    );
                }
                return Ok(channels);
            }
            Err(error) if discovery_started.elapsed() >= configuration.rtt.timeout => {
                return Err(error);
            }
            Err(_) => {
                if !waiting_for_channels_logged {
                    send_probe_log(
                        events,
                        ProbeLogLevel::Debug,
                        "RTT control block found; waiting for configured channels",
                    );
                    waiting_for_channels_logged = true;
                }
                thread::sleep(RTT_CHANNEL_DISCOVERY_RETRY_INTERVAL);
            }
        }
    }
}

fn send_probe_log(
    events: &Sender<SystemTraceWorkerEvent>,
    level: ProbeLogLevel,
    message: impl Into<String>,
) {
    let _ = events.send(SystemTraceWorkerEvent::ProbeLog(ProbeLogMessage {
        level,
        message: message.into(),
    }));
}

fn validate_rtt_channels(
    channels: &[RttChannelDescriptor],
    configuration: &SystemTraceConnectionConfiguration,
) -> Result<(), DebugProbeError> {
    validate_rtt_channel_selection(configuration)?;
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

fn validate_rtt_channel_selection(
    configuration: &SystemTraceConnectionConfiguration,
) -> Result<(), DebugProbeError> {
    if configuration.systemview_enabled
        && configuration.terminal_up_channel == Some(configuration.up_channel)
    {
        return Err(DebugProbeError::new(
            DebugProbeErrorKind::InvalidConfiguration,
            "plain RTT terminal and SystemView must use different up-channels",
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
    pub(crate) fn reload_system_trace_assets(&mut self) {
        let elf_path = self.system_trace_settings.elf_path.trim().to_owned();
        if !elf_path.is_empty() {
            match read_systemview_elf(Path::new(&elf_path)) {
                Ok(elf) => {
                    if let Some(address) = elf.rtt_control_block() {
                        self.system_trace_settings.control_block_address = format!("0x{address:x}");
                    }
                    self.system_trace.set_elf(Some(elf));
                }
                Err(error) => self.last_error = Some(error),
            }
        }
        let description_path = self
            .system_trace_settings
            .description_path
            .trim()
            .to_owned();
        if !description_path.is_empty() {
            match read_systemview_description(Path::new(&description_path)) {
                Ok(description) => self.system_trace.set_description(Some(description)),
                Err(error) => self.last_error = Some(error),
            }
        }
    }

    pub(crate) fn begin_open_system_trace(&mut self, path: PathBuf) {
        let trace_is_idle = matches!(
            self.system_trace_connection,
            SystemTraceConnectionState::Disconnected
                | SystemTraceConnectionState::Error(_)
                | SystemTraceConnectionState::Attached { .. }
        );
        if self.pending.is_some() || !trace_is_idle {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.status = format!("Opening SystemView trace {}…", path.display());
        self.last_error = None;
        thread::spawn(move || {
            let result = read_systemview_trace_file(&path)
                .map(LoadedSystemTrace::from_events)
                .map(|trace| WorkerSuccess::SystemTraceLoaded {
                    path,
                    trace: Box::new(trace),
                });
            let _ = sender.send(result);
        });
    }

    pub(crate) fn begin_save_system_trace_history(&mut self, path: PathBuf) {
        if self.pending.is_some() || !self.system_trace.history_is_temporary() {
            return;
        }
        let Some(source) = self.system_trace.history_path().map(Path::to_owned) else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.status = format!("Saving SystemView trace to {}…", path.display());
        self.last_error = None;
        thread::spawn(move || {
            let result = save_systemview_history(&source, &path)
                .map(|()| WorkerSuccess::SystemTraceSaved { source, path });
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
        if self.pending.is_some() {
            return;
        }
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
            self.system_trace.begin_live_trace();
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
        let mut completed_history = None;
        if let Some(events) = &self.system_trace_worker_events {
            loop {
                match events.try_recv() {
                    Ok(SystemTraceWorkerEvent::ProbeLog(message)) => {
                        self.system_trace.ingest_probe_log(message);
                    }
                    Ok(SystemTraceWorkerEvent::Attached {
                        channels,
                        systemview_enabled,
                    }) => {
                        self.system_trace_connection = SystemTraceConnectionState::Attached {
                            channels,
                            systemview_enabled,
                        };
                    }
                    Ok(SystemTraceWorkerEvent::Tracing {
                        recording_path,
                        history_path,
                        history_is_temporary,
                    }) => {
                        self.system_trace
                            .set_history_path(history_path, history_is_temporary);
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
                        completed_history = self.system_trace.history_path().map(Path::to_owned);
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
                        self.system_trace.ingest_probe_log(ProbeLogMessage {
                            level: ProbeLogLevel::Error,
                            message: error.clone(),
                        });
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
                            let error = "debug-probe worker ended unexpectedly".to_owned();
                            self.system_trace.ingest_probe_log(ProbeLogMessage {
                                level: ProbeLogLevel::Error,
                                message: error.clone(),
                            });
                            self.system_trace_connection = SystemTraceConnectionState::Error(error);
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
        if let Some(path) = completed_history {
            self.begin_open_system_trace(path);
        }
    }
}

/// Display state is intentionally independent of a debug-probe backend. A
/// future capture worker can feed this type events from any supported probe.
pub(crate) struct LoadedSystemTrace {
    events: VecDeque<TraceEvent>,
    model: TraceModel,
    marker_names: BTreeMap<u64, String>,
    api_event_lanes: BTreeSet<(u32, u32)>,
    target_frequencies_hz: Option<(u64, u64)>,
    log_count: usize,
    target_dropped_event_count: u64,
}

impl LoadedSystemTrace {
    fn from_events(events: Vec<TraceEvent>) -> Self {
        let mut model = TraceModel::new();
        let mut marker_names = BTreeMap::new();
        let mut api_event_lanes = BTreeSet::new();
        let mut target_frequencies_hz = None;
        let mut log_count = 0_usize;
        let mut target_dropped_event_count = 0_u64;
        for event in &events {
            model.apply(event);
            if let Some(event_id) = api_event_id(&event.kind) {
                api_event_lanes.insert((event.core_id, event_id));
            }
            match &event.kind {
                TraceEventKind::MarkerName { marker_id, name } => {
                    marker_names.insert(*marker_id, name.clone());
                }
                TraceEventKind::TargetInfo {
                    system_frequency_hz,
                    cpu_frequency_hz,
                    ..
                } => {
                    target_frequencies_hz = Some((*system_frequency_hz, *cpu_frequency_hz));
                }
                TraceEventKind::Overflow { dropped_events } => {
                    target_dropped_event_count =
                        target_dropped_event_count.saturating_add(*dropped_events);
                }
                kind if is_log_event(kind) => log_count = log_count.saturating_add(1),
                _ => {}
            }
        }
        Self {
            events: events.into(),
            model,
            marker_names,
            api_event_lanes,
            target_frequencies_hz,
            log_count,
            target_dropped_event_count,
        }
    }

    pub(crate) fn event_count(&self) -> usize {
        self.events.len()
    }
}

#[derive(Debug)]
pub(crate) struct SystemTraceDisplayState {
    events: VecDeque<TraceEvent>,
    terminal_lines: VecDeque<RttTerminalLine>,
    probe_logs: VecDeque<ProbeLogEntry>,
    model: TraceModel,
    marker_names: BTreeMap<u64, String>,
    api_event_lanes: BTreeSet<(u32, u32)>,
    elf: Option<SystemViewElf>,
    description: Option<SystemViewDescription>,
    target_frequencies_hz: Option<(u64, u64)>,
    log_count: usize,
    display_skipped_event_count: u64,
    target_dropped_event_count: u64,
    history_path: Option<PathBuf>,
    history_is_temporary: bool,
    event_capacity: usize,
    terminal_line_capacity: usize,
    probe_log_capacity: usize,
    next_probe_log_sequence: u64,
    pub(crate) selected_sequence: Option<u64>,
    event_table_focus_sequence: Option<u64>,
    timeline_focus_sequence: Option<u64>,
    pub(crate) event_view: TraceEventView,
    event_view_order: [TraceEventView; TraceEventView::ALL.len()],
    dragged_event_view: Option<TraceEventView>,
    trace_instrument_order: [TraceInstrument; TraceInstrument::ALL.len()],
    dragged_trace_instrument: Option<TraceInstrument>,
    layout_changed: bool,
    pub(crate) event_filter: String,
    pub(crate) event_category_filter: TraceEventCategoryFilter,
    pub(crate) event_filter_cache: EventFilterCache,
    event_search_generation: u64,
    pub(crate) time_display: TraceTimeDisplay,
    pub(crate) go_to_value: String,
    pub(crate) follow_latest: bool,
    pub(crate) terminal_wrap: bool,
    pub(crate) timeline_smart_timing: bool,
    timeline_timing_measurement: Option<TimelineTimingMeasurement>,
    timeline_timing_dragging: bool,
    timeline_ticks_per_division: Option<f64>,
    timeline_view_start_ticks: Option<u64>,
    timeline_visible_range: Option<(u64, u64)>,
    time_references: Vec<u64>,
    timeline_view_height: f32,
    event_view_height: f32,
    timeline_label_width: f32,
}

impl Default for SystemTraceDisplayState {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            terminal_lines: VecDeque::new(),
            probe_logs: VecDeque::new(),
            model: TraceModel::new(),
            marker_names: BTreeMap::new(),
            api_event_lanes: BTreeSet::new(),
            elf: None,
            description: None,
            target_frequencies_hz: None,
            log_count: 0,
            display_skipped_event_count: 0,
            target_dropped_event_count: 0,
            history_path: None,
            history_is_temporary: false,
            event_capacity: DEFAULT_EVENT_CAPACITY,
            terminal_line_capacity: DEFAULT_TERMINAL_LINE_CAPACITY,
            probe_log_capacity: DEFAULT_PROBE_LOG_CAPACITY,
            next_probe_log_sequence: 0,
            selected_sequence: None,
            event_table_focus_sequence: None,
            timeline_focus_sequence: None,
            event_view: TraceEventView::Events,
            event_view_order: TraceEventView::ALL,
            dragged_event_view: None,
            trace_instrument_order: TraceInstrument::ALL,
            dragged_trace_instrument: None,
            layout_changed: false,
            event_filter: String::new(),
            event_category_filter: TraceEventCategoryFilter::default(),
            event_filter_cache: EventFilterCache::default(),
            event_search_generation: 0,
            time_display: TraceTimeDisplay::Target,
            go_to_value: String::new(),
            follow_latest: true,
            terminal_wrap: true,
            timeline_smart_timing: true,
            timeline_timing_measurement: None,
            timeline_timing_dragging: false,
            timeline_ticks_per_division: None,
            timeline_view_start_ticks: None,
            timeline_visible_range: None,
            time_references: Vec::new(),
            timeline_view_height: DEFAULT_TIMELINE_VIEW_HEIGHT,
            event_view_height: DEFAULT_EVENT_VIEW_HEIGHT,
            timeline_label_width: DEFAULT_TIMELINE_LABEL_WIDTH,
        }
    }
}

impl SystemTraceDisplayState {
    pub(crate) fn events(&self) -> &VecDeque<TraceEvent> {
        &self.events
    }

    pub(crate) fn api_event_lanes(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.api_event_lanes.iter().copied()
    }

    pub(crate) fn event_view_order(&self) -> &[TraceEventView; TraceEventView::ALL.len()] {
        &self.event_view_order
    }

    pub(crate) fn set_event_view_order(&mut self, order: &[TraceEventView]) -> bool {
        if order.len() != TraceEventView::ALL.len()
            || !TraceEventView::ALL
                .iter()
                .all(|view| order.iter().filter(|candidate| *candidate == view).count() == 1)
        {
            return false;
        }
        self.event_view_order.copy_from_slice(order);
        true
    }

    pub(crate) fn begin_event_view_drag(&mut self, view: TraceEventView) {
        self.dragged_event_view = Some(view);
    }

    pub(crate) fn dragged_event_view(&self) -> Option<TraceEventView> {
        self.dragged_event_view
    }

    pub(crate) fn drag_event_view_to(&mut self, target: TraceEventView) {
        let Some(dragged) = self.dragged_event_view else {
            return;
        };
        let Some(from) = self
            .event_view_order
            .iter()
            .position(|view| *view == dragged)
        else {
            return;
        };
        let Some(to) = self
            .event_view_order
            .iter()
            .position(|view| *view == target)
        else {
            return;
        };
        if from == to {
            return;
        }
        if from < to {
            self.event_view_order[from..=to].rotate_left(1);
        } else {
            self.event_view_order[to..=from].rotate_right(1);
        }
        self.layout_changed = true;
    }

    pub(crate) fn finish_event_view_drag(&mut self) {
        self.dragged_event_view = None;
    }

    pub(crate) fn trace_instrument_order(&self) -> &[TraceInstrument; TraceInstrument::ALL.len()] {
        &self.trace_instrument_order
    }

    pub(crate) fn set_trace_instrument_order(&mut self, order: &[TraceInstrument]) -> bool {
        if order.len() != TraceInstrument::ALL.len()
            || !TraceInstrument::ALL.iter().all(|instrument| {
                order
                    .iter()
                    .filter(|candidate| *candidate == instrument)
                    .count()
                    == 1
            })
        {
            return false;
        }
        self.trace_instrument_order.copy_from_slice(order);
        true
    }

    pub(crate) fn begin_trace_instrument_drag(&mut self, instrument: TraceInstrument) {
        self.dragged_trace_instrument = Some(instrument);
    }

    pub(crate) fn dragged_trace_instrument(&self) -> Option<TraceInstrument> {
        self.dragged_trace_instrument
    }

    pub(crate) fn drag_trace_instrument_to(&mut self, target: TraceInstrument) {
        let Some(dragged) = self.dragged_trace_instrument else {
            return;
        };
        if dragged != target {
            self.trace_instrument_order.swap(0, 1);
            self.layout_changed = true;
        }
    }

    pub(crate) fn finish_trace_instrument_drag(&mut self) {
        self.dragged_trace_instrument = None;
    }

    pub(crate) fn take_layout_changed(&mut self) -> bool {
        if self.dragged_event_view.is_some() || self.dragged_trace_instrument.is_some() {
            return false;
        }
        std::mem::take(&mut self.layout_changed)
    }

    pub(crate) fn model(&self) -> &TraceModel {
        &self.model
    }

    pub(crate) fn elf(&self) -> Option<&SystemViewElf> {
        self.elf.as_ref()
    }

    pub(crate) fn set_elf(&mut self, elf: Option<SystemViewElf>) {
        self.elf = elf;
        self.invalidate_event_search_metadata();
    }

    pub(crate) fn description(&self) -> Option<&SystemViewDescription> {
        self.description.as_ref()
    }

    pub(crate) fn set_description(&mut self, description: Option<SystemViewDescription>) {
        self.description = description;
        self.invalidate_event_search_metadata();
    }

    pub(crate) fn event_search_generation(&self) -> u64 {
        self.event_search_generation
    }

    fn invalidate_event_search_metadata(&mut self) {
        self.event_search_generation = self.event_search_generation.wrapping_add(1);
    }

    pub(crate) fn description_event_id(&self, event_id: u32) -> Option<u32> {
        let description = self.description.as_ref()?;
        if description.event(event_id).is_some() {
            return Some(event_id);
        }
        self.events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                TraceEventKind::ModuleDescription { event_offset, .. } => {
                    let event_offset = u32::try_from(*event_offset).ok()?;
                    let local_event_id = event_id.checked_sub(event_offset)?;
                    description
                        .event(local_event_id)
                        .is_some()
                        .then_some(local_event_id)
                }
                _ => None,
            })
    }

    pub(crate) fn resource_name(&self, resource_id: u64) -> Option<&str> {
        if let Some(name) = self
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                TraceEventKind::ResourceName {
                    resource_id: candidate,
                    name,
                } if *candidate == resource_id => Some(name.as_str()),
                _ => None,
            })
        {
            return Some(name);
        }
        let address = self.resource_address(resource_id)?;
        self.elf.as_ref()?.symbol_name(address)
    }

    pub(crate) fn marker_name(&self, marker_id: u64) -> Option<&str> {
        self.marker_names.get(&marker_id).map(String::as_str)
    }

    pub(crate) fn resource_address(&self, resource_id: u64) -> Option<u64> {
        let (ram_base, id_shift) = self
            .events
            .iter()
            .rev()
            .find_map(|event| match event.kind {
                TraceEventKind::TargetInfo {
                    ram_base, id_shift, ..
                } => Some((ram_base, id_shift)),
                _ => None,
            })?;
        let shifted = resource_id.checked_shl(u32::try_from(id_shift).ok()?)?;
        ram_base.checked_add(shifted)
    }

    pub(crate) fn target_frequencies_hz(&self) -> Option<(u64, u64)> {
        self.target_frequencies_hz
    }

    pub(crate) fn terminal_lines(&self) -> &VecDeque<RttTerminalLine> {
        &self.terminal_lines
    }

    pub(crate) fn probe_logs(&self) -> &VecDeque<ProbeLogEntry> {
        &self.probe_logs
    }

    pub(crate) fn ingest_probe_log(&mut self, message: ProbeLogMessage) {
        if self.probe_logs.len() == self.probe_log_capacity {
            self.probe_logs.pop_front();
        }
        self.probe_logs.push_back(ProbeLogEntry {
            sequence: self.next_probe_log_sequence,
            level: message.level,
            message: message.message,
        });
        self.next_probe_log_sequence = self.next_probe_log_sequence.saturating_add(1);
    }

    pub(crate) fn timeline_ticks_per_division(&self) -> Option<f64> {
        self.timeline_ticks_per_division
    }

    pub(crate) fn set_timeline_ticks_per_division(&mut self, ticks: Option<f64>) {
        self.timeline_ticks_per_division = ticks.filter(|ticks| ticks.is_finite() && *ticks > 0.0);
    }

    pub(crate) fn timeline_view_start_ticks(&self) -> Option<u64> {
        self.timeline_view_start_ticks
    }

    pub(crate) fn set_timeline_view_start_ticks(&mut self, timestamp: Option<u64>) {
        self.timeline_view_start_ticks = timestamp;
    }

    pub(crate) fn timeline_timing_measurement(&self) -> Option<TimelineTimingMeasurement> {
        self.timeline_timing_measurement
    }

    pub(crate) fn timeline_timing_dragging(&self) -> bool {
        self.timeline_timing_dragging
    }

    pub(crate) fn start_timeline_timing(&mut self, edge: TimelineTimingEdge) {
        self.timeline_timing_measurement = Some(TimelineTimingMeasurement {
            first: edge,
            second: edge,
        });
        self.timeline_timing_dragging = true;
    }

    pub(crate) fn update_timeline_timing(&mut self, edge: TimelineTimingEdge) {
        if let Some(measurement) = &mut self.timeline_timing_measurement {
            measurement.second = edge;
        }
    }

    pub(crate) fn finish_timeline_timing(&mut self) {
        self.timeline_timing_dragging = false;
    }

    pub(crate) fn clear_timeline_timing(&mut self) {
        self.timeline_timing_measurement = None;
        self.timeline_timing_dragging = false;
    }

    pub(crate) fn timeline_visible_range(&self) -> Option<(u64, u64)> {
        self.timeline_visible_range
    }

    pub(crate) fn set_timeline_visible_range(&mut self, range: Option<(u64, u64)>) {
        self.timeline_visible_range = range;
    }

    pub(crate) fn time_references(&self) -> &[u64] {
        &self.time_references
    }

    pub(crate) fn add_time_reference(&mut self, timestamp_ticks: u64) {
        if !self.time_references.contains(&timestamp_ticks) {
            self.time_references.push(timestamp_ticks);
            self.time_references.sort_unstable();
        }
    }

    pub(crate) fn clear_time_references(&mut self) {
        self.time_references.clear();
    }

    pub(crate) fn timeline_view_height(&self) -> f32 {
        self.timeline_view_height
    }

    pub(crate) fn event_view_height(&self) -> f32 {
        self.event_view_height
    }

    pub(crate) fn timeline_label_width(&self) -> f32 {
        self.timeline_label_width
    }

    pub(crate) fn set_timeline_label_width(&mut self, width: f32) -> bool {
        if !width.is_finite() || width <= 0.0 {
            return false;
        }
        self.timeline_label_width = width.clamp(MIN_TIMELINE_LABEL_WIDTH, MAX_TIMELINE_LABEL_WIDTH);
        true
    }

    pub(crate) fn set_trace_instrument_heights(
        &mut self,
        timeline_height: f32,
        analysis_height: f32,
    ) -> bool {
        if !timeline_height.is_finite()
            || !analysis_height.is_finite()
            || timeline_height <= 0.0
            || analysis_height <= 0.0
        {
            return false;
        }
        self.timeline_view_height = timeline_height.clamp(1.0, MAX_TRACE_INSTRUMENT_HEIGHT);
        self.event_view_height = analysis_height.clamp(1.0, MAX_TRACE_INSTRUMENT_HEIGHT);
        true
    }

    pub(crate) fn fit_trace_instruments_to_height(&mut self, available_height: f32) {
        if !available_height.is_finite() {
            return;
        }
        let target_height = available_height.clamp(0.0, MAX_TRACE_INSTRUMENT_HEIGHT * 2.0);
        let current_height = self.timeline_view_height + self.event_view_height;
        let timeline_fraction = if current_height > 0.0 {
            self.timeline_view_height / current_height
        } else {
            0.5
        };
        let effective_minimum = MIN_TRACE_INSTRUMENT_HEIGHT.min(target_height * 0.5);
        let minimum_timeline_height =
            effective_minimum.max(target_height - MAX_TRACE_INSTRUMENT_HEIGHT);
        let maximum_timeline_height =
            MAX_TRACE_INSTRUMENT_HEIGHT.min(target_height - effective_minimum);
        self.timeline_view_height = (target_height * timeline_fraction)
            .clamp(minimum_timeline_height, maximum_timeline_height);
        self.event_view_height = target_height - self.timeline_view_height;
    }

    pub(crate) fn move_trace_instrument_splitter(&mut self, delta: f32) {
        if !delta.is_finite() {
            return;
        }
        let timeline_delta = if self.trace_instrument_order[0] == TraceInstrument::Timeline {
            delta
        } else {
            -delta
        };
        let total_height = self.timeline_view_height + self.event_view_height;
        let effective_minimum = MIN_TRACE_INSTRUMENT_HEIGHT.min(total_height * 0.5);
        let minimum_timeline_height =
            effective_minimum.max(total_height - MAX_TRACE_INSTRUMENT_HEIGHT);
        let maximum_timeline_height =
            MAX_TRACE_INSTRUMENT_HEIGHT.min(total_height - effective_minimum);
        self.timeline_view_height = (self.timeline_view_height + timeline_delta)
            .clamp(minimum_timeline_height, maximum_timeline_height);
        self.event_view_height = total_height - self.timeline_view_height;
    }

    pub(crate) fn finish_trace_instrument_resize(&mut self) {
        self.layout_changed = true;
    }

    pub(crate) fn select_from_timeline(&mut self, sequence: u64) {
        self.selected_sequence = Some(sequence);
        self.event_table_focus_sequence = Some(sequence);
        self.timeline_focus_sequence = None;
        self.event_view = TraceEventView::Events;
        self.event_filter.clear();
    }

    pub(crate) fn select_from_event_table(&mut self, sequence: u64) {
        self.selected_sequence = Some(sequence);
        self.timeline_focus_sequence = Some(sequence);
        self.event_table_focus_sequence = None;
    }

    pub(crate) fn take_event_table_focus(&mut self) -> Option<u64> {
        self.event_table_focus_sequence.take()
    }

    pub(crate) fn take_timeline_focus(&mut self) -> Option<u64> {
        self.timeline_focus_sequence.take()
    }

    pub(crate) fn ingest(&mut self, event: TraceEvent) {
        if let Some(event_id) = api_event_id(&event.kind) {
            self.api_event_lanes.insert((event.core_id, event_id));
        }
        if matches!(
            &event.kind,
            TraceEventKind::TaskInfo { .. }
                | TraceEventKind::TargetInfo { .. }
                | TraceEventKind::ModuleDescription { .. }
                | TraceEventKind::ResourceName { .. }
                | TraceEventKind::MarkerName { .. }
        ) {
            self.invalidate_event_search_metadata();
        }
        if let TraceEventKind::TargetInfo {
            system_frequency_hz,
            cpu_frequency_hz,
            ..
        } = &event.kind
        {
            self.target_frequencies_hz = Some((*system_frequency_hz, *cpu_frequency_hz));
        }
        if let TraceEventKind::MarkerName { marker_id, name } = &event.kind {
            self.marker_names.insert(*marker_id, name.clone());
        }
        self.model.apply(&event);
        if let TraceEventKind::Overflow { dropped_events } = &event.kind {
            self.target_dropped_event_count = self
                .target_dropped_event_count
                .saturating_add(*dropped_events);
        }
        if is_log_event(&event.kind) {
            self.log_count = self.log_count.saturating_add(1);
        }
        if self.events.len() == self.event_capacity {
            if let Some(discarded) = self.events.pop_front() {
                if is_log_event(&discarded.kind) {
                    self.log_count = self.log_count.saturating_sub(1);
                }
                if Some(discarded.sequence) == self.selected_sequence {
                    self.selected_sequence = None;
                    self.event_table_focus_sequence = None;
                    self.timeline_focus_sequence = None;
                }
            }
        }
        self.events.push_back(event);
    }

    pub(crate) fn ingest_display_gap(
        &mut self,
        sequence: u64,
        core_id: u32,
        timestamp_ticks: u64,
        skipped_events: u64,
    ) {
        self.display_skipped_event_count = self
            .display_skipped_event_count
            .saturating_add(skipped_events);
        self.model.apply(&TraceEvent {
            sequence,
            core_id,
            timestamp_ticks,
            kind: TraceEventKind::Overflow {
                dropped_events: skipped_events,
            },
        });
    }

    pub(crate) fn ingest_terminal_line(&mut self, line: RttTerminalLine) {
        if self.terminal_lines.len() == self.terminal_line_capacity {
            self.terminal_lines.pop_front();
        }
        self.terminal_lines.push_back(line);
    }

    #[cfg(test)]
    pub(crate) fn replace_trace_events(&mut self, events: Vec<TraceEvent>) {
        self.replace_loaded_trace(LoadedSystemTrace::from_events(events));
    }

    pub(crate) fn replace_loaded_trace(&mut self, trace: LoadedSystemTrace) {
        self.clear_trace_events();
        self.event_capacity = trace.events.len().max(DEFAULT_EVENT_CAPACITY);
        self.event_filter.clear();
        self.event_view = TraceEventView::Events;
        self.events = trace.events;
        self.model = trace.model;
        self.marker_names = trace.marker_names;
        self.api_event_lanes = trace.api_event_lanes;
        self.target_frequencies_hz = trace.target_frequencies_hz;
        self.log_count = trace.log_count;
        self.target_dropped_event_count = trace.target_dropped_event_count;
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
        self.probe_logs.clear();
    }

    pub(crate) fn clear_trace_events(&mut self) {
        self.events.clear();
        self.model = TraceModel::new();
        self.marker_names.clear();
        self.api_event_lanes.clear();
        self.event_capacity = DEFAULT_EVENT_CAPACITY;
        self.event_filter_cache = EventFilterCache::default();
        self.invalidate_event_search_metadata();
        self.target_frequencies_hz = None;
        self.log_count = 0;
        self.display_skipped_event_count = 0;
        self.target_dropped_event_count = 0;
        self.selected_sequence = None;
        self.event_table_focus_sequence = None;
        self.timeline_focus_sequence = None;
        self.clear_timeline_timing();
        self.timeline_ticks_per_division = None;
        self.timeline_view_start_ticks = None;
        self.timeline_visible_range = None;
        self.time_references.clear();
    }

    pub(crate) fn begin_live_trace(&mut self) {
        self.discard_temporary_history();
        self.history_path = None;
        self.history_is_temporary = false;
        self.clear_trace_events();
    }

    pub(crate) fn set_history_path(&mut self, path: PathBuf, temporary: bool) {
        self.discard_temporary_history();
        self.history_path = Some(path);
        self.history_is_temporary = temporary;
    }

    pub(crate) fn history_path(&self) -> Option<&Path> {
        self.history_path.as_deref()
    }

    pub(crate) fn history_is_temporary(&self) -> bool {
        self.history_is_temporary && self.history_path.is_some()
    }

    fn discard_temporary_history(&mut self) {
        if self.history_is_temporary {
            if let Some(path) = &self.history_path {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    pub(crate) fn cleanup_temporary_history(&mut self) {
        self.discard_temporary_history();
        self.history_path = None;
        self.history_is_temporary = false;
    }

    pub(crate) fn time_bounds(&self) -> Option<(u64, u64)> {
        let first = self.events.front()?.timestamp_ticks;
        let last = self.events.back()?.timestamp_ticks;
        Some((first.min(last), first.max(last)))
    }

    pub(crate) fn dropped_event_count(&self) -> u64 {
        self.target_dropped_event_count
    }

    pub(crate) fn display_skipped_event_count(&self) -> u64 {
        self.display_skipped_event_count
    }

    pub(crate) fn selected_event(&self) -> Option<&TraceEvent> {
        let selected_sequence = self.selected_sequence?;
        self.events
            .iter()
            .find(|event| event.sequence == selected_sequence)
    }

    pub(crate) fn log_count(&self) -> usize {
        self.log_count
    }

    #[cfg(test)]
    fn with_capacity(event_capacity: usize) -> Self {
        Self {
            event_capacity,
            ..Self::default()
        }
    }
}

fn is_log_event(kind: &TraceEventKind) -> bool {
    matches!(
        kind,
        TraceEventKind::FormattedPrint { .. } | TraceEventKind::PrintElf { .. }
    )
}

pub(crate) fn api_event_id(kind: &TraceEventKind) -> Option<u32> {
    match kind {
        TraceEventKind::ApiCall { event_id, .. } => Some(*event_id),
        TraceEventKind::ApiCallEnded { event_id, .. } => u32::try_from(*event_id).ok(),
        _ => None,
    }
}

pub(crate) fn choose_systemview_trace_to_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open raw SystemView trace")
        .add_filter("SystemView data", &["SVDat", "svdat"])
        .pick_file()
}

pub(crate) fn choose_systemview_elf() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Select application ELF")
        .add_filter("ELF application", &["elf", "axf", "out"])
        .pick_file()
}

pub(crate) fn choose_systemview_description() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Select SystemView description")
        .add_filter("SystemView description", &["txt"])
        .pick_file()
}

pub(crate) fn read_systemview_elf(path: &Path) -> Result<SystemViewElf, String> {
    let data = std::fs::read(path)
        .map_err(|error| format!("failed to read application ELF {}: {error}", path.display()))?;
    SystemViewElf::parse(&data).map_err(|error| {
        format!(
            "failed to parse application ELF {}: {error}",
            path.display()
        )
    })
}

pub(crate) fn read_systemview_description(path: &Path) -> Result<SystemViewDescription, String> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "failed to read SystemView description {}: {error}",
            path.display()
        )
    })?;
    SystemViewDescription::parse(&text).map_err(|error| {
        format!(
            "failed to parse SystemView description {}: {error}",
            path.display()
        )
    })
}

pub(crate) fn choose_systemview_recording_path() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Record raw SystemView trace")
        .add_filter("SystemView data", &["SVDat"])
        .set_file_name("system-trace.SVDat")
        .save_file()
        .map(normalize_systemview_recording_path)
}

pub(crate) fn choose_systemview_history_save_path() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Save SystemView trace")
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

fn save_systemview_history(source: &Path, path: &Path) -> Result<(), String> {
    std::fs::copy(source, path).map(|_| ()).map_err(|error| {
        format!(
            "failed to save SystemView trace from {} to {}: {error}",
            source.display(),
            path.display()
        )
    })
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

    struct ObservingStreamingTransport {
        chunks: VecDeque<Vec<u8>>,
        receiver: Receiver<LiveTraceUpdate>,
        chunks_observed: usize,
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

    impl ProbeCommandTransport for ObservingStreamingTransport {
        fn send(
            &mut self,
            _command: DebugProbeCommand,
        ) -> Result<DebugProbeReply, DebugProbeError> {
            panic!("streaming test must use send_streaming")
        }

        fn send_streaming(
            &mut self,
            _command: DebugProbeCommand,
            publish: &mut dyn FnMut(DebugProbeReply) -> Result<(), DebugProbeError>,
        ) -> Result<(), DebugProbeError> {
            while let Some(data) = self.chunks.pop_front() {
                let more = !self.chunks.is_empty();
                publish(DebugProbeReply::RttData { data, more })?;
                assert!(matches!(
                    self.receiver.try_recv(),
                    Ok(LiveTraceUpdate::Event(_))
                ));
                self.chunks_observed += 1;
            }
            Ok(())
        }

        fn shutdown(self) -> Result<(), DebugProbeError> {
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

    fn lifecycle_events(
        events: impl IntoIterator<Item = SystemTraceWorkerEvent>,
    ) -> Vec<SystemTraceWorkerEvent> {
        events
            .into_iter()
            .filter(|event| !matches!(event, SystemTraceWorkerEvent::ProbeLog(_)))
            .collect()
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
    fn systemview_read_size_uses_the_rtt_ring_usable_capacity() {
        let channels = [RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Up,
            name: Some("SystemView".to_owned()),
            buffer_size: 8 * 1024,
        }];

        assert_eq!(
            systemview_rtt_read_size(&channels, 1, &connection_configuration().probe),
            8 * 1024 - 1
        );
    }

    #[test]
    fn systemview_read_size_stays_within_the_helper_transfer_limit() {
        let channels = [RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Up,
            name: Some("SystemView".to_owned()),
            buffer_size: RTT_READ_SIZE * 2,
        }];

        assert_eq!(
            systemview_rtt_read_size(&channels, 1, &connection_configuration().probe),
            RTT_READ_SIZE
        );
    }

    #[test]
    fn network_bmp_uses_shorter_reads_to_release_target_buffer_space_sooner() {
        let channels = [RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Up,
            name: Some("SystemView".to_owned()),
            buffer_size: 8 * 1024,
        }];
        let probe = ProbeSelector::connection_id("1d50:6018:192.0.2.42:2000");

        assert_eq!(
            systemview_rtt_read_size(&channels, 1, &probe),
            NETWORK_BMP_RTT_READ_SIZE
        );
        assert_eq!(
            systemview_rtt_read_batch(&probe),
            NETWORK_BMP_RTT_READ_BATCH
        );
        assert_eq!(
            systemview_rtt_read_batch(&connection_configuration().probe),
            1
        );

        let network_jlink =
            ProbeSelector::connection_id("wavecraft:jlink-network:192.0.2.43:19020");
        assert_eq!(
            systemview_rtt_read_size(&channels, 1, &network_jlink),
            8 * 1024 - 1
        );
        assert_eq!(systemview_rtt_read_batch(&network_jlink), 1);
    }

    #[test]
    fn streamed_rtt_chunks_reach_the_timeline_before_the_batch_completes() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let mut transport = ObservingStreamingTransport {
            chunks: VecDeque::from([vec![10, 1], vec![17, 1]]),
            receiver,
            chunks_observed: 0,
        };
        let mut decoder = SystemViewDecoder::default();
        let mut publisher = TraceEventPublisher::new(sender);

        let bytes =
            read_and_decode_rtt(&mut transport, 1, 2, 2, &mut decoder, &mut publisher, None)
                .expect("streamed RTT data decodes");

        assert_eq!(bytes, 4);
        assert_eq!(transport.chunks_observed, 2);
    }

    #[test]
    fn network_bmp_stop_drain_covers_full_rtt_buffer_and_confirms_it_is_empty() {
        let channels = [RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Up,
            name: Some("SystemView".to_owned()),
            buffer_size: 8 * 1024,
        }];

        let read_limit = systemview_stop_drain_read_limit(&channels, 1, NETWORK_BMP_RTT_READ_SIZE);
        let reads_for_one_full_buffer = (8_usize * 1024 - 1).div_ceil(NETWORK_BMP_RTT_READ_SIZE);

        assert!(read_limit > reads_for_one_full_buffer);
    }

    #[test]
    fn full_live_event_queue_reports_a_display_gap_without_blocking() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut publisher = TraceEventPublisher::new(sender);

        publisher
            .publish(event(0, 1, TraceEventKind::Idle))
            .expect("first event fits");
        publisher
            .publish(event(1, 2, TraceEventKind::Idle))
            .expect("a full display queue does not block the probe worker");
        assert_eq!(
            receiver.recv().expect("first event is queued"),
            LiveTraceUpdate::Event(event(0, 1, TraceEventKind::Idle))
        );

        publisher
            .publish(event(2, 3, TraceEventKind::Idle))
            .expect("overflow marker fits after the UI drains the queue");

        assert_eq!(
            receiver.recv().expect("overflow marker is queued"),
            LiveTraceUpdate::DisplayGap {
                sequence: 2,
                core_id: 0,
                timestamp_ticks: 3,
                skipped_events: 2,
            }
        );
    }

    #[test]
    fn target_loss_and_live_display_skips_are_reported_separately() {
        let mut state = SystemTraceDisplayState::default();
        state.ingest(event(0, 1, TraceEventKind::Overflow { dropped_events: 3 }));

        state.ingest_display_gap(10, 0, 20, 7);

        assert_eq!(state.dropped_event_count(), 3);
        assert_eq!(state.display_skipped_event_count(), 7);
        assert_eq!(
            state.model().core(0).map(|core| core.dropped_events()),
            Some(10)
        );
    }

    #[test]
    fn temporary_recording_retains_complete_history_until_cleanup() {
        let mut recording =
            create_systemview_recording(None).expect("temporary history is created");
        assert!(recording.temporary);
        let path = recording.path.clone();
        recording
            .writer
            .write_all(&[10, 1])
            .expect("trace packet is written");
        let (event_sender, event_receiver) = mpsc::channel();

        finish_systemview_recording(Some(recording), &event_sender)
            .expect("temporary history is flushed");

        assert!(path.is_file());
        assert_eq!(
            read_systemview_trace_file(&path).expect("temporary history can be reopened"),
            vec![event(0, 1, TraceEventKind::TraceStarted)]
        );
        assert!(!event_receiver
            .try_iter()
            .any(|event| matches!(event, SystemTraceWorkerEvent::RecordingSaved(_))));
        let mut state = SystemTraceDisplayState::default();
        state.set_history_path(path.clone(), true);
        state.cleanup_temporary_history();
        assert!(!path.exists());
    }

    #[test]
    fn temporary_history_can_be_promoted_to_a_saved_trace() {
        let directory = crate::app::test_support::scratch_directory("systemview-history-save");
        let source = directory.join("temporary.SVDat");
        let saved = directory.join("saved.SVDat");
        std::fs::write(&source, [10, 1]).expect("temporary history is written");
        std::fs::write(&saved, b"old trace").expect("existing destination is written");

        save_systemview_history(&source, &saved)
            .expect("temporary history replaces the confirmed destination");
        let mut state = SystemTraceDisplayState::default();
        state.set_history_path(source.clone(), true);
        state.set_history_path(saved.clone(), false);

        assert!(!source.exists());
        assert_eq!(std::fs::read(&saved).expect("saved trace exists"), [10, 1]);
        assert_eq!(state.history_path(), Some(saved.as_path()));
        assert!(!state.history_is_temporary());
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
    fn checked_in_systemview_recording_exercises_trace_instruments() {
        let bytes = include_bytes!("../../../docs/examples/nrf52832-freertos.SVDat");
        let events = decode_systemview_stream(std::io::Cursor::new(bytes.as_slice()))
            .expect("checked-in demo is a valid SystemView recording");
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
    fn retains_target_frequencies_after_the_metadata_event_leaves_the_display_buffer() {
        let mut state = SystemTraceDisplayState::with_capacity(1);
        state.ingest_all([
            event(
                0,
                0,
                TraceEventKind::TargetInfo {
                    system_frequency_hz: 1_000_000,
                    cpu_frequency_hz: 64_000_000,
                    ram_base: 0x2000_0000,
                    id_shift: 0,
                },
            ),
            event(1, 1, TraceEventKind::TaskStarted { task_id: 7 }),
        ]);

        assert_eq!(state.events().len(), 1);
        assert_eq!(state.target_frequencies_hz(), Some((1_000_000, 64_000_000)));

        state.clear_trace_events();

        assert_eq!(state.target_frequencies_hz(), None);
    }

    #[test]
    fn retains_api_lanes_after_their_events_leave_the_display_buffer() {
        let mut state = SystemTraceDisplayState::with_capacity(1);
        state.ingest_all([
            event(
                0,
                10,
                TraceEventKind::ApiCall {
                    event_id: 41,
                    payload: vec![1, 2, 3],
                },
            ),
            event(1, 20, TraceEventKind::Idle),
        ]);

        assert_eq!(state.events().len(), 1);
        assert_eq!(state.api_event_lanes().collect::<Vec<_>>(), [(0, 41)]);

        state.clear_trace_events();

        assert!(state.api_event_lanes().next().is_none());
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
        state.ingest_probe_log(ProbeLogMessage {
            level: ProbeLogLevel::Info,
            message: "probe attached".to_owned(),
        });
        state.selected_sequence = Some(0);
        state.start_timeline_timing(TimelineTimingEdge {
            core_id: 0,
            context: ExecutionContext::Task(1),
            timestamp_ticks: 1,
        });

        state.clear();

        assert!(state.events().is_empty());
        assert!(state.terminal_lines().is_empty());
        assert!(state.probe_logs().is_empty());
        assert!(state.model().cores().is_empty());
        assert_eq!(state.selected_sequence, None);
        assert_eq!(state.timeline_timing_measurement(), None);
        assert!(!state.timeline_timing_dragging());
    }

    #[test]
    fn event_categories_can_show_one_category_and_restore_all() {
        let mut filter = TraceEventCategoryFilter::default();
        assert_eq!(filter.enabled_count(), TraceEventCategory::ALL.len());

        filter.show_only(TraceEventCategory::Markers);

        assert_eq!(filter.enabled_count(), 1);
        assert!(filter.is_enabled(TraceEventCategory::Markers));
        assert!(!filter.is_enabled(TraceEventCategory::Tasks));

        filter.show_all();

        assert_eq!(filter.enabled_count(), TraceEventCategory::ALL.len());
    }

    #[test]
    fn event_views_move_to_the_hovered_drag_position() {
        let mut state = SystemTraceDisplayState::default();

        state.begin_event_view_drag(TraceEventView::ProbeLog);
        state.drag_event_view_to(TraceEventView::Logs);

        assert_eq!(
            state.event_view_order(),
            &[
                TraceEventView::Events,
                TraceEventView::ProbeLog,
                TraceEventView::Logs,
                TraceEventView::Terminal,
                TraceEventView::CpuLoad,
                TraceEventView::ContextStatistics,
            ]
        );

        state.drag_event_view_to(TraceEventView::ContextStatistics);
        assert!(!state.take_layout_changed());
        state.finish_event_view_drag();

        assert_eq!(
            state.event_view_order(),
            &[
                TraceEventView::Events,
                TraceEventView::Logs,
                TraceEventView::Terminal,
                TraceEventView::CpuLoad,
                TraceEventView::ContextStatistics,
                TraceEventView::ProbeLog,
            ]
        );
        assert_eq!(state.dragged_event_view(), None);
        assert!(state.take_layout_changed());
        assert!(!state.take_layout_changed());
    }

    #[test]
    fn trace_instruments_swap_at_the_drag_target() {
        let mut state = SystemTraceDisplayState::default();

        state.begin_trace_instrument_drag(TraceInstrument::Timeline);
        state.drag_trace_instrument_to(TraceInstrument::Analysis);
        assert!(!state.take_layout_changed());
        state.finish_trace_instrument_drag();

        assert_eq!(
            state.trace_instrument_order(),
            &[TraceInstrument::Analysis, TraceInstrument::Timeline]
        );
        assert_eq!(state.dragged_trace_instrument(), None);
        assert!(state.take_layout_changed());
    }

    #[test]
    fn restored_trace_layout_rejects_missing_and_duplicate_entries() {
        let mut state = SystemTraceDisplayState::default();

        assert!(!state.set_event_view_order(&[TraceEventView::Logs, TraceEventView::Events,]));
        assert!(!state
            .set_trace_instrument_order(&[TraceInstrument::Analysis, TraceInstrument::Analysis,]));

        assert_eq!(state.event_view_order(), &TraceEventView::ALL);
        assert_eq!(state.trace_instrument_order(), &TraceInstrument::ALL);
    }

    #[test]
    fn time_references_are_sorted_deduplicated_and_cleared_with_trace_data() {
        let mut state = SystemTraceDisplayState::default();
        state.add_time_reference(20);
        state.add_time_reference(10);
        state.add_time_reference(20);

        assert_eq!(state.time_references(), &[10, 20]);

        state.clear_trace_events();

        assert!(state.time_references().is_empty());
    }

    #[test]
    fn timeline_time_view_resets_with_trace_data() {
        let mut state = SystemTraceDisplayState::default();

        state.set_timeline_ticks_per_division(Some(64.0));
        state.set_timeline_view_start_ticks(Some(1_024));
        assert_eq!(state.timeline_ticks_per_division(), Some(64.0));
        assert_eq!(state.timeline_view_start_ticks(), Some(1_024));
        state.set_timeline_ticks_per_division(Some(f64::NAN));
        assert_eq!(state.timeline_ticks_per_division(), None);
        state.set_timeline_ticks_per_division(Some(64.0));

        state.clear_trace_events();

        assert_eq!(state.timeline_ticks_per_division(), None);
        assert_eq!(state.timeline_view_start_ticks(), None);
    }

    #[test]
    fn trace_splitter_preserves_combined_height_and_bounds() {
        let mut state = SystemTraceDisplayState {
            timeline_view_height: 480.0,
            event_view_height: 360.0,
            ..SystemTraceDisplayState::default()
        };
        assert_eq!(state.timeline_view_height(), 480.0);
        assert_eq!(state.event_view_height(), 360.0);

        state.move_trace_instrument_splitter(50.0);
        assert_eq!(state.timeline_view_height(), 530.0);
        assert_eq!(state.event_view_height(), 310.0);
        assert!(!state.take_layout_changed());
        state.finish_trace_instrument_resize();
        assert!(state.take_layout_changed());
        state.move_trace_instrument_splitter(1_000.0);
        assert_eq!(state.timeline_view_height(), 700.0);
        assert_eq!(state.event_view_height(), MIN_TRACE_INSTRUMENT_HEIGHT);
        assert_eq!(
            state.timeline_view_height() + state.event_view_height(),
            840.0
        );

        state.move_trace_instrument_splitter(f32::NAN);
        assert_eq!(state.timeline_view_height(), 700.0);
        assert_eq!(state.event_view_height(), MIN_TRACE_INSTRUMENT_HEIGHT);
    }

    #[test]
    fn restored_trace_instrument_heights_validate_and_clamp_values() {
        let mut state = SystemTraceDisplayState::default();

        assert!(state.set_trace_instrument_heights(420.0, 280.0));
        assert_eq!(state.timeline_view_height(), 420.0);
        assert_eq!(state.event_view_height(), 280.0);
        assert!(!state.set_trace_instrument_heights(f32::NAN, 200.0));
        assert!(!state.set_trace_instrument_heights(200.0, 0.0));
        assert!(state.set_trace_instrument_heights(10_000.0, 10_000.0));
        assert_eq!(state.timeline_view_height(), MAX_TRACE_INSTRUMENT_HEIGHT);
        assert_eq!(state.event_view_height(), MAX_TRACE_INSTRUMENT_HEIGHT);
    }

    #[test]
    fn timeline_label_width_validates_and_clamps_values() {
        let mut state = SystemTraceDisplayState::default();

        assert!(state.set_timeline_label_width(240.0));
        assert_eq!(state.timeline_label_width(), 240.0);
        assert!(!state.set_timeline_label_width(f32::NAN));
        assert!(!state.set_timeline_label_width(0.0));
        assert!(state.set_timeline_label_width(10_000.0));
        assert_eq!(state.timeline_label_width(), MAX_TIMELINE_LABEL_WIDTH);
    }

    #[test]
    fn trace_splitter_follows_the_visual_panel_order() {
        let mut state = SystemTraceDisplayState {
            timeline_view_height: 480.0,
            event_view_height: 360.0,
            ..SystemTraceDisplayState::default()
        };
        state.begin_trace_instrument_drag(TraceInstrument::Timeline);
        state.drag_trace_instrument_to(TraceInstrument::Analysis);
        state.finish_trace_instrument_drag();

        state.move_trace_instrument_splitter(50.0);

        assert_eq!(state.timeline_view_height(), 430.0);
        assert_eq!(state.event_view_height(), 410.0);
    }

    #[test]
    fn trace_instruments_fit_available_height_and_preserve_split() {
        let mut state = SystemTraceDisplayState {
            timeline_view_height: 300.0,
            event_view_height: 200.0,
            ..SystemTraceDisplayState::default()
        };

        state.fit_trace_instruments_to_height(400.0);

        assert!((state.timeline_view_height() - 240.0).abs() < 0.001);
        assert!((state.event_view_height() - 160.0).abs() < 0.001);
        state.fit_trace_instruments_to_height(100.0);
        assert_eq!(
            state.timeline_view_height() + state.event_view_height(),
            100.0
        );
        assert_eq!(state.timeline_view_height(), 50.0);
        assert_eq!(state.event_view_height(), 50.0);
    }

    #[test]
    fn timeline_and_event_selections_request_reciprocal_focus() {
        let mut state = SystemTraceDisplayState {
            event_view: TraceEventView::Logs,
            event_filter: "filtered".to_owned(),
            ..SystemTraceDisplayState::default()
        };

        state.select_from_timeline(17);

        assert_eq!(state.selected_sequence, Some(17));
        assert_eq!(state.event_view, TraceEventView::Events);
        assert!(state.event_filter.is_empty());
        assert_eq!(state.take_event_table_focus(), Some(17));
        assert_eq!(state.take_event_table_focus(), None);

        state.select_from_event_table(23);

        assert_eq!(state.selected_sequence, Some(23));
        assert_eq!(state.take_timeline_focus(), Some(23));
        assert_eq!(state.take_timeline_focus(), None);
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
                Ok(DebugProbeReply::RttData {
                    data: b"boot ".to_vec(),
                    more: false,
                }),
                Ok(DebugProbeReply::RttData {
                    data: b"ready\r\n".to_vec(),
                    more: false,
                }),
            ]),
            commands: Arc::clone(&commands),
            shutdown,
        };
        let mut decoder = RttTerminalDecoder::new();
        let (line_sender, line_receiver) = mpsc::sync_channel(16);
        let mut line_publisher = TerminalLinePublisher::new(line_sender);

        assert_eq!(
            read_rtt_terminal(&mut transport, 0, &mut decoder, &mut line_publisher)
                .expect("first terminal read succeeds"),
            5
        );
        assert!(line_receiver.try_recv().is_err());
        assert_eq!(
            read_rtt_terminal(&mut transport, 0, &mut decoder, &mut line_publisher)
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
    fn connection_waits_for_systemview_channels_after_partial_rtt_initialization() {
        let terminal_channel = RttChannelDescriptor {
            number: 0,
            direction: RttChannelDirection::Up,
            name: Some("Terminal".to_owned()),
            buffer_size: 512,
        };
        let up_channel = RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Up,
            name: Some("SysView".to_owned()),
            buffer_size: 1024,
        };
        let down_channel = RttChannelDescriptor {
            number: 1,
            direction: RttChannelDirection::Down,
            name: Some("SysView".to_owned()),
            buffer_size: 8,
        };
        let commands = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let transport = FakeTransport {
            replies: VecDeque::from([
                Ok(DebugProbeReply::ProbeOpened(probe_descriptor())),
                Ok(DebugProbeReply::TargetAttached),
                Ok(DebugProbeReply::RttAttached(vec![terminal_channel.clone()])),
                Ok(DebugProbeReply::RttAttached(vec![
                    terminal_channel,
                    up_channel,
                    down_channel,
                ])),
                Ok(DebugProbeReply::SessionClosed),
            ]),
            commands: Arc::clone(&commands),
            shutdown: Arc::clone(&shutdown),
        };
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let (trace_event_sender, _trace_event_receiver) = mpsc::sync_channel(16);
        let (terminal_line_sender, _terminal_line_receiver) = mpsc::sync_channel(16);
        command_sender
            .send(SystemTraceWorkerCommand::Disconnect)
            .expect("disconnect is queued");
        let mut configuration = connection_configuration();
        configuration.rtt.timeout = Duration::from_millis(100);

        run_connection_worker(
            transport,
            configuration,
            command_receiver,
            event_sender,
            trace_event_sender,
            terminal_line_sender,
        );

        assert_eq!(
            commands
                .lock()
                .expect("commands are available")
                .iter()
                .filter(|command| matches!(command, DebugProbeCommand::AttachRtt { .. }))
                .count(),
            2
        );
        assert!(event_receiver.try_iter().any(|event| matches!(
            event,
            SystemTraceWorkerEvent::ProbeLog(ProbeLogMessage {
                level: ProbeLogLevel::Debug,
                message,
            }) if message == "RTT control block found; waiting for configured channels"
        )));
        assert!(*shutdown.lock().expect("shutdown flag is available"));
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
        let (trace_event_sender, _trace_event_receiver) = mpsc::sync_channel(16);
        let (terminal_line_sender, _terminal_line_receiver) = mpsc::sync_channel(16);
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

        let events = event_receiver.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            SystemTraceWorkerEvent::ProbeLog(ProbeLogMessage {
                level: ProbeLogLevel::Success,
                message,
            }) if message == "target reset completed"
        )));
        assert_eq!(
            lifecycle_events(events),
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
        let (trace_event_sender, _trace_event_receiver) = mpsc::sync_channel(16);
        let (terminal_line_sender, _terminal_line_receiver) = mpsc::sync_channel(16);

        run_connection_worker(
            transport,
            connection_configuration(),
            command_receiver,
            event_sender,
            trace_event_sender,
            terminal_line_sender,
        );

        let events = event_receiver.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            SystemTraceWorkerEvent::ProbeLog(ProbeLogMessage {
                level: ProbeLogLevel::Info,
                message,
            }) if message.starts_with("opening debug probe")
        )));
        assert_eq!(
            lifecycle_events(events),
            [SystemTraceWorkerEvent::Error(
                "target was not found".to_owned()
            )]
        );
        assert!(*shutdown.lock().expect("shutdown flag is available"));
    }

    #[test]
    fn jlink_requires_an_explicit_target_before_attach() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let transport = FakeTransport {
            replies: VecDeque::from([Ok(DebugProbeReply::ProbeOpened(probe_descriptor()))]),
            commands: Arc::clone(&commands),
            shutdown: Arc::clone(&shutdown),
        };
        let (_command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let (trace_event_sender, _trace_event_receiver) = mpsc::sync_channel(16);
        let (terminal_line_sender, _terminal_line_receiver) = mpsc::sync_channel(16);
        let mut configuration = connection_configuration();
        configuration.attach.target = TargetSelector::Auto;

        run_connection_worker(
            transport,
            configuration,
            command_receiver,
            event_sender,
            trace_event_sender,
            terminal_line_sender,
        );

        assert_eq!(
            lifecycle_events(event_receiver.try_iter()),
            [SystemTraceWorkerEvent::Error(
                "select the target MCU explicitly when using J-Link; automatic target detection is not reliable for this probe".to_owned()
            )]
        );
        assert!(matches!(
            commands.lock().expect("commands are available").as_slice(),
            [DebugProbeCommand::OpenProbe { .. }]
        ));
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
                Ok(DebugProbeReply::RttData {
                    data: vec![10, 3, 4, 7, 2],
                    more: false,
                }),
                Ok(DebugProbeReply::RttBytesWritten(1)),
                Ok(DebugProbeReply::RttData {
                    data: Vec::new(),
                    more: false,
                }),
                Ok(DebugProbeReply::SessionClosed),
            ]),
            commands: Arc::clone(&commands),
            shutdown: Arc::clone(&shutdown),
        };
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let (trace_event_sender, trace_event_receiver) = mpsc::sync_channel(16);
        let (terminal_line_sender, _terminal_line_receiver) = mpsc::sync_channel(16);
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

        let events = event_receiver.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            SystemTraceWorkerEvent::ProbeLog(ProbeLogMessage {
                level: ProbeLogLevel::Success,
                message,
            }) if message.starts_with("SystemView recording saved to")
        )));
        assert_eq!(
            lifecycle_events(events),
            [
                SystemTraceWorkerEvent::Attached {
                    channels: vec![up_channel, down_channel],
                    systemview_enabled: true,
                },
                SystemTraceWorkerEvent::Tracing {
                    recording_path: Some(recording_path.clone()),
                    history_path: recording_path.clone(),
                    history_is_temporary: false,
                },
                SystemTraceWorkerEvent::RecordingSaved(recording_path.clone()),
                SystemTraceWorkerEvent::Stopped,
                SystemTraceWorkerEvent::Disconnected,
            ]
        );
        let decoded = trace_event_receiver
            .try_iter()
            .filter_map(|update| match update {
                LiveTraceUpdate::Event(event) => Some(event),
                LiveTraceUpdate::DisplayGap { .. } => None,
            })
            .collect::<Vec<_>>();
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
