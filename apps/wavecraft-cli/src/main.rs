use clap::{Args, Parser, Subcommand, ValueEnum};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use wavecraft_automation::{
    AutomationEvent, AutomationEventKind, AutomationManager, CaptureConfiguration, DeviceBackend,
    DeviceSelector, InstrumentKind as AutomationInstrumentKind,
};
use wavecraft_core::{CaptureBatch, CaptureCoordinator, CaptureEvent, CaptureSource, DeviceId};
use wavecraft_recording::{
    export_saleae_recording, import_saleae_recording, CompletionStatus, RecordingReader,
    RecordingWriter, FORMAT_VERSION,
};
use wavecraft_sim::{SimulatedLogicAnalyzer, SimulatedOscilloscope};
use wavecraft_systemview::SystemViewDecoder;
use wavecraft_trace::TraceModel;
use wavecraft_usb::{
    discover_supported_devices, load_device_fpga, prepare_device, probe_device, DeviceModel,
    DsCopeCaptureConfig, DsCopeCaptureSource, DsLogicCaptureConfig, DsLogicCaptureSource, FpgaLoad,
    LinkStatus,
};

type FileRecorder = RecordingWriter<BufWriter<File>>;

#[derive(Debug, Parser)]
#[command(
    name = "wavecraft-cli",
    version,
    about = "Discover, capture from, and inspect WaveCraft instruments",
    long_about = "Discover, configure, and capture from WaveCraft logic analyzers and oscilloscopes.\n\
                  Machine-readable device output uses versioned JSON; capture events use NDJSON."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Discover and inspect explicitly selected instruments.
    Devices {
        #[command(subcommand)]
        command: DevicesCommand,
    },
    /// Run a capture through the stable automation interface.
    Capture {
        #[command(subcommand)]
        command: CaptureCommand,
    },
    /// Inspect or convert capture recordings.
    Recording {
        #[command(subcommand)]
        command: RecordingCommand,
    },
    /// Run a deterministic simulated logic-and-scope capture.
    Simulate(SimulateArguments),
    /// List connected USB instruments (legacy human-readable command).
    List,
    /// Probe every connected USB instrument (legacy command).
    Probe,
    /// Load model-specific FPGA bitstreams into connected USB instruments.
    LoadFpga(ResourceDirectoryArguments),
    /// Prepare connected USB instruments for capture.
    Prepare(ResourceDirectoryArguments),
    /// Capture from the first supported DSLogic instrument.
    CaptureLogic(CaptureLogicArguments),
    /// Capture from the first supported DSCope U3P100.
    CaptureScope(HardwareCaptureArguments),
    /// Capture concurrently from the first supported DSLogic and DSCope.
    CaptureUsb(HardwareCaptureArguments),
    /// Inspect a WaveCraft recording (legacy alias for `recording inspect`).
    Inspect(RecordingPathArguments),
    /// Import a Saleae Logic 2 recording (legacy alias for `recording import`).
    Import(RecordingConversionArguments),
    /// Export a WaveCraft recording to Saleae Logic 2 (legacy alias).
    Export(RecordingConversionArguments),
    /// Existing SystemView stream utilities.
    Trace {
        #[command(subcommand)]
        command: TraceCommand,
    },
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum OutputFormat {
    #[default]
    Human,
    Json,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum EventFormat {
    #[default]
    Human,
    Ndjson,
}

#[derive(Debug, Subcommand)]
enum DevicesCommand {
    /// List supported connected devices.
    List {
        /// Select human-readable or schema-versioned JSON output.
        #[arg(long, value_enum, default_value_t)]
        format: OutputFormat,
    },
    /// Probe one exact device connection ID.
    Probe {
        /// Stable connection ID returned by `devices list`.
        #[arg(long)]
        id: String,
        /// Select human-readable or schema-versioned JSON output.
        #[arg(long, value_enum, default_value_t)]
        format: OutputFormat,
    },
}

#[derive(Debug, Subcommand)]
enum CaptureCommand {
    /// Perform one finite capture and write a lossless `.dsv` recording.
    Run(AutomationCaptureArguments),
}

#[derive(Debug, Args)]
struct AutomationCaptureArguments {
    /// Exact device connection ID returned by `devices list`.
    #[arg(long)]
    device: String,
    /// Requested sample rate in hertz.
    #[arg(long)]
    sample_rate: u64,
    /// Requested number of samples per enabled channel.
    #[arg(long)]
    samples: u64,
    /// Comma-delimited zero-based channel indexes.
    #[arg(long, value_delimiter = ',', required = true)]
    channels: Vec<u8>,
    /// New `.dsv` recording path; existing files are never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// Directory containing DSLogic/DSCope FPGA resources.
    #[arg(long)]
    resource_directory: Option<PathBuf>,
    /// Capture timeout in milliseconds.
    #[arg(long)]
    timeout_ms: Option<u64>,
    /// Select human-readable events or schema-versioned NDJSON.
    #[arg(long, value_enum, default_value_t)]
    events: EventFormat,
}

#[derive(Debug, Subcommand)]
enum RecordingCommand {
    /// Validate and summarize a WaveCraft `.dsv` recording.
    Inspect(RecordingPathArguments),
    /// Convert a Saleae Logic 2 `.sal` capture to WaveCraft `.dsv`.
    Import(RecordingConversionArguments),
    /// Convert a WaveCraft `.dsv` capture to Saleae Logic 2 `.sal`.
    Export(RecordingConversionArguments),
}

#[derive(Debug, Subcommand)]
enum TraceCommand {
    Decode(RecordingPathArguments),
}

#[derive(Debug, Args)]
struct SimulateArguments {
    /// Capture duration in seconds.
    #[arg(default_value_t = 2)]
    seconds: u64,
    /// Optional new `.dsv` recording path.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ResourceDirectoryArguments {
    resource_directory: PathBuf,
}

#[derive(Debug, Args)]
struct CaptureLogicArguments {
    resource_directory: PathBuf,
    /// Capture duration in seconds.
    #[arg(default_value_t = 1)]
    seconds: u64,
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct HardwareCaptureArguments {
    resource_directory: PathBuf,
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct RecordingPathArguments {
    path: PathBuf,
}

#[derive(Debug, Args)]
struct RecordingConversionArguments {
    input: PathBuf,
    output: PathBuf,
}

#[derive(Default)]
struct DeviceStats {
    frames: u64,
    samples: u64,
    first_timestamp: Option<Duration>,
    last_timestamp: Option<Duration>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args().collect::<Vec<_>>();
    match arguments.get(1).map(String::as_str) {
        None => return run_simulation(2, None),
        Some(value) if value.parse::<u64>().is_ok() && arguments.len() == 2 => {
            return run_simulation(value.parse()?, None);
        }
        _ => {}
    }

    run_command(Cli::parse_from(arguments).command)
}

fn run_command(command: Command) -> Result<(), Box<dyn Error>> {
    match command {
        Command::Devices { command } => match command {
            DevicesCommand::List { format } => {
                automation_list_devices(matches!(format, OutputFormat::Json))
            }
            DevicesCommand::Probe { id, format } => {
                automation_probe_device(&id, matches!(format, OutputFormat::Json))
            }
        },
        Command::Capture {
            command: CaptureCommand::Run(arguments),
        } => automation_capture_run(arguments),
        Command::Recording { command } => match command {
            RecordingCommand::Inspect(arguments) => inspect_recording(&arguments.path),
            RecordingCommand::Import(arguments) => {
                import_saleae(&arguments.input, &arguments.output)
            }
            RecordingCommand::Export(arguments) => {
                export_saleae(&arguments.input, &arguments.output)
            }
        },
        Command::Simulate(arguments) => {
            run_simulation(arguments.seconds, arguments.output.as_deref())
        }
        Command::List => list_devices(),
        Command::Probe => probe_devices(),
        Command::LoadFpga(arguments) => load_fpgas(&arguments.resource_directory),
        Command::Prepare(arguments) => prepare_devices(&arguments.resource_directory),
        Command::CaptureLogic(arguments) => capture_logic(
            &arguments.resource_directory,
            arguments.seconds,
            arguments.output.as_deref(),
        ),
        Command::CaptureScope(arguments) => {
            capture_scope(&arguments.resource_directory, arguments.output.as_deref())
        }
        Command::CaptureUsb(arguments) => {
            capture_usb(&arguments.resource_directory, arguments.output.as_deref())
        }
        Command::Inspect(arguments) => inspect_recording(&arguments.path),
        Command::Import(arguments) => import_saleae(&arguments.input, &arguments.output),
        Command::Export(arguments) => export_saleae(&arguments.input, &arguments.output),
        Command::Trace {
            command: TraceCommand::Decode(arguments),
        } => decode_systemview_trace(&arguments.path),
    }
}

fn decode_systemview_trace(path: &Path) -> Result<(), Box<dyn Error>> {
    let mut input = BufReader::new(File::open(path)?);
    let mut decoder = SystemViewDecoder::default();
    let mut model = TraceModel::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut event_count = 0_u64;
    let mut first_timestamp = None;
    let mut last_timestamp = None;

    loop {
        let bytes_read = input.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        for event in decoder.push(&buffer[..bytes_read])? {
            first_timestamp.get_or_insert(event.timestamp_ticks);
            last_timestamp = Some(event.timestamp_ticks);
            event_count = event_count.saturating_add(1);
            model.apply(&event);
        }
    }
    decoder.finish()?;

    println!("SystemView trace: {}", path.display());
    println!("events: {event_count}");
    match (first_timestamp, last_timestamp) {
        (Some(first), Some(last)) => {
            println!("timestamp ticks: {first}..{last}");
        }
        _ => println!("timestamp ticks: none"),
    }
    for (core_id, core) in model.cores() {
        println!(
            "core {core_id}: {} task(s), {} execution interval(s), {} dropped event(s), continuity {}",
            core.tasks().len(),
            core.intervals().len(),
            core.dropped_events(),
            if core.continuity_known() {
                "known"
            } else {
                "unknown"
            }
        );
    }
    Ok(())
}

fn automation_capture_run(arguments: AutomationCaptureArguments) -> Result<(), Box<dyn Error>> {
    let ndjson_events = matches!(arguments.events, EventFormat::Ndjson);
    let mut manager = automation_manager_for_device(&arguments.device);
    if let Some(resource_directory) = &arguments.resource_directory {
        manager = manager.with_resource_directory(resource_directory);
    }
    let device = match manager.open_device(DeviceSelector::connection_id(&arguments.device)) {
        Ok(device) => device,
        Err(error) => return report_capture_error(error, ndjson_events),
    };
    let mut configuration = match device.descriptor().kind {
        AutomationInstrumentKind::LogicAnalyzer => CaptureConfiguration::finite_logic(
            arguments.sample_rate,
            arguments.samples,
            arguments.channels,
            &arguments.output,
        ),
        AutomationInstrumentKind::Oscilloscope => CaptureConfiguration::finite_scope(
            arguments.sample_rate,
            arguments.samples,
            arguments.channels,
            &arguments.output,
        ),
        AutomationInstrumentKind::RadioReceiver => {
            return Err("radio receiver capture is not implemented by this command".into())
        }
    };
    if let Some(timeout_ms) = arguments.timeout_ms {
        configuration = configuration.with_timeout(Duration::from_millis(timeout_ms));
    }
    let capture = match device.start_capture(configuration) {
        Ok(capture) => capture,
        Err(error) => return report_capture_error(error, ndjson_events),
    };

    loop {
        if let Some(event) = capture.next_event(Duration::from_millis(100))? {
            print_capture_event(&event, ndjson_events)?;
        }
        if capture.status().is_terminal() {
            while let Some(event) = capture.next_event(Duration::ZERO)? {
                print_capture_event(&event, ndjson_events)?;
            }
            break;
        }
    }

    match capture.wait() {
        Ok(completed) => {
            if ndjson_events {
                println!("{}", serde_json::to_string(&completed)?);
                std::io::stdout().flush()?;
            } else {
                println!(
                    "capture {} completed: {} samples in {} frame(s), recording {}",
                    completed.summary.capture_id,
                    completed.summary.captured_samples,
                    completed.summary.frame_count,
                    completed.recording_path().display()
                );
            }
            Ok(())
        }
        Err(error) => report_capture_error(error, ndjson_events),
    }
}

fn print_capture_event(event: &AutomationEvent, ndjson: bool) -> Result<(), Box<dyn Error>> {
    if ndjson {
        println!("{}", serde_json::to_string(event)?);
        std::io::stdout().flush()?;
        return Ok(());
    }
    match event.event {
        AutomationEventKind::Status { status } => {
            println!("capture {}: {status:?}", event.capture_id);
        }
        AutomationEventKind::Progress {
            captured_samples,
            requested_samples,
        } => {
            println!(
                "capture {}: {captured_samples}/{requested_samples} samples",
                event.capture_id
            );
        }
    }
    Ok(())
}

fn report_capture_error(
    error: wavecraft_automation::AutomationError,
    ndjson: bool,
) -> Result<(), Box<dyn Error>> {
    if ndjson {
        println!("{}", serde_json::to_string(&error)?);
        std::io::stdout().flush()?;
    }
    Err(Box::new(error))
}

fn automation_list_devices(json: bool) -> Result<(), Box<dyn Error>> {
    let listing = match AutomationManager::new().device_list() {
        Ok(listing) => listing,
        Err(error) => return report_automation_error(error, json),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&listing)?);
        return Ok(());
    }

    for device in listing.devices {
        let location = device
            .physical_location
            .as_deref()
            .unwrap_or("deterministic simulator");
        println!(
            "{}: {} ({}, {}) at {}; {:?}, {:?}",
            device.connection_id,
            device.display_name,
            device.kind,
            device.backend,
            location,
            device.ownership,
            device.preparation
        );
        if let Some(warning) = device.link.warning {
            eprintln!("warning: {warning}");
        }
    }
    Ok(())
}

fn automation_probe_device(device_id: &str, json: bool) -> Result<(), Box<dyn Error>> {
    let manager = automation_manager_for_device(device_id);
    let handle = match manager.open_device(DeviceSelector::connection_id(device_id)) {
        Ok(handle) => handle,
        Err(error) => return report_automation_error(error, json),
    };
    let probe = match handle.probe() {
        Ok(probe) => probe,
        Err(error) => return report_automation_error(error, json),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&probe)?);
    } else if probe.device.backend == DeviceBackend::Simulator {
        println!(
            "{}: {} is ready (simulator, {} channels)",
            probe.device.connection_id,
            probe.device.display_name,
            probe.device.capabilities.channel_count
        );
    } else {
        println!(
            "{}: {} firmware {}, hardware status 0x{:02x}, {:?}",
            probe.device.connection_id,
            probe.device.display_name,
            probe.firmware_version.as_deref().unwrap_or("unknown"),
            probe.hardware_status_raw.unwrap_or_default(),
            probe.preparation
        );
    }
    Ok(())
}

fn automation_manager_for_device(device_id: &str) -> AutomationManager {
    if device_id.starts_with("sim:") {
        AutomationManager::simulated()
    } else {
        AutomationManager::new()
    }
}

fn report_automation_error(
    error: wavecraft_automation::AutomationError,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(&error)?);
    }
    Err(Box::new(error))
}

fn import_saleae(input: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    let input_file = File::open(input)?;
    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    import_saleae_recording(BufReader::new(input_file), BufWriter::new(output_file))?;
    println!(
        "imported Saleae capture {} to {}",
        input.display(),
        output.display()
    );
    Ok(())
}

fn export_saleae(input: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    let input_file = File::open(input)?;
    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    export_saleae_recording(BufReader::new(input_file), BufWriter::new(output_file))?;
    println!(
        "exported WaveCraft capture {} to {}",
        input.display(),
        output.display()
    );
    Ok(())
}

fn capture_usb(resource_directory: &Path, output: Option<&Path>) -> Result<(), Box<dyn Error>> {
    const SAMPLE_RATE_HZ: u64 = 1_000_000;
    const SAMPLE_COUNT: u64 = 1_000_000;
    const CAPTURE_TIMEOUT: Duration = Duration::from_secs(11);

    let logic_config = DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)?
        .with_operation_mode(wavecraft_usb::DsLogicOperationMode::Stream);
    let scope_config = DsCopeCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)?;
    let report = discover_supported_devices()?;
    let logic_device = report
        .devices
        .iter()
        .find(|device| {
            device.profile.model.is_logic_analyzer() && device.profile.model.supports_capture()
        })
        .cloned()
        .ok_or("no supported DSLogic analyzer detected")?;
    let scope_device = report
        .devices
        .iter()
        .find(|device| device.profile.model == DeviceModel::DsCopeU3P100)
        .cloned()
        .ok_or("no DSCope U3P100 detected")?;

    for device in [&logic_device, &scope_device] {
        let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
        let prepared = prepare_device(device, &bitstream_path)?;
        println!(
            "{} ready: {} firmware {}, HDL {}",
            device.location.stable_id(),
            device.profile.display_name,
            prepared.firmware,
            prepared.hdl
        );
    }

    let logic_id = DeviceId::new(logic_device.location.stable_id());
    let scope_id = DeviceId::new(scope_device.location.stable_id());
    let sources: Vec<Box<dyn CaptureSource>> = vec![
        Box::new(DsLogicCaptureSource::new(logic_device, logic_config)?),
        Box::new(DsCopeCaptureSource::new(scope_device, scope_config)?),
    ];
    let session = CaptureCoordinator::start(sources)?;
    let mut metadata = capture_metadata("capture-usb", SAMPLE_RATE_HZ, SAMPLE_COUNT);
    metadata.insert("scope.volts_per_div".to_owned(), "1".to_owned());
    let mut recorder = start_recording(output, session.descriptors(), metadata)?;
    println!(
        "capturing concurrently from {} physical instruments at {SAMPLE_RATE_HZ} Hz",
        session.descriptors().len()
    );

    let deadline = Instant::now() + CAPTURE_TIMEOUT;
    let mut stats = HashMap::<DeviceId, DeviceStats>::new();
    let mut stopped = HashSet::<DeviceId>::new();
    let mut scope_min = [f32::INFINITY; 2];
    let mut scope_max = [f32::NEG_INFINITY; 2];
    while stopped.len() < 2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            session.request_stop();
            let stop_result = session.stop();
            finish_recording(&mut recorder, CompletionStatus::Cancelled)?;
            stop_result?;
            return Err("concurrent USB capture did not finish within 11 seconds".into());
        }
        match session.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(event) => {
                write_recording_event(&mut recorder, &event)?;
                match event {
                    CaptureEvent::Frame(frame) => {
                        let entry = stats.entry(frame.device_id).or_default();
                        entry.frames += 1;
                        entry.samples += frame.payload.sample_count() as u64;
                        entry.first_timestamp.get_or_insert(frame.captured_at);
                        entry.last_timestamp = Some(frame.captured_at);
                        if let CaptureBatch::Scope(batch) = frame.payload {
                            for (index, channel) in batch.channels.iter().enumerate().take(2) {
                                for value in channel {
                                    scope_min[index] = scope_min[index].min(*value);
                                    scope_max[index] = scope_max[index].max(*value);
                                }
                            }
                        }
                    }
                    CaptureEvent::SourceFailed { device_id, error } => {
                        let stop_result = session.stop();
                        finish_recording(&mut recorder, CompletionStatus::Failed)?;
                        stop_result?;
                        return Err(format!("capture source {device_id} failed: {error}").into());
                    }
                    CaptureEvent::SourceStopped { device_id } => {
                        stopped.insert(device_id);
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let stop_result = session.stop();
                finish_recording(&mut recorder, CompletionStatus::Failed)?;
                stop_result?;
                return Err("concurrent USB capture event stream ended unexpectedly".into());
            }
        }
    }
    let stop_result = session.stop();
    let completion = if stop_result.is_ok() {
        CompletionStatus::Complete
    } else {
        CompletionStatus::Failed
    };
    finish_recording(&mut recorder, completion)?;
    stop_result?;

    let logic_stats = stats
        .get(&logic_id)
        .ok_or("DSLogic emitted no capture frames")?;
    let scope_stats = stats
        .get(&scope_id)
        .ok_or("DSCope emitted no capture frames")?;
    let start_skew = duration_difference(
        logic_stats.first_timestamp.unwrap_or_default(),
        scope_stats.first_timestamp.unwrap_or_default(),
    );
    println!("capture complete; shared-timeline start skew {start_skew:?}");
    println!(
        "  {logic_id}: {} frames, {} logic samples, timeline {:?}..{:?}",
        logic_stats.frames,
        logic_stats.samples,
        logic_stats.first_timestamp.unwrap_or_default(),
        logic_stats.last_timestamp.unwrap_or_default()
    );
    println!(
        "  {scope_id}: {} frames, {} samples/channel, timeline {:?}..{:?}, CH0 {:.3}..{:.3} V, CH1 {:.3}..{:.3} V",
        scope_stats.frames,
        scope_stats.samples,
        scope_stats.first_timestamp.unwrap_or_default(),
        scope_stats.last_timestamp.unwrap_or_default(),
        scope_min[0],
        scope_max[0],
        scope_min[1],
        scope_max[1]
    );
    Ok(())
}

fn duration_difference(left: Duration, right: Duration) -> Duration {
    left.max(right) - left.min(right)
}

fn capture_metadata(
    command: &str,
    sample_rate_hz: u64,
    sample_count: u64,
) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert("capture.command".to_owned(), command.to_owned());
    metadata.insert(
        "capture.sample_rate_hz".to_owned(),
        sample_rate_hz.to_string(),
    );
    metadata.insert(
        "capture.requested_sample_count".to_owned(),
        sample_count.to_string(),
    );
    metadata
}

fn start_recording(
    output: Option<&Path>,
    devices: &[wavecraft_core::DeviceDescriptor],
    metadata: BTreeMap<String, String>,
) -> Result<Option<FileRecorder>, Box<dyn Error>> {
    let Some(path) = output else {
        return Ok(None);
    };
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to create capture file {} without overwriting an existing file: {error}",
                path.display()
            )
        })?;
    let recorder = RecordingWriter::new(BufWriter::new(file), devices, metadata)?;
    println!("recording full capture to {}", path.display());
    Ok(Some(recorder))
}

fn write_recording_event(
    recorder: &mut Option<FileRecorder>,
    event: &CaptureEvent,
) -> Result<(), Box<dyn Error>> {
    if let Some(recorder) = recorder {
        recorder.write_event(event)?;
    }
    Ok(())
}

fn finish_recording(
    recorder: &mut Option<FileRecorder>,
    status: CompletionStatus,
) -> Result<(), Box<dyn Error>> {
    if let Some(recorder) = recorder {
        recorder.finish(status)?;
    }
    Ok(())
}

fn inspect_recording(path: &Path) -> Result<(), Box<dyn Error>> {
    let file = File::open(path)
        .map_err(|error| format!("failed to open recording {}: {error}", path.display()))?;
    let mut reader = RecordingReader::new(BufReader::new(file))?;
    println!(
        "WaveCraft recording format {}, {} device(s)",
        FORMAT_VERSION,
        reader.header().devices.len()
    );
    for device in &reader.header().devices {
        println!("  {}: {} ({:?})", device.id, device.name, device.kind);
    }
    for (key, value) in &reader.header().metadata {
        println!("  {key}: {value}");
    }

    let mut stats = HashMap::<DeviceId, DeviceStats>::new();
    let mut failures = Vec::new();
    let mut stopped = HashSet::new();
    while let Some(event) = reader.read_event()? {
        match event {
            CaptureEvent::Frame(frame) => {
                let entry = stats.entry(frame.device_id).or_default();
                entry.frames += 1;
                entry.samples += frame.payload.sample_count() as u64;
                entry.first_timestamp.get_or_insert(frame.captured_at);
                entry.last_timestamp = Some(frame.captured_at);
            }
            CaptureEvent::SourceFailed { device_id, error } => {
                failures.push(format!("{device_id}: {error}"));
            }
            CaptureEvent::SourceStopped { device_id } => {
                stopped.insert(device_id);
            }
        }
    }

    let completion = reader
        .completion_status()
        .ok_or("recording has no completion status")?;
    println!(
        "completion: {completion:?}; {} source(s) stopped",
        stopped.len()
    );
    for device in &reader.header().devices {
        if let Some(device_stats) = stats.get(&device.id) {
            println!(
                "  {}: {} frames, {} samples, timeline {:?}..{:?}",
                device.id,
                device_stats.frames,
                device_stats.samples,
                device_stats.first_timestamp.unwrap_or_default(),
                device_stats.last_timestamp.unwrap_or_default()
            );
        } else {
            println!("  {}: no sample frames", device.id);
        }
    }
    for failure in failures {
        println!("  source failure: {failure}");
    }
    Ok(())
}

fn capture_scope(resource_directory: &Path, output: Option<&Path>) -> Result<(), Box<dyn Error>> {
    const SAMPLE_RATE_HZ: u64 = 1_000_000;
    const SAMPLE_COUNT: u64 = 1_000_000;
    let config = DsCopeCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)?;
    let report = discover_supported_devices()?;
    let device = report
        .devices
        .into_iter()
        .find(|device| device.profile.model == DeviceModel::DsCopeU3P100)
        .ok_or("no DSCope U3P100 detected")?;

    let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
    let prepared = prepare_device(&device, &bitstream_path)?;
    println!(
        "{} ready: firmware {}, HDL {}; requesting up to {} samples/channel from two channels at {} Hz, factory-calibrated 1 V/div",
        device.location.stable_id(),
        prepared.firmware,
        prepared.hdl,
        config.sample_count(),
        config.sample_rate_hz()
    );

    let source = DsCopeCaptureSource::new(device, config)?;
    let session = CaptureCoordinator::start(vec![Box::new(source)])?;
    let mut metadata = capture_metadata("capture-scope", SAMPLE_RATE_HZ, SAMPLE_COUNT);
    metadata.insert("scope.volts_per_div".to_owned(), "1".to_owned());
    let mut recorder = start_recording(output, session.descriptors(), metadata)?;
    let deadline = Instant::now() + Duration::from_secs(11);
    let mut stats = DeviceStats::default();
    let mut channel_min = [f32::INFINITY; 2];
    let mut channel_max = [f32::NEG_INFINITY; 2];
    let mut stopped = false;
    while !stopped {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            session.request_stop();
            let stop_result = session.stop();
            finish_recording(&mut recorder, CompletionStatus::Cancelled)?;
            stop_result?;
            return Err("scope capture did not finish within 11 seconds".into());
        }
        match session.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(event) => {
                write_recording_event(&mut recorder, &event)?;
                match event {
                    CaptureEvent::Frame(frame) => {
                        stats.frames += 1;
                        stats.samples += frame.payload.sample_count() as u64;
                        stats.first_timestamp.get_or_insert(frame.captured_at);
                        stats.last_timestamp = Some(frame.captured_at);
                        if let CaptureBatch::Scope(batch) = frame.payload {
                            for (index, channel) in batch.channels.iter().enumerate().take(2) {
                                for value in channel {
                                    channel_min[index] = channel_min[index].min(*value);
                                    channel_max[index] = channel_max[index].max(*value);
                                }
                            }
                        }
                    }
                    CaptureEvent::SourceFailed { device_id, error } => {
                        let stop_result = session.stop();
                        finish_recording(&mut recorder, CompletionStatus::Failed)?;
                        stop_result?;
                        return Err(format!("capture source {device_id} failed: {error}").into());
                    }
                    CaptureEvent::SourceStopped { .. } => stopped = true,
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let stop_result = session.stop();
                finish_recording(&mut recorder, CompletionStatus::Failed)?;
                stop_result?;
                return Err("scope capture event stream ended unexpectedly".into());
            }
        }
    }
    let stop_result = session.stop();
    let completion = if stop_result.is_ok() {
        CompletionStatus::Complete
    } else {
        CompletionStatus::Failed
    };
    finish_recording(&mut recorder, completion)?;
    stop_result?;

    println!(
        "capture complete: {} frames, {} samples/channel, CH0 {:.3}..{:.3} V, CH1 {:.3}..{:.3} V",
        stats.frames, stats.samples, channel_min[0], channel_max[0], channel_min[1], channel_max[1]
    );
    Ok(())
}

fn capture_logic(
    resource_directory: &Path,
    seconds: u64,
    output: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    if seconds == 0 {
        return Err("capture duration must be at least one second".into());
    }
    const SAMPLE_RATE_HZ: u64 = 1_000_000;
    let sample_count = SAMPLE_RATE_HZ
        .checked_mul(seconds)
        .ok_or("capture duration is too large")?;
    let config = DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, sample_count)?
        .with_operation_mode(wavecraft_usb::DsLogicOperationMode::Stream);
    let report = discover_supported_devices()?;
    let device = report
        .devices
        .into_iter()
        .find(|device| {
            device.profile.model.is_logic_analyzer() && device.profile.model.supports_capture()
        })
        .ok_or("no supported DSLogic analyzer detected")?;

    let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
    let prepared = prepare_device(&device, &bitstream_path)?;
    println!(
        "{} ready: firmware {}, HDL {}; capturing {} samples at {} Hz",
        device.location.stable_id(),
        prepared.firmware,
        prepared.hdl,
        config.sample_count(),
        config.sample_rate_hz()
    );

    let source = DsLogicCaptureSource::new(device, config)?;
    let session = CaptureCoordinator::start(vec![Box::new(source)])?;
    let metadata = capture_metadata("capture-logic", SAMPLE_RATE_HZ, sample_count);
    let mut recorder = start_recording(output, session.descriptors(), metadata)?;
    let deadline = Instant::now() + Duration::from_secs(seconds.saturating_add(10));
    let mut stats = DeviceStats::default();
    let mut stopped = false;
    while !stopped {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            session.request_stop();
            let stop_result = session.stop();
            finish_recording(&mut recorder, CompletionStatus::Cancelled)?;
            stop_result?;
            return Err(format!(
                "logic capture did not finish within {} seconds",
                seconds + 10
            )
            .into());
        }
        match session.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(event) => {
                write_recording_event(&mut recorder, &event)?;
                match event {
                    CaptureEvent::Frame(frame) => {
                        stats.frames += 1;
                        stats.samples += frame.payload.sample_count() as u64;
                        stats.first_timestamp.get_or_insert(frame.captured_at);
                        stats.last_timestamp = Some(frame.captured_at);
                    }
                    CaptureEvent::SourceFailed { device_id, error } => {
                        let stop_result = session.stop();
                        finish_recording(&mut recorder, CompletionStatus::Failed)?;
                        stop_result?;
                        return Err(format!("capture source {device_id} failed: {error}").into());
                    }
                    CaptureEvent::SourceStopped { .. } => stopped = true,
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let stop_result = session.stop();
                finish_recording(&mut recorder, CompletionStatus::Failed)?;
                stop_result?;
                return Err("logic capture event stream ended unexpectedly".into());
            }
        }
    }
    let stop_result = session.stop();
    let completion = if stop_result.is_ok() {
        CompletionStatus::Complete
    } else {
        CompletionStatus::Failed
    };
    finish_recording(&mut recorder, completion)?;
    stop_result?;

    println!(
        "capture complete: {} frames, {} samples, timeline {:?}..{:?}",
        stats.frames,
        stats.samples,
        stats.first_timestamp.unwrap_or_default(),
        stats.last_timestamp.unwrap_or_default()
    );
    Ok(())
}

fn load_fpgas(resource_directory: &Path) -> Result<(), Box<dyn Error>> {
    let report = discover_supported_devices()?;
    if report.devices.is_empty() {
        println!("no supported instruments detected");
        return Ok(());
    }

    let mut failures = 0_usize;
    for device in report.devices {
        let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
        print!(
            "{}: {} using {}: ",
            device.location.stable_id(),
            device.profile.display_name,
            bitstream_path.display()
        );
        match load_device_fpga(&device, &bitstream_path) {
            Ok(FpgaLoad::AlreadyConfigured(status)) => {
                println!("already configured (status 0x{:02x})", status.raw);
            }
            Ok(FpgaLoad::Configured { bytes, status }) => {
                println!(
                    "configured with {bytes} bytes (status 0x{:02x})",
                    status.raw
                );
            }
            Err(error) => {
                failures += 1;
                eprintln!("failed\n  {error}");
            }
        }
    }

    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} FPGA load(s) failed").into())
    }
}

fn prepare_devices(resource_directory: &Path) -> Result<(), Box<dyn Error>> {
    let report = discover_supported_devices()?;
    if report.devices.is_empty() {
        println!("no supported instruments detected");
        return Ok(());
    }

    let mut failures = 0_usize;
    for device in report.devices {
        let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
        print!(
            "{}: {}: ",
            device.location.stable_id(),
            device.profile.display_name
        );
        match prepare_device(&device, &bitstream_path) {
            Ok(prepared) => {
                let fpga = match prepared.fpga {
                    FpgaLoad::AlreadyConfigured(_) => "FPGA already configured".to_owned(),
                    FpgaLoad::Configured { bytes, .. } => {
                        format!("FPGA loaded with {bytes} bytes")
                    }
                };
                println!(
                    "ready; firmware {}, HDL {}, {fpga}",
                    prepared.firmware, prepared.hdl
                );
            }
            Err(error) => {
                failures += 1;
                eprintln!("failed\n  {error}");
            }
        }
    }

    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} device preparation(s) failed").into())
    }
}

fn probe_devices() -> Result<(), Box<dyn Error>> {
    let report = discover_supported_devices()?;
    if report.devices.is_empty() {
        println!("no supported instruments detected");
        return Ok(());
    }

    let mut failures = 0_usize;
    for device in report.devices {
        println!(
            "{}: {} at {}",
            device.location.stable_id(),
            device.profile.display_name,
            device.location.platform_path
        );
        match probe_device(&device) {
            Ok(probe) => {
                println!(
                    "  configuration {}, interface {}, alternate setting {}",
                    probe.configuration, probe.interface, probe.alternate_setting
                );
                println!(
                    "  bulk endpoints: OUT 0x{:02x} ({} bytes), IN 0x{:02x} ({} bytes)",
                    probe.bulk_out.address,
                    probe.bulk_out.max_packet_size,
                    probe.bulk_in.address,
                    probe.bulk_in.max_packet_size
                );
                let compatibility = if probe.firmware_version.is_compatible() {
                    "compatible"
                } else {
                    failures += 1;
                    "incompatible"
                };
                println!(
                    "  firmware {}, {compatibility}; interface claim and release succeeded",
                    probe.firmware_version
                );
                println!(
                    "  hardware status 0x{:02x}: FPGA {}, initialized {}, GPIF {}, FIFO {}",
                    probe.hardware_status.raw,
                    status_word(probe.hardware_status.fpga_configured, "configured"),
                    yes_no(probe.hardware_status.fpga_initialized),
                    status_word(probe.hardware_status.gpif_done, "done"),
                    status_word(probe.hardware_status.fifo_overflow, "overflow")
                );
            }
            Err(error) => {
                failures += 1;
                eprintln!("  probe failed: {error}");
            }
        }
    }

    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} device probe(s) failed").into())
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn status_word(value: bool, enabled: &'static str) -> &'static str {
    if value {
        enabled
    } else {
        "not set"
    }
}

fn list_devices() -> Result<(), Box<dyn Error>> {
    let report = discover_supported_devices()?;
    if report.devices.is_empty() {
        println!("no supported instruments detected");
    }
    for device in report.devices {
        println!(
            "{}: {} at {} ({}, {})",
            device.location.stable_id(),
            device.profile.display_name,
            device.location.platform_path,
            device.speed.description(),
            if device.profile.model.supports_capture() {
                "acquisition available"
            } else {
                "discovery/probe/preparation available"
            }
        );
        match device.link_status() {
            LinkStatus::Optimal => {}
            LinkStatus::ReducedPerformance => eprintln!(
                "warning: {} is using {}; connect it to a USB 3 port with a USB 3 cable for full capture performance",
                device.profile.display_name,
                device.speed.description()
            ),
            LinkStatus::Unsupported => eprintln!(
                "error: {} is using {}; {} or faster is required, and USB 3 is recommended",
                device.profile.display_name,
                device.speed.description(),
                device.profile.minimum_speed.description()
            ),
        }
    }
    Ok(())
}

fn run_simulation(run_seconds: u64, output: Option<&Path>) -> Result<(), Box<dyn Error>> {
    let logic = SimulatedLogicAnalyzer::new("sim-logic-0", 100_000, 8, 1_024)?;
    let scope = SimulatedOscilloscope::new("sim-scope-0", 50_000, 2, 512)?;
    let session = CaptureCoordinator::start(vec![Box::new(logic), Box::new(scope)])?;
    let mut metadata = BTreeMap::new();
    metadata.insert("capture.command".to_owned(), "simulate".to_owned());
    metadata.insert(
        "capture.duration_seconds".to_owned(),
        run_seconds.to_string(),
    );
    let mut recorder = start_recording(output, session.descriptors(), metadata)?;

    println!(
        "capturing from {} instruments for {run_seconds}s",
        session.descriptors().len()
    );
    for descriptor in session.descriptors() {
        println!(
            "  {}: {} ({:?})",
            descriptor.id, descriptor.name, descriptor.kind
        );
    }

    let deadline = Instant::now() + Duration::from_secs(run_seconds);
    let mut stats = HashMap::<DeviceId, DeviceStats>::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match session.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(event) => {
                write_recording_event(&mut recorder, &event)?;
                match event {
                    CaptureEvent::Frame(frame) => {
                        let entry = stats.entry(frame.device_id).or_default();
                        entry.frames += 1;
                        entry.samples += frame.payload.sample_count() as u64;
                        entry.first_timestamp.get_or_insert(frame.captured_at);
                        entry.last_timestamp = Some(frame.captured_at);
                    }
                    CaptureEvent::SourceFailed { device_id, error } => {
                        let stop_result = session.stop();
                        finish_recording(&mut recorder, CompletionStatus::Failed)?;
                        stop_result?;
                        return Err(format!("capture source {device_id} failed: {error}").into());
                    }
                    CaptureEvent::SourceStopped { device_id } => {
                        println!("capture source {device_id} stopped");
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let stop_result = session.stop();
    let completion = if stop_result.is_ok() {
        CompletionStatus::Complete
    } else {
        CompletionStatus::Failed
    };
    finish_recording(&mut recorder, completion)?;
    stop_result?;

    println!("capture summary:");
    for (device_id, device_stats) in stats {
        println!(
            "  {device_id}: {} frames, {} samples, timeline {:?}..{:?}",
            device_stats.frames,
            device_stats.samples,
            device_stats.first_timestamp.unwrap_or_default(),
            device_stats.last_timestamp.unwrap_or_default()
        );
    }
    Ok(())
}

#[cfg(test)]
mod automation_cli_tests {
    use super::*;

    #[test]
    fn parses_direct_capture_arguments_in_any_order() {
        let parsed = Cli::try_parse_from([
            "wavecraft-cli",
            "capture",
            "run",
            "--samples",
            "10000",
            "--device",
            "sim:logic-0",
            "--channels",
            "0,2,7",
            "--output",
            "capture.dsv",
            "--resource-directory",
            "resources/fpga",
            "--sample-rate",
            "100000",
            "--events",
            "ndjson",
            "--timeout-ms",
            "5000",
        ])
        .expect("valid capture arguments should parse");
        let Command::Capture {
            command: CaptureCommand::Run(parsed),
        } = parsed.command
        else {
            panic!("capture run should produce capture arguments");
        };

        assert_eq!(parsed.device, "sim:logic-0");
        assert_eq!(parsed.sample_rate, 100_000);
        assert_eq!(parsed.samples, 10_000);
        assert_eq!(parsed.channels, [0, 2, 7]);
        assert_eq!(parsed.output, PathBuf::from("capture.dsv"));
        assert_eq!(
            parsed.resource_directory.as_deref(),
            Some(Path::new("resources/fpga"))
        );
        assert_eq!(parsed.timeout_ms, Some(5_000));
        assert!(matches!(parsed.events, EventFormat::Ndjson));
    }

    #[test]
    fn rejects_duplicate_capture_options() {
        let error = Cli::try_parse_from([
            "wavecraft-cli",
            "capture",
            "run",
            "--device",
            "sim:logic-0",
            "--device",
            "sim:logic-1",
        ])
        .expect_err("duplicate device selection should fail");

        assert!(error.to_string().contains("cannot be used multiple times"));
    }

    #[test]
    fn parses_grouped_recording_commands_and_legacy_aliases() {
        let grouped = Cli::try_parse_from(["wavecraft-cli", "recording", "inspect", "capture.dsv"])
            .expect("grouped recording command should parse");
        assert!(matches!(
            grouped.command,
            Command::Recording {
                command: RecordingCommand::Inspect(_)
            }
        ));

        let legacy = Cli::try_parse_from(["wavecraft-cli", "inspect", "capture.dsv"])
            .expect("legacy recording alias should parse");
        assert!(matches!(legacy.command, Command::Inspect(_)));
    }

    #[test]
    fn devices_options_accept_conventional_ordering() {
        let parsed = Cli::try_parse_from([
            "wavecraft-cli",
            "devices",
            "probe",
            "--format",
            "json",
            "--id",
            "sim:logic-0",
        ])
        .expect("device options should parse in any order");

        assert!(matches!(
            parsed.command,
            Command::Devices {
                command: DevicesCommand::Probe {
                    format: OutputFormat::Json,
                    ..
                }
            }
        ));
    }
}
