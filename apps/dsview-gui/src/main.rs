#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use dsview_core::{
    CaptureBatch, CaptureCoordinator, CaptureEvent, CaptureSession, CaptureSource,
    DeviceDescriptor, DeviceId,
};
use dsview_recording::{CompletionStatus, RecordingWriter};
use dsview_usb::{
    discover_supported_devices, prepare_device, DeviceModel, DiscoveredDevice, DsCopeCaptureConfig,
    DsCopeCaptureSource, DsLogicCaptureConfig, DsLogicCaptureSource, LinkStatus,
};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

const SAMPLE_RATE_HZ: u64 = 1_000_000;
const SAMPLE_COUNT: u64 = 1_000_000;
const LOGIC_CHANNEL_COUNT: usize = 16;
const MAX_WAVEFORM_SAMPLES: usize = 8_192;
const BACKGROUND: Color32 = Color32::from_rgb(12, 17, 25);
const PANEL: Color32 = Color32::from_rgb(20, 27, 38);
const GRID: Color32 = Color32::from_rgb(43, 55, 72);
const TEXT_MUTED: Color32 = Color32::from_rgb(145, 160, 180);
const ACCENT: Color32 = Color32::from_rgb(70, 210, 185);
const SCOPE_COLORS: [Color32; 2] = [
    Color32::from_rgb(255, 204, 74),
    Color32::from_rgb(99, 179, 255),
];

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([920.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DSView",
        options,
        Box::new(|creation_context| Ok(Box::new(DsViewApp::new(creation_context)))),
    )
}

struct DeviceEntry {
    device: DiscoveredDevice,
    selected: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaveformTab {
    LogicAnalyzer,
    Oscilloscope,
    Combined,
}

impl WaveformTab {
    fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "Logic Analyzer",
            Self::Oscilloscope => "Oscilloscope",
            Self::Combined => "Combined",
        }
    }
}

fn waveform_tabs(has_logic_analyzer: bool, has_oscilloscope: bool) -> Vec<WaveformTab> {
    let mut tabs = Vec::with_capacity(3);
    if has_logic_analyzer {
        tabs.push(WaveformTab::LogicAnalyzer);
    }
    if has_oscilloscope {
        tabs.push(WaveformTab::Oscilloscope);
    }
    if has_logic_analyzer && has_oscilloscope {
        tabs.push(WaveformTab::Combined);
    }
    tabs
}

fn default_logic_channel_colors() -> [Color32; LOGIC_CHANNEL_COUNT] {
    [
        Color32::from_rgb(90, 220, 145),
        Color32::from_rgb(255, 204, 74),
        Color32::from_rgb(74, 210, 230),
        Color32::from_rgb(238, 112, 214),
        Color32::from_rgb(255, 145, 77),
        Color32::from_rgb(99, 179, 255),
        Color32::from_rgb(180, 230, 80),
        Color32::from_rgb(255, 120, 160),
        Color32::from_rgb(70, 210, 185),
        Color32::from_rgb(245, 177, 66),
        Color32::from_rgb(174, 130, 255),
        Color32::from_rgb(100, 205, 255),
        Color32::from_rgb(245, 100, 100),
        Color32::from_rgb(105, 235, 190),
        Color32::from_rgb(225, 215, 90),
        Color32::from_rgb(195, 155, 255),
    ]
}

#[derive(Default)]
struct DeviceStats {
    frames: u64,
    samples: u64,
    first_timestamp: Option<Duration>,
    last_timestamp: Option<Duration>,
}

#[derive(Default)]
struct WaveformStore {
    logic_samples: VecDeque<u32>,
    logic_channels: u8,
    logic_rate_hz: u64,
    scope_channels: Vec<VecDeque<f32>>,
    scope_rate_hz: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WaveformViewport {
    start: f32,
    span: f32,
}

impl Default for WaveformViewport {
    fn default() -> Self {
        Self {
            start: 0.0,
            span: 1.0,
        }
    }
}

impl WaveformViewport {
    const MIN_SPAN: f32 = 1.0 / 256.0;

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn zoom(&mut self, scroll_delta: f32, cursor_fraction: f32) {
        if scroll_delta == 0.0 {
            return;
        }
        let cursor_fraction = cursor_fraction.clamp(0.0, 1.0);
        let anchor = self.start + cursor_fraction * self.span;
        let new_span = (self.span * (-scroll_delta * 0.01).exp()).clamp(Self::MIN_SPAN, 1.0);
        self.start = (anchor - cursor_fraction * new_span).clamp(0.0, 1.0 - new_span);
        self.span = new_span;
    }

    fn sample_range(self, sample_count: usize) -> Range<usize> {
        if sample_count == 0 {
            return 0..0;
        }
        let start = ((self.start * sample_count as f32).floor() as usize).min(sample_count - 1);
        let mut end = ((self.start + self.span) * sample_count as f32).ceil() as usize;
        end = end.clamp(start + 1, sample_count);
        start..end
    }

    fn zoom_factor(self) -> f32 {
        1.0 / self.span
    }
}

impl WaveformStore {
    fn ingest(&mut self, batch: &CaptureBatch) {
        match batch {
            CaptureBatch::Logic(logic) => {
                self.logic_channels = logic.channel_count;
                self.logic_rate_hz = logic.sample_rate_hz;
                extend_bounded(&mut self.logic_samples, logic.samples.iter().copied());
            }
            CaptureBatch::Scope(scope) => {
                self.scope_rate_hz = scope.sample_rate_hz;
                if self.scope_channels.len() != scope.channels.len() {
                    self.scope_channels = vec![VecDeque::new(); scope.channels.len()];
                }
                for (destination, source) in
                    self.scope_channels.iter_mut().zip(scope.channels.iter())
                {
                    extend_bounded(destination, source.iter().copied());
                }
            }
        }
    }
}

fn extend_bounded<T>(destination: &mut VecDeque<T>, values: impl IntoIterator<Item = T>) {
    destination.extend(values);
    let excess = destination.len().saturating_sub(MAX_WAVEFORM_SAMPLES);
    destination.drain(..excess);
}

struct ActiveCapture {
    session: CaptureSession,
    stats: HashMap<DeviceId, DeviceStats>,
    stopped: HashSet<DeviceId>,
    waveforms: WaveformStore,
    recorder: Option<FileRecorder>,
    recording_path: Option<String>,
}

struct CompletedCapture {
    stats: HashMap<DeviceId, DeviceStats>,
    waveforms: WaveformStore,
    recording_path: Option<String>,
}

type FileRecorder = RecordingWriter<BufWriter<File>>;

enum WorkerSuccess {
    Devices(Vec<DiscoveredDevice>),
    Capture(CaptureSession),
}

type WorkerResult = Result<WorkerSuccess, String>;

struct DsViewApp {
    resource_directory: String,
    record_capture: bool,
    recording_path: String,
    devices: Vec<DeviceEntry>,
    active_waveform_tab: Option<WaveformTab>,
    combined_logic_ratio: f32,
    logic_channel_colors: [Color32; LOGIC_CHANNEL_COUNT],
    logic_viewport: WaveformViewport,
    scope_viewport: WaveformViewport,
    pending: Option<Receiver<WorkerResult>>,
    capture: Option<ActiveCapture>,
    completed_capture: Option<CompletedCapture>,
    status: String,
    last_error: Option<String>,
}

impl DsViewApp {
    fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        configure_style(&creation_context.egui_ctx);
        let mut app = Self {
            resource_directory: default_resource_directory(),
            record_capture: false,
            recording_path: "capture.dsv".to_owned(),
            devices: Vec::new(),
            active_waveform_tab: None,
            combined_logic_ratio: 0.55,
            logic_channel_colors: default_logic_channel_colors(),
            logic_viewport: WaveformViewport::default(),
            scope_viewport: WaveformViewport::default(),
            pending: None,
            capture: None,
            completed_capture: None,
            status: "Ready".to_owned(),
            last_error: None,
        };
        app.begin_refresh();
        app
    }

    fn begin_refresh(&mut self) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.completed_capture = None;
        self.status = "Discovering USB instruments…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = discover_supported_devices()
                .map(|report| WorkerSuccess::Devices(report.devices))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn begin_capture(&mut self) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        let devices = self
            .devices
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.device.clone())
            .collect::<Vec<_>>();
        if devices.is_empty() {
            self.last_error = Some("Select at least one supported instrument".to_owned());
            return;
        }
        if self.record_capture && self.recording_path.trim().is_empty() {
            self.last_error = Some("Enter a recording file path".to_owned());
            return;
        }

        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.completed_capture = None;
        self.status = format!("Preparing {} instrument(s)…", devices.len());
        self.last_error = None;
        thread::spawn(move || {
            let result = start_capture(devices, &resource_directory).map(WorkerSuccess::Capture);
            let _ = sender.send(result);
        });
    }

    fn stop_capture(&mut self) {
        let Some(mut active) = self.capture.take() else {
            return;
        };
        self.status = "Stopping capture…".to_owned();
        let stop_result = active.session.stop();
        let completion = if stop_result.is_ok() {
            CompletionStatus::Cancelled
        } else {
            CompletionStatus::Failed
        };
        let recording_result = finish_gui_recording(&mut active.recorder, completion);
        self.completed_capture = Some(CompletedCapture {
            stats: active.stats,
            waveforms: active.waveforms,
            recording_path: active.recording_path,
        });
        match (stop_result, recording_result) {
            (Ok(()), Ok(())) => self.status = "Capture stopped".to_owned(),
            (Err(error), Ok(())) => self.set_error(error.to_string()),
            (Ok(()), Err(error)) => self.set_error(error),
            (Err(stop_error), Err(recording_error)) => self.set_error(format!(
                "{stop_error}; additionally failed to finish recording: {recording_error}"
            )),
        }
    }

    fn poll_worker(&mut self) {
        let Some(receiver) = self.pending.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err("background operation ended unexpectedly".to_owned())
            }
        };
        self.pending = None;
        match result {
            Ok(WorkerSuccess::Devices(devices)) => {
                let selected = self
                    .devices
                    .iter()
                    .filter(|entry| entry.selected)
                    .map(|entry| entry.device.location.stable_id())
                    .collect::<HashSet<_>>();
                self.devices = devices
                    .into_iter()
                    .map(|device| {
                        let stable_id = device.location.stable_id();
                        let selected = if selected.is_empty() {
                            device.link_status() != LinkStatus::Unsupported
                        } else {
                            selected.contains(&stable_id)
                        };
                        DeviceEntry { device, selected }
                    })
                    .collect();
                self.status = if self.devices.is_empty() {
                    "No supported instruments detected".to_owned()
                } else {
                    format!("Detected {} supported instrument(s)", self.devices.len())
                };
            }
            Ok(WorkerSuccess::Capture(session)) => {
                let count = session.descriptors().len();
                let recording_path = self
                    .record_capture
                    .then(|| self.recording_path.trim().to_owned());
                let recorder = match recording_path.as_deref() {
                    Some(path) => match start_gui_recording(Path::new(path), session.descriptors())
                    {
                        Ok(recorder) => Some(recorder),
                        Err(error) => {
                            let cleanup = session.stop().err();
                            self.set_error(match cleanup {
                                Some(cleanup) => {
                                    format!("{error}; additionally failed to stop: {cleanup}")
                                }
                                None => error,
                            });
                            return;
                        }
                    },
                    None => None,
                };
                self.capture = Some(ActiveCapture {
                    session,
                    stats: HashMap::new(),
                    stopped: HashSet::new(),
                    waveforms: WaveformStore::default(),
                    recorder,
                    recording_path: recording_path.clone(),
                });
                self.status = match recording_path {
                    Some(path) => {
                        format!("Capturing {count} instrument(s) to {path}")
                    }
                    None => format!("Capturing from {count} instrument(s) at 1 MHz"),
                };
            }
            Err(error) => self.set_error(error),
        }
    }

    fn poll_capture(&mut self) {
        let mut finish_status = None::<(Result<String, String>, CompletionStatus)>;
        if let Some(active) = self.capture.as_mut() {
            loop {
                match active.session.try_recv() {
                    Ok(event) => {
                        if let Some(recorder) = active.recorder.as_mut() {
                            if let Err(error) = recorder.write_event(&event) {
                                active.session.request_stop();
                                finish_status = Some((
                                    Err(format!("failed to write recording: {error}")),
                                    CompletionStatus::Failed,
                                ));
                                break;
                            }
                        }
                        match event {
                            CaptureEvent::Frame(frame) => {
                                let entry = active.stats.entry(frame.device_id).or_default();
                                entry.frames += 1;
                                entry.samples += frame.payload.sample_count() as u64;
                                entry.first_timestamp.get_or_insert(frame.captured_at);
                                entry.last_timestamp = Some(frame.captured_at);
                                active.waveforms.ingest(&frame.payload);
                            }
                            CaptureEvent::SourceFailed { device_id, error } => {
                                active.session.request_stop();
                                finish_status = Some((
                                    Err(format!("{device_id}: {error}")),
                                    CompletionStatus::Failed,
                                ));
                                break;
                            }
                            CaptureEvent::SourceStopped { device_id } => {
                                active.stopped.insert(device_id);
                                if active.stopped.len() == active.session.descriptors().len() {
                                    finish_status = Some((
                                        Ok("Capture complete".to_owned()),
                                        CompletionStatus::Complete,
                                    ));
                                    break;
                                }
                            }
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finish_status = Some((
                            Err("capture event stream disconnected".to_owned()),
                            CompletionStatus::Failed,
                        ));
                        break;
                    }
                }
            }
        }

        if let Some((result, completion)) = finish_status {
            if let Some(mut active) = self.capture.take() {
                let stop_result = active.session.stop();
                let recording_completion = if stop_result.is_ok() {
                    completion
                } else {
                    CompletionStatus::Failed
                };
                let recording_result =
                    finish_gui_recording(&mut active.recorder, recording_completion);
                let mut error = result.as_ref().err().cloned();
                if let Err(stop_error) = stop_result {
                    append_error(&mut error, format!("failed to stop capture: {stop_error}"));
                }
                if let Err(recording_error) = recording_result {
                    append_error(
                        &mut error,
                        format!("failed to finish recording: {recording_error}"),
                    );
                }
                self.completed_capture = Some(CompletedCapture {
                    stats: active.stats,
                    waveforms: active.waveforms,
                    recording_path: active.recording_path,
                });
                match error {
                    Some(error) => self.set_error(error),
                    None => {
                        self.status = result.ok().unwrap_or_else(|| "Capture complete".to_owned());
                    }
                }
            }
        }
    }

    fn set_error(&mut self, error: String) {
        self.status = "Operation failed".to_owned();
        self.last_error = Some(error);
    }

    fn available_waveform_tabs(&self) -> Vec<WaveformTab> {
        let has_logic_analyzer = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model == DeviceModel::DsLogicU3Pro16);
        let has_oscilloscope = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model == DeviceModel::DsCopeU3P100);
        waveform_tabs(has_logic_analyzer, has_oscilloscope)
    }

    fn reconcile_waveform_tab(&mut self) {
        let available = self.available_waveform_tabs();
        if self
            .active_waveform_tab
            .is_some_and(|active| available.contains(&active))
        {
            return;
        }
        self.active_waveform_tab = if available.contains(&WaveformTab::Combined) {
            Some(WaveformTab::Combined)
        } else {
            available.first().copied()
        };
    }

    fn is_busy(&self) -> bool {
        self.pending.is_some() || self.capture.is_some()
    }
}

impl eframe::App for DsViewApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();
        self.reconcile_waveform_tab();
        self.poll_capture();

        let mut refresh_clicked = false;
        let mut start_clicked = false;
        let mut stop_clicked = false;

        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(12.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("DSVIEW").strong().size(18.0).color(ACCENT));
                    ui.separator();
                    ui.label(RichText::new("USB capture workbench").color(TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        stop_clicked = ui
                            .add_enabled(self.capture.is_some(), egui::Button::new("■ Stop"))
                            .clicked();
                        start_clicked = ui
                            .add_enabled(
                                !self.is_busy() && self.devices.iter().any(|entry| entry.selected),
                                egui::Button::new("▶ Capture"),
                            )
                            .clicked();
                        refresh_clicked = ui
                            .add_enabled(!self.is_busy(), egui::Button::new("↻ Refresh"))
                            .clicked();
                    });
                });
            });

        egui::SidePanel::left("devices")
            .resizable(true)
            .default_width(300.0)
            .width_range(250.0..=420.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(
                        RichText::new("CAPTURE RESOURCES")
                            .strong()
                            .color(TEXT_MUTED),
                    );
                    ui.add_space(8.0);
                    ui.label("FPGA resource directory");
                    ui.add_enabled(
                        !self.is_busy(),
                        egui::TextEdit::singleline(&mut self.resource_directory)
                            .desired_width(f32::INFINITY),
                    );
                    ui.small(
                        RichText::new("Contains DSLogicU3Pro16.bin and DSCopeU3P100.bin")
                            .color(TEXT_MUTED),
                    );
                    ui.add_space(14.0);
                    ui.add_enabled(
                        !self.is_busy(),
                        egui::Checkbox::new(&mut self.record_capture, "Record complete capture"),
                    );
                    if self.record_capture {
                        ui.label("Recording file");
                        ui.add_enabled(
                            !self.is_busy(),
                            egui::TextEdit::singleline(&mut self.recording_path)
                                .desired_width(f32::INFINITY),
                        );
                        ui.small(
                            RichText::new("Lossless .dsv file; existing files are not overwritten")
                                .color(TEXT_MUTED),
                        );
                    }
                    ui.add_space(18.0);
                    ui.label(RichText::new("INSTRUMENTS").strong().color(TEXT_MUTED));
                    ui.add_space(6.0);

                    if self.devices.is_empty() {
                        ui.label(RichText::new("No supported devices").color(TEXT_MUTED));
                    }
                    for entry in &mut self.devices {
                        ui.add_space(4.0);
                        egui::Frame::none()
                            .fill(BACKGROUND)
                            .rounding(egui::Rounding::same(6.0))
                            .inner_margin(egui::Margin::same(10.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.add_enabled(
                                        self.capture.is_none() && self.pending.is_none(),
                                        egui::Checkbox::new(
                                            &mut entry.selected,
                                            RichText::new(entry.device.profile.display_name)
                                                .strong(),
                                        ),
                                    );
                                });
                                ui.small(
                                    RichText::new(entry.device.location.stable_id())
                                        .color(TEXT_MUTED),
                                );
                                let (link_text, link_color) = match entry.device.link_status() {
                                    LinkStatus::Optimal => ("USB 3 · optimal", ACCENT),
                                    LinkStatus::ReducedPerformance => {
                                        ("USB 2 · reduced performance", SCOPE_COLORS[0])
                                    }
                                    LinkStatus::Unsupported => {
                                        ("Unsupported link speed", Color32::LIGHT_RED)
                                    }
                                };
                                ui.small(RichText::new(link_text).color(link_color));
                            });
                    }

                    ui.add_space(18.0);
                    ui.label(RichText::new("CAPTURE MODE").strong().color(TEXT_MUTED));
                    ui.add_space(6.0);
                    egui::Grid::new("capture_config")
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Sample rate");
                            ui.monospace("1 MHz");
                            ui.end_row();
                            ui.label("Buffer");
                            ui.monospace("1M samples");
                            ui.end_row();
                            ui.label("Scope range");
                            ui.monospace("1 V/div");
                            ui.end_row();
                        });

                    if let Some(active) = &self.capture {
                        ui.add_space(18.0);
                        ui.label(RichText::new("SESSION").strong().color(TEXT_MUTED));
                        ui.add_space(6.0);
                        for descriptor in active.session.descriptors() {
                            let stats = active.stats.get(&descriptor.id);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("●").color(ACCENT));
                                ui.label(&descriptor.name);
                            });
                            if let Some(stats) = stats {
                                ui.small(
                                    RichText::new(format!(
                                        "{} frames · {} samples",
                                        stats.frames, stats.samples
                                    ))
                                    .color(TEXT_MUTED),
                                );
                            }
                        }
                        if let Some(skew) = capture_start_skew(&active.stats) {
                            ui.add_space(6.0);
                            ui.label(format!("Start skew: {skew:?}"));
                        }
                    } else if let Some(completed) = &self.completed_capture {
                        ui.add_space(18.0);
                        ui.label(RichText::new("LAST CAPTURE").strong().color(TEXT_MUTED));
                        if let Some(path) = &completed.recording_path {
                            ui.small(
                                RichText::new(format!("Recording file: {path}")).color(ACCENT),
                            );
                        }
                        for (device_id, stats) in &completed.stats {
                            ui.add_space(4.0);
                            ui.label(device_id.to_string());
                            ui.small(
                                RichText::new(format!(
                                    "{} frames · {} samples",
                                    stats.frames, stats.samples
                                ))
                                .color(TEXT_MUTED),
                            );
                        }
                        if let Some(skew) = capture_start_skew(&completed.stats) {
                            ui.add_space(6.0);
                            ui.label(format!("Start skew: {skew:?}"));
                        }
                    }
                });
            });

        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(9.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    let color = if self.last_error.is_some() {
                        Color32::LIGHT_RED
                    } else if self.is_busy() {
                        ACCENT
                    } else {
                        TEXT_MUTED
                    };
                    ui.label(RichText::new("●").color(color));
                    ui.label(&self.status);
                    if let Some(error) = &self.last_error {
                        ui.separator();
                        ui.label(RichText::new(error).color(Color32::LIGHT_RED));
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::same(16.0)),
            )
            .show(context, |ui| {
                let available_tabs = self.available_waveform_tabs();
                if available_tabs.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(
                                "Connect a supported logic analyzer or oscilloscope, then Refresh",
                            )
                            .color(TEXT_MUTED),
                        );
                    });
                    return;
                }

                ui.horizontal(|ui| {
                    for tab in &available_tabs {
                        if ui
                            .selectable_label(self.active_waveform_tab == Some(*tab), tab.label())
                            .clicked()
                        {
                            self.active_waveform_tab = Some(*tab);
                        }
                    }
                });
                ui.separator();
                ui.add_space(8.0);

                let empty_waveforms = WaveformStore::default();
                let waveforms = self
                    .capture
                    .as_ref()
                    .map(|active| &active.waveforms)
                    .or_else(|| {
                        self.completed_capture
                            .as_ref()
                            .map(|capture| &capture.waveforms)
                    })
                    .unwrap_or(&empty_waveforms);
                let mut combined_logic_ratio = self.combined_logic_ratio;
                let mut logic_channel_colors = self.logic_channel_colors;
                let mut logic_viewport = self.logic_viewport;
                let mut scope_viewport = self.scope_viewport;
                match self.active_waveform_tab {
                    Some(WaveformTab::LogicAnalyzer) => {
                        waveform_card_fill(
                            ui,
                            "DIGITAL",
                            "DSLogic · channels 0–15",
                            |ui, rect| {
                                paint_logic(
                                    ui,
                                    rect,
                                    waveforms,
                                    &mut logic_channel_colors,
                                    &mut logic_viewport,
                                )
                            },
                        );
                    }
                    Some(WaveformTab::Oscilloscope) => {
                        waveform_card_fill(
                            ui,
                            "OSCILLOSCOPE",
                            "DSCope · CH0 / CH1",
                            |ui, rect| paint_scope(ui, rect, waveforms, &mut scope_viewport),
                        );
                    }
                    Some(WaveformTab::Combined) => {
                        combined_waveform_cards(
                            ui,
                            waveforms,
                            &mut combined_logic_ratio,
                            &mut logic_channel_colors,
                            &mut logic_viewport,
                            &mut scope_viewport,
                        );
                    }
                    None => {}
                }
                self.combined_logic_ratio = combined_logic_ratio;
                self.logic_channel_colors = logic_channel_colors;
                self.logic_viewport = logic_viewport;
                self.scope_viewport = scope_viewport;
            });

        if refresh_clicked {
            self.begin_refresh();
        }
        if start_clicked {
            self.begin_capture();
        }
        if stop_clicked {
            self.stop_capture();
        }
        if self.pending.is_some() || self.capture.is_some() {
            context.request_repaint_after(Duration::from_millis(16));
        }
    }
}

fn default_resource_directory() -> String {
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(executable_directory) = executable.parent() {
            #[cfg(target_os = "macos")]
            if executable_directory
                .file_name()
                .and_then(|name| name.to_str())
                == Some("MacOS")
            {
                if let Some(contents_directory) = executable_directory.parent() {
                    candidates.push(contents_directory.join("Resources/fpga"));
                }
            }
            candidates.push(executable_directory.join("resources/fpga"));
        }
    }
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("resources/fpga"),
    );
    candidates.push(PathBuf::from("resources/fpga"));
    let selected = candidates
        .into_iter()
        .find(|candidate| has_required_fpga_resources(candidate))
        .unwrap_or_else(|| PathBuf::from("resources/fpga"));
    selected
        .canonicalize()
        .unwrap_or(selected)
        .to_string_lossy()
        .into_owned()
}

fn has_required_fpga_resources(directory: &Path) -> bool {
    directory.join("DSLogicU3Pro16.bin").is_file() && directory.join("DSCopeU3P100.bin").is_file()
}

fn start_gui_recording(path: &Path, devices: &[DeviceDescriptor]) -> Result<FileRecorder, String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to create recording {} without overwriting an existing file: {error}",
                path.display()
            )
        })?;
    let mut metadata = BTreeMap::new();
    metadata.insert("capture.command".to_owned(), "gui".to_owned());
    metadata.insert(
        "capture.sample_rate_hz".to_owned(),
        SAMPLE_RATE_HZ.to_string(),
    );
    metadata.insert(
        "capture.requested_sample_count".to_owned(),
        SAMPLE_COUNT.to_string(),
    );
    metadata.insert("scope.volts_per_div".to_owned(), "1".to_owned());
    RecordingWriter::new(BufWriter::new(file), devices, metadata).map_err(|error| error.to_string())
}

fn finish_gui_recording(
    recorder: &mut Option<FileRecorder>,
    status: CompletionStatus,
) -> Result<(), String> {
    if let Some(recorder) = recorder {
        recorder.finish(status).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn append_error(error: &mut Option<String>, additional: String) {
    match error {
        Some(error) => {
            error.push_str("; ");
            error.push_str(&additional);
        }
        None => *error = Some(additional),
    }
}

fn start_capture(
    devices: Vec<DiscoveredDevice>,
    resource_directory: &Path,
) -> Result<CaptureSession, String> {
    let logic_config = DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)
        .map_err(|error| error.to_string())?;
    let scope_config = DsCopeCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)
        .map_err(|error| error.to_string())?;
    let mut sources = Vec::<Box<dyn CaptureSource>>::with_capacity(devices.len());
    for device in devices {
        if device.link_status() == LinkStatus::Unsupported {
            return Err(format!(
                "{} is connected at an unsupported speed",
                device.profile.display_name
            ));
        }
        let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
        prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
        match device.profile.model {
            DeviceModel::DsLogicU3Pro16 => sources.push(Box::new(
                DsLogicCaptureSource::new(device, logic_config)
                    .map_err(|error| error.to_string())?,
            )),
            DeviceModel::DsCopeU3P100 => sources.push(Box::new(
                DsCopeCaptureSource::new(device, scope_config)
                    .map_err(|error| error.to_string())?,
            )),
        }
    }
    CaptureCoordinator::start(sources).map_err(|error| error.to_string())
}

fn configure_style(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BACKGROUND;
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    visuals.widgets.active.bg_fill = ACCENT.gamma_multiply(0.6);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(45, 61, 77);
    context.set_visuals(visuals);
}

const WAVEFORM_CARD_CHROME_HEIGHT: f32 = 58.0;
const COMBINED_DIVIDER_HEIGHT: f32 = 10.0;
const MIN_LOGIC_PANE_HEIGHT: f32 = 260.0;
const MIN_SCOPE_PANE_HEIGHT: f32 = 140.0;

fn waveform_card_fill(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    paint: impl FnOnce(&egui::Ui, Rect),
) {
    let plot_height = (ui.available_height() - WAVEFORM_CARD_CHROME_HEIGHT).max(64.0);
    waveform_card(ui, title, subtitle, plot_height, paint);
}

fn combined_pane_heights(total_height: f32, logic_ratio: f32) -> (f32, f32) {
    let usable_height = (total_height - COMBINED_DIVIDER_HEIGHT).max(0.0);
    if usable_height == 0.0 {
        return (0.0, 0.0);
    }
    let minimum_total = MIN_LOGIC_PANE_HEIGHT + MIN_SCOPE_PANE_HEIGHT;
    let minimum_scale = (usable_height / minimum_total).min(1.0);
    let minimum_logic_height = MIN_LOGIC_PANE_HEIGHT * minimum_scale;
    let minimum_scope_height = MIN_SCOPE_PANE_HEIGHT * minimum_scale;
    let logic_height = (usable_height * logic_ratio.clamp(0.0, 1.0))
        .clamp(minimum_logic_height, usable_height - minimum_scope_height);
    (logic_height, usable_height - logic_height)
}

fn combined_waveform_cards(
    ui: &mut egui::Ui,
    waveforms: &WaveformStore,
    logic_ratio: &mut f32,
    logic_channel_colors: &mut [Color32; LOGIC_CHANNEL_COUNT],
    logic_viewport: &mut WaveformViewport,
    scope_viewport: &mut WaveformViewport,
) {
    let available = ui.available_rect_before_wrap();
    let (logic_height, _) = combined_pane_heights(available.height(), *logic_ratio);
    let divider_y = available.top() + logic_height;
    let divider_hit_rect = Rect::from_min_max(
        Pos2::new(available.left(), divider_y),
        Pos2::new(
            available.right(),
            (divider_y + COMBINED_DIVIDER_HEIGHT).min(available.bottom()),
        ),
    );
    let divider_response = ui
        .interact(
            divider_hit_rect,
            ui.make_persistent_id("combined_waveform_divider"),
            Sense::drag(),
        )
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if divider_response.dragged() {
        if let Some(pointer) = divider_response.interact_pointer_pos() {
            let usable_height = (available.height() - COMBINED_DIVIDER_HEIGHT).max(1.0);
            let minimum_total = MIN_LOGIC_PANE_HEIGHT + MIN_SCOPE_PANE_HEIGHT;
            let minimum_scale = (usable_height / minimum_total).min(1.0);
            let minimum_logic_ratio = MIN_LOGIC_PANE_HEIGHT * minimum_scale / usable_height;
            let minimum_scope_ratio = MIN_SCOPE_PANE_HEIGHT * minimum_scale / usable_height;
            *logic_ratio = ((pointer.y - available.top()) / usable_height)
                .clamp(minimum_logic_ratio, 1.0 - minimum_scope_ratio);
        }
    }

    let (logic_height, scope_height) = combined_pane_heights(available.height(), *logic_ratio);
    let logic_rect = Rect::from_min_size(available.min, Vec2::new(available.width(), logic_height));
    let scope_rect = Rect::from_min_size(
        Pos2::new(
            available.left(),
            logic_rect.bottom() + COMBINED_DIVIDER_HEIGHT,
        ),
        Vec2::new(available.width(), scope_height),
    );
    ui.advance_cursor_after_rect(available);

    let mut logic_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("combined_logic_waveform")
            .max_rect(logic_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    waveform_card_fill(
        &mut logic_ui,
        "DIGITAL",
        "DSLogic · channels 0–15",
        |ui, rect| paint_logic(ui, rect, waveforms, logic_channel_colors, logic_viewport),
    );

    let mut scope_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("combined_scope_waveform")
            .max_rect(scope_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    waveform_card_fill(
        &mut scope_ui,
        "OSCILLOSCOPE",
        "DSCope · CH0 / CH1",
        |ui, rect| paint_scope(ui, rect, waveforms, scope_viewport),
    );

    let divider_color = if divider_response.hovered() || divider_response.dragged() {
        ACCENT
    } else {
        GRID
    };
    let divider_center = logic_rect.bottom() + COMBINED_DIVIDER_HEIGHT * 0.5;
    ui.painter().line_segment(
        [
            Pos2::new(available.left(), divider_center),
            Pos2::new(available.right(), divider_center),
        ],
        Stroke::new(2.0_f32, divider_color),
    );
}

fn waveform_card(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    height: f32,
    paint: impl FnOnce(&egui::Ui, Rect),
) {
    egui::Frame::none()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, GRID))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).strong().color(ACCENT));
                ui.label(RichText::new(subtitle).color(TEXT_MUTED));
            });
            ui.add_space(8.0);
            let size = Vec2::new(ui.available_width(), height);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            paint(ui, rect);
        });
}

fn paint_plot_background(ui: &egui::Ui, rect: Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, BACKGROUND);
    for index in 0..=10 {
        let x = egui::lerp(rect.x_range(), index as f32 / 10.0);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0_f32, GRID.gamma_multiply(0.55)),
        );
    }
    for index in 0..=4 {
        let y = egui::lerp(rect.y_range(), index as f32 / 4.0);
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0_f32, GRID.gamma_multiply(0.55)),
        );
    }
}

fn update_waveform_viewport(
    ui: &egui::Ui,
    rect: Rect,
    id_salt: &'static str,
    viewport: &mut WaveformViewport,
) {
    let response = ui
        .interact(rect, ui.make_persistent_id(id_salt), Sense::click())
        .on_hover_cursor(egui::CursorIcon::ZoomIn)
        .on_hover_text("Scroll to zoom · double-click to reset");
    if response.double_clicked() {
        viewport.reset();
    }
    if response.hovered() {
        let scroll_delta = ui.input(|input| input.raw_scroll_delta.y);
        if scroll_delta != 0.0 {
            let pointer_fraction = response
                .hover_pos()
                .map(|pointer| (pointer.x - rect.left()) / rect.width().max(1.0))
                .unwrap_or(0.5);
            viewport.zoom(scroll_delta, pointer_fraction);
            ui.ctx().request_repaint();
        }
    }
}

fn paint_zoom_factor(painter: &egui::Painter, rect: Rect, viewport: WaveformViewport) {
    painter.text(
        rect.right_top() + Vec2::new(-6.0, 6.0),
        egui::Align2::RIGHT_TOP,
        format!("{:.1}×", viewport.zoom_factor()),
        FontId::monospace(10.0),
        TEXT_MUTED,
    );
}

fn paint_logic(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    channel_colors: &mut [Color32; LOGIC_CHANNEL_COUNT],
    viewport: &mut WaveformViewport,
) {
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    let label_width = 42.0;
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + label_width, rect.top()),
        rect.right_bottom(),
    );
    update_waveform_viewport(ui, plot, "logic_waveform_zoom", viewport);
    let lane_height = plot.height() / LOGIC_CHANNEL_COUNT as f32;
    let sample_count = waveforms.logic_samples.len();
    let visible_range = viewport.sample_range(sample_count);
    let visible_count = visible_range.len();
    let step = (visible_count as f32 / plot.width().max(1.0))
        .floor()
        .max(1.0) as usize;
    for (channel, channel_color) in channel_colors.iter_mut().enumerate() {
        let lane_top = plot.top() + channel as f32 * lane_height;
        let high = lane_top + lane_height * 0.25;
        let low = lane_top + lane_height * 0.75;
        painter.line_segment(
            [
                Pos2::new(plot.left(), lane_top),
                Pos2::new(plot.right(), lane_top),
            ],
            Stroke::new(0.6_f32, GRID.gamma_multiply(0.45)),
        );
        let label_rect = Rect::from_min_max(
            Pos2::new(rect.left(), lane_top),
            Pos2::new(plot.left(), lane_top + lane_height),
        );
        let label_response = ui
            .interact(
                label_rect,
                ui.make_persistent_id(("logic_channel_label", channel)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!("Click to change D{channel} color"));
        if label_response.hovered() {
            painter.rect_filled(
                label_rect.shrink(1.0),
                3.0,
                channel_color.gamma_multiply(0.18),
            );
        }
        painter.text(
            Pos2::new(rect.left() + 4.0, lane_top + lane_height * 0.5),
            egui::Align2::LEFT_CENTER,
            format!("D{channel}"),
            FontId::monospace(10.0),
            *channel_color,
        );

        let popup_id = ui.make_persistent_id(("logic_channel_color_popup", channel));
        if label_response.clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &label_response,
            egui::popup::PopupCloseBehavior::CloseOnClickOutside,
            |popup_ui| {
                popup_ui.set_min_width(280.0);
                popup_ui.label(RichText::new(format!("D{channel} color")).strong());
                egui::color_picker::color_picker_color32(
                    popup_ui,
                    channel_color,
                    egui::color_picker::Alpha::Opaque,
                );
                if popup_ui.button("Reset color").clicked() {
                    *channel_color = default_logic_channel_colors()[channel];
                }
            },
        );

        if visible_count == 0 {
            continue;
        }
        let mut points = Vec::new();
        let mut previous_y = None;
        for index in visible_range.clone().step_by(step) {
            let sample = waveforms.logic_samples[index];
            let y = if sample & (1_u32 << channel) != 0 {
                high
            } else {
                low
            };
            let x = plot.left()
                + index.saturating_sub(visible_range.start) as f32
                    / visible_count.saturating_sub(1).max(1) as f32
                    * plot.width();
            if let Some(previous_y) = previous_y {
                points.push(Pos2::new(x, previous_y));
            }
            points.push(Pos2::new(x, y));
            previous_y = Some(y);
        }
        if points.len() > 1 {
            painter.add(egui::Shape::line(
                points,
                Stroke::new(1.2_f32, *channel_color),
            ));
        }
    }
    if visible_count == 0 {
        paint_empty(painter, plot, "Waiting for logic samples");
    } else {
        paint_time_span(painter, plot, visible_count, waveforms.logic_rate_hz);
    }
    paint_zoom_factor(painter, plot, *viewport);
}

fn paint_scope(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    viewport: &mut WaveformViewport,
) {
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    update_waveform_viewport(ui, rect, "scope_waveform_zoom", viewport);
    if waveforms.scope_channels.is_empty()
        || waveforms.scope_channels.iter().all(VecDeque::is_empty)
    {
        paint_empty(painter, rect, "Waiting for oscilloscope samples");
        paint_zoom_factor(painter, rect, *viewport);
        return;
    }

    let sample_count = waveforms
        .scope_channels
        .iter()
        .map(VecDeque::len)
        .min()
        .unwrap_or(0);
    if sample_count == 0 {
        paint_empty(painter, rect, "Waiting for oscilloscope samples");
        paint_zoom_factor(painter, rect, *viewport);
        return;
    }
    let visible_range = viewport.sample_range(sample_count);
    let visible_count = visible_range.len();
    let mut minimum = f32::INFINITY;
    let mut maximum = f32::NEG_INFINITY;
    for channel in &waveforms.scope_channels {
        for index in visible_range.clone() {
            minimum = minimum.min(channel[index]);
            maximum = maximum.max(channel[index]);
        }
    }
    let padding = ((maximum - minimum) * 0.12).max(0.02);
    minimum -= padding;
    maximum += padding;
    let voltage_span = (maximum - minimum).max(f32::EPSILON);
    let step = (visible_count as f32 / rect.width().max(1.0))
        .floor()
        .max(1.0) as usize;

    for (channel_index, channel) in waveforms.scope_channels.iter().enumerate() {
        let points = visible_range
            .clone()
            .step_by(step)
            .map(|index| {
                let x = rect.left()
                    + index.saturating_sub(visible_range.start) as f32
                        / visible_count.saturating_sub(1).max(1) as f32
                        * rect.width();
                let y = rect.bottom() - (channel[index] - minimum) / voltage_span * rect.height();
                Pos2::new(x, y)
            })
            .collect::<Vec<_>>();
        if points.len() > 1 {
            let color = SCOPE_COLORS[channel_index.min(SCOPE_COLORS.len() - 1)];
            painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, color)));
        }
    }
    painter.text(
        rect.left_top() + Vec2::new(6.0, 6.0),
        egui::Align2::LEFT_TOP,
        format!("{maximum:.3} V"),
        FontId::monospace(10.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.left_bottom() + Vec2::new(6.0, -6.0),
        egui::Align2::LEFT_BOTTOM,
        format!("{minimum:.3} V"),
        FontId::monospace(10.0),
        TEXT_MUTED,
    );
    paint_time_span(painter, rect, visible_count, waveforms.scope_rate_hz);
    paint_zoom_factor(painter, rect, *viewport);
}

fn paint_empty(painter: &egui::Painter, rect: Rect, text: &str) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        FontId::proportional(14.0),
        TEXT_MUTED,
    );
}

fn paint_time_span(painter: &egui::Painter, rect: Rect, samples: usize, rate_hz: u64) {
    if rate_hz == 0 {
        return;
    }
    let span = Duration::from_secs_f64(samples as f64 / rate_hz as f64);
    painter.text(
        rect.right_bottom() + Vec2::new(-6.0, -6.0),
        egui::Align2::RIGHT_BOTTOM,
        format_duration(span),
        FontId::monospace(10.0),
        TEXT_MUTED,
    );
}

fn format_duration(duration: Duration) -> String {
    if duration >= Duration::from_secs(1) {
        format!("{:.2} s", duration.as_secs_f64())
    } else if duration >= Duration::from_millis(1) {
        format!("{:.2} ms", duration.as_secs_f64() * 1_000.0)
    } else {
        format!("{:.1} µs", duration.as_secs_f64() * 1_000_000.0)
    }
}

fn capture_start_skew(stats: &HashMap<DeviceId, DeviceStats>) -> Option<Duration> {
    let mut timestamps = stats
        .values()
        .filter_map(|stats| stats.first_timestamp)
        .collect::<Vec<_>>();
    if timestamps.len() < 2 {
        return None;
    }
    timestamps.sort_unstable();
    Some(timestamps[timestamps.len() - 1] - timestamps[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsview_core::{LogicBatch, ScopeBatch};

    #[test]
    fn waveform_store_keeps_newest_samples_within_bound() {
        let mut store = WaveformStore::default();
        store.ingest(&CaptureBatch::Logic(LogicBatch {
            sample_rate_hz: 1_000,
            channel_count: 2,
            samples: (0..MAX_WAVEFORM_SAMPLES as u32 + 10).collect(),
        }));

        assert_eq!(store.logic_samples.len(), MAX_WAVEFORM_SAMPLES);
        assert_eq!(store.logic_samples.front(), Some(&10));
    }

    #[test]
    fn waveform_store_tracks_scope_channels() {
        let mut store = WaveformStore::default();
        store.ingest(&CaptureBatch::Scope(ScopeBatch {
            sample_rate_hz: 1_000_000,
            channels: vec![vec![0.1, 0.2], vec![-0.1, -0.2]],
        }));

        assert_eq!(store.scope_rate_hz, 1_000_000);
        assert_eq!(store.scope_channels[0].back(), Some(&0.2));
        assert_eq!(store.scope_channels[1].back(), Some(&-0.2));
    }

    #[test]
    fn waveform_tabs_require_their_connected_devices() {
        assert!(waveform_tabs(false, false).is_empty());
        assert_eq!(waveform_tabs(true, false), vec![WaveformTab::LogicAnalyzer]);
        assert_eq!(waveform_tabs(false, true), vec![WaveformTab::Oscilloscope]);
        assert_eq!(
            waveform_tabs(true, true),
            vec![
                WaveformTab::LogicAnalyzer,
                WaveformTab::Oscilloscope,
                WaveformTab::Combined,
            ]
        );
    }

    #[test]
    fn combined_panes_scale_with_window_and_respect_ratio() {
        let (logic, scope) = combined_pane_heights(610.0, 0.6);
        assert!((logic - 360.0).abs() < f32::EPSILON);
        assert!((scope - 240.0).abs() < f32::EPSILON);

        let (logic, scope) = combined_pane_heights(1_010.0, 0.6);
        assert!((logic - 600.0).abs() < f32::EPSILON);
        assert!((scope - 400.0).abs() < f32::EPSILON);

        let (logic, scope) = combined_pane_heights(310.0, 0.99);
        assert!((logic - 195.0).abs() < f32::EPSILON);
        assert!((scope - 105.0).abs() < f32::EPSILON);
    }

    #[test]
    fn default_resource_directory_contains_both_bitstreams() {
        let directory = PathBuf::from(default_resource_directory());
        assert!(has_required_fpga_resources(&directory));
    }

    #[test]
    fn every_logic_channel_has_a_distinct_default_color() {
        let colors = default_logic_channel_colors();
        for (index, color) in colors.iter().enumerate() {
            assert!(!colors[..index].contains(color));
        }
    }

    #[test]
    fn waveform_zoom_preserves_the_sample_under_the_pointer() {
        let mut viewport = WaveformViewport::default();

        viewport.zoom(50.0, 0.25);

        assert!(viewport.span < 1.0);
        assert!((viewport.start + viewport.span * 0.25 - 0.25).abs() < 0.000_001);
        assert!(viewport.sample_range(1_000).len() < 1_000);
    }

    #[test]
    fn waveform_zoom_out_is_clamped_to_the_full_capture() {
        let mut viewport = WaveformViewport::default();
        viewport.zoom(50.0, 0.75);

        viewport.zoom(-10_000.0, 0.75);

        assert_eq!(viewport, WaveformViewport::default());
        assert_eq!(viewport.sample_range(1_000), 0..1_000);
    }
}
