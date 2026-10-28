use dsview_core::{
    CaptureBatch, CaptureCoordinator, CaptureEvent, CaptureSession, CaptureSource, DeviceId,
};
use dsview_usb::{
    discover_supported_devices, prepare_device, DeviceModel, DiscoveredDevice, DsCopeCaptureConfig,
    DsCopeCaptureSource, DsLogicCaptureConfig, DsLogicCaptureSource, LinkStatus,
};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

const SAMPLE_RATE_HZ: u64 = 1_000_000;
const SAMPLE_COUNT: u64 = 1_000_000;
const MAX_WAVEFORM_SAMPLES: usize = 8_192;
const BACKGROUND: Color32 = Color32::from_rgb(12, 17, 25);
const PANEL: Color32 = Color32::from_rgb(20, 27, 38);
const GRID: Color32 = Color32::from_rgb(43, 55, 72);
const TEXT_MUTED: Color32 = Color32::from_rgb(145, 160, 180);
const ACCENT: Color32 = Color32::from_rgb(70, 210, 185);
const LOGIC_COLOR: Color32 = Color32::from_rgb(90, 220, 145);
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
}

struct CompletedCapture {
    stats: HashMap<DeviceId, DeviceStats>,
    waveforms: WaveformStore,
}

enum WorkerSuccess {
    Devices(Vec<DiscoveredDevice>),
    Capture(CaptureSession),
}

type WorkerResult = Result<WorkerSuccess, String>;

struct DsViewApp {
    resource_directory: String,
    devices: Vec<DeviceEntry>,
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
            resource_directory: "resources/fpga".to_owned(),
            devices: Vec::new(),
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
        let Some(active) = self.capture.take() else {
            return;
        };
        self.status = "Stopping capture…".to_owned();
        let stop_result = active.session.stop();
        self.completed_capture = Some(CompletedCapture {
            stats: active.stats,
            waveforms: active.waveforms,
        });
        match stop_result {
            Ok(()) => self.status = "Capture stopped".to_owned(),
            Err(error) => self.set_error(error.to_string()),
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
                self.capture = Some(ActiveCapture {
                    session,
                    stats: HashMap::new(),
                    stopped: HashSet::new(),
                    waveforms: WaveformStore::default(),
                });
                self.status = format!("Capturing from {count} instrument(s) at 1 MHz");
            }
            Err(error) => self.set_error(error),
        }
    }

    fn poll_capture(&mut self) {
        let mut finish_status = None;
        if let Some(active) = self.capture.as_mut() {
            loop {
                match active.session.try_recv() {
                    Ok(CaptureEvent::Frame(frame)) => {
                        let entry = active.stats.entry(frame.device_id).or_default();
                        entry.frames += 1;
                        entry.samples += frame.payload.sample_count() as u64;
                        entry.first_timestamp.get_or_insert(frame.captured_at);
                        entry.last_timestamp = Some(frame.captured_at);
                        active.waveforms.ingest(&frame.payload);
                    }
                    Ok(CaptureEvent::SourceFailed { device_id, error }) => {
                        active.session.request_stop();
                        finish_status = Some(Err(format!("{device_id}: {error}")));
                        break;
                    }
                    Ok(CaptureEvent::SourceStopped { device_id }) => {
                        active.stopped.insert(device_id);
                        if active.stopped.len() == active.session.descriptors().len() {
                            finish_status = Some(Ok("Capture complete".to_owned()));
                            break;
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finish_status = Some(Err("capture event stream disconnected".to_owned()));
                        break;
                    }
                }
            }
        }

        if let Some(result) = finish_status {
            if let Some(active) = self.capture.take() {
                let stop_result = active.session.stop();
                match (result, stop_result) {
                    (Ok(status), Ok(())) => self.status = status,
                    (Err(error), Ok(())) => self.set_error(error),
                    (Ok(_), Err(error)) => self.set_error(error.to_string()),
                    (Err(primary), Err(cleanup)) => {
                        self.set_error(format!(
                            "{primary}; additionally failed to stop: {cleanup}"
                        ));
                    }
                }
                self.completed_capture = Some(CompletedCapture {
                    stats: active.stats,
                    waveforms: active.waveforms,
                });
            }
        }
    }

    fn set_error(&mut self, error: String) {
        self.status = "Operation failed".to_owned();
        self.last_error = Some(error);
    }

    fn is_busy(&self) -> bool {
        self.pending.is_some() || self.capture.is_some()
    }
}

impl eframe::App for DsViewApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();
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
                                        RichText::new(entry.device.profile.display_name).strong(),
                                    ),
                                );
                            });
                            ui.small(
                                RichText::new(entry.device.location.stable_id()).color(TEXT_MUTED),
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
                egui::ScrollArea::vertical().show(ui, |ui| {
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
                    waveform_card(
                        ui,
                        "DIGITAL",
                        "DSLogic · channels 0–15",
                        330.0,
                        |ui, rect| paint_logic(ui, rect, waveforms),
                    );
                    ui.add_space(14.0);
                    waveform_card(
                        ui,
                        "OSCILLOSCOPE",
                        "DSCope · CH0 / CH1",
                        280.0,
                        |ui, rect| paint_scope(ui, rect, waveforms),
                    );
                });
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

fn paint_logic(ui: &egui::Ui, rect: Rect, waveforms: &WaveformStore) {
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    if waveforms.logic_samples.is_empty() {
        paint_empty(painter, rect, "Waiting for logic samples");
        return;
    }

    let channel_count = usize::from(waveforms.logic_channels.min(16));
    let label_width = 38.0;
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + label_width, rect.top()),
        rect.right_bottom(),
    );
    let lane_height = plot.height() / channel_count.max(1) as f32;
    let sample_count = waveforms.logic_samples.len();
    let step = (sample_count as f32 / plot.width().max(1.0))
        .floor()
        .max(1.0) as usize;
    for channel in 0..channel_count {
        let lane_top = plot.top() + channel as f32 * lane_height;
        let high = lane_top + lane_height * 0.25;
        let low = lane_top + lane_height * 0.75;
        painter.text(
            Pos2::new(rect.left() + 4.0, lane_top + lane_height * 0.5),
            egui::Align2::LEFT_CENTER,
            format!("D{channel}"),
            FontId::monospace(10.0),
            TEXT_MUTED,
        );
        let mut points = Vec::new();
        let mut previous_y = None;
        for index in (0..sample_count).step_by(step) {
            let sample = waveforms.logic_samples[index];
            let y = if sample & (1_u32 << channel) != 0 {
                high
            } else {
                low
            };
            let x = plot.left()
                + index as f32 / sample_count.saturating_sub(1).max(1) as f32 * plot.width();
            if let Some(previous_y) = previous_y {
                points.push(Pos2::new(x, previous_y));
            }
            points.push(Pos2::new(x, y));
            previous_y = Some(y);
        }
        if points.len() > 1 {
            painter.add(egui::Shape::line(points, Stroke::new(1.2_f32, LOGIC_COLOR)));
        }
    }
    paint_time_span(painter, rect, sample_count, waveforms.logic_rate_hz);
}

fn paint_scope(ui: &egui::Ui, rect: Rect, waveforms: &WaveformStore) {
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    if waveforms.scope_channels.is_empty()
        || waveforms.scope_channels.iter().all(VecDeque::is_empty)
    {
        paint_empty(painter, rect, "Waiting for oscilloscope samples");
        return;
    }

    let mut minimum = f32::INFINITY;
    let mut maximum = f32::NEG_INFINITY;
    for value in waveforms.scope_channels.iter().flatten() {
        minimum = minimum.min(*value);
        maximum = maximum.max(*value);
    }
    let padding = ((maximum - minimum) * 0.12).max(0.02);
    minimum -= padding;
    maximum += padding;
    let voltage_span = (maximum - minimum).max(f32::EPSILON);
    let sample_count = waveforms.scope_channels.first().map_or(0, VecDeque::len);
    let step = (sample_count as f32 / rect.width().max(1.0))
        .floor()
        .max(1.0) as usize;

    for (channel_index, channel) in waveforms.scope_channels.iter().enumerate() {
        let points = (0..channel.len())
            .step_by(step)
            .map(|index| {
                let x = rect.left()
                    + index as f32 / channel.len().saturating_sub(1).max(1) as f32 * rect.width();
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
    paint_time_span(painter, rect, sample_count, waveforms.scope_rate_hz);
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
}
