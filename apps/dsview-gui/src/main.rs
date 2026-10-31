#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod measurements;

use dsview_core::{
    CaptureBatch, CaptureCoordinator, CaptureEvent, CaptureSession, CaptureSource,
    DeviceDescriptor, DeviceId, InstrumentKind, ScopeBatch,
};
use dsview_recording::{CompletionStatus, RecordingReader, RecordingWriter};
use dsview_usb::{
    apply_dscope_configuration, auto_calibrate_dscope_with_progress, discover_supported_devices,
    prepare_device, read_dscope_calibration, write_dscope_calibration, DeviceModel,
    DiscoveredDevice, DsCopeBandwidthLimit, DsCopeCalibrationFrame, DsCopeCalibrationValues,
    DsCopeCaptureConfig, DsCopeCaptureSource, DsCopeChannelConfig, DsCopeCoupling,
    DsCopeOperationMode, DsCopeRuntimeCalibrationControl, DsCopeRuntimeTriggerControl,
    DsCopeTriggerConfig, DsCopeTriggerSlope, DsCopeTriggerSource, DsLogicCaptureConfig,
    DsLogicCaptureSource, DsLogicInputFilter, DsLogicOperationMode, DsLogicStopOption, LinkStatus,
    UsbSpeedClass, DSCOPE_SAMPLE_COUNTS, DSCOPE_SAMPLE_RATES_HZ, DSCOPE_TRIGGER_HOLDOFF_MAX_NS,
    DSCOPE_TRIGGER_LEVEL_CENTER, DSCOPE_TRIGGER_LEVEL_MAX, DSCOPE_TRIGGER_LEVEL_MIN,
    DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX, DSCOPE_TRIGGER_POSITION_MAX_PERCENT,
    DSCOPE_TRIGGER_POSITION_MIN_PERCENT, DSCOPE_VERTICAL_OFFSET_CENTER, DSCOPE_VERTICAL_OFFSET_MAX,
    DSCOPE_VERTICAL_OFFSET_MIN, DSCOPE_VOLTS_PER_DIV_MV, DSLOGIC_MAX_SAMPLE_COUNT,
    DSLOGIC_SAMPLE_ALIGNMENT, DSLOGIC_SAMPLE_RATES_HZ,
};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use image::ImageFormat;
use measurements::{
    calculate_scope_measurements, ScopeMeasurementResults, ScopeMeasurementSettings,
    SCOPE_MEASUREMENT_KINDS,
};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const SAMPLE_RATE_HZ: u64 = 1_000_000;
const SAMPLE_COUNT: u64 = 1_000_000;
const LOGIC_CHANNEL_COUNT: usize = 16;
// Keep the complete largest hardware capture in memory. Rendering is already
// decimated to the plot width, so retaining the samples does not increase the
// number of painted points. This also keeps the hardware trigger region in the
// waveform instead of displaying only the final tail of a capture.
const MAX_WAVEFORM_SAMPLES: usize = 2_000_000;
// Continuous Auto acquisition uses a software ring large enough for the
// selected OSC span. Bound its total sample storage across enabled channels.
const SCOPE_ROLLING_MEMORY_BUDGET_BYTES: usize = 128 * 1024 * 1024;
const WAVEFORM_STATUS_HEIGHT: f32 = 20.0;
const TIME_RULER_HEIGHT: f32 = 24.0;
const SCOPE_HORIZONTAL_DIVISIONS: usize = 10;
const SCOPE_VERTICAL_DIVISIONS: usize = 8;
const SCOPE_ADC_VERTICAL_DIVISIONS: f32 = 10.0;
const SCOPE_CONFIGURATION_DEBOUNCE: Duration = Duration::from_millis(350);
const TOOLTIP_DELAY_SECONDS: f32 = 0.1;
const DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS: f64 = 1.0e-3;
const TIMEBASE_STEPS_SECONDS: [f64; 30] = [
    2.0e-9, 5.0e-9, 10.0e-9, 20.0e-9, 50.0e-9, 100.0e-9, 200.0e-9, 500.0e-9, 1.0e-6, 2.0e-6,
    5.0e-6, 10.0e-6, 20.0e-6, 50.0e-6, 100.0e-6, 200.0e-6, 500.0e-6, 1.0e-3, 2.0e-3, 5.0e-3,
    10.0e-3, 20.0e-3, 50.0e-3, 100.0e-3, 200.0e-3, 500.0e-3, 1.0, 2.0, 5.0, 10.0,
];
const BACKGROUND: Color32 = Color32::from_rgb(12, 17, 25);
const PANEL: Color32 = Color32::from_rgb(20, 27, 38);
const GRID: Color32 = Color32::from_rgb(43, 55, 72);
const TEXT_MUTED: Color32 = Color32::from_rgb(145, 160, 180);
const ACCENT: Color32 = Color32::from_rgb(70, 210, 185);
const SCOPE_COLORS: [Color32; 2] = [
    Color32::from_rgb(255, 204, 74),
    Color32::from_rgb(99, 179, 255),
];
const MATH_COLOR: Color32 = Color32::from_rgb(213, 126, 255);
const FFT_LENGTHS: [usize; 5] = [1_024, 2_048, 4_096, 8_192, 16_384];
const SCOPE_MARKER_LIMIT: usize = 10;
const CHANNEL_NAME_MAX_CHARS: usize = 32;
const LOGIC_CHANNEL_NAME_MAX_CHARS: usize = 8;
const DEFAULT_LOGIC_CHANNEL_MARKER_OPACITY: u8 = 128;
static SCOPE_WAVEFORM_REVISION: AtomicU64 = AtomicU64::new(1);

fn toolbar_action_button(label: &str, color: Color32) -> egui::Button<'static> {
    egui::Button::new(label.to_owned())
        .fill(color.gamma_multiply(0.24))
        .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.85)))
}

fn empty_logic_channel_names() -> [String; LOGIC_CHANNEL_COUNT] {
    std::array::from_fn(|_| String::new())
}

fn empty_scope_channel_names() -> [String; 2] {
    std::array::from_fn(|_| String::new())
}

fn normalized_channel_name(name: &str) -> String {
    name.trim().chars().take(CHANNEL_NAME_MAX_CHARS).collect()
}

fn channel_label(prefix: &str, index: usize, name: &str) -> String {
    let identifier = format!("{prefix}{index}");
    let name = name.trim();
    if name.is_empty() {
        identifier
    } else {
        format!("{identifier} · {name}")
    }
}

fn scope_channel_label(index: usize, names: &[String; 2]) -> String {
    channel_label("CH", index, &names[index])
}

fn scope_channels_subtitle(names: &[String; 2]) -> String {
    format!(
        "DSCope · {} / {}",
        scope_channel_label(0, names),
        scope_channel_label(1, names)
    )
}

#[derive(Clone, Copy, Debug)]
struct PendingSnapshot {
    plot_rect: Rect,
    pixels_per_point: f32,
}

#[derive(Debug, Eq, PartialEq)]
struct SnapshotImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn crop_snapshot_image(
    screenshot: &egui::ColorImage,
    plot_rect: Rect,
    pixels_per_point: f32,
) -> Result<SnapshotImage, String> {
    if screenshot.size[0] == 0 || screenshot.size[1] == 0 {
        return Err("the captured image is empty".to_owned());
    }
    if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
        return Err("the display scale is invalid".to_owned());
    }

    let image_width = screenshot.size[0];
    let image_height = screenshot.size[1];
    let left = (plot_rect.left() * pixels_per_point).floor().max(0.0) as usize;
    let top = (plot_rect.top() * pixels_per_point).floor().max(0.0) as usize;
    let right = (plot_rect.right() * pixels_per_point)
        .ceil()
        .clamp(0.0, image_width as f32) as usize;
    let bottom = (plot_rect.bottom() * pixels_per_point)
        .ceil()
        .clamp(0.0, image_height as f32) as usize;
    let left = left.min(image_width);
    let top = top.min(image_height);
    if right <= left || bottom <= top {
        return Err("the waveform plot is outside the captured image".to_owned());
    }

    let width = right - left;
    let height = bottom - top;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in top..bottom {
        let row_start = y * image_width + left;
        let row_end = row_start + width;
        for pixel in &screenshot.pixels[row_start..row_end] {
            rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
        }
    }

    Ok(SnapshotImage {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

fn available_snapshot_path(directory: &Path) -> PathBuf {
    let default_path = directory.join("dsview-snapshot.png");
    if !default_path.exists() {
        return default_path;
    }
    for index in 2_u32.. {
        let path = directory.join(format!("dsview-snapshot-{index}.png"));
        if !path.exists() {
            return path;
        }
    }
    unreachable!("the snapshot file suffix cannot be exhausted")
}

fn save_snapshot_image(directory: &Path, snapshot: &SnapshotImage) -> Result<PathBuf, String> {
    let path = available_snapshot_path(directory);
    image::save_buffer_with_format(
        &path,
        &snapshot.rgba,
        snapshot.width,
        snapshot.height,
        image::ColorType::Rgba8,
        ImageFormat::Png,
    )
    .map_err(|error| format!("could not save {}: {error}", path.display()))?;
    Ok(path)
}

fn normalize_recording_path(mut path: PathBuf) -> PathBuf {
    let has_dsv_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("dsv"))
        .unwrap_or(false);
    if !has_dsv_extension {
        path.set_extension("dsv");
    }
    path
}

fn available_recording_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("capture");
    for index in 2_u32.. {
        let candidate = parent.join(format!("{stem}-{index}.dsv"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("the recording file suffix cannot be exhausted")
}

fn choose_recording_save_path(current_path: &str) -> Option<PathBuf> {
    let default_name = Path::new(current_path.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("capture.dsv");
    rfd::FileDialog::new()
        .set_title("Save complete DSView capture")
        .add_filter("DSView capture", &["dsv"])
        .set_file_name(default_name)
        .save_file()
        .map(normalize_recording_path)
        .map(available_recording_path)
}

fn choose_recording_to_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open DSView capture")
        .add_filter("DSView capture", &["dsv"])
        .pick_file()
}

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
struct ScopeChannelSettings {
    enabled: bool,
    volts_per_div_mv: u16,
    coupling: DsCopeCoupling,
    probe_attenuation: u8,
    vertical_offset: u16,
}

impl Default for ScopeChannelSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            volts_per_div_mv: 1_000,
            coupling: DsCopeCoupling::Dc,
            probe_attenuation: 1,
            vertical_offset: DSCOPE_VERTICAL_OFFSET_CENTER,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TriggerHoldoffUnit {
    Microseconds,
    Milliseconds,
    Seconds,
}

impl TriggerHoldoffUnit {
    fn label(self) -> &'static str {
        match self {
            Self::Microseconds => "µs",
            Self::Milliseconds => "ms",
            Self::Seconds => "s",
        }
    }

    fn nanoseconds(self) -> f64 {
        match self {
            Self::Microseconds => 1_000.0,
            Self::Milliseconds => 1_000_000.0,
            Self::Seconds => 1_000_000_000.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeTriggerSettings {
    position_percent: u8,
    source: DsCopeTriggerSource,
    auto_channel: u8,
    slope: DsCopeTriggerSlope,
    holdoff_ns: u64,
    holdoff_unit: TriggerHoldoffUnit,
    noise_sensitivity: u8,
    levels: [u8; 2],
}

impl Default for ScopeTriggerSettings {
    fn default() -> Self {
        Self::from_capture_config(DsCopeTriggerConfig::default())
    }
}

impl ScopeTriggerSettings {
    fn capture_config(self) -> Result<DsCopeTriggerConfig, String> {
        DsCopeTriggerConfig::new(self.source, self.slope)
            .with_position_percent(self.position_percent)
            .and_then(|trigger| trigger.with_auto_channel(self.auto_channel))
            .and_then(|trigger| trigger.with_holdoff_ns(self.holdoff_ns))
            .and_then(|trigger| trigger.with_noise_sensitivity(self.noise_sensitivity))
            .and_then(|trigger| trigger.with_levels(self.levels))
            .map_err(|error| error.to_string())
    }

    fn from_capture_config(config: DsCopeTriggerConfig) -> Self {
        let holdoff_unit = if config.holdoff_ns() >= 1_000_000_000 {
            TriggerHoldoffUnit::Seconds
        } else if config.holdoff_ns() >= 1_000_000 {
            TriggerHoldoffUnit::Milliseconds
        } else {
            TriggerHoldoffUnit::Microseconds
        };
        Self {
            position_percent: config.position_percent(),
            source: config.source(),
            auto_channel: config.auto_channel(),
            slope: config.slope(),
            holdoff_ns: config.holdoff_ns(),
            holdoff_unit,
            noise_sensitivity: config.noise_sensitivity(),
            levels: config.levels(),
        }
    }

    fn source_is_available(self, channels: [bool; 2]) -> bool {
        match self.source {
            DsCopeTriggerSource::Auto => channels[usize::from(self.auto_channel)],
            DsCopeTriggerSource::ChannelZero => channels[0],
            DsCopeTriggerSource::ChannelOne => channels[1],
            DsCopeTriggerSource::ChannelZeroAndOne => channels[0] && channels[1],
            DsCopeTriggerSource::ChannelZeroOrOne => channels[0] || channels[1],
        }
    }

    fn clamp_to_channels(&mut self, channels: [bool; 2]) {
        if !channels[usize::from(self.auto_channel)] {
            self.auto_channel = if channels[0] { 0 } else { 1 };
        }
        if !self.source_is_available(channels) {
            self.source = DsCopeTriggerSource::Auto;
        }
    }

    fn hardware_differs_from(self, other: Self) -> bool {
        self.position_percent != other.position_percent
            || self.source != other.source
            || self.auto_channel != other.auto_channel
            || self.slope != other.slope
            || self.holdoff_ns != other.holdoff_ns
            || self.noise_sensitivity != other.noise_sensitivity
            || self.levels != other.levels
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScopeMathOperation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl ScopeMathOperation {
    fn label(self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Multiply => "Multiply",
            Self::Divide => "Divide",
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "−",
            Self::Multiply => "×",
            Self::Divide => "÷",
        }
    }

    fn apply(self, left: f32, right: f32) -> Option<f32> {
        let value = match self {
            Self::Add => left + right,
            Self::Subtract => left - right,
            Self::Multiply => left * right,
            Self::Divide if right.abs() > f32::EPSILON => left / right,
            Self::Divide => return None,
        };
        value.is_finite().then_some(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeMathSettings {
    enabled: bool,
    operation: ScopeMathOperation,
    left_channel: usize,
    right_channel: usize,
}

impl Default for ScopeMathSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            operation: ScopeMathOperation::Add,
            left_channel: 0,
            right_channel: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScopeFftWindow {
    Rectangular,
    Hann,
    Hamming,
    Blackman,
    FlatTop,
}

impl ScopeFftWindow {
    fn label(self) -> &'static str {
        match self {
            Self::Rectangular => "Rectangular",
            Self::Hann => "Hann",
            Self::Hamming => "Hamming",
            Self::Blackman => "Blackman",
            Self::FlatTop => "Flat Top",
        }
    }

    fn coefficient(self, index: usize, length: usize) -> f64 {
        if self == Self::Rectangular || length <= 1 {
            return 1.0;
        }
        let phase = std::f64::consts::TAU * index as f64 / (length - 1) as f64;
        match self {
            Self::Rectangular => 1.0,
            Self::Hann => 0.5 * (1.0 - phase.cos()),
            Self::Hamming => 0.54 - 0.46 * phase.cos(),
            Self::Blackman => 0.426_59 - 0.496_56 * phase.cos() + 0.076_849 * (2.0 * phase).cos(),
            Self::FlatTop => {
                1.0 - 1.93 * phase.cos() + 1.29 * (2.0 * phase).cos() - 0.388 * (3.0 * phase).cos()
                    + 0.028 * (4.0 * phase).cos()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeFftSettings {
    enabled: bool,
    channel: usize,
    length: usize,
    window: ScopeFftWindow,
}

impl Default for ScopeFftSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: 0,
            length: 16_384,
            window: ScopeFftWindow::Hann,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeLissajousSettings {
    enabled: bool,
    x_channel: usize,
    y_channel: usize,
}

impl Default for ScopeLissajousSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            x_channel: 0,
            y_channel: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ScopeFunctionSettings {
    math: ScopeMathSettings,
    fft: ScopeFftSettings,
    lissajous: ScopeLissajousSettings,
    measurements: ScopeMeasurementSettings,
}

impl ScopeFunctionSettings {
    fn clamp_to_channels(&mut self, channels: [bool; 2]) {
        let fallback = if channels[0] { 0 } else { 1 };
        if !channels[self.math.left_channel] {
            self.math.left_channel = fallback;
        }
        if !channels[self.math.right_channel] {
            self.math.right_channel = fallback;
        }
        if !channels[self.fft.channel] {
            self.fft.channel = fallback;
        }
        if !channels[self.lissajous.x_channel] {
            self.lissajous.x_channel = fallback;
        }
        if !channels[self.lissajous.y_channel] {
            self.lissajous.y_channel = fallback;
        }
        if channels.into_iter().filter(|enabled| *enabled).count() < 2 {
            self.lissajous.enabled = false;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeChannelMarkerSettings {
    horizontal_count: usize,
    horizontal_positions: [f32; SCOPE_MARKER_LIMIT],
    horizontal_comparisons: [Option<usize>; SCOPE_MARKER_LIMIT],
    horizontal_colors: [Color32; SCOPE_MARKER_LIMIT],
    vertical_count: usize,
    vertical_positions: [f32; SCOPE_MARKER_LIMIT],
    vertical_comparisons: [Option<usize>; SCOPE_MARKER_LIMIT],
    vertical_colors: [Color32; SCOPE_MARKER_LIMIT],
}

impl ScopeChannelMarkerSettings {
    fn new(
        channel: usize,
        horizontal_positions: [f32; SCOPE_MARKER_LIMIT],
        vertical_positions: [f32; SCOPE_MARKER_LIMIT],
    ) -> Self {
        let colors = std::array::from_fn(|index| scope_marker_color(channel, index));
        Self {
            horizontal_count: 0,
            horizontal_positions,
            horizontal_comparisons: [None; SCOPE_MARKER_LIMIT],
            horizontal_colors: colors,
            vertical_count: 0,
            vertical_positions,
            vertical_comparisons: [None; SCOPE_MARKER_LIMIT],
            vertical_colors: colors,
        }
    }

    fn count(self, kind: ScopeMarkerKind) -> usize {
        match kind {
            ScopeMarkerKind::Time => self.horizontal_count,
            ScopeMarkerKind::Voltage => self.vertical_count,
        }
        .min(SCOPE_MARKER_LIMIT)
    }

    fn total_count(self) -> usize {
        self.count(ScopeMarkerKind::Time) + self.count(ScopeMarkerKind::Voltage)
    }

    fn marker(self, kind: ScopeMarkerKind, index: usize) -> Option<(f32, Option<usize>, Color32)> {
        (index < self.count(kind)).then(|| match kind {
            ScopeMarkerKind::Time => (
                self.horizontal_positions[index],
                self.horizontal_comparisons[index],
                self.horizontal_colors[index],
            ),
            ScopeMarkerKind::Voltage => (
                self.vertical_positions[index],
                self.vertical_comparisons[index],
                self.vertical_colors[index],
            ),
        })
    }

    fn set_marker(
        &mut self,
        kind: ScopeMarkerKind,
        index: usize,
        position: f32,
        comparison: Option<usize>,
        color: Color32,
    ) -> bool {
        if index >= SCOPE_MARKER_LIMIT {
            return false;
        }
        match kind {
            ScopeMarkerKind::Time => {
                self.horizontal_positions[index] = position.clamp(0.0, 1.0);
                self.horizontal_comparisons[index] = comparison;
                self.horizontal_colors[index] = color;
            }
            ScopeMarkerKind::Voltage => {
                self.vertical_positions[index] = position.clamp(0.0, 1.0);
                self.vertical_comparisons[index] = comparison;
                self.vertical_colors[index] = color;
            }
        }
        true
    }

    fn add_marker(
        &mut self,
        kind: ScopeMarkerKind,
        position: f32,
        comparison: Option<usize>,
        color: Color32,
    ) -> bool {
        let index = self.count(kind);
        if self.total_count() >= SCOPE_MARKER_LIMIT
            || index >= SCOPE_MARKER_LIMIT
            || !self.set_marker(kind, index, position, comparison, color)
        {
            return false;
        }
        match kind {
            ScopeMarkerKind::Time => self.horizontal_count += 1,
            ScopeMarkerKind::Voltage => self.vertical_count += 1,
        }
        true
    }

    fn remove_marker(&mut self, kind: ScopeMarkerKind, index: usize) -> bool {
        let count = self.count(kind);
        if index >= count {
            return false;
        }
        match kind {
            ScopeMarkerKind::Time => {
                self.horizontal_positions
                    .copy_within(index + 1..count, index);
                self.horizontal_comparisons
                    .copy_within(index + 1..count, index);
                self.horizontal_colors.copy_within(index + 1..count, index);
                self.horizontal_count -= 1;
                remap_marker_comparisons(
                    &mut self.horizontal_comparisons[..self.horizontal_count],
                    index,
                );
            }
            ScopeMarkerKind::Voltage => {
                self.vertical_positions.copy_within(index + 1..count, index);
                self.vertical_comparisons
                    .copy_within(index + 1..count, index);
                self.vertical_colors.copy_within(index + 1..count, index);
                self.vertical_count -= 1;
                remap_marker_comparisons(
                    &mut self.vertical_comparisons[..self.vertical_count],
                    index,
                );
            }
        }
        true
    }
}

fn remap_marker_comparisons(comparisons: &mut [Option<usize>], removed: usize) {
    for comparison in comparisons {
        *comparison = match *comparison {
            Some(index) if index == removed => None,
            Some(index) if index > removed => Some(index - 1),
            comparison => comparison,
        };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScopeMarkerKind {
    Time,
    Voltage,
}

impl ScopeMarkerKind {
    fn label(self) -> &'static str {
        match self {
            Self::Time => "Time (Δt)",
            Self::Voltage => "Amplitude (ΔV)",
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            Self::Time => "T",
            Self::Voltage => "V",
        }
    }
}

fn scope_marker_default_position(kind: ScopeMarkerKind, viewport: WaveformViewport) -> f32 {
    match kind {
        ScopeMarkerKind::Time => viewport.domain_fraction_at_screen(0.5).clamp(0.0, 1.0),
        ScopeMarkerKind::Voltage => 0.5,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeMarkerKey {
    channel: usize,
    kind: ScopeMarkerKind,
    index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeMarkerEditor {
    original: Option<ScopeMarkerKey>,
    channel: usize,
    kind: ScopeMarkerKind,
    position: f32,
    comparison: Option<usize>,
    color: Color32,
}

impl ScopeMarkerEditor {
    fn for_new(
        settings: ScopeMarkerSettings,
        viewport: WaveformViewport,
        channel: usize,
        kind: ScopeMarkerKind,
    ) -> Self {
        let channel_settings = settings.channels[channel];
        let index = channel_settings.count(kind);
        let color = match kind {
            ScopeMarkerKind::Time => {
                channel_settings.horizontal_colors[index.min(SCOPE_MARKER_LIMIT - 1)]
            }
            ScopeMarkerKind::Voltage => {
                channel_settings.vertical_colors[index.min(SCOPE_MARKER_LIMIT - 1)]
            }
        };
        Self {
            original: None,
            channel,
            kind,
            position: scope_marker_default_position(kind, viewport),
            comparison: None,
            color,
        }
    }

    fn for_existing(settings: ScopeMarkerSettings, key: ScopeMarkerKey) -> Option<Self> {
        let (position, comparison, color) =
            settings.channels[key.channel].marker(key.kind, key.index)?;
        Some(Self {
            original: Some(key),
            channel: key.channel,
            kind: key.kind,
            position,
            comparison,
            color,
        })
    }

    fn save(self, settings: &mut ScopeMarkerSettings) -> bool {
        let comparison = self
            .comparison
            .filter(|index| *index < settings.channels[self.channel].count(self.kind));
        match self.original {
            Some(key) if key.channel == self.channel && key.kind == self.kind => {
                settings.channels[self.channel].set_marker(
                    self.kind,
                    key.index,
                    self.position,
                    comparison.filter(|index| *index != key.index),
                    self.color,
                )
            }
            Some(key) => {
                let target_has_room = key.channel == self.channel
                    || settings.channels[self.channel].total_count() < SCOPE_MARKER_LIMIT;
                if !target_has_room
                    || settings.channels[self.channel].count(self.kind) >= SCOPE_MARKER_LIMIT
                {
                    return false;
                }
                if !settings.channels[key.channel].remove_marker(key.kind, key.index) {
                    return false;
                }
                settings.channels[self.channel].add_marker(
                    self.kind,
                    self.position,
                    comparison,
                    self.color,
                )
            }
            None => settings.channels[self.channel].add_marker(
                self.kind,
                self.position,
                comparison,
                self.color,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeMarkerSettings {
    channels: [ScopeChannelMarkerSettings; 2],
}

impl Default for ScopeMarkerSettings {
    fn default() -> Self {
        Self {
            channels: [
                ScopeChannelMarkerSettings::new(
                    0,
                    [0.05, 0.15, 0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85, 0.95],
                    [0.05, 0.15, 0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85, 0.95],
                ),
                ScopeChannelMarkerSettings::new(
                    1,
                    [0.10, 0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80, 0.90, 1.00],
                    [0.10, 0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80, 0.90, 1.00],
                ),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum ScopeQuickTimeMeasurement {
    #[default]
    Idle,
    AwaitingSecond {
        first_position: f32,
    },
    Complete {
        first_position: f32,
        second_position: f32,
    },
}

impl ScopeQuickTimeMeasurement {
    fn start(&mut self, position: f32) {
        *self = Self::AwaitingSecond {
            first_position: position.clamp(0.0, 1.0),
        };
    }

    fn finish(&mut self, position: f32) {
        if let Self::AwaitingSecond { first_position } = *self {
            *self = Self::Complete {
                first_position,
                second_position: position.clamp(0.0, 1.0),
            };
        }
    }

    fn clear(&mut self) {
        *self = Self::Idle;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeCaptureSettings {
    sample_rate_hz: u64,
    sample_count: u64,
    channels: [ScopeChannelSettings; 2],
    trigger: ScopeTriggerSettings,
    operation_mode: DsCopeOperationMode,
    bandwidth_limit: DsCopeBandwidthLimit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LogicCaptureSettings {
    sample_rate_hz: u64,
    sample_count: u64,
    enabled_channels: [bool; LOGIC_CHANNEL_COUNT],
    maximum_heights: [u8; LOGIC_CHANNEL_COUNT],
    operation_mode: DsLogicOperationMode,
    stop_option: DsLogicStopOption,
    threshold_voltage: f32,
    input_filter: DsLogicInputFilter,
    rle_enabled: bool,
    external_clock: bool,
    falling_clock_edge: bool,
}

impl Default for LogicCaptureSettings {
    fn default() -> Self {
        Self {
            sample_rate_hz: SAMPLE_RATE_HZ,
            sample_count: SAMPLE_COUNT,
            enabled_channels: [true; LOGIC_CHANNEL_COUNT],
            maximum_heights: [1; LOGIC_CHANNEL_COUNT],
            operation_mode: DsLogicOperationMode::Stream,
            stop_option: DsLogicStopOption::UploadCapturedData,
            threshold_voltage: 1.0,
            input_filter: DsLogicInputFilter::None,
            rle_enabled: false,
            external_clock: false,
            falling_clock_edge: false,
        }
    }
}

impl LogicCaptureSettings {
    fn capture_config(self) -> Result<DsLogicCaptureConfig, String> {
        let stream_mode = self.operation_mode == DsLogicOperationMode::Stream;
        let internal_test = self.operation_mode == DsLogicOperationMode::InternalTest;
        DsLogicCaptureConfig::new(
            if internal_test {
                100_000_000
            } else {
                self.sample_rate_hz
            },
            self.sample_count,
        )
        .map_err(|error| error.to_string())?
        .with_threshold_voltage(self.threshold_voltage)
        .map_err(|error| error.to_string())
        .map(|config| {
            config
                .with_operation_mode(self.operation_mode)
                .with_stop_option(if stream_mode {
                    DsLogicStopOption::StopImmediately
                } else {
                    self.stop_option
                })
                .with_input_filter(self.input_filter)
                .with_rle_enabled(self.rle_enabled && !stream_mode && !internal_test)
                .with_external_clock(self.external_clock)
                .with_falling_clock_edge(self.falling_clock_edge)
        })
        .and_then(|config| {
            config
                .with_enabled_channels(if internal_test {
                    [true; LOGIC_CHANNEL_COUNT]
                } else {
                    self.enabled_channels
                })
                .map_err(|error| error.to_string())
        })
    }

    fn maximum_sample_rate_hz(self, speed: UsbSpeedClass) -> u64 {
        let mut enabled_channels = self.enabled_channels;
        if !enabled_channels.iter().any(|enabled| *enabled) {
            enabled_channels[0] = true;
        }
        DsLogicCaptureConfig::new(SAMPLE_RATE_HZ, SAMPLE_COUNT)
            .expect("default logic capture configuration is valid")
            .with_operation_mode(self.operation_mode)
            .with_enabled_channels(enabled_channels)
            .expect("at least one logic channel is enabled")
            .maximum_sample_rate_hz(speed)
    }
}

fn aligned_logic_sample_count(duration_seconds: f64, sample_rate_hz: u64) -> Option<u64> {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 || sample_rate_hz == 0 {
        return None;
    }
    let requested = (duration_seconds * sample_rate_hz as f64).ceil();
    if requested < DSLOGIC_SAMPLE_ALIGNMENT as f64 || requested > u64::MAX as f64 {
        return None;
    }
    let aligned = (requested as u64).div_ceil(DSLOGIC_SAMPLE_ALIGNMENT) * DSLOGIC_SAMPLE_ALIGNMENT;
    (aligned <= DSLOGIC_MAX_SAMPLE_COUNT && aligned <= MAX_WAVEFORM_SAMPLES as u64)
        .then_some(aligned)
}

fn logic_capture_duration_options(sample_rate_hz: u64) -> Vec<(f64, u64)> {
    let mut options = Vec::new();
    for duration_seconds in TIMEBASE_STEPS_SECONDS {
        let Some(sample_count) = aligned_logic_sample_count(duration_seconds, sample_rate_hz)
        else {
            continue;
        };
        if options
            .last()
            .is_some_and(|(_, previous_count)| *previous_count == sample_count)
        {
            continue;
        }
        options.push((duration_seconds, sample_count));
    }
    options
}

fn logic_capture_duration_seconds(settings: LogicCaptureSettings) -> f64 {
    logic_capture_duration_options(settings.sample_rate_hz)
        .into_iter()
        .find_map(|(duration, sample_count)| {
            (sample_count == settings.sample_count).then_some(duration)
        })
        .unwrap_or(settings.sample_count as f64 / settings.sample_rate_hz as f64)
}

fn logic_capture_duration_label(settings: LogicCaptureSettings) -> String {
    format_duration(Duration::from_secs_f64(logic_capture_duration_seconds(
        settings,
    )))
}

fn set_logic_sample_rate_preserving_duration(
    settings: &mut LogicCaptureSettings,
    sample_rate_hz: u64,
) {
    if settings.sample_rate_hz == sample_rate_hz {
        return;
    }
    let duration_seconds = logic_capture_duration_seconds(*settings);
    settings.sample_rate_hz = sample_rate_hz;
    settings.sample_count = aligned_logic_sample_count(duration_seconds, sample_rate_hz)
        .unwrap_or_else(|| {
            if duration_seconds * (sample_rate_hz as f64) < DSLOGIC_SAMPLE_ALIGNMENT as f64 {
                DSLOGIC_SAMPLE_ALIGNMENT
            } else {
                let maximum = DSLOGIC_MAX_SAMPLE_COUNT.min(MAX_WAVEFORM_SAMPLES as u64);
                maximum / DSLOGIC_SAMPLE_ALIGNMENT * DSLOGIC_SAMPLE_ALIGNMENT
            }
        });
}

impl Default for ScopeCaptureSettings {
    fn default() -> Self {
        Self {
            sample_rate_hz: SAMPLE_RATE_HZ,
            sample_count: SAMPLE_COUNT,
            channels: [ScopeChannelSettings::default(); 2],
            trigger: ScopeTriggerSettings::default(),
            operation_mode: DsCopeOperationMode::Normal,
            bandwidth_limit: DsCopeBandwidthLimit::Full,
        }
    }
}

impl ScopeCaptureSettings {
    fn capture_config(self) -> Result<DsCopeCaptureConfig, String> {
        let mut channels = [DsCopeChannelConfig::default(); 2];
        for (destination, source) in channels.iter_mut().zip(self.channels) {
            // Keep the analog frontend centered. `ScopeChannelSettings` keeps
            // the independently movable display offset used by the plot.
            *destination = DsCopeChannelConfig::new(
                source.volts_per_div_mv,
                source.coupling,
                source.probe_attenuation,
            )
            .map_err(|error| error.to_string())?
            .with_enabled(source.enabled);
        }
        DsCopeCaptureConfig::with_channel_configs(self.sample_rate_hz, self.sample_count, channels)
            .map_err(|error| error.to_string())?
            .with_trigger_config(self.trigger.capture_config()?)
            .map_err(|error| error.to_string())
            .map(|config| config.with_options(self.operation_mode, self.bandwidth_limit))
    }

    fn enabled_channel_count(self) -> usize {
        self.channels
            .iter()
            .filter(|channel| channel.enabled)
            .count()
    }

    fn maximum_sample_rate_hz(self) -> u64 {
        if self.enabled_channel_count() == 1 {
            1_000_000_000
        } else {
            500_000_000
        }
    }

    fn maximum_sample_count(self) -> u64 {
        if self.enabled_channel_count() == 1 {
            2_000_000
        } else {
            1_000_000
        }
    }

    fn sample_rate_for_timebase(self, time_per_division_seconds: f64) -> u64 {
        if !time_per_division_seconds.is_finite() || time_per_division_seconds <= 0.0 {
            return self.sample_rate_hz;
        }
        let requested_rate = self.sample_count as f64
            / (time_per_division_seconds * SCOPE_HORIZONTAL_DIVISIONS as f64);
        let maximum_rate = self.maximum_sample_rate_hz();
        DSCOPE_SAMPLE_RATES_HZ
            .iter()
            .rev()
            .copied()
            .find(|rate| *rate <= maximum_rate && *rate as f64 <= requested_rate * (1.0 + 1.0e-9))
            .unwrap_or(DSCOPE_SAMPLE_RATES_HZ[0])
    }

    fn clamp_to_channel_limits(&mut self) {
        self.sample_rate_hz = self.sample_rate_hz.min(self.maximum_sample_rate_hz());
        self.sample_count = self.sample_count.min(self.maximum_sample_count());
        self.trigger
            .clamp_to_channels(self.channels.map(|channel| channel.enabled));
    }

    fn frontend_differs_from(self, other: Self) -> bool {
        self.sample_rate_hz != other.sample_rate_hz
            || self.operation_mode != other.operation_mode
            || self.bandwidth_limit != other.bandwidth_limit
            || self
                .channels
                .iter()
                .zip(other.channels)
                .any(|(left, right)| {
                    left.enabled != right.enabled
                        || left.volts_per_div_mv != right.volts_per_div_mv
                        || left.coupling != right.coupling
                })
            || self.trigger.hardware_differs_from(other.trigger)
    }

    fn runtime_frontend_differs_from(self, other: Self) -> bool {
        self.channels
            .iter()
            .zip(other.channels)
            .any(|(left, right)| {
                left.enabled != right.enabled || left.volts_per_div_mv != right.volts_per_div_mv
            })
    }

    fn from_capture_config(config: DsCopeCaptureConfig) -> Self {
        Self {
            sample_rate_hz: config.sample_rate_hz(),
            sample_count: config.sample_count(),
            channels: config.channels().map(|channel| ScopeChannelSettings {
                enabled: channel.enabled(),
                volts_per_div_mv: channel.volts_per_div_mv(),
                coupling: channel.coupling(),
                probe_attenuation: channel.probe_attenuation(),
                vertical_offset: channel.vertical_offset(),
            }),
            trigger: ScopeTriggerSettings::from_capture_config(config.trigger()),
            operation_mode: config.operation_mode(),
            bandwidth_limit: config.bandwidth_limit(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScopeCalibrationAction {
    Auto,
    Manual,
}

struct ManualCalibrationEditor {
    values: DsCopeCalibrationValues,
    range: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureSettingsTab {
    DsLogic,
    DsCope,
}

impl CaptureSettingsTab {
    fn label(self) -> &'static str {
        match self {
            Self::DsLogic => "DSLogic",
            Self::DsCope => "DSCope",
        }
    }
}

fn capture_settings_tabs(
    has_logic_analyzer: bool,
    has_oscilloscope: bool,
) -> Vec<CaptureSettingsTab> {
    let mut tabs = Vec::with_capacity(2);
    if has_logic_analyzer {
        tabs.push(CaptureSettingsTab::DsLogic);
    }
    if has_oscilloscope {
        tabs.push(CaptureSettingsTab::DsCope);
    }
    tabs
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureTarget {
    LogicAnalyzer,
    Oscilloscope,
    Combined,
}

impl CaptureTarget {
    fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "Logic Analyzer",
            Self::Oscilloscope => "Oscilloscope",
            Self::Combined => "Combined",
        }
    }

    fn includes_model(self, model: DeviceModel) -> bool {
        match self {
            Self::LogicAnalyzer => model == DeviceModel::DsLogicU3Pro16,
            Self::Oscilloscope => model == DeviceModel::DsCopeU3P100,
            Self::Combined => true,
        }
    }

    fn is_available(self, has_logic_analyzer: bool, has_oscilloscope: bool) -> bool {
        match self {
            Self::LogicAnalyzer => has_logic_analyzer,
            Self::Oscilloscope => has_oscilloscope,
            Self::Combined => has_logic_analyzer && has_oscilloscope,
        }
    }

    fn includes_oscilloscope(self) -> bool {
        matches!(self, Self::Oscilloscope | Self::Combined)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaveformTab {
    LogicAnalyzer,
    Oscilloscope,
    Lissajous,
    Combined,
}

impl WaveformTab {
    fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "Logic Analyzer",
            Self::Oscilloscope => "Oscilloscope",
            Self::Lissajous => "Lissajous",
            Self::Combined => "Combined",
        }
    }

    fn capture_target(self) -> CaptureTarget {
        match self {
            Self::LogicAnalyzer => CaptureTarget::LogicAnalyzer,
            Self::Oscilloscope | Self::Lissajous => CaptureTarget::Oscilloscope,
            Self::Combined => CaptureTarget::Combined,
        }
    }

    fn capture_settings_tab(self) -> Option<CaptureSettingsTab> {
        match self {
            Self::LogicAnalyzer => Some(CaptureSettingsTab::DsLogic),
            Self::Oscilloscope | Self::Lissajous => Some(CaptureSettingsTab::DsCope),
            Self::Combined => None,
        }
    }
}

fn waveform_tabs(
    has_logic_analyzer: bool,
    has_oscilloscope: bool,
    show_lissajous: bool,
) -> Vec<WaveformTab> {
    let mut tabs = Vec::with_capacity(4);
    if has_logic_analyzer {
        tabs.push(WaveformTab::LogicAnalyzer);
    }
    if has_oscilloscope {
        tabs.push(WaveformTab::Oscilloscope);
    }
    if has_oscilloscope && show_lissajous {
        tabs.push(WaveformTab::Lissajous);
    }
    if has_logic_analyzer && has_oscilloscope {
        tabs.push(WaveformTab::Combined);
    }
    tabs
}

fn scope_tools_panel_is_visible(active_tab: Option<WaveformTab>) -> bool {
    matches!(
        active_tab,
        Some(WaveformTab::Oscilloscope) | Some(WaveformTab::Lissajous)
    )
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

fn default_logic_channel_marker_opacities() -> [u8; LOGIC_CHANNEL_COUNT] {
    [DEFAULT_LOGIC_CHANNEL_MARKER_OPACITY; LOGIC_CHANNEL_COUNT]
}

#[derive(Clone, Debug, PartialEq)]
struct LogicChannelSettingsDraft {
    enabled: bool,
    maximum_height: u8,
    name: String,
    color: Color32,
    marker_opacity: u8,
}

fn default_logic_channel_settings_drafts() -> [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT] {
    let colors = default_logic_channel_colors();
    std::array::from_fn(|channel| LogicChannelSettingsDraft {
        enabled: true,
        maximum_height: 1,
        name: String::new(),
        color: colors[channel],
        marker_opacity: DEFAULT_LOGIC_CHANNEL_MARKER_OPACITY,
    })
}

struct WaveformStore {
    logic_samples: VecDeque<u32>,
    logic_channels: u8,
    logic_rate_hz: u64,
    logic_sample_offset: u64,
    logic_capacity: usize,
    scope_channels: Vec<VecDeque<f32>>,
    scope_rate_hz: u64,
    scope_sample_offset: u64,
    scope_trigger_sample_index: Option<u64>,
    scope_capture_boundaries: VecDeque<u64>,
    scope_capacity: usize,
    scope_rolling: bool,
    scope_revision: u64,
}

impl Default for WaveformStore {
    fn default() -> Self {
        Self {
            logic_samples: VecDeque::new(),
            logic_channels: 0,
            logic_rate_hz: 0,
            logic_sample_offset: 0,
            logic_capacity: MAX_WAVEFORM_SAMPLES,
            scope_channels: Vec::new(),
            scope_rate_hz: 0,
            scope_sample_offset: 0,
            scope_trigger_sample_index: None,
            scope_capture_boundaries: VecDeque::new(),
            scope_capacity: MAX_WAVEFORM_SAMPLES,
            scope_rolling: false,
            scope_revision: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeMeasurementCacheKey {
    waveform_revision: u64,
    channel: usize,
    range_start: usize,
    range_end: usize,
    sample_rate_hz: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ScopeMeasurementCache {
    key: Option<ScopeMeasurementCacheKey>,
    results: ScopeMeasurementResults,
}

impl ScopeMeasurementCache {
    fn results(
        &mut self,
        waveforms: &WaveformStore,
        channel: usize,
        visible_range: Range<usize>,
        sample_rate_hz: u64,
    ) -> ScopeMeasurementResults {
        let key = ScopeMeasurementCacheKey {
            waveform_revision: waveforms.scope_revision,
            channel,
            range_start: visible_range.start,
            range_end: visible_range.end,
            sample_rate_hz,
        };
        if self.key != Some(key) {
            self.results = waveforms
                .scope_channels
                .get(channel)
                .map(|samples| calculate_scope_measurements(samples, visible_range, sample_rate_hz))
                .unwrap_or_default();
            self.key = Some(key);
        }
        self.results
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcquisitionMode {
    Single,
    Repetitive,
    Loop,
    Run,
}

impl AcquisitionMode {
    fn runs_continuously(self) -> bool {
        matches!(self, Self::Repetitive | Self::Loop | Self::Run)
    }

    fn logic_label(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::Repetitive => "Repetitive",
            Self::Loop => "Loop",
            Self::Run => "Run",
        }
    }

    fn rearms_on_completion(self) -> bool {
        matches!(self, Self::Repetitive | Self::Run)
    }
}

fn repetitive_interval(operation_mode: DsLogicOperationMode, configured_seconds: f32) -> Duration {
    if operation_mode == DsLogicOperationMode::Stream {
        return Duration::from_millis(100);
    }
    let tenths = (configured_seconds.clamp(0.1, 10.0) * 10.0).round() as u64;
    Duration::from_millis(tenths * 100)
}

fn should_rearm_acquisition(
    mode: AcquisitionMode,
    run_requested: bool,
    completion: CompletionStatus,
    failed: bool,
) -> bool {
    mode.rearms_on_completion()
        && run_requested
        && completion == CompletionStatus::Complete
        && !failed
}

fn uses_rolling_scope_buffer(
    mode: AcquisitionMode,
    has_scope: bool,
    trigger_source: DsCopeTriggerSource,
) -> bool {
    mode.runs_continuously() && has_scope && trigger_source == DsCopeTriggerSource::Auto
}

fn scope_rolling_sample_capacity(
    config: DsCopeCaptureConfig,
    time_per_division_seconds: Option<f64>,
) -> usize {
    let fallback = config.sample_count() as usize;
    let Some(time_per_division_seconds) =
        time_per_division_seconds.filter(|seconds| seconds.is_finite() && *seconds > 0.0)
    else {
        return fallback;
    };
    let maximum = SCOPE_ROLLING_MEMORY_BUDGET_BYTES
        / config.enabled_channel_count().max(1)
        / std::mem::size_of::<f32>();
    let visible_samples = (time_per_division_seconds
        * SCOPE_HORIZONTAL_DIVISIONS as f64
        * config.sample_rate_hz() as f64)
        .ceil();
    visible_samples.clamp(1.0, maximum as f64) as usize
}

fn scope_trigger_controls_are_enabled(
    background_operation_pending: bool,
    capture_run_requested: bool,
    selected_scope_device: bool,
    active_capture_supports_runtime_trigger: Option<bool>,
) -> bool {
    if capture_run_requested && selected_scope_device {
        return true;
    }
    !background_operation_pending && active_capture_supports_runtime_trigger.unwrap_or(true)
}

fn scope_frontend_controls_are_enabled(
    background_operation_pending: bool,
    capture_run_requested: bool,
    selected_scope_device: bool,
) -> bool {
    !background_operation_pending || (capture_run_requested && selected_scope_device)
}

fn manual_calibration_control_is_enabled(
    background_operation_pending: bool,
    selected_scope_device: bool,
    editor_open: bool,
    active_mode: Option<AcquisitionMode>,
) -> bool {
    !background_operation_pending
        && selected_scope_device
        && !editor_open
        && active_mode.map_or(true, AcquisitionMode::runs_continuously)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WaveformViewport {
    start: f32,
    span: f32,
    anchor_locked: bool,
}

impl Default for WaveformViewport {
    fn default() -> Self {
        Self {
            start: 0.0,
            span: 1.0,
            anchor_locked: true,
        }
    }
}

impl WaveformViewport {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn time_per_division(self, sample_count: usize, sample_rate_hz: u64) -> Option<f64> {
        if sample_count == 0 || sample_rate_hz == 0 {
            return None;
        }
        Some(
            self.span as f64 * sample_count as f64
                / sample_rate_hz as f64
                / SCOPE_HORIZONTAL_DIVISIONS as f64,
        )
    }

    fn step_timebase(
        &mut self,
        scroll_delta: f32,
        sample_count: usize,
        sample_rate_hz: u64,
        cursor_fraction: f32,
    ) -> bool {
        let cursor_fraction = cursor_fraction.clamp(0.0, 1.0);
        let sample_fraction = self.start + cursor_fraction * self.span;
        let changed = self.step_timebase_at_sample(
            scroll_delta,
            sample_count,
            sample_rate_hz,
            sample_fraction,
            cursor_fraction,
        );
        if changed {
            self.anchor_locked = false;
        }
        changed
    }

    fn sample_range(self, sample_count: usize) -> Range<usize> {
        if sample_count == 0 {
            return 0..0;
        }
        let visible_start = self.start.max(0.0);
        let visible_end = (self.start + self.span).min(1.0);
        if visible_start >= visible_end {
            return 0..0;
        }
        let start = ((visible_start * sample_count as f32).floor() as usize).min(sample_count - 1);
        let mut end = (visible_end * sample_count as f32).ceil() as usize;
        end = end.clamp(start + 1, sample_count);
        start..end
    }

    fn sample_range_in_domain(
        self,
        available_sample_count: usize,
        domain_sample_count: usize,
    ) -> Range<usize> {
        if available_sample_count == 0 || domain_sample_count == 0 {
            return 0..0;
        }
        let visible_start = self.start.max(0.0);
        let visible_end = (self.start + self.span).min(1.0);
        if visible_start >= visible_end {
            return 0..0;
        }
        let start = ((visible_start * domain_sample_count as f32).floor() as usize)
            .min(available_sample_count);
        let end = ((visible_end * domain_sample_count as f32).ceil() as usize)
            .min(available_sample_count);
        start..end.max(start)
    }

    fn zoom_factor(self) -> f32 {
        1.0 / self.span
    }

    fn domain_fraction_at_screen(self, screen_fraction: f32) -> f32 {
        self.start + screen_fraction * self.span
    }

    fn screen_fraction_at_domain(self, domain_fraction: f32) -> f32 {
        (domain_fraction - self.start) / self.span.max(f32::EPSILON)
    }

    fn move_screen_anchor(&mut self, old_fraction: f32, new_fraction: f32) {
        let sample_anchor = self.start + old_fraction.clamp(0.0, 1.0) * self.span;
        self.start = sample_anchor - new_fraction.clamp(0.0, 1.0) * self.span;
        self.anchor_locked = true;
    }

    fn align_sample_to_screen(&mut self, sample_fraction: f32, screen_fraction: f32) {
        self.start =
            self.clamp_start(sample_fraction - screen_fraction.clamp(0.0, 1.0) * self.span);
    }

    fn align_trigger_to_screen(&mut self, sample_fraction: f32, screen_fraction: f32) {
        self.start = sample_fraction - screen_fraction.clamp(0.0, 1.0) * self.span;
    }

    fn clamp_start(self, start: f32) -> f32 {
        let first = 0.0_f32.min(1.0 - self.span);
        let last = 0.0_f32.max(1.0 - self.span);
        start.clamp(first, last)
    }

    fn pan_by_screen_delta(&mut self, delta_x: f32, plot_width: f32) -> bool {
        if delta_x == 0.0 || plot_width <= 0.0 {
            return false;
        }
        let previous_start = self.start;
        self.start = self.clamp_start(self.start - delta_x / plot_width * self.span);
        if (self.start - previous_start).abs() <= f32::EPSILON {
            return false;
        }
        self.anchor_locked = false;
        true
    }

    fn step_timebase_at_sample(
        &mut self,
        scroll_delta: f32,
        sample_count: usize,
        sample_rate_hz: u64,
        sample_fraction: f32,
        screen_fraction: f32,
    ) -> bool {
        if scroll_delta == 0.0 {
            return false;
        }
        let Some(current_timebase) = self.time_per_division(sample_count, sample_rate_hz) else {
            return false;
        };
        let Some(timebase) = adjacent_timebase(current_timebase, scroll_delta > 0.0) else {
            return false;
        };
        let full_duration = sample_count as f64 / sample_rate_hz as f64;
        let minimum_span = (1.0 / sample_count as f64).min(1.0);
        let maximum_span = TIMEBASE_STEPS_SECONDS[TIMEBASE_STEPS_SECONDS.len() - 1]
            * SCOPE_HORIZONTAL_DIVISIONS as f64
            / full_duration;
        self.span = (timebase * SCOPE_HORIZONTAL_DIVISIONS as f64 / full_duration)
            .clamp(minimum_span, maximum_span.max(minimum_span)) as f32;
        self.align_sample_to_screen(sample_fraction, screen_fraction);
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeFftViewport {
    start: f32,
    span: f32,
}

impl Default for ScopeFftViewport {
    fn default() -> Self {
        Self {
            start: 0.0,
            span: 1.0,
        }
    }
}

impl ScopeFftViewport {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn zoom(&mut self, scroll_delta: f32, pointer_fraction: f32, minimum_span: f32) {
        if scroll_delta == 0.0 {
            return;
        }
        let pointer_fraction = pointer_fraction.clamp(0.0, 1.0);
        let anchor = self.start + pointer_fraction * self.span;
        let factor = if scroll_delta > 0.0 { 0.8 } else { 1.25 };
        self.span = (self.span * factor).clamp(minimum_span, 1.0);
        self.start = (anchor - pointer_fraction * self.span).clamp(0.0, 1.0 - self.span);
    }

    fn pan(&mut self, delta_x: f32, plot_width: f32) {
        if plot_width <= 0.0 {
            return;
        }
        self.start = (self.start - delta_x / plot_width * self.span).clamp(0.0, 1.0 - self.span);
    }
}

fn apply_time_per_division(
    viewport: &mut WaveformViewport,
    sample_count: usize,
    sample_rate_hz: u64,
    time_per_division_seconds: f64,
) {
    if sample_count == 0 || sample_rate_hz == 0 || !time_per_division_seconds.is_finite() {
        return;
    }
    let new_span =
        (time_per_division_seconds * SCOPE_HORIZONTAL_DIVISIONS as f64 * sample_rate_hz as f64
            / sample_count as f64)
            .max(1.0 / sample_count as f64) as f32;
    if (new_span - viewport.span).abs() <= f32::EPSILON {
        return;
    }
    let center_sample_fraction = viewport.start + viewport.span * 0.5;
    viewport.span = new_span;
    viewport.start = viewport.clamp_start(center_sample_fraction - new_span * 0.5);
}

fn adjacent_timebase(current_seconds: f64, zoom_in: bool) -> Option<f64> {
    const COMPARISON_EPSILON: f64 = 1.0e-6;
    if zoom_in {
        TIMEBASE_STEPS_SECONDS
            .iter()
            .rev()
            .copied()
            .find(|step| *step < current_seconds * (1.0 - COMPARISON_EPSILON))
    } else {
        TIMEBASE_STEPS_SECONDS
            .iter()
            .copied()
            .find(|step| *step > current_seconds * (1.0 + COMPARISON_EPSILON))
    }
}

impl WaveformStore {
    fn can_continue_scope_roll(&self, config: DsCopeCaptureConfig, scope_capacity: usize) -> bool {
        self.scope_rolling
            && self.scope_rate_hz == config.sample_rate_hz()
            && self.scope_capacity == scope_capacity
            && self
                .scope_channels
                .iter()
                .map(|samples| !samples.is_empty())
                .eq(config.channels().iter().map(|channel| channel.enabled()))
    }

    fn mark_scope_capture_start(&mut self, scope_channels: [bool; 2]) {
        let sample_count = self
            .scope_channels
            .iter()
            .zip(scope_channels)
            .filter_map(|(samples, enabled)| enabled.then_some(samples.len() as u64))
            .min()
            .unwrap_or(0);
        if sample_count > 0 {
            self.scope_capture_boundaries
                .push_back(self.scope_sample_offset + sample_count);
        }
    }

    fn ingest(&mut self, batch: &CaptureBatch, scope_channels: [bool; 2]) {
        match batch {
            CaptureBatch::Logic(logic) => {
                self.logic_channels = logic.channel_count;
                self.logic_rate_hz = logic.sample_rate_hz;
                self.logic_sample_offset += extend_bounded_to(
                    &mut self.logic_samples,
                    logic.samples.iter().copied(),
                    self.logic_capacity,
                ) as u64;
            }
            CaptureBatch::Scope(scope) => {
                self.scope_revision = SCOPE_WAVEFORM_REVISION.fetch_add(1, Ordering::Relaxed);
                self.scope_rate_hz = scope.sample_rate_hz;
                if self.scope_channels.len() != scope_channels.len() {
                    self.scope_channels = vec![VecDeque::new(); scope_channels.len()];
                    self.scope_sample_offset = 0;
                    self.scope_capture_boundaries.clear();
                }
                let capture_start = self.scope_sample_offset
                    + self
                        .scope_channels
                        .iter()
                        .zip(scope_channels)
                        .filter_map(|(samples, enabled)| enabled.then_some(samples.len() as u64))
                        .min()
                        .unwrap_or(0);
                if let Some(trigger_sample_index) = scope.trigger_sample_index {
                    self.scope_trigger_sample_index =
                        Some(capture_start.saturating_add(trigger_sample_index));
                }
                let mut removed_samples = 0;
                for (source, physical_channel) in scope.channels.iter().zip(
                    scope_channels
                        .into_iter()
                        .enumerate()
                        .filter_map(|(index, enabled)| enabled.then_some(index)),
                ) {
                    let destination = &mut self.scope_channels[physical_channel];
                    removed_samples = removed_samples.max(extend_bounded_to(
                        destination,
                        source.iter().copied(),
                        self.scope_capacity,
                    ));
                }
                self.scope_sample_offset += removed_samples as u64;
                while self
                    .scope_capture_boundaries
                    .front()
                    .is_some_and(|boundary| *boundary <= self.scope_sample_offset)
                {
                    self.scope_capture_boundaries.pop_front();
                }
            }
        }
    }
}

fn extend_bounded_to<T>(
    destination: &mut VecDeque<T>,
    values: impl IntoIterator<Item = T>,
    capacity: usize,
) -> usize {
    destination.extend(values);
    let excess = destination.len().saturating_sub(capacity);
    destination.drain(..excess);
    excess
}

struct ActiveCapture {
    session: CaptureSession,
    mode: AcquisitionMode,
    target: CaptureTarget,
    preserve_display_until_complete: bool,
    scope_config: DsCopeCaptureConfig,
    scope_trigger_controls: Vec<DsCopeRuntimeTriggerControl>,
    scope_calibration_controls: Vec<DsCopeRuntimeCalibrationControl>,
    stopped: HashSet<DeviceId>,
    waveforms: WaveformStore,
    recorder: Option<FileRecorder>,
    recording_path: Option<PathBuf>,
    has_logic: bool,
    has_scope: bool,
}

#[derive(Clone, Copy, Debug)]
struct ScheduledRepetitiveCapture {
    due: Instant,
    target: CaptureTarget,
}

struct CompletedCapture {
    waveforms: WaveformStore,
    has_logic: bool,
    has_scope: bool,
}

struct CalibrationDisplay {
    waveforms: WaveformStore,
    scope_config: DsCopeCaptureConfig,
}

struct CalibrationProgressModal {
    progress: f32,
    phase: String,
    completed: bool,
    values: Option<DsCopeCalibrationValues>,
}

struct UsbSpeedWarning {
    devices: Vec<DiscoveredDevice>,
}

enum CalibrationModalAction {
    Close,
    Save(DsCopeCalibrationValues),
}

struct LoadedRecording {
    capture: CompletedCapture,
    logic_settings: Option<LogicCaptureSettings>,
    scope_settings: Option<ScopeCaptureSettings>,
    logic_channel_names: [String; LOGIC_CHANNEL_COUNT],
    scope_channel_names: [String; 2],
    completion: CompletionStatus,
}

type FileRecorder = RecordingWriter<BufWriter<File>>;

enum WorkerSuccess {
    Devices(Vec<DiscoveredDevice>),
    Capture {
        session: CaptureSession,
        mode: AcquisitionMode,
        target: CaptureTarget,
        logic_config: DsLogicCaptureConfig,
        scope_config: DsCopeCaptureConfig,
        scope_trigger_controls: Vec<DsCopeRuntimeTriggerControl>,
        scope_calibration_controls: Vec<DsCopeRuntimeCalibrationControl>,
    },
    ScopeConfigured(usize),
    ScopeAutoCalibrated(DsCopeCalibrationValues),
    ScopeAutoCalibrationSaved,
    ScopeCalibrationLoaded(DsCopeCalibrationValues),
    ScopeCalibrationSaved,
    RecordingLoaded {
        path: PathBuf,
        recording: Box<LoadedRecording>,
    },
}

type WorkerResult = Result<WorkerSuccess, String>;

struct DsViewApp {
    resource_directory: String,
    settings_open: bool,
    auto_calibration_confirmation_open: bool,
    manual_calibration: Option<ManualCalibrationEditor>,
    manual_calibration_load_requested: bool,
    manual_calibration_save_requested: Option<DsCopeCalibrationValues>,
    record_capture: bool,
    recording_path: String,
    capture_run_requested: bool,
    requested_capture_target: Option<CaptureTarget>,
    logic_acquisition_mode: AcquisitionMode,
    repetitive_interval_seconds: f32,
    scheduled_repetitive_capture: Option<ScheduledRepetitiveCapture>,
    devices: Vec<DeviceEntry>,
    active_capture_settings_tab: CaptureSettingsTab,
    logic_settings: LogicCaptureSettings,
    scope_settings: ScopeCaptureSettings,
    scope_auto_sample_rate: bool,
    scope_function_settings: ScopeFunctionSettings,
    scope_marker_settings: ScopeMarkerSettings,
    scope_quick_time_measurement: ScopeQuickTimeMeasurement,
    scope_marker_editor: Option<ScopeMarkerEditor>,
    active_waveform_tab: Option<WaveformTab>,
    combined_logic_ratio: f32,
    logic_channel_colors: [Color32; LOGIC_CHANNEL_COUNT],
    logic_channel_marker_opacities: [u8; LOGIC_CHANNEL_COUNT],
    logic_channel_settings_drafts: [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT],
    scope_channel_colors: [Color32; 2],
    logic_channel_names: [String; LOGIC_CHANNEL_COUNT],
    scope_channel_names: [String; 2],
    logic_viewport: WaveformViewport,
    scope_viewport: WaveformViewport,
    scope_fft_viewport: ScopeFftViewport,
    scope_measurement_caches: [ScopeMeasurementCache; 2],
    scope_measurement_window_positions: [Vec2; 2],
    snapshot_plot_rect: Option<Rect>,
    pending_snapshot: Option<PendingSnapshot>,
    scope_time_per_division_seconds: Option<f64>,
    scope_capture_restart_requested: bool,
    scope_configuration_due: Option<Instant>,
    pending: Option<Receiver<WorkerResult>>,
    calibration_progress: Option<Receiver<DsCopeCalibrationFrame>>,
    calibration_display: Option<CalibrationDisplay>,
    calibration_active: bool,
    calibration_modal: Option<CalibrationProgressModal>,
    usb_speed_warning: Option<UsbSpeedWarning>,
    capture: Option<ActiveCapture>,
    completed_capture: Option<CompletedCapture>,
    status: String,
    last_error: Option<String>,
}

fn default_scope_measurement_window_positions() -> [Vec2; 2] {
    [Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0)]
}

impl DsViewApp {
    fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        configure_style(&creation_context.egui_ctx);
        let mut app = Self {
            resource_directory: default_resource_directory(),
            settings_open: false,
            auto_calibration_confirmation_open: false,
            manual_calibration: None,
            manual_calibration_load_requested: false,
            manual_calibration_save_requested: None,
            record_capture: false,
            recording_path: "capture.dsv".to_owned(),
            capture_run_requested: false,
            requested_capture_target: None,
            logic_acquisition_mode: AcquisitionMode::Single,
            repetitive_interval_seconds: 1.0,
            scheduled_repetitive_capture: None,
            devices: Vec::new(),
            active_capture_settings_tab: CaptureSettingsTab::DsLogic,
            logic_settings: LogicCaptureSettings::default(),
            scope_settings: ScopeCaptureSettings::default(),
            scope_auto_sample_rate: true,
            scope_function_settings: ScopeFunctionSettings::default(),
            scope_marker_settings: ScopeMarkerSettings::default(),
            scope_quick_time_measurement: ScopeQuickTimeMeasurement::default(),
            scope_marker_editor: None,
            active_waveform_tab: None,
            combined_logic_ratio: 0.55,
            logic_channel_colors: default_logic_channel_colors(),
            logic_channel_marker_opacities: default_logic_channel_marker_opacities(),
            logic_channel_settings_drafts: default_logic_channel_settings_drafts(),
            scope_channel_colors: SCOPE_COLORS,
            logic_channel_names: empty_logic_channel_names(),
            scope_channel_names: empty_scope_channel_names(),
            logic_viewport: WaveformViewport::default(),
            scope_viewport: WaveformViewport::default(),
            scope_fft_viewport: ScopeFftViewport::default(),
            scope_measurement_caches: [ScopeMeasurementCache::default(); 2],
            scope_measurement_window_positions: default_scope_measurement_window_positions(),
            snapshot_plot_rect: None,
            pending_snapshot: None,
            scope_time_per_division_seconds: Some(DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS),
            scope_capture_restart_requested: false,
            scope_configuration_due: None,
            pending: None,
            calibration_progress: None,
            calibration_display: None,
            calibration_active: false,
            calibration_modal: None,
            usb_speed_warning: None,
            capture: None,
            completed_capture: None,
            status: "Ready".to_owned(),
            last_error: None,
        };
        app.begin_refresh();
        app
    }

    fn finish_pending_snapshot(&mut self, context: &egui::Context) {
        if self.pending_snapshot.is_none() {
            return;
        }
        let screenshot = context.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(screenshot) = screenshot else {
            return;
        };
        let pending = self
            .pending_snapshot
            .take()
            .expect("pending snapshot was checked above");
        let snapshot = match crop_snapshot_image(
            screenshot.as_ref(),
            pending.plot_rect,
            pending.pixels_per_point,
        ) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.status = "Snapshot failed".to_owned();
                self.last_error = Some(format!("Snapshot failed: {error}"));
                return;
            }
        };

        let Some(directory) = rfd::FileDialog::new()
            .set_title("Select snapshot directory")
            .pick_folder()
        else {
            self.status = "Snapshot canceled".to_owned();
            return;
        };
        match save_snapshot_image(&directory, &snapshot) {
            Ok(path) => {
                self.status = format!("Snapshot saved to {}", path.display());
                self.last_error = None;
            }
            Err(error) => {
                self.status = "Snapshot failed".to_owned();
                self.last_error = Some(format!("Snapshot failed: {error}"));
            }
        }
    }

    fn begin_open_recording(&mut self, path: PathBuf) {
        if self.is_busy() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.capture_run_requested = false;
        self.requested_capture_target = None;
        self.scheduled_repetitive_capture = None;
        self.scope_configuration_due = None;
        self.status = format!("Opening capture {}…", path.display());
        self.last_error = None;
        thread::spawn(move || {
            let result =
                load_gui_recording(&path).map(|recording| WorkerSuccess::RecordingLoaded {
                    path,
                    recording: Box::new(recording),
                });
            let _ = sender.send(result);
        });
    }

    fn begin_refresh(&mut self) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.capture_run_requested = false;
        self.requested_capture_target = None;
        self.scheduled_repetitive_capture = None;
        self.scope_capture_restart_requested = false;
        self.scope_configuration_due = None;
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

    fn begin_capture(&mut self, mode: AcquisitionMode, target: CaptureTarget) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        self.scheduled_repetitive_capture = None;
        if self.record_capture && mode.runs_continuously() {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            self.record_capture = false;
            self.last_error =
                Some("Complete-capture recording is available in Single mode only".to_owned());
            return;
        }
        let has_logic_analyzer = self.devices.iter().any(|entry| {
            entry.selected && entry.device.profile.model == DeviceModel::DsLogicU3Pro16
        });
        let has_oscilloscope = self
            .devices
            .iter()
            .any(|entry| entry.selected && entry.device.profile.model == DeviceModel::DsCopeU3P100);
        if !target.is_available(has_logic_analyzer, has_oscilloscope) {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            self.record_capture = false;
            self.last_error = Some(match target {
                CaptureTarget::LogicAnalyzer => {
                    "Select at least one Logic Analyzer instrument".to_owned()
                }
                CaptureTarget::Oscilloscope => {
                    "Select at least one Oscilloscope instrument".to_owned()
                }
                CaptureTarget::Combined => {
                    "Select at least one Logic Analyzer and one Oscilloscope instrument".to_owned()
                }
            });
            return;
        }
        let devices = self
            .devices
            .iter()
            .filter(|entry| entry.selected && target.includes_model(entry.device.profile.model))
            .map(|entry| entry.device.clone())
            .collect::<Vec<_>>();
        if self.record_capture && self.recording_path.trim().is_empty() {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            self.record_capture = false;
            self.last_error = Some("Enter a recording file path".to_owned());
            return;
        }
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.capture_run_requested = false;
                self.requested_capture_target = None;
                self.record_capture = false;
                self.last_error = Some(error);
                return;
            }
        };
        let logic_config = match self.logic_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.capture_run_requested = false;
                self.requested_capture_target = None;
                self.record_capture = false;
                self.last_error = Some(error);
                return;
            }
        }
        .with_loop_mode(mode == AcquisitionMode::Loop);
        if matches!(mode, AcquisitionMode::Repetitive | AcquisitionMode::Loop)
            && target != CaptureTarget::LogicAnalyzer
        {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            self.record_capture = false;
            self.last_error = Some(
                "Repetitive and Loop modes are available for Logic Analyzer capture only"
                    .to_owned(),
            );
            return;
        }
        if mode == AcquisitionMode::Loop
            && logic_config.operation_mode() != DsLogicOperationMode::Stream
        {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            self.record_capture = false;
            self.last_error = Some("Loop mode requires DSLogic Stream mode".to_owned());
            return;
        }
        let calibration_values = self.manual_calibration.as_ref().map(|editor| editor.values);

        let continuing_loop = mode == AcquisitionMode::Loop && self.capture_run_requested;
        self.capture_run_requested = true;
        self.requested_capture_target = Some(target);
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        if mode == AcquisitionMode::Single || (mode == AcquisitionMode::Loop && !continuing_loop) {
            self.completed_capture = None;
        }
        self.status = match mode {
            AcquisitionMode::Single => {
                format!("Preparing {} single acquisition…", target.label())
            }
            AcquisitionMode::Repetitive => {
                format!("Preparing {} repetitive acquisition…", target.label())
            }
            AcquisitionMode::Loop => {
                format!("Preparing {} loop acquisition…", target.label())
            }
            AcquisitionMode::Run => {
                format!("Preparing {} continuous acquisition…", target.label())
            }
        };
        self.last_error = None;
        thread::spawn(move || {
            let result = start_capture(
                devices,
                &resource_directory,
                logic_config,
                scope_config,
                calibration_values,
            )
            .map(|started| WorkerSuccess::Capture {
                session: started.session,
                mode,
                target,
                logic_config,
                scope_config,
                scope_trigger_controls: started.scope_trigger_controls,
                scope_calibration_controls: started.scope_calibration_controls,
            });
            let _ = sender.send(result);
        });
    }

    fn begin_scope_configuration(&mut self) {
        if self.pending.is_some() || self.capture.is_some() {
            return;
        }
        self.scope_configuration_due = None;
        let devices = self
            .devices
            .iter()
            .filter(|entry| {
                entry.selected && entry.device.profile.model == DeviceModel::DsCopeU3P100
            })
            .map(|entry| entry.device.clone())
            .collect::<Vec<_>>();
        if devices.is_empty() {
            self.status = "DSCope settings will be applied when capture starts".to_owned();
            return;
        }
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let device_count = devices.len();
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.status = format!("Applying DSCope configuration to {device_count} instrument(s)…");
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                for device in devices {
                    let bitstream_path =
                        resource_directory.join(device.profile.fpga_bitstream_name);
                    prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                    apply_dscope_configuration(&device, scope_config)
                        .map_err(|error| error.to_string())?;
                }
                Ok(WorkerSuccess::ScopeConfigured(device_count))
            })();
            let _ = sender.send(result);
        });
    }

    fn selected_scope_device(&mut self) -> Option<DiscoveredDevice> {
        let devices = self
            .devices
            .iter()
            .filter(|entry| {
                entry.selected && entry.device.profile.model == DeviceModel::DsCopeU3P100
            })
            .map(|entry| entry.device.clone())
            .collect::<Vec<_>>();
        if devices.len() != 1 {
            self.last_error =
                Some("Select exactly one connected DSCope U3P100 for calibration".to_owned());
            return None;
        }
        devices.into_iter().next()
    }

    fn begin_auto_calibration(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device() else {
            return;
        };
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        let (progress_sender, progress_receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.calibration_progress = Some(progress_receiver);
        self.calibration_display = None;
        self.calibration_active = true;
        self.calibration_modal = Some(CalibrationProgressModal {
            progress: 0.0,
            phase: "Preparing oscilloscope…".to_owned(),
            completed: false,
            values: None,
        });
        self.select_waveform_tab(WaveformTab::Oscilloscope);
        self.status = "Auto-calibrating DSCope offsets and ADC branches…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                let values = auto_calibrate_dscope_with_progress(&device, |frame| {
                    let _ = progress_sender.send(frame);
                })
                .map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeAutoCalibrated(values))
            })();
            let _ = sender.send(result);
        });
    }

    fn begin_save_auto_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device() else {
            return;
        };
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        if let Some(modal) = self.calibration_modal.as_mut() {
            modal.progress = 0.0;
            modal.phase = "Saving calibration to device NVM…".to_owned();
            modal.completed = false;
            modal.values = None;
        }
        self.status = "Saving DSCope calibration to device NVM…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                write_dscope_calibration(&device, values).map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeAutoCalibrationSaved)
            })();
            let _ = sender.send(result);
        });
    }

    fn begin_manual_calibration(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device() else {
            return;
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.status = "Loading DSCope calibration values…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                let values = read_dscope_calibration(&device).map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeCalibrationLoaded(values))
            })();
            let _ = sender.send(result);
        });
    }

    fn request_manual_calibration(&mut self) {
        if self.pending.is_some() || self.manual_calibration.is_some() {
            return;
        }
        if let Some(active) = self.capture.as_ref() {
            if active.mode != AcquisitionMode::Run {
                return;
            }
            self.manual_calibration_load_requested = true;
            self.scope_capture_restart_requested = false;
            active.session.request_stop();
            self.status = "Loading calibration while continuous acquisition pauses…".to_owned();
            self.last_error = None;
        } else {
            self.begin_manual_calibration();
        }
    }

    fn begin_save_manual_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.is_busy() {
            return;
        }
        let Some(device) = self.selected_scope_device() else {
            return;
        };
        let scope_config = match self.scope_settings.capture_config() {
            Ok(config) => config,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let resource_directory = PathBuf::from(self.resource_directory.trim());
        let (sender, receiver) = mpsc::channel();
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.status = "Saving DSCope calibration values to device NVM…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let result = (|| {
                let bitstream_path = resource_directory.join(device.profile.fpga_bitstream_name);
                prepare_device(&device, &bitstream_path).map_err(|error| error.to_string())?;
                write_dscope_calibration(&device, values).map_err(|error| error.to_string())?;
                apply_dscope_configuration(&device, scope_config)
                    .map_err(|error| error.to_string())?;
                Ok(WorkerSuccess::ScopeCalibrationSaved)
            })();
            let _ = sender.send(result);
        });
    }

    fn request_save_manual_calibration(&mut self, values: DsCopeCalibrationValues) {
        if self.pending.is_some() {
            return;
        }
        if let Some(active) = self.capture.as_ref() {
            self.manual_calibration_save_requested = Some(values);
            self.scope_capture_restart_requested = false;
            active.session.request_stop();
            self.status = "Pausing acquisition to save calibration…".to_owned();
            self.last_error = None;
        } else {
            self.begin_save_manual_calibration(values);
        }
    }

    fn stop_capture(&mut self) {
        self.capture_run_requested = false;
        self.requested_capture_target = None;
        self.scheduled_repetitive_capture = None;
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
        let recording_path = active.recording_path.take();
        if recording_path.is_some() {
            self.record_capture = false;
        }
        let has_logic = active.has_logic;
        let has_scope = active.has_scope;
        if !active.preserve_display_until_complete {
            self.completed_capture = Some(CompletedCapture {
                waveforms: active.waveforms,
                has_logic,
                has_scope,
            });
        }
        match (stop_result, recording_result) {
            (Ok(()), Ok(())) => {
                self.status = recording_path.map_or_else(
                    || "Capture stopped".to_owned(),
                    |path| format!("Capture stopped · recording saved to {}", path.display()),
                );
            }
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
                self.usb_speed_warning = usb_speed_warning(&devices);
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
            Ok(WorkerSuccess::Capture {
                session,
                mode,
                target,
                logic_config,
                mut scope_config,
                scope_trigger_controls,
                scope_calibration_controls,
            }) => {
                if !self.capture_run_requested {
                    self.requested_capture_target = None;
                    match session.stop() {
                        Ok(()) => self.status = "Acquisition stopped".to_owned(),
                        Err(error) => self.set_error(error.to_string()),
                    }
                    return;
                }
                if self.scope_capture_restart_requested {
                    self.scope_capture_restart_requested = false;
                    match session.stop() {
                        Ok(()) => {
                            self.status = "Reconfiguring oscilloscope frontend…".to_owned();
                            self.begin_capture(mode, target);
                        }
                        Err(error) => self.set_error(error.to_string()),
                    }
                    return;
                }
                if !scope_trigger_controls.is_empty() {
                    let desired_trigger = match self.scope_settings.trigger.capture_config() {
                        Ok(trigger) => trigger,
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
                    };
                    if desired_trigger != scope_config.trigger() {
                        let update_result = scope_config
                            .with_trigger_config(desired_trigger)
                            .map_err(|error| error.to_string())
                            .and_then(|updated_config| {
                                for control in &scope_trigger_controls {
                                    control
                                        .update(desired_trigger)
                                        .map_err(|error| error.to_string())?;
                                }
                                Ok(updated_config)
                            });
                        match update_result {
                            Ok(updated_config) => scope_config = updated_config,
                            Err(error) => {
                                let cleanup = session.stop().err();
                                self.set_error(match cleanup {
                                    Some(cleanup) => format!(
                                        "failed to apply the latest trigger settings: {error}; additionally failed to stop: {cleanup}"
                                    ),
                                    None => format!(
                                        "failed to apply the latest trigger settings: {error}"
                                    ),
                                });
                                return;
                            }
                        }
                    }
                }
                let count = session.descriptors().len();
                let recording_path = self
                    .record_capture
                    .then(|| PathBuf::from(self.recording_path.trim()));
                let recorder = match recording_path.as_deref() {
                    Some(path) => {
                        match start_gui_recording(
                            path,
                            session.descriptors(),
                            GuiRecordingSettings {
                                logic_config,
                                logic_settings: self.logic_settings,
                                scope_config,
                                scope_settings: self.scope_settings,
                                logic_channel_names: &self.logic_channel_names,
                                scope_channel_names: &self.scope_channel_names,
                            },
                        ) {
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
                        }
                    }
                    None => None,
                };
                let has_logic = session
                    .descriptors()
                    .iter()
                    .any(|device| device.kind == InstrumentKind::LogicAnalyzer);
                let has_scope = session
                    .descriptors()
                    .iter()
                    .any(|device| device.kind == InstrumentKind::Oscilloscope);
                let has_scope_trigger = !scope_trigger_controls.is_empty();
                let rolling =
                    uses_rolling_scope_buffer(mode, has_scope, scope_config.trigger().source());
                let scope_capacity = if rolling {
                    scope_rolling_sample_capacity(
                        scope_config,
                        self.scope_time_per_division_seconds,
                    )
                } else {
                    scope_config.sample_count() as usize
                };
                let continue_roll = rolling
                    && self.completed_capture.as_ref().is_some_and(|capture| {
                        capture
                            .waveforms
                            .can_continue_scope_roll(scope_config, scope_capacity)
                    });
                let continue_logic_loop = mode == AcquisitionMode::Loop
                    && self
                        .completed_capture
                        .as_ref()
                        .is_some_and(|capture| capture.has_logic);
                let preserve_display_until_complete =
                    mode == AcquisitionMode::Run || mode == AcquisitionMode::Repetitive;
                let preserve_display_until_complete = preserve_display_until_complete
                    && self.completed_capture.is_some()
                    && !continue_roll;
                let mut waveforms = if continue_roll || continue_logic_loop {
                    self.completed_capture
                        .take()
                        .map(|capture| capture.waveforms)
                        .unwrap_or_default()
                } else {
                    WaveformStore::default()
                };
                waveforms.logic_capacity =
                    (logic_config.sample_count() as usize).clamp(1, MAX_WAVEFORM_SAMPLES);
                waveforms.scope_capacity = scope_capacity;
                waveforms.scope_rolling = rolling;
                self.capture = Some(ActiveCapture {
                    session,
                    mode,
                    target,
                    preserve_display_until_complete,
                    scope_config,
                    scope_trigger_controls,
                    scope_calibration_controls,
                    stopped: HashSet::new(),
                    waveforms,
                    recorder,
                    recording_path: recording_path.clone(),
                    has_logic,
                    has_scope,
                });
                self.status = match (mode, recording_path) {
                    (AcquisitionMode::Single, Some(path)) => {
                        format!(
                            "{} Single acquisition from {count} instrument(s) to {}",
                            target.label(),
                            path.display()
                        )
                    }
                    (AcquisitionMode::Single, None) => {
                        if has_scope_trigger
                            && scope_config.trigger().source() != DsCopeTriggerSource::Auto
                        {
                            "Single · waiting for trigger".to_owned()
                        } else {
                            format!(
                                "{} Single acquisition from {count} instrument(s)",
                                target.label()
                            )
                        }
                    }
                    (AcquisitionMode::Repetitive, _) => format!(
                        "{} Repetitive acquisition from {count} instrument(s)",
                        target.label()
                    ),
                    (AcquisitionMode::Loop, _) => format!(
                        "{} Loop acquisition from {count} instrument(s)",
                        target.label()
                    ),
                    (AcquisitionMode::Run, _) => {
                        if has_scope_trigger
                            && scope_config.trigger().source() == DsCopeTriggerSource::Auto
                        {
                            "Run · Auto/Roll".to_owned()
                        } else if has_scope_trigger {
                            "Run · waiting for trigger".to_owned()
                        } else {
                            format!(
                                "{} Run acquisition from {count} instrument(s)",
                                target.label()
                            )
                        }
                    }
                };
            }
            Ok(WorkerSuccess::RecordingLoaded { path, recording }) => {
                let LoadedRecording {
                    capture,
                    logic_settings,
                    scope_settings,
                    logic_channel_names,
                    scope_channel_names,
                    completion,
                } = *recording;
                if let Some(logic_settings) = logic_settings {
                    self.logic_settings = logic_settings;
                }
                if let Some(scope_settings) = scope_settings {
                    self.scope_settings = scope_settings;
                    self.scope_auto_sample_rate = false;
                }
                self.completed_capture = Some(capture);
                self.logic_channel_names = logic_channel_names;
                self.scope_channel_names = scope_channel_names;
                self.record_capture = false;
                self.logic_viewport.reset();
                self.scope_viewport.reset();
                self.scope_fft_viewport.reset();
                self.scope_measurement_caches = [ScopeMeasurementCache::default(); 2];
                self.scope_time_per_division_seconds = None;
                self.status = format!(
                    "Opened {} · {} recording",
                    path.display(),
                    recording_completion_label(completion)
                );
                self.last_error = None;
            }
            Ok(WorkerSuccess::ScopeConfigured(device_count)) => {
                self.status =
                    format!("Applied DSCope configuration to {device_count} instrument(s)");
            }
            Ok(WorkerSuccess::ScopeAutoCalibrated(values)) => {
                self.calibration_active = false;
                self.calibration_progress = None;
                self.calibration_display = None;
                if let Some(modal) = self.calibration_modal.as_mut() {
                    modal.progress = 1.0;
                    modal.phase = "Calibration is done. Save the result to device NVM?".to_owned();
                    modal.completed = true;
                    modal.values = Some(values);
                }
                self.status = "DSCope auto calibration finished · result not saved".to_owned();
            }
            Ok(WorkerSuccess::ScopeAutoCalibrationSaved) => {
                if let Some(modal) = self.calibration_modal.as_mut() {
                    modal.progress = 1.0;
                    modal.phase = "Calibration was saved to device NVM.".to_owned();
                    modal.completed = true;
                    modal.values = None;
                }
                self.status = "DSCope auto calibration saved and verified".to_owned();
            }
            Ok(WorkerSuccess::ScopeCalibrationLoaded(values)) => {
                let range = DSCOPE_VOLTS_PER_DIV_MV
                    .iter()
                    .position(|value| *value == self.scope_settings.channels[0].volts_per_div_mv)
                    .unwrap_or(0);
                for channel in &mut self.scope_settings.channels {
                    channel.volts_per_div_mv = DSCOPE_VOLTS_PER_DIV_MV[range];
                }
                self.manual_calibration = Some(ManualCalibrationEditor { values, range });
                self.status = "Loaded DSCope calibration values".to_owned();
                if self.capture_run_requested {
                    if let Some(target) = self.requested_capture_target {
                        self.begin_capture(AcquisitionMode::Run, target);
                    }
                }
            }
            Ok(WorkerSuccess::ScopeCalibrationSaved) => {
                self.manual_calibration = None;
                self.status = "DSCope manual calibration saved and verified".to_owned();
                if self.capture_run_requested {
                    if let Some(target) = self.requested_capture_target {
                        self.begin_capture(AcquisitionMode::Run, target);
                    }
                }
            }
            Err(error) => self.set_error(error),
        }
    }

    fn poll_calibration_progress(&mut self) {
        let mut latest = None;
        let mut disconnected = false;
        if let Some(receiver) = self.calibration_progress.as_ref() {
            loop {
                match receiver.try_recv() {
                    Ok(frame) => latest = Some(frame),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.calibration_progress = None;
        }
        let Some(frame) = latest else {
            return;
        };

        let config = frame.config();
        if let Some(modal) = self.calibration_modal.as_mut() {
            modal.progress = modal.progress.max(frame.progress());
            modal.phase = frame.phase().to_owned();
        }
        let enabled_channels = config.channels().map(|channel| channel.enabled());
        let mut waveforms = WaveformStore {
            scope_capacity: config.sample_count() as usize,
            ..WaveformStore::default()
        };
        waveforms.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: config.sample_rate_hz(),
                trigger_sample_index: None,
                channels: frame.channels().to_vec(),
            }),
            enabled_channels,
        );
        self.calibration_display = Some(CalibrationDisplay {
            waveforms,
            scope_config: config,
        });
        self.scope_viewport.reset();
        self.scope_time_per_division_seconds = Some(
            config.sample_count() as f64
                / config.sample_rate_hz() as f64
                / SCOPE_HORIZONTAL_DIVISIONS as f64,
        );
        let volts_per_div_mv = config.channels()[0].volts_per_div_mv();
        self.status = format!(
            "Auto-calibrating DSCope · {volts_per_div_mv} mV/div · {}",
            if config.enabled_channel_count() == 1 {
                "interleaved ADC"
            } else {
                "dual-channel ADC"
            }
        );
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
                                let enabled_scope_channels = active
                                    .scope_config
                                    .channels()
                                    .map(|channel| channel.enabled());
                                if active.waveforms.scope_rolling
                                    && frame.sequence == 0
                                    && matches!(&frame.payload, CaptureBatch::Scope(_))
                                {
                                    active
                                        .waveforms
                                        .mark_scope_capture_start(enabled_scope_channels);
                                }
                                active
                                    .waveforms
                                    .ingest(&frame.payload, enabled_scope_channels);
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
                let mode = active.mode;
                let target = active.target;
                let preserve_display_until_complete = active.preserve_display_until_complete;
                let recording_path = active.recording_path.take();
                let has_logic = active.has_logic;
                let has_scope = active.has_scope;
                let restart_for_frontend =
                    self.scope_capture_restart_requested && self.capture_run_requested;
                self.scope_capture_restart_requested = false;
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
                let update_completed_capture = if restart_for_frontend {
                    !preserve_display_until_complete
                } else {
                    completion == CompletionStatus::Complete || !preserve_display_until_complete
                };
                if update_completed_capture {
                    self.completed_capture = Some(CompletedCapture {
                        waveforms: active.waveforms,
                        has_logic,
                        has_scope,
                    });
                }
                if error.is_none() && self.manual_calibration_load_requested {
                    self.manual_calibration_load_requested = false;
                    self.begin_manual_calibration();
                    return;
                }
                if error.is_none() {
                    if let Some(values) = self.manual_calibration_save_requested.take() {
                        self.begin_save_manual_calibration(values);
                        return;
                    }
                }
                let repeat = error.is_none()
                    && (restart_for_frontend
                        || should_rearm_acquisition(
                            mode,
                            self.capture_run_requested,
                            completion,
                            false,
                        ));
                if !repeat {
                    self.capture_run_requested = false;
                    self.requested_capture_target = None;
                }
                if recording_path.is_some() {
                    self.record_capture = false;
                }
                match error {
                    Some(error) => self.set_error(error),
                    None if restart_for_frontend => {
                        self.status = "Reconfiguring oscilloscope frontend…".to_owned();
                    }
                    None => {
                        let capture_status =
                            result.ok().unwrap_or_else(|| "Capture complete".to_owned());
                        self.status = recording_path.map_or(capture_status.clone(), |path| {
                            format!("{capture_status} · recording saved to {}", path.display())
                        });
                    }
                }
                if repeat {
                    if mode == AcquisitionMode::Repetitive && !restart_for_frontend {
                        let interval = repetitive_interval(
                            self.logic_settings.operation_mode,
                            self.repetitive_interval_seconds,
                        );
                        self.scheduled_repetitive_capture = Some(ScheduledRepetitiveCapture {
                            due: Instant::now() + interval,
                            target,
                        });
                        self.status = format!(
                            "Repetitive · next acquisition in {:.1} s",
                            interval.as_secs_f32()
                        );
                    } else {
                        self.begin_capture(mode, target);
                    }
                }
            }
        }
    }

    fn poll_repetitive_capture(&mut self) {
        let Some(scheduled) = self.scheduled_repetitive_capture else {
            return;
        };
        if !self.capture_run_requested {
            self.scheduled_repetitive_capture = None;
            return;
        }
        if Instant::now() < scheduled.due {
            return;
        }
        self.scheduled_repetitive_capture = None;
        self.begin_capture(AcquisitionMode::Repetitive, scheduled.target);
    }

    fn set_error(&mut self, error: String) {
        self.capture_run_requested = false;
        self.requested_capture_target = None;
        self.scheduled_repetitive_capture = None;
        self.scope_capture_restart_requested = false;
        self.manual_calibration_load_requested = false;
        self.manual_calibration_save_requested = None;
        self.calibration_active = false;
        self.calibration_progress = None;
        self.calibration_display = None;
        self.calibration_modal = None;
        self.status = "Operation failed".to_owned();
        self.last_error = Some(error);
    }

    fn available_waveform_tabs(&self) -> Vec<WaveformTab> {
        let (displayed_logic, displayed_scope) = self
            .capture
            .as_ref()
            .map(|capture| (capture.has_logic, capture.has_scope))
            .or_else(|| {
                self.completed_capture
                    .as_ref()
                    .map(|capture| (capture.has_logic, capture.has_scope))
            })
            .unwrap_or((false, false));
        let has_logic_analyzer = displayed_logic
            || self
                .devices
                .iter()
                .any(|entry| entry.device.profile.model == DeviceModel::DsLogicU3Pro16);
        let has_oscilloscope = displayed_scope
            || self
                .devices
                .iter()
                .any(|entry| entry.device.profile.model == DeviceModel::DsCopeU3P100);
        waveform_tabs(
            has_logic_analyzer,
            has_oscilloscope,
            self.scope_function_settings.lissajous.enabled,
        )
    }

    fn select_waveform_tab(&mut self, tab: WaveformTab) {
        self.active_waveform_tab = Some(tab);
        if let Some(settings_tab) = tab.capture_settings_tab() {
            self.active_capture_settings_tab = settings_tab;
        }
    }

    fn available_capture_settings_tabs(&self) -> Vec<CaptureSettingsTab> {
        let has_logic_analyzer = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model == DeviceModel::DsLogicU3Pro16);
        let has_oscilloscope = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model == DeviceModel::DsCopeU3P100);
        capture_settings_tabs(has_logic_analyzer, has_oscilloscope)
    }

    fn reconcile_capture_settings_tab(&mut self) {
        let available = self.available_capture_settings_tabs();
        if available.contains(&self.active_capture_settings_tab) {
            return;
        }
        if let Some(tab) = available.first() {
            self.active_capture_settings_tab = *tab;
        }
    }

    fn reconcile_waveform_tab(&mut self) {
        let available = self.available_waveform_tabs();
        if self
            .active_waveform_tab
            .is_some_and(|active| available.contains(&active))
        {
            return;
        }
        let selected = if available.contains(&WaveformTab::Combined) {
            Some(WaveformTab::Combined)
        } else {
            available.first().copied()
        };
        if let Some(tab) = selected {
            self.select_waveform_tab(tab);
        } else {
            self.active_waveform_tab = None;
        }
    }

    fn is_busy(&self) -> bool {
        self.pending.is_some()
            || self.capture.is_some()
            || self.scheduled_repetitive_capture.is_some()
    }
}

fn format_sample_count(sample_count: u64) -> String {
    if sample_count >= 1_000_000 {
        format!("{}M samples", sample_count / 1_000_000)
    } else if sample_count >= 1_000 {
        format!("{}k samples", sample_count / 1_000)
    } else {
        format!("{sample_count} samples")
    }
}

fn format_input_scale(volts_per_div_mv: u16) -> String {
    if volts_per_div_mv >= 1_000 {
        format!("{} V/div", volts_per_div_mv / 1_000)
    } else {
        format!("{volts_per_div_mv} mV/div")
    }
}

fn step_scope_input_range(current: u16, steps: i32) -> u16 {
    let current_index = DSCOPE_VOLTS_PER_DIV_MV
        .iter()
        .position(|candidate| *candidate == current)
        .unwrap_or(0) as i32;
    let maximum_index = DSCOPE_VOLTS_PER_DIV_MV.len() as i32 - 1;
    DSCOPE_VOLTS_PER_DIV_MV[(current_index + steps).clamp(0, maximum_index) as usize]
}

fn paint_scope_input_range_encoder(
    ui: &mut egui::Ui,
    channel_index: usize,
    volts_per_div_mv: &mut u16,
    channel_color: &mut Color32,
) -> bool {
    let mut steps = 0;
    ui.horizontal(|ui| {
        let encoder_id = ui.make_persistent_id(("scope_range_encoder", channel_index));
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(58.0), Sense::click_and_drag());
        let response = response
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_text(
                "Drag the knob vertically or scroll to rotate it · double-click to change color",
            );
        if response.clicked() {
            response.request_focus();
        }
        let popup_id = ui.make_persistent_id(("scope_channel_color_popup", channel_index));
        if response.double_clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }
        if response.hovered() {
            let scroll_delta = ui.input_mut(|input| {
                let delta = input.raw_scroll_delta.y;
                input.raw_scroll_delta.y = 0.0;
                delta
            });
            if scroll_delta.abs() > f32::EPSILON {
                steps += scroll_delta.signum() as i32;
            }
        }
        if response.dragged() {
            let pointer_delta = ui.input(|input| input.pointer.delta());
            let rotation_delta = pointer_delta.x - pointer_delta.y;
            let drag_steps = ui.data_mut(|data| {
                let accumulator = data.get_temp_mut_or_default::<f32>(encoder_id);
                *accumulator += rotation_delta;
                let whole_steps = (*accumulator / 12.0).trunc() as i32;
                *accumulator -= whole_steps as f32 * 12.0;
                whole_steps
            });
            steps += drag_steps;
        }
        if response.drag_stopped() {
            ui.data_mut(|data| data.remove_temp::<f32>(encoder_id));
        }
        if response.has_focus() {
            ui.input(|input| {
                if input.key_pressed(egui::Key::ArrowUp) || input.key_pressed(egui::Key::ArrowRight)
                {
                    steps += 1;
                }
                if input.key_pressed(egui::Key::ArrowDown)
                    || input.key_pressed(egui::Key::ArrowLeft)
                {
                    steps -= 1;
                }
            });
        }

        let current_index = DSCOPE_VOLTS_PER_DIV_MV
            .iter()
            .position(|candidate| *candidate == *volts_per_div_mv)
            .unwrap_or(0);
        let fraction = current_index as f32 / (DSCOPE_VOLTS_PER_DIV_MV.len() - 1) as f32;
        let center = rect.center();
        let radius = 22.0;
        let painter = ui.painter();
        painter.circle_filled(center, radius, channel_color.gamma_multiply(0.14));
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(1.4_f32, channel_color.gamma_multiply(0.7)),
        );
        for tick_index in 0..DSCOPE_VOLTS_PER_DIV_MV.len() {
            let tick_fraction = tick_index as f32 / (DSCOPE_VOLTS_PER_DIV_MV.len() - 1) as f32;
            let angle = std::f32::consts::PI * (0.75 + tick_fraction * 1.5);
            let direction = Vec2::new(angle.cos(), angle.sin());
            painter.line_segment(
                [center + direction * 25.0, center + direction * 28.0],
                Stroke::new(1.0_f32, channel_color.gamma_multiply(0.55)),
            );
        }
        let angle = std::f32::consts::PI * (0.75 + fraction * 1.5);
        let direction = Vec2::new(angle.cos(), angle.sin());
        painter.line_segment(
            [center, center + direction * 16.0],
            Stroke::new(2.8_f32, *channel_color),
        );
        painter.circle_filled(center, 2.5, *channel_color);

        ui.vertical(|ui| {
            ui.label(
                RichText::new(format_input_scale(*volts_per_div_mv))
                    .strong()
                    .color(*channel_color),
            );
            ui.small(RichText::new("drag / scroll").color(TEXT_MUTED));
        });

        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &response,
            egui::popup::PopupCloseBehavior::CloseOnClickOutside,
            |popup_ui| {
                popup_ui.set_min_width(280.0);
                popup_ui.label(RichText::new(format!("CH{channel_index} color")).strong());
                egui::color_picker::color_picker_color32(
                    popup_ui,
                    channel_color,
                    egui::color_picker::Alpha::Opaque,
                );
                if popup_ui.button("Reset color").clicked() {
                    *channel_color = SCOPE_COLORS[channel_index];
                }
            },
        );
    });

    let next = step_scope_input_range(*volts_per_div_mv, steps);
    let changed = next != *volts_per_div_mv;
    *volts_per_div_mv = next;
    changed
}

fn coupling_label(coupling: DsCopeCoupling) -> &'static str {
    match coupling {
        DsCopeCoupling::Dc => "DC",
        DsCopeCoupling::Ac => "AC",
    }
}

fn trigger_source_label(source: DsCopeTriggerSource, channel_names: &[String; 2]) -> String {
    match source {
        DsCopeTriggerSource::Auto => "Auto".to_owned(),
        DsCopeTriggerSource::ChannelZero => scope_channel_label(0, channel_names),
        DsCopeTriggerSource::ChannelOne => scope_channel_label(1, channel_names),
        DsCopeTriggerSource::ChannelZeroAndOne => format!(
            "{} AND {}",
            scope_channel_label(0, channel_names),
            scope_channel_label(1, channel_names)
        ),
        DsCopeTriggerSource::ChannelZeroOrOne => format!(
            "{} OR {}",
            scope_channel_label(0, channel_names),
            scope_channel_label(1, channel_names)
        ),
    }
}

fn trigger_slope_label(slope: DsCopeTriggerSlope) -> &'static str {
    match slope {
        DsCopeTriggerSlope::Rising => "Rising edge",
        DsCopeTriggerSlope::Falling => "Falling edge",
    }
}

fn paint_scope_trigger_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeTriggerSettings,
    channels: [bool; 2],
    channel_names: &[String; 2],
    enabled: bool,
) {
    egui::CollapsingHeader::new(RichText::new("Trigger").strong())
        .default_open(false)
        .show(ui, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                egui::Grid::new("dscope_trigger_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Source");
                        egui::ComboBox::from_id_salt("scope_trigger_source")
                            .selected_text(trigger_source_label(settings.source, channel_names))
                            .show_ui(ui, |ui| {
                                for source in [
                                    DsCopeTriggerSource::Auto,
                                    DsCopeTriggerSource::ChannelZero,
                                    DsCopeTriggerSource::ChannelOne,
                                    DsCopeTriggerSource::ChannelZeroAndOne,
                                    DsCopeTriggerSource::ChannelZeroOrOne,
                                ] {
                                    let mut candidate = *settings;
                                    candidate.source = source;
                                    ui.add_enabled_ui(
                                        candidate.source_is_available(channels),
                                        |ui| {
                                            ui.selectable_value(
                                                &mut settings.source,
                                                source,
                                                trigger_source_label(source, channel_names),
                                            );
                                        },
                                    );
                                }
                            });
                        ui.end_row();

                        if settings.source == DsCopeTriggerSource::Auto {
                            ui.label("Auto channel");
                            egui::ComboBox::from_id_salt("scope_trigger_auto_channel")
                                .selected_text(scope_channel_label(
                                    usize::from(settings.auto_channel),
                                    channel_names,
                                ))
                                .show_ui(ui, |ui| {
                                    for (channel, channel_enabled) in
                                        channels.into_iter().enumerate()
                                    {
                                        ui.add_enabled_ui(channel_enabled, |ui| {
                                            ui.selectable_value(
                                                &mut settings.auto_channel,
                                                channel as u8,
                                                scope_channel_label(channel, channel_names),
                                            );
                                        });
                                    }
                                });
                            ui.end_row();
                        }

                        ui.label("Edge");
                        egui::ComboBox::from_id_salt("scope_trigger_slope")
                            .selected_text(trigger_slope_label(settings.slope))
                            .show_ui(ui, |ui| {
                                for slope in
                                    [DsCopeTriggerSlope::Rising, DsCopeTriggerSlope::Falling]
                                {
                                    ui.selectable_value(
                                        &mut settings.slope,
                                        slope,
                                        trigger_slope_label(slope),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Holdoff");
                        ui.horizontal(|ui| {
                            let multiplier = settings.holdoff_unit.nanoseconds();
                            let mut value = settings.holdoff_ns as f64 / multiplier;
                            let maximum = DSCOPE_TRIGGER_HOLDOFF_MAX_NS as f64 / multiplier;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut value)
                                        .range(0.0..=maximum)
                                        .speed(0.1)
                                        .max_decimals(3),
                                )
                                .changed()
                            {
                                settings.holdoff_ns = (value * multiplier).round() as u64;
                            }
                            egui::ComboBox::from_id_salt("scope_trigger_holdoff_unit")
                                .selected_text(settings.holdoff_unit.label())
                                .show_ui(ui, |ui| {
                                    for unit in [
                                        TriggerHoldoffUnit::Microseconds,
                                        TriggerHoldoffUnit::Milliseconds,
                                        TriggerHoldoffUnit::Seconds,
                                    ] {
                                        ui.selectable_value(
                                            &mut settings.holdoff_unit,
                                            unit,
                                            unit.label(),
                                        );
                                    }
                                });
                        });
                        ui.end_row();

                        ui.label("Noise sensitivity");
                        ui.add(egui::Slider::new(
                            &mut settings.noise_sensitivity,
                            0..=DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX,
                        ));
                        ui.end_row();
                    });
            });
        });
}

fn paint_scope_channel_picker(
    ui: &mut egui::Ui,
    id: &'static str,
    channel: &mut usize,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(scope_channel_label(*channel, channel_names))
        .show_ui(ui, |ui| {
            for (index, enabled) in channels.into_iter().enumerate() {
                ui.add_enabled_ui(enabled, |ui| {
                    ui.selectable_value(channel, index, scope_channel_label(index, channel_names));
                });
            }
        });
}

fn paint_scope_function_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeFunctionSettings,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::CollapsingHeader::new(RichText::new("Functions").strong())
        .default_open(false)
        .show(ui, |ui| {
            ui.checkbox(&mut settings.math.enabled, "Math");
            ui.add_enabled_ui(settings.math.enabled, |ui| {
                egui::Grid::new("dscope_math_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Operation");
                        egui::ComboBox::from_id_salt("scope_math_operation")
                            .selected_text(settings.math.operation.label())
                            .show_ui(ui, |ui| {
                                for operation in [
                                    ScopeMathOperation::Add,
                                    ScopeMathOperation::Subtract,
                                    ScopeMathOperation::Multiply,
                                    ScopeMathOperation::Divide,
                                ] {
                                    ui.selectable_value(
                                        &mut settings.math.operation,
                                        operation,
                                        operation.label(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("First source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_math_left_channel",
                            &mut settings.math.left_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();

                        ui.label("Second source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_math_right_channel",
                            &mut settings.math.right_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();
                    });
            });

            ui.add_space(6.0);
            ui.checkbox(&mut settings.fft.enabled, "FFT");
            ui.add_enabled_ui(settings.fft.enabled, |ui| {
                egui::Grid::new("dscope_fft_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_fft_channel",
                            &mut settings.fft.channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();

                        ui.label("Length");
                        egui::ComboBox::from_id_salt("scope_fft_length")
                            .selected_text(settings.fft.length.to_string())
                            .show_ui(ui, |ui| {
                                for length in FFT_LENGTHS {
                                    ui.selectable_value(
                                        &mut settings.fft.length,
                                        length,
                                        length.to_string(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Window");
                        egui::ComboBox::from_id_salt("scope_fft_window")
                            .selected_text(settings.fft.window.label())
                            .show_ui(ui, |ui| {
                                for window in [
                                    ScopeFftWindow::Rectangular,
                                    ScopeFftWindow::Hann,
                                    ScopeFftWindow::Hamming,
                                    ScopeFftWindow::Blackman,
                                    ScopeFftWindow::FlatTop,
                                ] {
                                    ui.selectable_value(
                                        &mut settings.fft.window,
                                        window,
                                        window.label(),
                                    );
                                }
                            });
                        ui.end_row();
                    });
                ui.small(RichText::new("Magnitude in dBV").color(TEXT_MUTED));
            });

            ui.add_space(6.0);
            let lissajous_available = channels.into_iter().filter(|enabled| *enabled).count() >= 2;
            ui.add_enabled(
                lissajous_available,
                egui::Checkbox::new(&mut settings.lissajous.enabled, "Lissajous tab (XY)"),
            );
            ui.add_enabled_ui(settings.lissajous.enabled && lissajous_available, |ui| {
                egui::Grid::new("dscope_lissajous_settings")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("X source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_lissajous_x_channel",
                            &mut settings.lissajous.x_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();

                        ui.label("Y source");
                        paint_scope_channel_picker(
                            ui,
                            "scope_lissajous_y_channel",
                            &mut settings.lissajous.y_channel,
                            channels,
                            channel_names,
                        );
                        ui.end_row();
                    });
                ui.small(
                    RichText::new("Plots Y-channel voltage against X-channel voltage")
                        .color(TEXT_MUTED),
                );
            });
            if !lissajous_available {
                ui.small(RichText::new("Requires two enabled channels").color(TEXT_MUTED));
            }
        });
}

fn paint_scope_measurement_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeMeasurementSettings,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::CollapsingHeader::new(RichText::new("Measurements").strong())
        .default_open(false)
        .show(ui, |ui| {
            for (channel, channel_enabled) in channels.into_iter().enumerate() {
                let channel_settings = &mut settings.channels[channel];
                ui.add_enabled_ui(channel_enabled, |ui| {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut channel_settings.enabled,
                                RichText::new(format!(
                                    "{} measurements",
                                    scope_channel_label(channel, channel_names)
                                ))
                                .strong()
                                .color(SCOPE_COLORS[channel]),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Clear").clicked() {
                                        channel_settings.selected.fill(false);
                                    }
                                    if ui.small_button("All").clicked() {
                                        channel_settings.selected.fill(true);
                                    }
                                },
                            );
                        });
                        ui.add_enabled_ui(channel_settings.enabled, |ui| {
                            egui::Grid::new(("dscope_measurement_settings", channel))
                                .num_columns(2)
                                .spacing([12.0, 4.0])
                                .show(ui, |ui| {
                                    for (index, kind) in
                                        SCOPE_MEASUREMENT_KINDS.into_iter().enumerate()
                                    {
                                        ui.checkbox(
                                            &mut channel_settings.selected[kind.index()],
                                            kind.label(),
                                        );
                                        if index % 2 == 1 {
                                            ui.end_row();
                                        }
                                    }
                                });
                        });
                    });
                });
                if !channel_enabled {
                    ui.small(
                        RichText::new(format!(
                            "{} is disabled",
                            scope_channel_label(channel, channel_names)
                        ))
                        .color(TEXT_MUTED),
                    );
                }
                if channel + 1 < channels.len() {
                    ui.add_space(6.0);
                }
            }
            ui.small(RichText::new("Calculated over the visible window").color(TEXT_MUTED));
        });
}

fn paint_scope_marker_settings(
    ui: &mut egui::Ui,
    settings: &mut ScopeMarkerSettings,
    editor: &mut Option<ScopeMarkerEditor>,
    viewport: WaveformViewport,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    egui::CollapsingHeader::new(RichText::new("Markers").strong())
        .default_open(false)
        .show(ui, |ui| {
            for (channel, channel_enabled) in channels.into_iter().enumerate() {
                ui.add_enabled_ui(channel_enabled, |ui| {
                    ui.group(|ui| {
                        let total = settings.channels[channel].total_count();
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} markers",
                                    scope_channel_label(channel, channel_names)
                                ))
                                    .strong()
                                    .color(SCOPE_COLORS[channel]),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            total < SCOPE_MARKER_LIMIT,
                                            egui::Button::new("+ Add marker"),
                                        )
                                        .clicked()
                                    {
                                        *editor = Some(ScopeMarkerEditor::for_new(
                                            *settings,
                                            viewport,
                                            channel,
                                            ScopeMarkerKind::Time,
                                        ));
                                    }
                                    ui.monospace(format!("{total} / {SCOPE_MARKER_LIMIT}"));
                                },
                            );
                        });

                        let mut remove = None;
                        for kind in [ScopeMarkerKind::Time, ScopeMarkerKind::Voltage] {
                            let count = settings.channels[channel].count(kind);
                            for index in 0..count {
                                let (_, comparison, color) = settings.channels[channel]
                                    .marker(kind, index)
                                    .expect("marker index is active");
                                ui.horizontal(|ui| {
                                    ui.colored_label(
                                        color,
                                        RichText::new(format!(
                                            "{}{}{}",
                                            kind.short_label(),
                                            channel,
                                            index + 1
                                        ))
                                        .strong(),
                                    );
                                    ui.small(kind.label());
                                    ui.small(match comparison {
                                        Some(other) => format!(
                                            "vs {}{}{}",
                                            kind.short_label(),
                                            channel,
                                            other + 1
                                        ),
                                        None => "Standalone".to_owned(),
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .small_button("×")
                                                .on_hover_text("Remove marker")
                                                .clicked()
                                            {
                                                remove = Some((kind, index));
                                            }
                                            if ui.small_button("Edit…").clicked() {
                                                *editor = ScopeMarkerEditor::for_existing(
                                                    *settings,
                                                    ScopeMarkerKey {
                                                        channel,
                                                        kind,
                                                        index,
                                                    },
                                                );
                                            }
                                        },
                                    );
                                });
                            }
                        }
                        if let Some((kind, index)) = remove {
                            settings.channels[channel].remove_marker(kind, index);
                        }
                        if total > 0 {
                            ui.small(
                                RichText::new(
                                    "Drag time markers left/right and amplitude markers up/down in the OSC plot",
                                )
                                .color(TEXT_MUTED),
                            );
                        }
                    });
                });
                if !channel_enabled {
                    ui.small(
                        RichText::new(format!(
                            "{} is disabled",
                            scope_channel_label(channel, channel_names)
                        ))
                        .color(TEXT_MUTED),
                    );
                }
                if channel + 1 < channels.len() {
                    ui.add_space(6.0);
                }
            }
        });
}

fn paint_scope_marker_editor(
    context: &egui::Context,
    editor_state: &mut Option<ScopeMarkerEditor>,
    settings: &mut ScopeMarkerSettings,
    viewport: WaveformViewport,
    channels: [bool; 2],
    channel_names: &[String; 2],
) {
    let Some(mut editor) = *editor_state else {
        return;
    };
    let mut open = true;
    let mut cancel = false;
    let mut save = false;
    let screen_rect = context.input(|input| input.screen_rect());
    egui::Area::new(egui::Id::new("scope_marker_modal_backdrop"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen_rect.min)
        .show(context, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, Color32::from_black_alpha(96));
        });
    let title = if editor.original.is_some() {
        "Marker settings"
    } else {
        "Add marker"
    };
    egui::Window::new(title)
        .open(&mut open)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(360.0)
        .show(context, |ui| {
            ui.label(
                RichText::new(format!(
                    "{} marker",
                    scope_channel_label(editor.channel, channel_names)
                ))
                .strong()
                .color(SCOPE_COLORS[editor.channel]),
            );
            let kind_before = editor.kind;
            ui.horizontal(|ui| {
                ui.label("Type");
                egui::ComboBox::from_id_salt("scope_marker_kind")
                    .selected_text(editor.kind.label())
                    .show_ui(ui, |ui| {
                        for kind in [ScopeMarkerKind::Time, ScopeMarkerKind::Voltage] {
                            ui.selectable_value(&mut editor.kind, kind, kind.label());
                        }
                    });
            });
            if kind_before != editor.kind {
                editor.comparison = None;
                if editor.original.is_none() {
                    editor.position = scope_marker_default_position(editor.kind, viewport);
                }
            }

            ui.add_space(8.0);
            let position_label = match editor.kind {
                ScopeMarkerKind::Time => "Horizontal position",
                ScopeMarkerKind::Voltage => "Vertical position",
            };
            ui.label(position_label);
            let mut position_percent = editor.position * 100.0;
            if ui
                .add(
                    egui::Slider::new(&mut position_percent, 0.0..=100.0)
                        .suffix(" %")
                        .max_decimals(1),
                )
                .changed()
            {
                editor.position = position_percent / 100.0;
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Color");
                ui.color_edit_button_srgba(&mut editor.color);
            });

            ui.add_space(8.0);
            ui.label("Math");
            let marker_count = settings.channels[editor.channel].count(editor.kind);
            let original_index = editor
                .original
                .filter(|key| key.channel == editor.channel && key.kind == editor.kind)
                .map(|key| key.index);
            if editor
                .comparison
                .is_some_and(|index| index >= marker_count || Some(index) == original_index)
            {
                editor.comparison = None;
            }
            egui::ComboBox::from_id_salt("scope_marker_comparison")
                .selected_text(match editor.comparison {
                    Some(index) => format!(
                        "Compare with {}{}{}",
                        editor.kind.short_label(),
                        editor.channel,
                        index + 1
                    ),
                    None => "Standalone (no calculation)".to_owned(),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut editor.comparison,
                        None,
                        "Standalone (no calculation)",
                    );
                    for index in 0..marker_count {
                        if Some(index) == original_index {
                            continue;
                        }
                        ui.selectable_value(
                            &mut editor.comparison,
                            Some(index),
                            format!(
                                "{}{}{}",
                                editor.kind.short_label(),
                                editor.channel,
                                index + 1
                            ),
                        );
                    }
                });
            ui.small(
                RichText::new(match (editor.kind, editor.comparison) {
                    (ScopeMarkerKind::Time, Some(_)) => {
                        "The OSC plot displays Δt and the reciprocal frequency."
                    }
                    (ScopeMarkerKind::Voltage, Some(_)) => {
                        "The OSC plot displays ΔV using this channel's scale."
                    }
                    (_, None) => "Standalone markers do not display a delta calculation.",
                })
                .color(TEXT_MUTED),
            );

            let stays_on_channel = editor
                .original
                .is_some_and(|key| key.channel == editor.channel);
            let target_total = settings.channels[editor.channel].total_count();
            let can_save =
                channels[editor.channel] && (stays_on_channel || target_total < SCOPE_MARKER_LIMIT);
            if !can_save {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "This channel already has the maximum of {SCOPE_MARKER_LIMIT} markers."
                    ))
                    .color(Color32::LIGHT_RED),
                );
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui
                    .add_enabled(can_save, egui::Button::new("Save marker"))
                    .clicked()
                {
                    save = true;
                }
            });
        });

    if context.input(|input| input.key_pressed(egui::Key::Escape)) {
        cancel = true;
    }
    if save {
        editor.save(settings);
        *editor_state = None;
    } else if cancel || !open {
        *editor_state = None;
    } else {
        *editor_state = Some(editor);
    }
}

fn paint_scope_channel_settings(
    ui: &mut egui::Ui,
    channel_index: usize,
    settings: &mut ScopeChannelSettings,
    channel_color: &mut Color32,
    channel_name: &mut String,
    other_channel_enabled: bool,
    availability: CaptureSettingsAvailability,
) {
    egui::Frame::none()
        .fill(PANEL.gamma_multiply(0.72))
        .rounding(egui::Rounding::same(5.0))
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let can_toggle_enable = !settings.enabled || other_channel_enabled;
            let enable_label = format!(
                "{} enabled",
                channel_label("CH", channel_index, channel_name)
            );
            let enable_control = egui::Checkbox::new(&mut settings.enabled, enable_label);
            ui.add_enabled(
                availability.channel_enable && can_toggle_enable,
                enable_control,
            )
            .on_hover_text("At least one oscilloscope channel must remain enabled");
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add_enabled(
                    availability.capture,
                    egui::TextEdit::singleline(channel_name)
                        .char_limit(CHANNEL_NAME_MAX_CHARS)
                        .hint_text(format!("CH{channel_index}")),
                );
            });
            ui.add_enabled_ui(availability.input_range && settings.enabled, |ui| {
                ui.label("Input range");
                paint_scope_input_range_encoder(
                    ui,
                    channel_index,
                    &mut settings.volts_per_div_mv,
                    channel_color,
                );
            });
            ui.add_space(12.0);
            ui.add_enabled_ui(availability.capture && settings.enabled, |ui| {
                egui::Grid::new(("scope_channel_secondary_settings", channel_index))
                    .num_columns(2)
                    .spacing([8.0, 5.0])
                    .show(ui, |ui| {
                        ui.label("Coupling");
                        egui::ComboBox::from_id_salt(("scope_coupling", channel_index))
                            .selected_text(coupling_label(settings.coupling))
                            .show_ui(ui, |ui| {
                                for coupling in [DsCopeCoupling::Dc, DsCopeCoupling::Ac] {
                                    ui.selectable_value(
                                        &mut settings.coupling,
                                        coupling,
                                        coupling_label(coupling),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Probe");
                        egui::ComboBox::from_id_salt(("scope_probe", channel_index))
                            .selected_text(format!("{}×", settings.probe_attenuation))
                            .show_ui(ui, |ui| {
                                for attenuation in [1, 10, 100] {
                                    ui.selectable_value(
                                        &mut settings.probe_attenuation,
                                        attenuation,
                                        format!("{attenuation}×"),
                                    );
                                }
                            });
                        ui.end_row();
                    });
            });
        });
}

#[derive(Clone, Copy)]
struct CaptureSettingsAvailability {
    capture: bool,
    sample_rate: bool,
    channel_enable: bool,
    input_range: bool,
    manual_calibration: bool,
}

struct ScopeCaptureSettingsEditor<'a> {
    settings: &'a mut ScopeCaptureSettings,
    auto_sample_rate: &'a mut bool,
}

struct LogicCaptureSettingsEditor<'a> {
    settings: &'a mut LogicCaptureSettings,
    speed: UsbSpeedClass,
    acquisition_mode: &'a mut AcquisitionMode,
    repetitive_interval_seconds: &'a mut f32,
}

struct ScopeVisualizationSettingsEditor<'a> {
    channel_colors: &'a mut [Color32; 2],
    scope_channel_names: &'a mut [String; 2],
}

fn paint_capture_settings(
    ui: &mut egui::Ui,
    tab: CaptureSettingsTab,
    logic_editor: LogicCaptureSettingsEditor<'_>,
    scope_editor: ScopeCaptureSettingsEditor<'_>,
    visualization_editor: ScopeVisualizationSettingsEditor<'_>,
    availability: CaptureSettingsAvailability,
    calibration_action: &mut Option<ScopeCalibrationAction>,
) {
    let LogicCaptureSettingsEditor {
        settings: logic_settings,
        speed: logic_speed,
        acquisition_mode: logic_acquisition_mode,
        repetitive_interval_seconds,
    } = logic_editor;
    let ScopeCaptureSettingsEditor {
        settings: scope_settings,
        auto_sample_rate: scope_auto_sample_rate,
    } = scope_editor;
    let ScopeVisualizationSettingsEditor {
        channel_colors: scope_channel_colors,
        scope_channel_names,
    } = visualization_editor;
    let card_width = ui.available_width();
    egui::Frame::none()
        .fill(BACKGROUND)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            let content_width = (card_width - 20.0).max(0.0);
            ui.set_width(content_width);
            ui.set_max_width(content_width);
            ui.label(
                RichText::new(match tab {
                    CaptureSettingsTab::DsLogic => "DSLogic U3Pro16",
                    CaptureSettingsTab::DsCope => "DSCope U3P100",
                })
                .strong(),
            );
            ui.add_space(6.0);
            match tab {
                CaptureSettingsTab::DsLogic => {
                    if logic_settings.operation_mode == DsLogicOperationMode::InternalTest {
                        logic_settings.enabled_channels = [true; LOGIC_CHANNEL_COUNT];
                        set_logic_sample_rate_preserving_duration(
                            logic_settings,
                            100_000_000,
                        );
                    }
                    let maximum_sample_rate_hz =
                        logic_settings.maximum_sample_rate_hz(logic_speed);
                    if logic_settings.sample_rate_hz > maximum_sample_rate_hz {
                        let sample_rate_hz = DSLOGIC_SAMPLE_RATES_HZ
                            .iter()
                            .rev()
                            .copied()
                            .find(|rate| *rate <= maximum_sample_rate_hz)
                            .unwrap_or(SAMPLE_RATE_HZ);
                        set_logic_sample_rate_preserving_duration(
                            logic_settings,
                            sample_rate_hz,
                        );
                    }
                    let enabled_channel_count = logic_settings
                        .enabled_channels
                        .iter()
                        .filter(|enabled| **enabled)
                        .count();
                    egui::Grid::new("dslogic_capture_settings")
                        .num_columns(2)
                        .min_col_width(90.0)
                        .max_col_width(((content_width - 12.0) * 0.5).max(90.0))
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Samples");
                            ui.monospace(format_sample_count(logic_settings.sample_count));
                            ui.end_row();
                            ui.label("Maximum");
                            ui.monospace(format_sample_rate(maximum_sample_rate_hz));
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    let capture_summary = format!(
                        "Capture · {} @ {}",
                        logic_capture_duration_label(*logic_settings),
                        format_sample_rate(logic_settings.sample_rate_hz)
                    );
                    egui::CollapsingHeader::new(RichText::new(capture_summary).strong())
                        .id_salt("dslogic_capture_section")
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width());
                            egui::Grid::new("dslogic_capture_menu")
                                .num_columns(2)
                                .max_col_width(126.0)
                                .spacing([8.0, 6.0])
                                .show(ui, |ui| {
                                    ui.label("Mode");
                                    ui.add_enabled_ui(availability.capture, |ui| {
                                        let loop_available = logic_settings.operation_mode
                                            == DsLogicOperationMode::Stream;
                                        egui::ComboBox::from_id_salt("logic_acquisition_mode")
                                            .width(100.0)
                                            .selected_text(logic_acquisition_mode.logic_label())
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    logic_acquisition_mode,
                                                    AcquisitionMode::Single,
                                                    "Single",
                                                );
                                                ui.selectable_value(
                                                    logic_acquisition_mode,
                                                    AcquisitionMode::Repetitive,
                                                    "Repetitive",
                                                );
                                                ui.add_enabled_ui(loop_available, |ui| {
                                                    ui.selectable_value(
                                                        logic_acquisition_mode,
                                                        AcquisitionMode::Loop,
                                                        "Loop",
                                                    )
                                                    .on_disabled_hover_text(
                                                        "Loop requires DSLogic Stream mode",
                                                    );
                                                });
                                            });
                                    });
                                    ui.end_row();

                                    ui.label("Duration");
                                    ui.add_enabled_ui(availability.capture, |ui| {
                                        egui::ComboBox::from_id_salt("logic_capture_duration")
                                            .width(100.0)
                                            .selected_text(logic_capture_duration_label(
                                                *logic_settings,
                                            ))
                                            .show_ui(ui, |ui| {
                                                for (duration_seconds, sample_count) in
                                                    logic_capture_duration_options(
                                                        logic_settings.sample_rate_hz,
                                                    )
                                                {
                                                    ui.selectable_value(
                                                        &mut logic_settings.sample_count,
                                                        sample_count,
                                                        format_duration(Duration::from_secs_f64(
                                                            duration_seconds,
                                                        )),
                                                    );
                                                }
                                            });
                                    });
                                    ui.end_row();

                                    ui.label("Sample rate");
                                    ui.add_enabled_ui(
                                        availability.capture
                                            && logic_settings.operation_mode
                                                != DsLogicOperationMode::InternalTest,
                                        |ui| {
                                            let previous_rate = logic_settings.sample_rate_hz;
                                            egui::ComboBox::from_id_salt("logic_sample_rate")
                                                .width(100.0)
                                                .selected_text(format_sample_rate(
                                                    logic_settings.sample_rate_hz,
                                                ))
                                                .show_ui(ui, |ui| {
                                                    for rate in DSLOGIC_SAMPLE_RATES_HZ
                                                        .iter()
                                                        .copied()
                                                        .filter(|rate| {
                                                            *rate <= maximum_sample_rate_hz
                                                        })
                                                    {
                                                        ui.selectable_value(
                                                            &mut logic_settings.sample_rate_hz,
                                                            rate,
                                                            format_sample_rate(rate),
                                                        );
                                                    }
                                                });
                                            if logic_settings.sample_rate_hz != previous_rate {
                                                let selected_rate = logic_settings.sample_rate_hz;
                                                logic_settings.sample_rate_hz = previous_rate;
                                                set_logic_sample_rate_preserving_duration(
                                                    logic_settings,
                                                    selected_rate,
                                                );
                                            }
                                        },
                                    );
                                    ui.end_row();

                                    if *logic_acquisition_mode == AcquisitionMode::Repetitive {
                                        ui.label("Repeat interval");
                                        if logic_settings.operation_mode
                                            == DsLogicOperationMode::Stream
                                        {
                                            ui.label(RichText::new("0.1 s").color(TEXT_MUTED))
                                                .on_hover_text(
                                                    "Stream-mode Repetitive uses the original fixed 0.1 s interval",
                                                );
                                        } else {
                                            ui.add_enabled(
                                                availability.capture,
                                                egui::DragValue::new(
                                                    repetitive_interval_seconds,
                                                )
                                                .range(0.1..=10.0)
                                                .speed(0.1)
                                                .fixed_decimals(1)
                                                .suffix(" s"),
                                            )
                                            .on_hover_text("Delay between complete captures");
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(RichText::new("Options").strong())
                        .default_open(false)
                        .show(ui, |ui| {
                            const CONTROL_WIDTH: f32 = 100.0;
                            ui.set_max_width(ui.available_width());
                            ui.add_enabled_ui(availability.capture, |ui| {
                                egui::Grid::new("dslogic_options")
                                    .num_columns(2)
                                    .max_col_width(126.0)
                                    .spacing([8.0, 6.0])
                                    .show(ui, |ui| {
                                        ui.label("Operation mode");
                                        egui::ComboBox::from_id_salt("logic_operation_mode")
                                            .width(CONTROL_WIDTH)
                                            .selected_text(match logic_settings.operation_mode {
                                                DsLogicOperationMode::Buffer => "Buffer mode",
                                                DsLogicOperationMode::Stream => "Stream mode",
                                                DsLogicOperationMode::InternalTest => {
                                                    "Internal test"
                                                }
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut logic_settings.operation_mode,
                                                    DsLogicOperationMode::Buffer,
                                                    "Buffer mode",
                                                );
                                                ui.selectable_value(
                                                    &mut logic_settings.operation_mode,
                                                    DsLogicOperationMode::Stream,
                                                    "Stream mode",
                                                );
                                                ui.selectable_value(
                                                    &mut logic_settings.operation_mode,
                                                    DsLogicOperationMode::InternalTest,
                                                    "Internal test",
                                                );
                                            });
                                        ui.end_row();

                                        ui.label("Stop options");
                                        ui.add_enabled_ui(
                                            logic_settings.operation_mode
                                                != DsLogicOperationMode::Stream,
                                            |ui| {
                                                egui::ComboBox::from_id_salt("logic_stop_option")
                                                    .width(CONTROL_WIDTH)
                                                    .selected_text(match logic_settings.stop_option {
                                                        DsLogicStopOption::StopImmediately => {
                                                            "Stop immediately"
                                                        }
                                                        DsLogicStopOption::UploadCapturedData => {
                                                            "Upload data"
                                                        }
                                                    })
                                                    .show_ui(ui, |ui| {
                                                        ui.selectable_value(
                                                            &mut logic_settings.stop_option,
                                                            DsLogicStopOption::StopImmediately,
                                                            "Stop immediately",
                                                        );
                                                        ui.selectable_value(
                                                            &mut logic_settings.stop_option,
                                                            DsLogicStopOption::UploadCapturedData,
                                                            "Upload data",
                                                        );
                                                    });
                                            },
                                        );
                                        ui.end_row();

                                        ui.label("Threshold level");
                                        ui.add(
                                            egui::DragValue::new(
                                                &mut logic_settings.threshold_voltage,
                                            )
                                            .range(0.0..=5.0)
                                            .speed(0.1)
                                            .fixed_decimals(1)
                                            .suffix(" V"),
                                        );
                                        ui.end_row();

                                        ui.label("Filter targets");
                                        egui::ComboBox::from_id_salt("logic_input_filter")
                                            .width(CONTROL_WIDTH)
                                            .selected_text(match logic_settings.input_filter {
                                                DsLogicInputFilter::None => "None",
                                                DsLogicInputFilter::OneSampleClock => {
                                                    "1 sample clock"
                                                }
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut logic_settings.input_filter,
                                                    DsLogicInputFilter::None,
                                                    "None",
                                                );
                                                ui.selectable_value(
                                                    &mut logic_settings.input_filter,
                                                    DsLogicInputFilter::OneSampleClock,
                                                    "1 sample clock",
                                                );
                                            });
                                        ui.end_row();

                                        ui.label("Enable RLE compress");
                                        ui.add_enabled(
                                            logic_settings.operation_mode
                                                != DsLogicOperationMode::Stream,
                                            egui::Checkbox::without_text(
                                                &mut logic_settings.rle_enabled,
                                            ),
                                        );
                                        ui.end_row();

                                        ui.label("Using external clock");
                                        ui.checkbox(&mut logic_settings.external_clock, "");
                                        ui.end_row();

                                        ui.label("Using clock negedge");
                                        ui.checkbox(&mut logic_settings.falling_clock_edge, "");
                                        ui.end_row();
                                    });
                            });
                        });
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(
                        RichText::new(format!(
                            "Channels ({enabled_channel_count}/{LOGIC_CHANNEL_COUNT} enabled)"
                        ))
                        .strong(),
                    )
                    .id_salt("dslogic_channels_section")
                    .default_open(false)
                    .show(ui, |ui| {
                            let channel_controls_enabled = availability.capture
                                && logic_settings.operation_mode
                                    != DsLogicOperationMode::InternalTest;
                            ui.add_enabled_ui(channel_controls_enabled, |ui| {
                                egui::Grid::new("dslogic_channels")
                                    .num_columns(8)
                                    .min_col_width(20.0)
                                    .max_col_width(24.0)
                                    .spacing([5.0, 4.0])
                                    .show(ui, |ui| {
                                        for (channel, enabled) in logic_settings
                                            .enabled_channels
                                            .iter_mut()
                                            .enumerate()
                                        {
                                            ui.vertical_centered(|ui| {
                                                ui.small(channel.to_string());
                                                ui.add(egui::Checkbox::without_text(enabled));
                                            });
                                            if channel % 8 == 7 {
                                                ui.end_row();
                                            }
                                        }
                                    });
                                ui.horizontal(|ui| {
                                    if ui.small_button("Enable all").clicked() {
                                        logic_settings.enabled_channels =
                                            [true; LOGIC_CHANNEL_COUNT];
                                    }
                                    if ui.small_button("Disable all").clicked() {
                                        logic_settings.enabled_channels =
                                            [false; LOGIC_CHANNEL_COUNT];
                                    }
                                });
                            });
                            if !logic_settings
                                .enabled_channels
                                .iter()
                                .any(|enabled| *enabled)
                            {
                                ui.small(
                                    RichText::new("Enable at least one channel")
                                        .color(Color32::LIGHT_RED),
                                );
                            }
                        });
                }
                CaptureSettingsTab::DsCope => {
                    let maximum_sample_rate_hz = scope_settings.maximum_sample_rate_hz();
                    let maximum_sample_count = scope_settings.maximum_sample_count();
                    if scope_settings.sample_rate_hz > maximum_sample_rate_hz {
                        scope_settings.sample_rate_hz = maximum_sample_rate_hz;
                    }
                    if scope_settings.sample_count > maximum_sample_count {
                        scope_settings.sample_count = maximum_sample_count;
                    }
                    egui::Grid::new("dscope_common_capture_settings")
                        .num_columns(2)
                        .spacing([8.0, 5.0])
                        .show(ui, |ui| {
                            ui.label("Sample rate");
                            ui.horizontal(|ui| {
                                ui.add_enabled_ui(
                                    availability.sample_rate && !*scope_auto_sample_rate,
                                    |ui| {
                                        egui::ComboBox::from_id_salt("scope_sample_rate")
                                            .width(100.0)
                                            .selected_text(format_sample_rate(
                                                scope_settings.sample_rate_hz,
                                            ))
                                            .show_ui(ui, |ui| {
                                                for rate in DSCOPE_SAMPLE_RATES_HZ
                                                    .into_iter()
                                                    .filter(|rate| *rate <= maximum_sample_rate_hz)
                                                {
                                                    ui.selectable_value(
                                                        &mut scope_settings.sample_rate_hz,
                                                        rate,
                                                        format_sample_rate(rate),
                                                    );
                                                }
                                            });
                                    },
                                );
                                ui.add_enabled(
                                    availability.sample_rate,
                                    egui::Checkbox::new(scope_auto_sample_rate, "Auto"),
                                );
                            });
                            ui.end_row();

                            ui.label("Buffer");
                            ui.add_enabled_ui(availability.capture, |ui| {
                                egui::ComboBox::from_id_salt("scope_sample_count")
                                    .selected_text(format_sample_count(scope_settings.sample_count))
                                    .show_ui(ui, |ui| {
                                        for count in DSCOPE_SAMPLE_COUNTS
                                            .into_iter()
                                            .filter(|count| *count <= maximum_sample_count)
                                        {
                                            ui.selectable_value(
                                                &mut scope_settings.sample_count,
                                                count,
                                                format_sample_count(count),
                                            );
                                        }
                                    });
                            });
                            ui.end_row();

                            ui.label("Maximum");
                            ui.monospace(format_sample_rate(maximum_sample_rate_hz));
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(RichText::new("Options").strong())
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.add_enabled_ui(availability.capture, |ui| {
                                egui::Grid::new("dscope_options")
                                    .num_columns(2)
                                    .spacing([8.0, 6.0])
                                    .show(ui, |ui| {
                                        ui.label("Operating mode");
                                        egui::ComboBox::from_id_salt("scope_operation_mode")
                                            .selected_text(match scope_settings.operation_mode {
                                                DsCopeOperationMode::Normal => "Normal",
                                                DsCopeOperationMode::InternalTest => {
                                                    "Internal test"
                                                }
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut scope_settings.operation_mode,
                                                    DsCopeOperationMode::Normal,
                                                    "Normal",
                                                );
                                                ui.selectable_value(
                                                    &mut scope_settings.operation_mode,
                                                    DsCopeOperationMode::InternalTest,
                                                    "Internal test",
                                                );
                                            });
                                        ui.end_row();

                                        ui.label("Bandwidth");
                                        egui::ComboBox::from_id_salt("scope_bandwidth_limit")
                                            .selected_text(match scope_settings.bandwidth_limit {
                                                DsCopeBandwidthLimit::Full => "Full",
                                                DsCopeBandwidthLimit::Mhz20 => "20 MHz",
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut scope_settings.bandwidth_limit,
                                                    DsCopeBandwidthLimit::Full,
                                                    "Full",
                                                );
                                                ui.selectable_value(
                                                    &mut scope_settings.bandwidth_limit,
                                                    DsCopeBandwidthLimit::Mhz20,
                                                    "20 MHz",
                                                );
                                            });
                                        ui.end_row();
                                    });
                            });
                            ui.add_space(6.0);
                            ui.label(RichText::new("Calibration").color(TEXT_MUTED));
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(availability.capture, egui::Button::new("Auto…"))
                                    .clicked()
                                {
                                    *calibration_action = Some(ScopeCalibrationAction::Auto);
                                }
                                if ui
                                    .add_enabled(
                                        availability.manual_calibration,
                                        egui::Button::new("Manual…"),
                                    )
                                    .clicked()
                                {
                                    *calibration_action = Some(ScopeCalibrationAction::Manual);
                                }
                            });
                        });
                    ui.add_space(8.0);
                    let channel_count = scope_settings.channels.len();
                    let channel_enabled = scope_settings.channels.map(|channel| channel.enabled);
                    for (index, channel) in scope_settings.channels.iter_mut().enumerate() {
                        paint_scope_channel_settings(
                            ui,
                            index,
                            channel,
                            &mut scope_channel_colors[index],
                            &mut scope_channel_names[index],
                            channel_enabled[1 - index],
                            availability,
                        );
                        if index + 1 < channel_count {
                            ui.add_space(6.0);
                        }
                    }
                }
            }
        });
    scope_settings.clamp_to_channel_limits();
}

struct ScopeToolsOptions {
    viewport: WaveformViewport,
    trigger_controls_enabled: bool,
}

fn paint_scope_tools(
    ui: &mut egui::Ui,
    scope_settings: &mut ScopeCaptureSettings,
    function_settings: &mut ScopeFunctionSettings,
    marker_settings: &mut ScopeMarkerSettings,
    marker_editor: &mut Option<ScopeMarkerEditor>,
    channel_names: &[String; 2],
    options: ScopeToolsOptions,
) {
    let ScopeToolsOptions {
        viewport,
        trigger_controls_enabled,
    } = options;
    let channel_enabled = scope_settings.channels.map(|channel| channel.enabled);
    scope_settings.trigger.clamp_to_channels(channel_enabled);
    function_settings.clamp_to_channels(channel_enabled);

    paint_scope_trigger_settings(
        ui,
        &mut scope_settings.trigger,
        channel_enabled,
        channel_names,
        trigger_controls_enabled,
    );
    ui.add_space(8.0);
    paint_scope_function_settings(ui, function_settings, channel_enabled, channel_names);
    ui.add_space(8.0);
    paint_scope_measurement_settings(
        ui,
        &mut function_settings.measurements,
        channel_enabled,
        channel_names,
    );
    ui.add_space(8.0);
    paint_scope_marker_settings(
        ui,
        marker_settings,
        marker_editor,
        viewport,
        channel_enabled,
        channel_names,
    );
}

fn paint_calibration_progress_modal(
    context: &egui::Context,
    state: &CalibrationProgressModal,
) -> Option<CalibrationModalAction> {
    let screen_rect = context.input(|input| input.screen_rect());
    egui::Area::new(egui::Id::new("calibration_progress_modal_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen_rect.min)
        .show(context, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, Color32::from_black_alpha(112));
        });

    let mut action = None;
    egui::Window::new("DSCope Auto Calibration")
        .id(egui::Id::new("calibration_progress_modal"))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(430.0)
        .show(context, |ui| {
            if state.completed {
                ui.label(RichText::new(&state.phase).strong().color(ACCENT));
                ui.add_space(8.0);
                ui.add(egui::ProgressBar::new(1.0).show_percentage().fill(ACCENT));
                ui.add_space(12.0);
                if let Some(values) = state.values {
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            action = Some(CalibrationModalAction::Close);
                        }
                        if ui.button("Save").clicked() {
                            action = Some(CalibrationModalAction::Save(values));
                        }
                    });
                } else if ui.button("Close").clicked() {
                    action = Some(CalibrationModalAction::Close);
                }
            } else {
                ui.label(RichText::new(&state.phase).strong());
                ui.add_space(8.0);
                ui.add(
                    egui::ProgressBar::new(state.progress)
                        .show_percentage()
                        .animate(true)
                        .fill(ACCENT),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Keep all probes disconnected. Do not disconnect the oscilloscope.",
                    )
                    .color(Color32::LIGHT_RED),
                );
            }
        });
    action
}

fn paint_instrument_settings(
    ui: &mut egui::Ui,
    devices: &mut [DeviceEntry],
    selection_enabled: bool,
) {
    if devices.is_empty() {
        ui.label(RichText::new("No supported devices").color(TEXT_MUTED));
        return;
    }

    for entry in devices {
        ui.add_space(4.0);
        egui::Frame::none()
            .fill(BACKGROUND)
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(10.0))
            .show(ui, |ui| {
                ui.add_enabled(
                    selection_enabled,
                    egui::Checkbox::new(
                        &mut entry.selected,
                        RichText::new(entry.device.profile.display_name).strong(),
                    ),
                );
                ui.small(RichText::new(entry.device.location.stable_id()).color(TEXT_MUTED));
                let (link_text, link_color) = match entry.device.link_status() {
                    LinkStatus::Optimal => ("USB 3 · optimal", ACCENT),
                    LinkStatus::ReducedPerformance => {
                        ("USB 2 · reduced performance", SCOPE_COLORS[0])
                    }
                    LinkStatus::Unsupported => ("Unsupported link speed", Color32::LIGHT_RED),
                };
                ui.small(RichText::new(link_text).color(link_color));
            });
    }
}

fn usb_speed_warning(devices: &[DiscoveredDevice]) -> Option<UsbSpeedWarning> {
    let devices = devices
        .iter()
        .filter(|device| device.speed < UsbSpeedClass::SuperSpeed)
        .cloned()
        .collect::<Vec<_>>();
    (!devices.is_empty()).then_some(UsbSpeedWarning { devices })
}

fn paint_usb_speed_warning(context: &egui::Context, warning: &UsbSpeedWarning) -> bool {
    let mut dismissed = false;
    let screen_rect = context.input(|input| input.screen_rect());
    egui::Area::new(egui::Id::new("usb_speed_warning_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen_rect.min)
        .show(context, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen_rect.size(), Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, Color32::from_black_alpha(112));
        });

    egui::Area::new(egui::Id::new("usb_speed_warning"))
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .order(egui::Order::Foreground)
        .movable(false)
        .show(context, |ui| {
            egui::Frame::window(&context.style())
                .inner_margin(egui::Margin::same(12.0))
                .show(ui, |ui| {
                    ui.set_max_width(340.0);
                    ui.label(
                        RichText::new("USB 3 connection recommended")
                            .strong()
                            .color(SCOPE_COLORS[0]),
                    );
                    ui.add_space(4.0);
                    for device in &warning.devices {
                        ui.add(
                            egui::Label::new(format!(
                                "{} is using {}.",
                                device.profile.display_name,
                                device.speed.description()
                            ))
                            .wrap(),
                        );
                    }
                    ui.add_space(4.0);
                    ui.label("Use a USB 3 port and cable for full performance.");
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("OK").clicked() {
                            dismissed = true;
                        }
                    });
                });
        });
    dismissed
}

impl eframe::App for DsViewApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_calibration_progress();
        self.poll_worker();
        self.reconcile_capture_settings_tab();
        self.reconcile_waveform_tab();
        self.poll_capture();
        self.poll_repetitive_capture();
        self.finish_pending_snapshot(context);

        let mut refresh_clicked = false;
        let mut snapshot_clicked = false;
        let mut record_now_clicked = false;
        let mut open_recording_clicked = false;
        let mut start_request = None;
        let mut stop_clicked = false;
        let mut calibration_action = None;
        let capture_settings_enabled = !self.is_busy();
        let control_target = self.active_waveform_tab.map(WaveformTab::capture_target);
        let selected_logic_device = self.devices.iter().any(|entry| {
            entry.selected && entry.device.profile.model == DeviceModel::DsLogicU3Pro16
        });
        let logic_settings_speed = self
            .devices
            .iter()
            .filter(|entry| {
                entry.selected && entry.device.profile.model == DeviceModel::DsLogicU3Pro16
            })
            .map(|entry| entry.device.speed)
            .min()
            .or_else(|| {
                self.devices
                    .iter()
                    .filter(|entry| entry.device.profile.model == DeviceModel::DsLogicU3Pro16)
                    .map(|entry| entry.device.speed)
                    .min()
            })
            .unwrap_or(UsbSpeedClass::HighSpeed);
        let selected_scope_device = self
            .devices
            .iter()
            .any(|entry| entry.selected && entry.device.profile.model == DeviceModel::DsCopeU3P100);
        let control_target_available = control_target.is_some_and(|target| {
            target.is_available(selected_logic_device, selected_scope_device)
        });
        let scope_capture_requested = self.capture_run_requested
            && self
                .requested_capture_target
                .is_some_and(CaptureTarget::includes_oscilloscope);
        let manual_calibration_enabled = manual_calibration_control_is_enabled(
            self.pending.is_some(),
            selected_scope_device,
            self.manual_calibration.is_some(),
            self.capture
                .as_ref()
                .filter(|active| active.target.includes_oscilloscope())
                .map(|active| active.mode),
        );
        let trigger_controls_enabled = scope_trigger_controls_are_enabled(
            self.pending.is_some(),
            scope_capture_requested,
            selected_scope_device,
            self.capture
                .as_ref()
                .map(|active| !active.scope_trigger_controls.is_empty()),
        );
        let frontend_controls_enabled = scope_frontend_controls_are_enabled(
            self.pending.is_some(),
            scope_capture_requested,
            selected_scope_device,
        ) && !(self.record_capture && self.is_busy());
        let scope_settings_before_ui = self.scope_settings;
        let scope_auto_sample_rate_before_ui = self.scope_auto_sample_rate;
        let manual_calibration_before_ui =
            self.manual_calibration.as_ref().map(|editor| editor.values);
        let manual_calibration_range_before_ui =
            self.manual_calibration.as_ref().map(|editor| editor.range);

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
                    ui.menu_button("App", |ui| {
                        if ui.button("Settings…").clicked() {
                            self.settings_open = true;
                            ui.close_menu();
                        }
                    });
                    ui.separator();
                    ui.label(RichText::new("USB capture workbench").color(TEXT_MUTED));
                    refresh_clicked = ui
                        .add_enabled(
                            !self.is_busy(),
                            toolbar_action_button("↻ Refresh", SCOPE_COLORS[1]),
                        )
                        .clicked();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let logic_only = control_target == Some(CaptureTarget::LogicAnalyzer);
                        let acquisition_active = self.capture_run_requested
                            && self.requested_capture_target == control_target
                            && (self.capture.is_some()
                                || self.pending.is_some()
                                || self.scheduled_repetitive_capture.is_some());
                        let start_mode = if logic_only {
                            self.logic_acquisition_mode
                        } else {
                            AcquisitionMode::Run
                        };
                        let run_stop_clicked = ui
                            .add_enabled(
                                acquisition_active
                                    || (!self.is_busy()
                                        && control_target_available
                                        && !self.record_capture),
                                if acquisition_active {
                                    toolbar_action_button("■ Stop", Color32::LIGHT_RED)
                                } else {
                                    toolbar_action_button("▶ Run", ACCENT)
                                },
                            )
                            .on_hover_text(if acquisition_active {
                                "Stop the active acquisition"
                            } else if logic_only {
                                match start_mode {
                                    AcquisitionMode::Single => {
                                        "Wait for one trigger, acquire once, then stop"
                                    }
                                    AcquisitionMode::Repetitive => {
                                        "Repeatedly acquire complete captures"
                                    }
                                    AcquisitionMode::Loop => {
                                        "Continuously append stream captures to the rolling display"
                                    }
                                    AcquisitionMode::Run => "Start the selected acquisition",
                                }
                            } else {
                                "Run continuously; Auto trigger rolls, edge triggers update the display"
                            })
                            .clicked();
                        if run_stop_clicked {
                            if acquisition_active {
                                stop_clicked = true;
                            } else if let Some(target) = control_target {
                                start_request = Some((start_mode, target));
                            }
                        }
                        if ui
                            .add_enabled(
                                !self.is_busy() && control_target_available,
                                toolbar_action_button("▶ Single", SCOPE_COLORS[0]),
                            )
                            .on_hover_text("Wait for one trigger, acquire once, then stop")
                            .clicked()
                        {
                            if let Some(target) = control_target {
                                start_request = Some((AcquisitionMode::Single, target));
                            }
                        }
                        if ui
                            .add_enabled(
                                !self.is_busy(),
                                toolbar_action_button("↑ Open capture", TEXT_MUTED),
                            )
                            .on_hover_text("Open a saved .dsv capture")
                            .clicked()
                        {
                            open_recording_clicked = true;
                        }
                        if ui
                            .add_enabled(
                                !self.is_busy() && control_target_available,
                                toolbar_action_button("● Record", Color32::LIGHT_RED),
                            )
                            .on_hover_text("Record a complete Single capture to a .dsv file")
                            .clicked()
                        {
                            record_now_clicked = true;
                        }
                        ui.add_space(12.0);
                        if ui
                            .add_enabled(
                                self.snapshot_plot_rect.is_some()
                                    && self.pending_snapshot.is_none(),
                                toolbar_action_button("📷", TEXT_MUTED),
                            )
                            .on_hover_text("Save the visible waveform plot as a PNG snapshot")
                            .clicked()
                        {
                            snapshot_clicked = true;
                        }
                    });
                });
            });

        let available_waveform_tabs = self.available_waveform_tabs();
        if !available_waveform_tabs.is_empty() {
            egui::TopBottomPanel::top("waveform_tabs")
                .frame(
                    egui::Frame::none()
                        .fill(PANEL)
                        .inner_margin(egui::Margin::symmetric(14.0, 7.0)),
                )
                .show(context, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("VIEW").strong().color(TEXT_MUTED));
                        ui.separator();
                        for tab in &available_waveform_tabs {
                            if ui
                                .selectable_label(
                                    self.active_waveform_tab == Some(*tab),
                                    tab.label(),
                                )
                                .clicked()
                            {
                                self.select_waveform_tab(*tab);
                            }
                        }
                    });
                });
        }

        egui::SidePanel::left("capture_setup_fixed")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(context, |ui| {
                let available_capture_settings_tabs = self.available_capture_settings_tabs();
                let content_width = ui.available_width();
                egui::ScrollArea::vertical()
                    .max_width(content_width)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(content_width);
                        ui.set_max_width(content_width);
                        ui.label(RichText::new("CAPTURE SETUP").strong().color(TEXT_MUTED));
                        ui.add_space(8.0);
                        if available_capture_settings_tabs.len() > 1 {
                            ui.horizontal(|ui| {
                                for tab in &available_capture_settings_tabs {
                                    ui.selectable_value(
                                        &mut self.active_capture_settings_tab,
                                        *tab,
                                        tab.label(),
                                    );
                                }
                            });
                            ui.add_space(6.0);
                        }
                        if available_capture_settings_tabs.is_empty() {
                            ui.label(
                                RichText::new(
                                    "Connect a supported instrument to configure capture.",
                                )
                                .color(TEXT_MUTED),
                            );
                        } else {
                            paint_capture_settings(
                                ui,
                                self.active_capture_settings_tab,
                                LogicCaptureSettingsEditor {
                                    settings: &mut self.logic_settings,
                                    speed: logic_settings_speed,
                                    acquisition_mode: &mut self.logic_acquisition_mode,
                                    repetitive_interval_seconds: &mut self
                                        .repetitive_interval_seconds,
                                },
                                ScopeCaptureSettingsEditor {
                                    settings: &mut self.scope_settings,
                                    auto_sample_rate: &mut self.scope_auto_sample_rate,
                                },
                                ScopeVisualizationSettingsEditor {
                                    channel_colors: &mut self.scope_channel_colors,
                                    scope_channel_names: &mut self.scope_channel_names,
                                },
                                CaptureSettingsAvailability {
                                    capture: capture_settings_enabled,
                                    sample_rate: frontend_controls_enabled,
                                    channel_enable: frontend_controls_enabled,
                                    input_range: frontend_controls_enabled,
                                    manual_calibration: manual_calibration_enabled,
                                },
                                &mut calibration_action,
                            );
                        }
                    });
            });

        if self.logic_acquisition_mode == AcquisitionMode::Loop
            && self.logic_settings.operation_mode != DsLogicOperationMode::Stream
        {
            self.logic_acquisition_mode = AcquisitionMode::Single;
        }

        if scope_tools_panel_is_visible(self.active_waveform_tab) {
            egui::SidePanel::right("scope_tools_fixed")
                .resizable(false)
                .exact_width(300.0)
                .frame(
                    egui::Frame::none()
                        .fill(PANEL)
                        .inner_margin(egui::Margin::same(14.0)),
                )
                .show(context, |ui| {
                    let content_width = ui.available_width();
                    egui::ScrollArea::vertical()
                        .max_width(content_width)
                        .show(ui, |ui| {
                            ui.set_width(content_width);
                            ui.label(RichText::new("SCOPE TOOLS").strong().color(TEXT_MUTED));
                            ui.add_space(8.0);
                            egui::Frame::none()
                                .fill(BACKGROUND)
                                .rounding(egui::Rounding::same(6.0))
                                .inner_margin(egui::Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.set_width((content_width - 20.0).max(0.0));
                                    paint_scope_tools(
                                        ui,
                                        &mut self.scope_settings,
                                        &mut self.scope_function_settings,
                                        &mut self.scope_marker_settings,
                                        &mut self.scope_marker_editor,
                                        &self.scope_channel_names,
                                        ScopeToolsOptions {
                                            viewport: self.scope_viewport,
                                            trigger_controls_enabled,
                                        },
                                    );
                                });
                        });
                });
        }

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

        let mut snapshot_plot_rect = None;
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

                let empty_waveforms = WaveformStore::default();
                let waveforms = self
                    .capture
                    .as_ref()
                    .filter(|active| !active.preserve_display_until_complete)
                    .map(|active| &active.waveforms)
                    .or_else(|| {
                        self.calibration_active
                            .then_some(self.calibration_display.as_ref())
                            .flatten()
                            .map(|display| &display.waveforms)
                    })
                    .or_else(|| {
                        self.completed_capture
                            .as_ref()
                            .map(|capture| &capture.waveforms)
                    })
                    .unwrap_or(&empty_waveforms);
                let mut combined_logic_ratio = self.combined_logic_ratio;
                let mut logic_channel_colors = self.logic_channel_colors;
                let mut logic_channel_marker_opacities = self.logic_channel_marker_opacities;
                let mut logic_channel_names = self.logic_channel_names.clone();
                let mut logic_channel_settings_drafts = self.logic_channel_settings_drafts.clone();
                let mut logic_enabled_channels = self.logic_settings.enabled_channels;
                let mut logic_maximum_heights = self.logic_settings.maximum_heights;
                let mut logic_viewport = self.logic_viewport;
                let mut scope_viewport = self.scope_viewport;
                let mut scope_fft_viewport = self.scope_fft_viewport;
                let mut scope_measurement_caches = self.scope_measurement_caches;
                let mut scope_measurement_window_positions =
                    self.scope_measurement_window_positions;
                let mut scope_marker_settings = self.scope_marker_settings;
                let mut scope_quick_time_measurement = self.scope_quick_time_measurement;
                let mut scope_time_per_division_seconds = self.scope_time_per_division_seconds;
                let mut scope_timebase_changed = false;
                let mut scope_vertical_offsets = self
                    .scope_settings
                    .channels
                    .map(|channel| channel.vertical_offset);
                let mut scope_trigger_settings = self.scope_settings.trigger;
                let scope_settings = self
                    .capture
                    .as_ref()
                    .map(|active| ScopeCaptureSettings::from_capture_config(active.scope_config))
                    .or_else(|| {
                        self.calibration_active
                            .then_some(self.calibration_display.as_ref())
                            .flatten()
                            .map(|display| {
                                ScopeCaptureSettings::from_capture_config(display.scope_config)
                            })
                    })
                    .unwrap_or(self.scope_settings);
                match self.active_waveform_tab {
                    Some(WaveformTab::LogicAnalyzer) => {
                        snapshot_plot_rect = Some(waveform_card_fill(
                            ui,
                            "DIGITAL",
                            "DSLogic · channels 0–15",
                            |ui, rect| {
                                paint_logic(
                                    ui,
                                    rect,
                                    waveforms,
                                    LogicWaveformOptions {
                                        channel_colors: &mut logic_channel_colors,
                                        channel_marker_opacities:
                                            &mut logic_channel_marker_opacities,
                                        channel_names: &mut logic_channel_names,
                                        channel_settings_drafts: &mut logic_channel_settings_drafts,
                                        maximum_heights: &mut logic_maximum_heights,
                                        enabled_channels: &mut logic_enabled_channels,
                                    },
                                    &mut logic_viewport,
                                )
                            },
                        ));
                    }
                    Some(WaveformTab::Oscilloscope) => {
                        let subtitle = scope_channels_subtitle(&self.scope_channel_names);
                        snapshot_plot_rect = Some(waveform_card_fill(
                            ui,
                            "OSCILLOSCOPE",
                            &subtitle,
                            |ui, rect| {
                                paint_scope(
                                    ui,
                                    rect,
                                    waveforms,
                                    &mut scope_viewport,
                                    &mut scope_fft_viewport,
                                    true,
                                    ScopeWaveformOptions {
                                        settings: scope_settings,
                                        function_settings: self.scope_function_settings,
                                        channel_names: &self.scope_channel_names,
                                        measurement_caches: &mut scope_measurement_caches,
                                        measurement_window_positions:
                                            &mut scope_measurement_window_positions,
                                        marker_settings: &mut scope_marker_settings,
                                        quick_time_measurement: &mut scope_quick_time_measurement,
                                        channel_colors: self.scope_channel_colors,
                                        vertical_offsets: &mut scope_vertical_offsets,
                                        trigger_settings: &mut scope_trigger_settings,
                                        time_per_division_seconds:
                                            &mut scope_time_per_division_seconds,
                                        timebase_changed: &mut scope_timebase_changed,
                                        offset_controls_enabled: frontend_controls_enabled,
                                        trigger_controls_enabled,
                                    },
                                )
                            },
                        ));
                    }
                    Some(WaveformTab::Lissajous) => {
                        let lissajous = self.scope_function_settings.lissajous;
                        let subtitle = format!(
                            "DSCope · {} → X / {} → Y",
                            scope_channel_label(lissajous.x_channel, &self.scope_channel_names),
                            scope_channel_label(lissajous.y_channel, &self.scope_channel_names)
                        );
                        let mut lissajous_scope_settings = scope_settings;
                        for (channel, vertical_offset) in lissajous_scope_settings
                            .channels
                            .iter_mut()
                            .zip(scope_vertical_offsets)
                        {
                            channel.vertical_offset = vertical_offset;
                        }
                        snapshot_plot_rect = Some(waveform_card_fill(
                            ui,
                            "LISSAJOUS",
                            &subtitle,
                            |ui, rect| {
                                paint_scope_lissajous(
                                    ui,
                                    rect,
                                    waveforms,
                                    ScopeLissajousPaintOptions {
                                        viewport: scope_viewport,
                                        settings: lissajous_scope_settings,
                                        lissajous,
                                        channel_colors: self.scope_channel_colors,
                                        channel_names: &self.scope_channel_names,
                                    },
                                )
                            },
                        ));
                    }
                    Some(WaveformTab::Combined) => {
                        snapshot_plot_rect = Some(combined_waveform_cards(
                            ui,
                            waveforms,
                            &mut combined_logic_ratio,
                            LogicWaveformOptions {
                                channel_colors: &mut logic_channel_colors,
                                channel_marker_opacities: &mut logic_channel_marker_opacities,
                                channel_names: &mut logic_channel_names,
                                channel_settings_drafts: &mut logic_channel_settings_drafts,
                                maximum_heights: &mut logic_maximum_heights,
                                enabled_channels: &mut logic_enabled_channels,
                            },
                            CombinedWaveformViewports {
                                logic: &mut logic_viewport,
                                scope: &mut scope_viewport,
                                fft: &mut scope_fft_viewport,
                            },
                            ScopeWaveformOptions {
                                settings: scope_settings,
                                function_settings: self.scope_function_settings,
                                channel_names: &self.scope_channel_names,
                                measurement_caches: &mut scope_measurement_caches,
                                measurement_window_positions:
                                    &mut scope_measurement_window_positions,
                                marker_settings: &mut scope_marker_settings,
                                quick_time_measurement: &mut scope_quick_time_measurement,
                                channel_colors: self.scope_channel_colors,
                                vertical_offsets: &mut scope_vertical_offsets,
                                trigger_settings: &mut scope_trigger_settings,
                                time_per_division_seconds: &mut scope_time_per_division_seconds,
                                timebase_changed: &mut scope_timebase_changed,
                                offset_controls_enabled: frontend_controls_enabled,
                                trigger_controls_enabled,
                            },
                        ));
                    }
                    None => {}
                }
                self.combined_logic_ratio = combined_logic_ratio;
                self.logic_channel_colors = logic_channel_colors;
                self.logic_channel_marker_opacities = logic_channel_marker_opacities;
                self.logic_channel_names = logic_channel_names;
                self.logic_channel_settings_drafts = logic_channel_settings_drafts;
                self.logic_settings.enabled_channels = logic_enabled_channels;
                self.logic_settings.maximum_heights = logic_maximum_heights;
                self.logic_viewport = logic_viewport;
                self.scope_viewport = scope_viewport;
                self.scope_fft_viewport = scope_fft_viewport;
                self.scope_measurement_caches = scope_measurement_caches;
                self.scope_measurement_window_positions = scope_measurement_window_positions;
                self.scope_marker_settings = scope_marker_settings;
                self.scope_quick_time_measurement = scope_quick_time_measurement;
                self.scope_time_per_division_seconds = scope_time_per_division_seconds;
                let automatic_sample_rate_inputs_changed = scope_timebase_changed
                    || (!scope_auto_sample_rate_before_ui && self.scope_auto_sample_rate)
                    || self.scope_settings.sample_count != scope_settings_before_ui.sample_count
                    || self.scope_settings.enabled_channel_count()
                        != scope_settings_before_ui.enabled_channel_count();
                if self.scope_auto_sample_rate && automatic_sample_rate_inputs_changed {
                    if let Some(time_per_division) = scope_time_per_division_seconds {
                        self.scope_settings.sample_rate_hz = self
                            .scope_settings
                            .sample_rate_for_timebase(time_per_division);
                    }
                }
                for (channel, vertical_offset) in self
                    .scope_settings
                    .channels
                    .iter_mut()
                    .zip(scope_vertical_offsets)
                {
                    channel.vertical_offset = vertical_offset;
                }
                self.scope_settings.trigger = scope_trigger_settings;
            });
        self.snapshot_plot_rect = snapshot_plot_rect;
        if snapshot_clicked {
            if let Some(plot_rect) = snapshot_plot_rect {
                self.pending_snapshot = Some(PendingSnapshot {
                    plot_rect,
                    pixels_per_point: context.pixels_per_point(),
                });
                self.status = "Preparing snapshot…".to_owned();
                context.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            }
        }

        let mut settings_open = self.settings_open;
        let mut close_settings = false;
        let settings_locked = self.is_busy();
        egui::Window::new("Settings")
            .open(&mut settings_open)
            .collapsible(false)
            .resizable(true)
            .default_size([560.0, 620.0])
            .min_width(480.0)
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(RichText::new("FPGA RESOURCES").strong().color(ACCENT));
                    ui.add_space(8.0);
                    ui.label("Resource directory");
                    ui.add_enabled(
                        !settings_locked,
                        egui::TextEdit::singleline(&mut self.resource_directory)
                            .desired_width(f32::INFINITY),
                    );
                    ui.small(
                        RichText::new("Contains DSLogicU3Pro16.bin and DSCopeU3P100.bin")
                            .color(TEXT_MUTED),
                    );
                    ui.add_space(10.0);

                    let resource_directory = Path::new(self.resource_directory.trim());
                    if has_required_fpga_resources(resource_directory) {
                        ui.label(RichText::new("✓ Required FPGA bitstreams found").color(ACCENT));
                    } else {
                        ui.label(
                            RichText::new(
                                "Required FPGA bitstreams were not found in this directory",
                            )
                            .color(Color32::LIGHT_RED),
                        );
                    }

                    ui.add_space(10.0);
                    if ui
                        .add_enabled(!settings_locked, egui::Button::new("Use bundled resources"))
                        .clicked()
                    {
                        self.resource_directory = default_resource_directory();
                    }

                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("INSTRUMENTS").strong().color(ACCENT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(!settings_locked, egui::Button::new("↻ Refresh"))
                                .clicked()
                            {
                                refresh_clicked = true;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    paint_instrument_settings(ui, &mut self.devices, !settings_locked);

                    if settings_locked {
                        ui.add_space(8.0);
                        ui.small(
                            RichText::new("Settings are locked while an operation is running")
                                .color(TEXT_MUTED),
                        );
                    }

                    ui.add_space(16.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close_settings = true;
                        }
                    });
                });
            });
        if close_settings {
            settings_open = false;
        }
        self.settings_open = settings_open;

        let dismiss_usb_speed_warning = self
            .usb_speed_warning
            .as_ref()
            .is_some_and(|warning| paint_usb_speed_warning(context, warning));
        if dismiss_usb_speed_warning {
            self.usb_speed_warning = None;
        }

        let mut auto_confirmation_open = self.auto_calibration_confirmation_open;
        let mut auto_confirmed = false;
        let mut auto_cancelled = false;
        egui::Window::new("DSCope Auto Calibration")
            .id(egui::Id::new("auto_calibration_confirmation"))
            .open(&mut auto_confirmation_open)
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label(RichText::new("Disconnect all probes before continuing.").strong());
                ui.add_space(6.0);
                ui.label(
                    "DSView will run the vendor calibration sequence for both channels: offset transfer, all input ranges, and ADC branch balance. It can take a while. Device NVM is changed only if you choose Save when calibration finishes.",
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Keep all probes disconnected during calibration. Do not disconnect the oscilloscope while calibration is running.",
                    )
                    .color(Color32::LIGHT_RED),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        auto_cancelled = true;
                    }
                    if ui.button("Start calibration").clicked() {
                        auto_confirmed = true;
                    }
                });
            });
        if auto_confirmed || auto_cancelled {
            auto_confirmation_open = false;
        }
        self.auto_calibration_confirmation_open = auto_confirmation_open;

        let mut save_manual_values = None;
        let mut close_manual = false;
        let manual_calibration_actions_enabled = self.pending.is_none();
        if let Some(editor) = self.manual_calibration.as_mut() {
            let mut manual_open = true;
            egui::Window::new("DSCope Manual Calibration")
                .open(&mut manual_open)
                .collapsible(false)
                .resizable(false)
                .default_width(430.0)
                .show(context, |ui| {
                    ui.label(
                        "Adjust VGAIN and VOFF for the selected input range. VCOMB is shared by all ranges of a channel. Active acquisition previews edits; device NVM changes only after Save.",
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("Input range");
                        egui::ComboBox::from_id_salt("manual_calibration_range")
                            .selected_text(format!(
                                "{} mV/div",
                                DSCOPE_VOLTS_PER_DIV_MV[editor.range]
                            ))
                            .show_ui(ui, |ui| {
                                for (range, value) in
                                    DSCOPE_VOLTS_PER_DIV_MV.into_iter().enumerate()
                                {
                                    ui.selectable_value(
                                        &mut editor.range,
                                        range,
                                        format!("{value} mV/div"),
                                    );
                                }
                            });
                    });
                    ui.add_space(8.0);
                    for channel in 0..2 {
                        ui.group(|ui| {
                            ui.label(RichText::new(format!("CH{channel}")).strong());
                            let mut gain = editor
                                .values
                                .vga_gain(channel, editor.range)
                                .expect("manual calibration selection is valid");
                            ui.horizontal(|ui| {
                                ui.label("VGAIN");
                                ui.add(
                                    egui::DragValue::new(&mut gain)
                                        .range(1..=0x1fff)
                                        .speed(1),
                                );
                            });
                            let _ = editor.values.set_vga_gain(channel, editor.range, gain);

                            let mut offset = editor
                                .values
                                .preoffset(channel, editor.range)
                                .expect("manual calibration selection is valid");
                            ui.horizontal(|ui| {
                                ui.label("VOFF");
                                ui.add(egui::Slider::new(&mut offset, 0..=1_023));
                            });
                            let _ = editor.values.set_preoffset(channel, editor.range, offset);

                            let mut comb = i16::from(
                                editor
                                    .values
                                    .comb_comp(channel)
                                    .expect("manual calibration channel is valid"),
                            );
                            ui.horizontal(|ui| {
                                ui.label("VCOMB");
                                ui.add(egui::Slider::new(&mut comb, -127..=127));
                            });
                            let _ = editor.values.set_comb_comp(channel, comb);
                        });
                        ui.add_space(6.0);
                    }
                    ui.label(
                        RichText::new(
                            "Incorrect calibration values can distort measurements. Save only values established with a known reference.",
                        )
                        .color(Color32::LIGHT_RED),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close_manual = true;
                        }
                        if ui
                            .add_enabled(
                                manual_calibration_actions_enabled,
                                egui::Button::new("Save to device"),
                            )
                            .clicked()
                        {
                            save_manual_values = Some(editor.values);
                        }
                    });
                });
            if !manual_open {
                close_manual = true;
            }
        }
        if close_manual {
            self.manual_calibration = None;
        }

        paint_scope_marker_editor(
            context,
            &mut self.scope_marker_editor,
            &mut self.scope_marker_settings,
            self.scope_viewport,
            self.scope_settings.channels.map(|channel| channel.enabled),
            &self.scope_channel_names,
        );
        let calibration_modal_action = self
            .calibration_modal
            .as_ref()
            .and_then(|state| paint_calibration_progress_modal(context, state));
        match calibration_modal_action {
            Some(CalibrationModalAction::Close) => self.calibration_modal = None,
            Some(CalibrationModalAction::Save(values)) => self.begin_save_auto_calibration(values),
            None => {}
        }

        if record_now_clicked {
            self.record_capture = false;
            if let Some(path) = choose_recording_save_path(&self.recording_path) {
                self.recording_path = path.to_string_lossy().into_owned();
                self.record_capture = true;
                if let Some(target) = control_target {
                    start_request = Some((AcquisitionMode::Single, target));
                }
            }
        }
        if open_recording_clicked {
            if let Some(path) = choose_recording_to_open() {
                self.begin_open_recording(path);
            }
        }
        if refresh_clicked {
            self.begin_refresh();
        }
        if !open_recording_clicked {
            if let Some((mode, target)) = start_request {
                self.begin_capture(mode, target);
            }
        }
        if stop_clicked {
            self.capture_run_requested = false;
            self.requested_capture_target = None;
            let repetitive_waiting = self.scheduled_repetitive_capture.take().is_some();
            self.scope_capture_restart_requested = false;
            self.scope_configuration_due = None;
            if self.capture.is_some() {
                self.stop_capture();
            } else if self.pending.is_some() {
                self.status = "Stopping acquisition after device preparation…".to_owned();
            } else if repetitive_waiting {
                self.status = "Acquisition stopped".to_owned();
            }
        }
        match calibration_action {
            Some(ScopeCalibrationAction::Auto) => {
                self.auto_calibration_confirmation_open = true;
            }
            Some(ScopeCalibrationAction::Manual) => self.request_manual_calibration(),
            None => {}
        }
        if auto_confirmed {
            self.begin_auto_calibration();
        }
        if let Some(values) = save_manual_values {
            self.request_save_manual_calibration(values);
        }
        let manual_calibration_range_after_ui =
            self.manual_calibration.as_ref().map(|editor| editor.range);
        if manual_calibration_range_after_ui != manual_calibration_range_before_ui {
            if let Some(range) = manual_calibration_range_after_ui {
                let volts_per_div_mv = DSCOPE_VOLTS_PER_DIV_MV[range];
                for channel in &mut self.scope_settings.channels {
                    channel.volts_per_div_mv = volts_per_div_mv;
                }
            }
        }
        let manual_calibration_changed =
            self.manual_calibration.as_ref().map(|editor| editor.values)
                != manual_calibration_before_ui;
        let runtime_frontend_changed = self
            .scope_settings
            .runtime_frontend_differs_from(scope_settings_before_ui);
        let trigger_changed = self
            .scope_settings
            .trigger
            .hardware_differs_from(scope_settings_before_ui.trigger);
        let mut runtime_calibration_applied = false;
        if manual_calibration_changed && !runtime_frontend_changed && scope_capture_requested {
            if let (Some(values), Some(active)) = (
                self.manual_calibration.as_ref().map(|editor| editor.values),
                self.capture.as_ref(),
            ) {
                runtime_calibration_applied = active
                    .scope_calibration_controls
                    .iter()
                    .all(|control| control.update(values).is_ok());
            }
        }
        let manual_calibration_requires_restart =
            manual_calibration_changed && !runtime_calibration_applied;
        if (runtime_frontend_changed || manual_calibration_requires_restart)
            && scope_capture_requested
            && selected_scope_device
        {
            self.scope_configuration_due = Some(Instant::now() + SCOPE_CONFIGURATION_DEBOUNCE);
            self.status = "Adjusting oscilloscope frontend…".to_owned();
            self.last_error = None;
        } else if runtime_calibration_applied {
            self.scope_configuration_due = None;
            self.status = "Applying manual calibration during capture…".to_owned();
            self.last_error = None;
        } else if trigger_changed && self.capture.is_some() {
            let update_result = self
                .scope_settings
                .trigger
                .capture_config()
                .and_then(|trigger| {
                    let Some(active) = self.capture.as_mut() else {
                        return Err("capture ended before the trigger update".to_owned());
                    };
                    let updated_config = active
                        .scope_config
                        .with_trigger_config(trigger)
                        .map_err(|error| error.to_string())?;
                    for control in &active.scope_trigger_controls {
                        control.update(trigger).map_err(|error| error.to_string())?;
                    }
                    active.scope_config = updated_config;
                    Ok(())
                });
            match update_result {
                Ok(()) => {
                    self.scope_configuration_due = None;
                    self.status = "Updating trigger during capture…".to_owned();
                    self.last_error = None;
                }
                Err(error) => self.set_error(error),
            }
        } else if self
            .scope_settings
            .frontend_differs_from(scope_settings_before_ui)
        {
            self.scope_configuration_due = Some(Instant::now() + SCOPE_CONFIGURATION_DEBOUNCE);
            self.status = "Adjusting DSCope configuration…".to_owned();
        }
        let scope_configuration_is_due = self
            .scope_configuration_due
            .is_some_and(|due| Instant::now() >= due);
        if !refresh_clicked
            && start_request.is_none()
            && !stop_clicked
            && scope_configuration_is_due
        {
            if scope_capture_requested && selected_scope_device {
                self.scope_configuration_due = None;
                self.scope_capture_restart_requested = true;
                if let Some(active) = self.capture.as_ref() {
                    active.session.request_stop();
                }
                self.status = "Reconfiguring oscilloscope frontend…".to_owned();
            } else if !self.is_busy() {
                self.begin_scope_configuration();
            }
        }
        if self.pending.is_some() || self.capture.is_some() {
            context.request_repaint_after(Duration::from_millis(16));
        } else if let Some(scheduled) = self.scheduled_repetitive_capture {
            context.request_repaint_after(scheduled.due.saturating_duration_since(Instant::now()));
        } else if let Some(due) = self.scope_configuration_due {
            context.request_repaint_after(due.saturating_duration_since(Instant::now()));
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

fn insert_channel_name_metadata(
    metadata: &mut BTreeMap<String, String>,
    logic_channel_names: &[String; LOGIC_CHANNEL_COUNT],
    scope_channel_names: &[String; 2],
) {
    for (index, name) in logic_channel_names.iter().enumerate() {
        metadata.insert(
            format!("logic.ch{index}.name"),
            normalized_channel_name(name),
        );
    }
    for (index, name) in scope_channel_names.iter().enumerate() {
        metadata.insert(
            format!("scope.ch{index}.name"),
            normalized_channel_name(name),
        );
    }
}

struct GuiRecordingSettings<'a> {
    logic_config: DsLogicCaptureConfig,
    logic_settings: LogicCaptureSettings,
    scope_config: DsCopeCaptureConfig,
    scope_settings: ScopeCaptureSettings,
    logic_channel_names: &'a [String; LOGIC_CHANNEL_COUNT],
    scope_channel_names: &'a [String; 2],
}

fn start_gui_recording(
    path: &Path,
    devices: &[DeviceDescriptor],
    settings: GuiRecordingSettings<'_>,
) -> Result<FileRecorder, String> {
    let GuiRecordingSettings {
        logic_config,
        logic_settings,
        scope_config,
        scope_settings,
        logic_channel_names,
        scope_channel_names,
    } = settings;
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
        "logic.sample_rate_hz".to_owned(),
        logic_config.sample_rate_hz().to_string(),
    );
    metadata.insert(
        "logic.sample_count".to_owned(),
        logic_config.sample_count().to_string(),
    );
    metadata.insert(
        "logic.operation_mode".to_owned(),
        match logic_config.operation_mode() {
            DsLogicOperationMode::Buffer => "buffer",
            DsLogicOperationMode::Stream => "stream",
            DsLogicOperationMode::InternalTest => "internal_test",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.stop_option".to_owned(),
        match logic_config.stop_option() {
            DsLogicStopOption::StopImmediately => "stop_immediately",
            DsLogicStopOption::UploadCapturedData => "upload_captured_data",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.threshold_voltage".to_owned(),
        logic_config.threshold_voltage().to_string(),
    );
    metadata.insert(
        "logic.input_filter".to_owned(),
        match logic_config.input_filter() {
            DsLogicInputFilter::None => "none",
            DsLogicInputFilter::OneSampleClock => "one_sample_clock",
        }
        .to_owned(),
    );
    metadata.insert(
        "logic.rle_enabled".to_owned(),
        logic_config.rle_enabled().to_string(),
    );
    metadata.insert(
        "logic.external_clock".to_owned(),
        logic_config.external_clock().to_string(),
    );
    metadata.insert(
        "logic.falling_clock_edge".to_owned(),
        logic_config.falling_clock_edge().to_string(),
    );
    for (channel, enabled) in logic_config.enabled_channels().into_iter().enumerate() {
        metadata.insert(format!("logic.ch{channel}.enabled"), enabled.to_string());
        metadata.insert(
            format!("logic.ch{channel}.maximum_height"),
            logic_settings.maximum_heights[channel].to_string(),
        );
    }
    insert_channel_name_metadata(&mut metadata, logic_channel_names, scope_channel_names);
    metadata.insert(
        "scope.sample_rate_hz".to_owned(),
        scope_config.sample_rate_hz().to_string(),
    );
    metadata.insert(
        "scope.sample_count".to_owned(),
        scope_config.sample_count().to_string(),
    );
    let trigger = scope_config.trigger();
    metadata.insert(
        "scope.trigger.position_percent".to_owned(),
        trigger.position_percent().to_string(),
    );
    metadata.insert(
        "scope.trigger.source".to_owned(),
        match trigger.source() {
            DsCopeTriggerSource::Auto => "auto",
            DsCopeTriggerSource::ChannelZero => "ch0",
            DsCopeTriggerSource::ChannelOne => "ch1",
            DsCopeTriggerSource::ChannelZeroAndOne => "ch0_and_ch1",
            DsCopeTriggerSource::ChannelZeroOrOne => "ch0_or_ch1",
        }
        .to_owned(),
    );
    metadata.insert(
        "scope.trigger.auto_channel".to_owned(),
        trigger.auto_channel().to_string(),
    );
    metadata.insert(
        "scope.trigger.slope".to_owned(),
        match trigger.slope() {
            DsCopeTriggerSlope::Rising => "rising",
            DsCopeTriggerSlope::Falling => "falling",
        }
        .to_owned(),
    );
    metadata.insert(
        "scope.trigger.holdoff_ns".to_owned(),
        trigger.holdoff_ns().to_string(),
    );
    metadata.insert(
        "scope.trigger.noise_sensitivity".to_owned(),
        trigger.noise_sensitivity().to_string(),
    );
    metadata.insert(
        "scope.trigger.level".to_owned(),
        trigger.level().to_string(),
    );
    for (channel, level) in trigger.levels().into_iter().enumerate() {
        metadata.insert(
            format!("scope.trigger.ch{channel}.level"),
            level.to_string(),
        );
    }
    for (index, channel) in scope_settings.channels.iter().enumerate() {
        metadata.insert(
            format!("scope.ch{index}.enabled"),
            channel.enabled.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.volts_per_div_mv"),
            channel.volts_per_div_mv.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.coupling"),
            coupling_label(channel.coupling).to_ascii_lowercase(),
        );
        metadata.insert(
            format!("scope.ch{index}.probe_attenuation"),
            channel.probe_attenuation.to_string(),
        );
        metadata.insert(
            format!("scope.ch{index}.vertical_offset"),
            channel.vertical_offset.to_string(),
        );
    }
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

fn recording_metadata_value<T: std::str::FromStr>(
    metadata: &BTreeMap<String, String>,
    key: &str,
) -> Option<T> {
    metadata.get(key)?.parse().ok()
}

fn channel_names_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> ([String; LOGIC_CHANNEL_COUNT], [String; 2]) {
    let logic_names = std::array::from_fn(|index| {
        metadata
            .get(&format!("logic.ch{index}.name"))
            .map(|name| normalized_channel_name(name))
            .unwrap_or_default()
    });
    let scope_names = std::array::from_fn(|index| {
        metadata
            .get(&format!("scope.ch{index}.name"))
            .map(|name| normalized_channel_name(name))
            .unwrap_or_default()
    });
    (logic_names, scope_names)
}

fn logic_settings_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> LogicCaptureSettings {
    let mut settings = LogicCaptureSettings::default();
    if let Some(sample_rate_hz) = recording_metadata_value::<u64>(metadata, "logic.sample_rate_hz")
    {
        if DSLOGIC_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            settings.sample_rate_hz = sample_rate_hz;
        }
    }
    if let Some(sample_count) = recording_metadata_value::<u64>(metadata, "logic.sample_count") {
        if (DSLOGIC_SAMPLE_ALIGNMENT..=DSLOGIC_MAX_SAMPLE_COUNT).contains(&sample_count) {
            settings.sample_count = sample_count;
        }
    }
    for (channel, enabled) in settings.enabled_channels.iter_mut().enumerate() {
        if let Some(recorded) =
            recording_metadata_value::<bool>(metadata, &format!("logic.ch{channel}.enabled"))
        {
            *enabled = recorded;
        }
    }
    if let Some(operation_mode) = metadata.get("logic.operation_mode") {
        settings.operation_mode = match operation_mode.as_str() {
            "stream" => DsLogicOperationMode::Stream,
            "internal_test" => DsLogicOperationMode::InternalTest,
            _ => DsLogicOperationMode::Buffer,
        };
    }
    if let Some(stop_option) = metadata.get("logic.stop_option") {
        settings.stop_option = match stop_option.as_str() {
            "stop_immediately" => DsLogicStopOption::StopImmediately,
            _ => DsLogicStopOption::UploadCapturedData,
        };
    }
    if let Some(threshold_voltage) =
        recording_metadata_value::<f32>(metadata, "logic.threshold_voltage")
    {
        if threshold_voltage.is_finite() && (0.0..=5.0).contains(&threshold_voltage) {
            settings.threshold_voltage = threshold_voltage;
        }
    }
    if let Some(input_filter) = metadata.get("logic.input_filter") {
        settings.input_filter = match input_filter.as_str() {
            "one_sample_clock" => DsLogicInputFilter::OneSampleClock,
            _ => DsLogicInputFilter::None,
        };
    }
    if let Some(maximum_height) = recording_metadata_value::<u8>(metadata, "logic.maximum_height") {
        if (1..=5).contains(&maximum_height) {
            settings.maximum_heights = [maximum_height; LOGIC_CHANNEL_COUNT];
        }
    }
    for (channel, maximum_height) in settings.maximum_heights.iter_mut().enumerate() {
        if let Some(recorded) =
            recording_metadata_value::<u8>(metadata, &format!("logic.ch{channel}.maximum_height"))
        {
            if (1..=5).contains(&recorded) {
                *maximum_height = recorded;
            }
        }
    }
    if let Some(rle_enabled) = recording_metadata_value(metadata, "logic.rle_enabled") {
        settings.rle_enabled = rle_enabled;
    }
    if let Some(external_clock) = recording_metadata_value(metadata, "logic.external_clock") {
        settings.external_clock = external_clock;
    }
    if let Some(falling_clock_edge) = recording_metadata_value(metadata, "logic.falling_clock_edge")
    {
        settings.falling_clock_edge = falling_clock_edge;
    }
    settings
}

fn scope_settings_from_recording_metadata(
    metadata: &BTreeMap<String, String>,
) -> ScopeCaptureSettings {
    let mut settings = ScopeCaptureSettings::default();
    if let Some(sample_rate_hz) = recording_metadata_value::<u64>(metadata, "scope.sample_rate_hz")
    {
        if DSCOPE_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            settings.sample_rate_hz = sample_rate_hz;
        }
    }
    if let Some(sample_count) = recording_metadata_value::<u64>(metadata, "scope.sample_count") {
        if DSCOPE_SAMPLE_COUNTS.contains(&sample_count) {
            settings.sample_count = sample_count;
        }
    }
    for (index, channel) in settings.channels.iter_mut().enumerate() {
        if let Some(enabled) =
            recording_metadata_value::<bool>(metadata, &format!("scope.ch{index}.enabled"))
        {
            channel.enabled = enabled;
        }
        if let Some(volts_per_div_mv) =
            recording_metadata_value::<u16>(metadata, &format!("scope.ch{index}.volts_per_div_mv"))
        {
            if DSCOPE_VOLTS_PER_DIV_MV.contains(&volts_per_div_mv) {
                channel.volts_per_div_mv = volts_per_div_mv;
            }
        }
        if let Some(coupling) = metadata.get(&format!("scope.ch{index}.coupling")) {
            channel.coupling = if coupling.eq_ignore_ascii_case("ac") {
                DsCopeCoupling::Ac
            } else {
                DsCopeCoupling::Dc
            };
        }
        if let Some(probe_attenuation) =
            recording_metadata_value::<u8>(metadata, &format!("scope.ch{index}.probe_attenuation"))
        {
            if matches!(probe_attenuation, 1 | 10 | 100) {
                channel.probe_attenuation = probe_attenuation;
            }
        }
        if let Some(vertical_offset) =
            recording_metadata_value::<u16>(metadata, &format!("scope.ch{index}.vertical_offset"))
        {
            if (DSCOPE_VERTICAL_OFFSET_MIN..=DSCOPE_VERTICAL_OFFSET_MAX).contains(&vertical_offset)
            {
                channel.vertical_offset = vertical_offset;
            }
        }
    }

    if let Some(position_percent) =
        recording_metadata_value::<u8>(metadata, "scope.trigger.position_percent")
    {
        if (DSCOPE_TRIGGER_POSITION_MIN_PERCENT..=DSCOPE_TRIGGER_POSITION_MAX_PERCENT)
            .contains(&position_percent)
        {
            settings.trigger.position_percent = position_percent;
        }
    }
    if let Some(source) = metadata.get("scope.trigger.source") {
        settings.trigger.source = match source.as_str() {
            "ch0" => DsCopeTriggerSource::ChannelZero,
            "ch1" => DsCopeTriggerSource::ChannelOne,
            "ch0_and_ch1" => DsCopeTriggerSource::ChannelZeroAndOne,
            "ch0_or_ch1" => DsCopeTriggerSource::ChannelZeroOrOne,
            _ => DsCopeTriggerSource::Auto,
        };
    }
    if let Some(auto_channel) =
        recording_metadata_value::<u8>(metadata, "scope.trigger.auto_channel")
    {
        if auto_channel <= 1 {
            settings.trigger.auto_channel = auto_channel;
        }
    }
    if let Some(slope) = metadata.get("scope.trigger.slope") {
        settings.trigger.slope = if slope == "falling" {
            DsCopeTriggerSlope::Falling
        } else {
            DsCopeTriggerSlope::Rising
        };
    }
    if let Some(holdoff_ns) = recording_metadata_value::<u64>(metadata, "scope.trigger.holdoff_ns")
    {
        settings.trigger.holdoff_ns = holdoff_ns.min(DSCOPE_TRIGGER_HOLDOFF_MAX_NS);
    }
    if let Some(noise_sensitivity) =
        recording_metadata_value::<u8>(metadata, "scope.trigger.noise_sensitivity")
    {
        settings.trigger.noise_sensitivity =
            noise_sensitivity.min(DSCOPE_TRIGGER_NOISE_SENSITIVITY_MAX);
    }
    for (index, level) in settings.trigger.levels.iter_mut().enumerate() {
        if let Some(recorded_level) =
            recording_metadata_value::<u8>(metadata, &format!("scope.trigger.ch{index}.level"))
        {
            *level = recorded_level.clamp(DSCOPE_TRIGGER_LEVEL_MIN, DSCOPE_TRIGGER_LEVEL_MAX);
        }
    }
    settings
        .trigger
        .clamp_to_channels(settings.channels.map(|channel| channel.enabled));
    settings
}

fn read_gui_recording(input: impl Read) -> Result<LoadedRecording, String> {
    let mut reader = RecordingReader::new(input).map_err(|error| error.to_string())?;
    let header = reader.header().clone();
    let has_scope_channel_metadata = (0..2).any(|index| {
        header
            .metadata
            .contains_key(&format!("scope.ch{index}.enabled"))
    });
    let logic_settings = logic_settings_from_recording_metadata(&header.metadata);
    let mut scope_settings = scope_settings_from_recording_metadata(&header.metadata);
    let (logic_channel_names, scope_channel_names) =
        channel_names_from_recording_metadata(&header.metadata);
    let mut enabled_scope_channels = scope_settings.channels.map(|channel| channel.enabled);
    if !enabled_scope_channels.into_iter().any(|enabled| enabled) {
        enabled_scope_channels = [true, true];
        scope_settings.channels[0].enabled = true;
        scope_settings.channels[1].enabled = true;
    }
    let mut waveforms = WaveformStore {
        scope_capacity: (scope_settings.sample_count as usize).min(MAX_WAVEFORM_SAMPLES),
        ..WaveformStore::default()
    };
    while let Some(event) = reader.read_event().map_err(|error| error.to_string())? {
        if let CaptureEvent::Frame(frame) = event {
            waveforms.ingest(&frame.payload, enabled_scope_channels);
        }
    }
    let completion = reader
        .completion_status()
        .ok_or_else(|| "recording has no completion status".to_owned())?;
    let has_logic = header
        .devices
        .iter()
        .any(|device| device.kind == InstrumentKind::LogicAnalyzer)
        || waveforms.logic_channels > 0;
    let has_scope = header
        .devices
        .iter()
        .any(|device| device.kind == InstrumentKind::Oscilloscope)
        || waveforms.scope_rate_hz > 0;
    if has_scope && !has_scope_channel_metadata {
        for (channel, samples) in scope_settings
            .channels
            .iter_mut()
            .zip(&waveforms.scope_channels)
        {
            channel.enabled = !samples.is_empty();
        }
        scope_settings
            .trigger
            .clamp_to_channels(scope_settings.channels.map(|channel| channel.enabled));
    }
    if !has_logic && !has_scope {
        return Err("recording does not contain a supported capture".to_owned());
    }
    Ok(LoadedRecording {
        capture: CompletedCapture {
            waveforms,
            has_logic,
            has_scope,
        },
        logic_settings: has_logic.then_some(logic_settings),
        scope_settings: has_scope.then_some(scope_settings),
        logic_channel_names,
        scope_channel_names,
        completion,
    })
}

fn load_gui_recording(path: &Path) -> Result<LoadedRecording, String> {
    let file = File::open(path)
        .map_err(|error| format!("failed to open recording {}: {error}", path.display()))?;
    read_gui_recording(BufReader::new(file))
        .map_err(|error| format!("failed to read recording {}: {error}", path.display()))
}

fn recording_completion_label(completion: CompletionStatus) -> &'static str {
    match completion {
        CompletionStatus::Complete => "complete",
        CompletionStatus::Cancelled => "cancelled",
        CompletionStatus::Failed => "failed",
    }
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

struct StartedCapture {
    session: CaptureSession,
    scope_trigger_controls: Vec<DsCopeRuntimeTriggerControl>,
    scope_calibration_controls: Vec<DsCopeRuntimeCalibrationControl>,
}

fn start_capture(
    devices: Vec<DiscoveredDevice>,
    resource_directory: &Path,
    logic_config: DsLogicCaptureConfig,
    scope_config: DsCopeCaptureConfig,
    calibration_values: Option<DsCopeCalibrationValues>,
) -> Result<StartedCapture, String> {
    let mut sources = Vec::<Box<dyn CaptureSource>>::with_capacity(devices.len());
    let mut scope_trigger_controls = Vec::new();
    let mut scope_calibration_controls = Vec::new();
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
            DeviceModel::DsCopeU3P100 => {
                let (source, control) =
                    DsCopeCaptureSource::with_runtime_trigger_control(device, scope_config)
                        .map_err(|error| error.to_string())?;
                let source = match calibration_values {
                    Some(values) => source.with_calibration_values(values),
                    None => source,
                };
                let (source, calibration_control) = source.with_runtime_calibration_control();
                sources.push(Box::new(source));
                scope_trigger_controls.push(control);
                scope_calibration_controls.push(calibration_control);
            }
        }
    }
    let session = CaptureCoordinator::start(sources).map_err(|error| error.to_string())?;
    Ok(StartedCapture {
        session,
        scope_trigger_controls,
        scope_calibration_controls,
    })
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
    context.style_mut(|style| {
        style.interaction.tooltip_delay = TOOLTIP_DELAY_SECONDS;
    });
}

const WAVEFORM_CARD_CHROME_HEIGHT: f32 = 58.0;
const COMBINED_DIVIDER_HEIGHT: f32 = 10.0;
const MIN_LOGIC_PANE_HEIGHT: f32 = 260.0;
const MIN_SCOPE_PANE_HEIGHT: f32 = 165.0;

struct ScopeWaveformOptions<'a> {
    settings: ScopeCaptureSettings,
    function_settings: ScopeFunctionSettings,
    channel_names: &'a [String; 2],
    measurement_caches: &'a mut [ScopeMeasurementCache; 2],
    measurement_window_positions: &'a mut [Vec2; 2],
    marker_settings: &'a mut ScopeMarkerSettings,
    quick_time_measurement: &'a mut ScopeQuickTimeMeasurement,
    channel_colors: [Color32; 2],
    vertical_offsets: &'a mut [u16; 2],
    trigger_settings: &'a mut ScopeTriggerSettings,
    time_per_division_seconds: &'a mut Option<f64>,
    timebase_changed: &'a mut bool,
    offset_controls_enabled: bool,
    trigger_controls_enabled: bool,
}

fn waveform_card_fill(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    paint: impl FnOnce(&egui::Ui, Rect),
) -> Rect {
    let plot_height = (ui.available_height() - WAVEFORM_CARD_CHROME_HEIGHT).max(64.0);
    waveform_card(ui, title, subtitle, plot_height, paint)
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

struct CombinedWaveformViewports<'a> {
    logic: &'a mut WaveformViewport,
    scope: &'a mut WaveformViewport,
    fft: &'a mut ScopeFftViewport,
}

struct LogicWaveformOptions<'a> {
    channel_colors: &'a mut [Color32; LOGIC_CHANNEL_COUNT],
    channel_marker_opacities: &'a mut [u8; LOGIC_CHANNEL_COUNT],
    channel_names: &'a mut [String; LOGIC_CHANNEL_COUNT],
    channel_settings_drafts: &'a mut [LogicChannelSettingsDraft; LOGIC_CHANNEL_COUNT],
    maximum_heights: &'a mut [u8; LOGIC_CHANNEL_COUNT],
    enabled_channels: &'a mut [bool; LOGIC_CHANNEL_COUNT],
}

fn combined_waveform_cards(
    ui: &mut egui::Ui,
    waveforms: &WaveformStore,
    logic_ratio: &mut f32,
    logic_options: LogicWaveformOptions<'_>,
    viewports: CombinedWaveformViewports<'_>,
    scope_options: ScopeWaveformOptions<'_>,
) -> Rect {
    let CombinedWaveformViewports {
        logic: logic_viewport,
        scope: scope_viewport,
        fft: scope_fft_viewport,
    } = viewports;
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
    let logic_plot_rect = waveform_card_fill(
        &mut logic_ui,
        "DIGITAL",
        "DSLogic · channels 0–15",
        |ui, rect| paint_logic(ui, rect, waveforms, logic_options, logic_viewport),
    );

    let mut scope_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("combined_scope_waveform")
            .max_rect(scope_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let scope_subtitle = scope_channels_subtitle(scope_options.channel_names);
    let scope_plot_rect = waveform_card_fill(
        &mut scope_ui,
        "OSCILLOSCOPE",
        &scope_subtitle,
        |ui, rect| {
            paint_scope(
                ui,
                rect,
                waveforms,
                scope_viewport,
                scope_fft_viewport,
                false,
                scope_options,
            )
        },
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
    logic_plot_rect.union(scope_plot_rect)
}

fn waveform_card(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    height: f32,
    paint: impl FnOnce(&egui::Ui, Rect),
) -> Rect {
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
            rect
        })
        .inner
}

fn paint_plot_background(ui: &egui::Ui, rect: Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, BACKGROUND);
}

fn fit_rect_to_grid(area: Rect, horizontal_divisions: usize, vertical_divisions: usize) -> Rect {
    if area.width() <= 0.0
        || area.height() <= 0.0
        || horizontal_divisions == 0
        || vertical_divisions == 0
    {
        return area;
    }
    let target_aspect = horizontal_divisions as f32 / vertical_divisions as f32;
    let area_aspect = area.width() / area.height();
    let size = if area_aspect > target_aspect {
        Vec2::new(area.height() * target_aspect, area.height())
    } else {
        Vec2::new(area.width(), area.width() / target_aspect)
    };
    Rect::from_center_size(area.center(), size)
}

fn align_waveform_chrome(status: Rect, ruler: Rect, plot: Rect) -> (Rect, Rect) {
    let ruler = Rect::from_min_max(
        Pos2::new(plot.left(), plot.top() - ruler.height()),
        Pos2::new(plot.right(), plot.top()),
    );
    let status = Rect::from_min_max(
        Pos2::new(plot.left(), ruler.top() - status.height()),
        Pos2::new(plot.right(), ruler.top()),
    );
    (status, ruler)
}

fn paint_plot_grid(ui: &egui::Ui, rect: Rect) {
    let painter = ui.painter();
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

fn split_waveform_chrome(rect: Rect) -> (Rect, Rect, Rect) {
    let status_bottom = (rect.top() + WAVEFORM_STATUS_HEIGHT).min(rect.bottom());
    let ruler_bottom = (status_bottom + TIME_RULER_HEIGHT).min(rect.bottom());
    (
        Rect::from_min_max(rect.left_top(), Pos2::new(rect.right(), status_bottom)),
        Rect::from_min_max(
            Pos2::new(rect.left(), status_bottom),
            Pos2::new(rect.right(), ruler_bottom),
        ),
        Rect::from_min_max(Pos2::new(rect.left(), ruler_bottom), rect.right_bottom()),
    )
}

fn time_scale_unit(maximum_seconds: f64) -> (&'static str, f64) {
    if maximum_seconds >= 1.0 {
        ("s", 1.0)
    } else if maximum_seconds >= 1.0e-3 {
        ("ms", 1.0e3)
    } else if maximum_seconds >= 1.0e-6 {
        ("µs", 1.0e6)
    } else {
        ("ns", 1.0e9)
    }
}

fn time_scale_precision(label_step: f64) -> usize {
    if !label_step.is_finite() || label_step <= 0.0 {
        return 3;
    }
    if label_step >= 10.0 {
        0
    } else if label_step >= 1.0 {
        1
    } else {
        ((-label_step.log10().floor()) as usize + 1).min(9)
    }
}

fn time_scale_label_stride(width: f32) -> usize {
    const MAJOR_INTERVALS: usize = 10;
    const MINIMUM_LABEL_SPACING: f32 = 68.0;
    let maximum_labels = (width / MINIMUM_LABEL_SPACING).floor() as usize;
    let maximum_labels = maximum_labels.clamp(2, MAJOR_INTERVALS + 1);
    MAJOR_INTERVALS.div_ceil(maximum_labels - 1)
}

fn time_scale_sample_window(
    sample_count: usize,
    sample_offset: u64,
    viewport: WaveformViewport,
) -> Option<(f64, f64)> {
    if sample_count == 0 {
        return None;
    }
    let sample_extent = sample_count.saturating_sub(1).max(1) as f64;
    let first_sample = sample_offset as f64 + viewport.start as f64 * sample_extent;
    let last_sample =
        sample_offset as f64 + (viewport.start + viewport.span) as f64 * sample_extent;
    Some((first_sample, last_sample))
}

fn paint_time_scale(
    painter: &egui::Painter,
    rect: Rect,
    first_sample: f64,
    last_sample: f64,
    sample_rate_hz: u64,
) {
    painter.rect_filled(rect, 4.0, PANEL.gamma_multiply(0.72));
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0_f32, GRID),
    );
    if sample_rate_hz == 0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "TIME",
            FontId::monospace(9.0),
            TEXT_MUTED,
        );
        return;
    }

    const MAJOR_INTERVALS: usize = 10;
    const SUBDIVISIONS_PER_INTERVAL: usize = 5;
    let maximum_seconds = first_sample.abs().max(last_sample.abs()) / sample_rate_hz as f64;
    let (unit, multiplier) = time_scale_unit(maximum_seconds);
    let label_stride = time_scale_label_stride(rect.width());
    let label_step = (last_sample - first_sample) / MAJOR_INTERVALS as f64 * label_stride as f64
        / sample_rate_hz as f64
        * multiplier;
    let precision = time_scale_precision(label_step);
    let total_subdivisions = MAJOR_INTERVALS * SUBDIVISIONS_PER_INTERVAL;

    for index in 0..=total_subdivisions {
        let fraction = index as f32 / total_subdivisions as f32;
        let x = egui::lerp(rect.x_range(), fraction);
        let is_major = index % SUBDIVISIONS_PER_INTERVAL == 0;
        let major_index = index / SUBDIVISIONS_PER_INTERVAL;
        let is_labelled =
            is_major && (major_index % label_stride == 0 || major_index == MAJOR_INTERVALS);
        let tick_height = if is_labelled {
            7.0
        } else if is_major {
            5.0
        } else {
            2.5
        };
        painter.line_segment(
            [
                Pos2::new(x, rect.bottom() - tick_height),
                Pos2::new(x, rect.bottom()),
            ],
            Stroke::new(
                if is_major { 1.0_f32 } else { 0.7_f32 },
                if is_major {
                    TEXT_MUTED
                } else {
                    TEXT_MUTED.gamma_multiply(0.55)
                },
            ),
        );

        if is_labelled {
            let sample = first_sample + (last_sample - first_sample) * fraction as f64;
            let seconds = sample / sample_rate_hz as f64;
            let alignment = if index == 0 {
                egui::Align2::LEFT_TOP
            } else if index == total_subdivisions {
                egui::Align2::RIGHT_TOP
            } else {
                egui::Align2::CENTER_TOP
            };
            painter.text(
                Pos2::new(x, rect.top() + 2.0),
                alignment,
                format!("{:.*} {unit}", precision, seconds * multiplier),
                FontId::monospace(9.0),
                TEXT_MUTED,
            );
        }
    }
}

fn paint_waveform_time_scale(
    painter: &egui::Painter,
    rect: Rect,
    sample_count: usize,
    sample_rate_hz: u64,
    sample_offset: u64,
    viewport: WaveformViewport,
) {
    let Some((first_sample, last_sample)) =
        time_scale_sample_window(sample_count, sample_offset, viewport)
    else {
        paint_time_scale(painter, rect, 0.0, 0.0, 0);
        return;
    };
    paint_time_scale(painter, rect, first_sample, last_sample, sample_rate_hz);
}

enum WaveformPlotClick {
    Primary { double_clicked: bool, position: f32 },
    Secondary,
}

fn update_waveform_viewport(
    ui: &egui::Ui,
    rect: Rect,
    id_salt: &'static str,
    viewport: &mut WaveformViewport,
    sample_count: usize,
    sample_rate_hz: u64,
    reset_on_double_click: bool,
) -> (bool, bool, Option<WaveformPlotClick>) {
    let mut panned = false;
    let mut timebase_changed = false;
    let response = ui.interact(
        rect,
        ui.make_persistent_id(id_salt),
        Sense::click_and_drag(),
    );
    let cursor = if response.dragged_by(egui::PointerButton::Primary) {
        egui::CursorIcon::Grabbing
    } else {
        egui::CursorIcon::Grab
    };
    let response = response.on_hover_cursor(cursor);
    let click = if response.double_clicked() {
        response
            .interact_pointer_pos()
            .map(|pointer| WaveformPlotClick::Primary {
                double_clicked: true,
                position: ((pointer.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0),
            })
    } else if response.clicked() {
        response
            .interact_pointer_pos()
            .map(|pointer| WaveformPlotClick::Primary {
                double_clicked: false,
                position: ((pointer.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0),
            })
    } else if response.secondary_clicked() {
        Some(WaveformPlotClick::Secondary)
    } else {
        None
    };
    if response.double_clicked() && reset_on_double_click {
        let previous = *viewport;
        viewport.reset();
        timebase_changed = *viewport != previous;
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        let delta_x = ui.input(|input| input.pointer.delta().x);
        if viewport.pan_by_screen_delta(delta_x, rect.width()) {
            panned = true;
            ui.ctx().request_repaint();
        }
    }
    if response.hovered() {
        let scroll_delta = ui.input(|input| input.raw_scroll_delta.y);
        if scroll_delta != 0.0 {
            let pointer_fraction = response
                .hover_pos()
                .map(|pointer| (pointer.x - rect.left()) / rect.width().max(1.0))
                .unwrap_or(0.5);
            timebase_changed = viewport.step_timebase(
                scroll_delta,
                sample_count,
                sample_rate_hz,
                pointer_fraction,
            );
            ui.ctx().request_repaint();
        }
    }
    (panned, timebase_changed, click)
}

fn paint_waveform_status(
    painter: &egui::Painter,
    rect: Rect,
    total_sample_count: usize,
    sample_rate_hz: u64,
    viewport: WaveformViewport,
) {
    painter.rect_filled(rect, 4.0, PANEL.gamma_multiply(0.9));
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0_f32, GRID),
    );
    if let Some(time_per_division) = viewport.time_per_division(total_sample_count, sample_rate_hz)
    {
        let per_division = Duration::from_secs_f64(time_per_division);
        let span = Duration::from_secs_f64(time_per_division * SCOPE_HORIZONTAL_DIVISIONS as f64);
        painter.text(
            rect.left_center() + Vec2::new(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            format!(
                "SPAN {}  ·  {}/div",
                format_duration(span),
                format_duration(per_division)
            ),
            FontId::monospace(10.0),
            TEXT_MUTED,
        );
    }
    let zoom_factor = viewport.zoom_factor();
    let zoom_label = if zoom_factor >= 1.0 {
        format!("ZOOM {zoom_factor:.1}×")
    } else {
        format!("ZOOM {zoom_factor:.2}×")
    };
    painter.text(
        rect.right_center() + Vec2::new(-6.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        zoom_label,
        FontId::monospace(10.0),
        ACCENT,
    );
}

fn paint_logic_zero_reference(painter: &egui::Painter, plot: Rect, y: f32, channel_color: Color32) {
    const DOT_SPACING: f32 = 7.0;
    let mut x = plot.left();
    while x <= plot.right() {
        painter.circle_filled(Pos2::new(x, y), 0.7, channel_color.gamma_multiply(0.3));
        x += DOT_SPACING;
    }
}

const LOGIC_CHANNEL_MARKER_MIN_BODY_WIDTH: f32 = 16.0;
const LOGIC_CHANNEL_MARKER_TIP_WIDTH: f32 = 6.0;
const LOGIC_CHANNEL_MARKER_CHARACTER_WIDTH: f32 = 6.0;
const LOGIC_CHANNEL_MARKER_HORIZONTAL_PADDING: f32 = 8.0;
const LOGIC_SIGNAL_DEFAULT_STEPS_PER_LANE: usize = 4;

#[derive(Clone, Copy, Debug)]
struct LogicSignalLayout {
    base_height: f32,
    levels: [(f32, f32); LOGIC_CHANNEL_COUNT],
}

fn logic_signal_layout(
    plot_top: f32,
    plot_height: f32,
    maximum_heights: &[u8; LOGIC_CHANNEL_COUNT],
) -> LogicSignalLayout {
    let requested_height_steps = maximum_heights
        .iter()
        .map(|height| usize::from((*height).clamp(1, 5)))
        .sum::<usize>();
    let minimum_steps = LOGIC_CHANNEL_COUNT * LOGIC_SIGNAL_DEFAULT_STEPS_PER_LANE;
    let required_steps = requested_height_steps + LOGIC_CHANNEL_COUNT + 1;
    let total_steps = required_steps
        .max(minimum_steps)
        .div_ceil(LOGIC_CHANNEL_COUNT)
        * LOGIC_CHANNEL_COUNT;
    let lane_steps = total_steps / LOGIC_CHANNEL_COUNT;
    let base_height = plot_height / total_steps as f32;
    let mut step_levels = [(0_i32, 0_i32); LOGIC_CHANNEL_COUNT];

    for channel in 0..LOGIC_CHANNEL_COUNT {
        let maximum_height = i32::from(maximum_heights[channel].clamp(1, 5));
        let nominal_low = (channel * lane_steps + lane_steps - 1) as i32;
        let mut high = nominal_low - maximum_height;
        let mut low = nominal_low;
        let minimum_high = if channel == 0 {
            1
        } else {
            step_levels[channel - 1].1 + 1
        };
        if high < minimum_high {
            let movement = minimum_high - high;
            high += movement;
            low += movement;
        }
        step_levels[channel] = (high, low);
    }

    for channel in (0..LOGIC_CHANNEL_COUNT).rev() {
        let maximum_low = if channel + 1 == LOGIC_CHANNEL_COUNT {
            total_steps as i32 - 1
        } else {
            step_levels[channel + 1].0 - 1
        };
        let (mut high, mut low) = step_levels[channel];
        if low > maximum_low {
            let movement = low - maximum_low;
            high -= movement;
            low -= movement;
        }
        step_levels[channel] = (high, low);
    }

    debug_assert!(step_levels[0].0 >= 1);
    debug_assert!(step_levels[LOGIC_CHANNEL_COUNT - 1].1 < total_steps as i32);
    for channel in 1..LOGIC_CHANNEL_COUNT {
        debug_assert!(step_levels[channel - 1].1 < step_levels[channel].0);
    }

    LogicSignalLayout {
        base_height,
        levels: step_levels.map(|(high, low)| {
            (
                plot_top + high as f32 * base_height,
                plot_top + low as f32 * base_height,
            )
        }),
    }
}

fn logic_channel_marker_text(channel: usize, channel_name: &str) -> String {
    let channel_name = channel_name.trim();
    if channel_name.is_empty() {
        channel.to_string()
    } else {
        channel_name
            .chars()
            .take(LOGIC_CHANNEL_NAME_MAX_CHARS)
            .collect()
    }
}

fn logic_channel_marker_body_width(marker_text: &str) -> f32 {
    (marker_text.chars().count() as f32 * LOGIC_CHANNEL_MARKER_CHARACTER_WIDTH
        + LOGIC_CHANNEL_MARKER_HORIZONTAL_PADDING)
        .max(LOGIC_CHANNEL_MARKER_MIN_BODY_WIDTH)
}

fn logic_channel_marker_rect(plot: Rect, y: f32, lane_height: f32, marker_text: &str) -> Rect {
    let half_height = (lane_height * 0.22).clamp(3.5, 6.5);
    let body_width = logic_channel_marker_body_width(marker_text);
    Rect::from_min_max(
        Pos2::new(plot.left() + 1.0, y - half_height),
        Pos2::new(
            plot.left() + 1.0 + body_width + LOGIC_CHANNEL_MARKER_TIP_WIDTH,
            y + half_height,
        ),
    )
}

fn paint_logic_channel_marker(
    painter: &egui::Painter,
    plot: Rect,
    y: f32,
    lane_height: f32,
    marker_text: &str,
    color: Color32,
    opacity: u8,
) {
    let marker = logic_channel_marker_rect(plot, y, lane_height, marker_text);
    let body_width = logic_channel_marker_body_width(marker_text);
    let body_right = marker.left() + body_width;
    let marker_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), opacity);
    let text_color = marker_text_color(color);
    let text_color =
        Color32::from_rgba_unmultiplied(text_color.r(), text_color.g(), text_color.b(), opacity);
    let outline_color =
        Color32::from_rgba_unmultiplied(BACKGROUND.r(), BACKGROUND.g(), BACKGROUND.b(), opacity);
    painter
        .with_clip_rect(plot)
        .add(egui::Shape::convex_polygon(
            vec![
                marker.left_top(),
                Pos2::new(body_right, marker.top()),
                marker.right_center(),
                Pos2::new(body_right, marker.bottom()),
                marker.left_bottom(),
            ],
            marker_color,
            Stroke::new(1.0_f32, outline_color),
        ));
    painter.with_clip_rect(plot).text(
        Pos2::new(marker.left() + body_width * 0.5, marker.center().y),
        egui::Align2::CENTER_CENTER,
        marker_text,
        FontId::monospace((marker.height() * 0.65).clamp(7.0, 9.0)),
        text_color,
    );
}

const MAX_LOGIC_TRANSITIONS_PER_PIXEL: usize = 4;

#[derive(Clone, Copy, Debug)]
struct LogicTransitionBucket {
    pixel: i32,
    start_high: bool,
    end_high: bool,
    transition_count: usize,
    transition_x: [f32; MAX_LOGIC_TRANSITIONS_PER_PIXEL],
}

impl Default for LogicTransitionBucket {
    fn default() -> Self {
        Self {
            pixel: -1,
            start_high: false,
            end_high: false,
            transition_count: 0,
            transition_x: [0.0; MAX_LOGIC_TRANSITIONS_PER_PIXEL],
        }
    }
}

impl LogicTransitionBucket {
    fn begin(&mut self, pixel: i32, start_high: bool) {
        self.pixel = pixel;
        self.start_high = start_high;
        self.end_high = start_high;
        self.transition_count = 0;
    }

    fn record(&mut self, x: f32, end_high: bool) {
        if self.transition_count < MAX_LOGIC_TRANSITIONS_PER_PIXEL {
            self.transition_x[self.transition_count] = x;
        }
        self.transition_count += 1;
        self.end_high = end_high;
    }

    fn flush(&mut self, points: &mut Vec<Pos2>, plot: Rect, high: f32, low: f32) {
        if self.transition_count == 0 {
            return;
        }
        let y = |is_high| if is_high { high } else { low };
        if self.transition_count <= MAX_LOGIC_TRANSITIONS_PER_PIXEL {
            let mut is_high = self.start_high;
            for x in self
                .transition_x
                .iter()
                .copied()
                .take(self.transition_count)
            {
                points.push(Pos2::new(x, y(is_high)));
                is_high = !is_high;
                points.push(Pos2::new(x, y(is_high)));
            }
        } else {
            // Once several edges occupy the same screen pixel, their exact
            // spacing is no longer visible. Draw an activity stroke instead
            // of dropping the entire bucket and aliasing the signal.
            let x = (plot.left() + self.pixel as f32 + 0.5).clamp(plot.left(), plot.right());
            points.push(Pos2::new(x, y(self.start_high)));
            points.push(Pos2::new(x, y(!self.start_high)));
            if self.end_high == self.start_high {
                points.push(Pos2::new(x, y(self.end_high)));
            }
        }
        self.transition_count = 0;
        self.pixel = -1;
    }
}

fn logic_waveform_points(
    samples: &VecDeque<u32>,
    visible_range: Range<usize>,
    plot: Rect,
    viewport: WaveformViewport,
    levels: &[(f32, f32); LOGIC_CHANNEL_COUNT],
    enabled_channels: &[bool; LOGIC_CHANNEL_COUNT],
) -> [Vec<Pos2>; LOGIC_CHANNEL_COUNT] {
    let mut points: [Vec<Pos2>; LOGIC_CHANNEL_COUNT] = std::array::from_fn(|_| Vec::new());
    let Some(first_index) = visible_range.clone().next() else {
        return points;
    };
    let sample_count = samples.len();
    let mut previous = samples[first_index];
    let first_x = waveform_sample_x(plot, viewport, first_index as f32, sample_count);
    for channel in 0..LOGIC_CHANNEL_COUNT {
        if enabled_channels[channel] {
            let (high, low) = levels[channel];
            let y = if previous & (1_u32 << channel) != 0 {
                high
            } else {
                low
            };
            points[channel].push(Pos2::new(first_x, y));
        }
    }

    let enabled_mask = enabled_channels
        .iter()
        .enumerate()
        .fold(0_u32, |mask, (channel, enabled)| {
            mask | (u32::from(*enabled) << channel)
        });
    let mut buckets = [LogicTransitionBucket::default(); LOGIC_CHANNEL_COUNT];
    for index in visible_range.clone().skip(1) {
        let sample = samples[index];
        let mut changed = (previous ^ sample) & enabled_mask;
        if changed != 0 {
            let x = waveform_sample_x(plot, viewport, index as f32, sample_count)
                .clamp(plot.left(), plot.right());
            let pixel = (x - plot.left()).floor() as i32;
            while changed != 0 {
                let channel = changed.trailing_zeros() as usize;
                changed &= changed - 1;
                let bucket = &mut buckets[channel];
                if bucket.pixel != pixel {
                    let (high, low) = levels[channel];
                    bucket.flush(&mut points[channel], plot, high, low);
                    bucket.begin(pixel, previous & (1_u32 << channel) != 0);
                }
                bucket.record(x, sample & (1_u32 << channel) != 0);
            }
        }
        previous = sample;
    }

    let last_index = visible_range.end - 1;
    let last_x = waveform_sample_x(plot, viewport, last_index as f32, sample_count);
    for channel in 0..LOGIC_CHANNEL_COUNT {
        if !enabled_channels[channel] {
            continue;
        }
        let (high, low) = levels[channel];
        buckets[channel].flush(&mut points[channel], plot, high, low);
        let y = if previous & (1_u32 << channel) != 0 {
            high
        } else {
            low
        };
        points[channel].push(Pos2::new(last_x, y));
    }
    points
}

fn paint_logic(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    options: LogicWaveformOptions<'_>,
    viewport: &mut WaveformViewport,
) {
    let LogicWaveformOptions {
        channel_colors,
        channel_marker_opacities,
        channel_names,
        channel_settings_drafts,
        maximum_heights,
        enabled_channels,
    } = options;
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    let waveform_area = rect;
    let (status_area, ruler_area, plot_area) = split_waveform_chrome(waveform_area);
    let plot = plot_area;
    let (status_bar, time_ruler) = align_waveform_chrome(status_area, ruler_area, plot);
    paint_plot_grid(ui, plot);
    let sample_count = waveforms.logic_samples.len();
    let display_sample_count = if sample_count == 0 {
        SAMPLE_COUNT as usize
    } else {
        sample_count
    };
    let display_sample_rate_hz = if waveforms.logic_rate_hz == 0 {
        SAMPLE_RATE_HZ
    } else {
        waveforms.logic_rate_hz
    };
    let _ = update_waveform_viewport(
        ui,
        plot,
        "logic_waveform_zoom",
        viewport,
        display_sample_count,
        display_sample_rate_hz,
        true,
    );
    let lane_height = plot.height() / LOGIC_CHANNEL_COUNT as f32;
    for channel in 0..LOGIC_CHANNEL_COUNT {
        let lane_top = plot.top() + channel as f32 * lane_height;
        painter.line_segment(
            [
                Pos2::new(plot.left(), lane_top),
                Pos2::new(plot.right(), lane_top),
            ],
            Stroke::new(0.6_f32, GRID.gamma_multiply(0.45)),
        );
    }
    let visible_range = viewport.sample_range(sample_count);
    let visible_count = visible_range.len();
    let signal_layout = logic_signal_layout(plot.top(), plot.height(), &*maximum_heights);
    let mut waveform_points = logic_waveform_points(
        &waveforms.logic_samples,
        visible_range.clone(),
        plot,
        *viewport,
        &signal_layout.levels,
        enabled_channels,
    );
    let marker_lane_height = signal_layout.base_height * LOGIC_SIGNAL_DEFAULT_STEPS_PER_LANE as f32;
    for channel in 0..LOGIC_CHANNEL_COUNT {
        if !enabled_channels[channel] {
            continue;
        }
        let channel_color = &mut channel_colors[channel];
        let channel_marker_opacity = &mut channel_marker_opacities[channel];
        let channel_name = &mut channel_names[channel];
        let channel_label = channel_label("D", channel, channel_name);
        let marker_text = logic_channel_marker_text(channel, channel_name);
        let (_, low) = signal_layout.levels[channel];
        paint_logic_zero_reference(painter, plot, low, *channel_color);
        let marker_rect = logic_channel_marker_rect(plot, low, marker_lane_height, &marker_text);
        let marker_response = ui
            .interact(
                marker_rect.expand(3.0),
                ui.make_persistent_id(("logic_channel_marker", channel)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!("Click to edit {channel_label} settings"));

        let popup_id = ui.make_persistent_id(("logic_channel_settings_popup", channel));
        if marker_response.clicked() {
            channel_settings_drafts[channel] = LogicChannelSettingsDraft {
                enabled: enabled_channels[channel],
                maximum_height: maximum_heights[channel],
                name: channel_name.clone(),
                color: *channel_color,
                marker_opacity: *channel_marker_opacity,
            };
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &marker_response,
            egui::popup::PopupCloseBehavior::CloseOnClickOutside,
            |popup_ui| {
                let draft = &mut channel_settings_drafts[channel];
                let mut save_requested = false;
                let mut cancel_requested = false;
                popup_ui.set_min_width(280.0);
                popup_ui.label(RichText::new(format!("D{channel} settings")).strong());
                popup_ui.checkbox(&mut draft.enabled, "Enabled");
                popup_ui.horizontal(|ui| {
                    ui.label("Max height");
                    ui.add(
                        egui::Slider::new(&mut draft.maximum_height, 1..=5)
                            .integer()
                            .suffix("×"),
                    );
                });
                popup_ui.separator();
                popup_ui.horizontal(|ui| {
                    ui.label("Name");
                    let name_edit = ui.add(
                        egui::TextEdit::singleline(&mut draft.name)
                            .char_limit(LOGIC_CHANNEL_NAME_MAX_CHARS)
                            .desired_width(190.0)
                            .hint_text("Optional name"),
                    );
                    if name_edit.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        save_requested = true;
                    }
                });
                popup_ui.separator();
                popup_ui.label("Color");
                egui::color_picker::color_picker_color32(
                    popup_ui,
                    &mut draft.color,
                    egui::color_picker::Alpha::Opaque,
                );
                if popup_ui.button("Reset color").clicked() {
                    draft.color = default_logic_channel_colors()[channel];
                }
                popup_ui.separator();
                popup_ui.horizontal(|ui| {
                    ui.label("Opacity");
                    let mut opacity_percent = f32::from(draft.marker_opacity) / 255.0 * 100.0;
                    if ui
                        .add(
                            egui::Slider::new(&mut opacity_percent, 10.0..=100.0)
                                .fixed_decimals(0)
                                .suffix("%"),
                        )
                        .changed()
                    {
                        draft.marker_opacity = (opacity_percent / 100.0 * 255.0).round() as u8;
                    }
                });
                popup_ui.separator();
                popup_ui.horizontal(|ui| {
                    save_requested |= ui.button("Save").clicked();
                    cancel_requested |= ui.button("Cancel").clicked();
                });
                if save_requested {
                    enabled_channels[channel] = draft.enabled;
                    maximum_heights[channel] = draft.maximum_height;
                    *channel_name = draft.name.clone();
                    *channel_color = draft.color;
                    *channel_marker_opacity = draft.marker_opacity;
                    popup_ui.memory_mut(egui::Memory::close_popup);
                } else if cancel_requested {
                    popup_ui.memory_mut(egui::Memory::close_popup);
                }
            },
        );

        if visible_count != 0 {
            let points = std::mem::take(&mut waveform_points[channel]);
            if points.len() > 1 {
                painter.add(egui::Shape::line(
                    points,
                    Stroke::new(1.2_f32, *channel_color),
                ));
            }
        }
        paint_logic_channel_marker(
            painter,
            plot,
            low,
            marker_lane_height,
            &marker_text,
            *channel_color,
            *channel_marker_opacity,
        );
    }
    if visible_count == 0 {
        paint_empty(painter, plot, "Waiting for logic samples");
    }
    paint_waveform_time_scale(
        painter,
        time_ruler,
        display_sample_count,
        display_sample_rate_hz,
        waveforms.logic_sample_offset,
        *viewport,
    );
    paint_waveform_status(
        painter,
        status_bar,
        display_sample_count,
        display_sample_rate_hz,
        *viewport,
    );
}

fn format_trigger_voltage(value: f32) -> String {
    if value.abs() >= 1.0 {
        format!("{value:.2} V")
    } else if value.abs() >= 0.1 {
        format!("{:.0} mV", value * 1_000.0)
    } else {
        format!("{:.2} mV", value * 1_000.0)
    }
}

fn format_sample_rate(rate_hz: u64) -> String {
    if rate_hz >= 1_000_000_000 {
        format!("{:.2} GSa/s", rate_hz as f64 / 1_000_000_000.0)
    } else if rate_hz >= 1_000_000 {
        format!("{:.2} MSa/s", rate_hz as f64 / 1_000_000.0)
    } else if rate_hz >= 1_000 {
        format!("{:.2} kSa/s", rate_hz as f64 / 1_000.0)
    } else {
        format!("{rate_hz} Sa/s")
    }
}

fn paint_scope_graticule(painter: &egui::Painter, rect: Rect) {
    for index in 0..=SCOPE_HORIZONTAL_DIVISIONS {
        let x = egui::lerp(
            rect.x_range(),
            index as f32 / SCOPE_HORIZONTAL_DIVISIONS as f32,
        );
        let center = index == SCOPE_HORIZONTAL_DIVISIONS / 2;
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(
                if center { 1.2_f32 } else { 0.8_f32 },
                if center {
                    TEXT_MUTED.gamma_multiply(0.45)
                } else {
                    GRID.gamma_multiply(0.8)
                },
            ),
        );
    }
    for index in 0..=SCOPE_VERTICAL_DIVISIONS {
        let y = egui::lerp(
            rect.y_range(),
            index as f32 / SCOPE_VERTICAL_DIVISIONS as f32,
        );
        let center = index == SCOPE_VERTICAL_DIVISIONS / 2;
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(
                if center { 1.2_f32 } else { 0.8_f32 },
                if center {
                    TEXT_MUTED.gamma_multiply(0.45)
                } else {
                    GRID.gamma_multiply(0.8)
                },
            ),
        );
    }

    let center = rect.center();
    for index in 0..=SCOPE_HORIZONTAL_DIVISIONS * 5 {
        let x = egui::lerp(
            rect.x_range(),
            index as f32 / (SCOPE_HORIZONTAL_DIVISIONS * 5) as f32,
        );
        painter.circle_filled(Pos2::new(x, center.y), 0.8, TEXT_MUTED.gamma_multiply(0.65));
    }
    for index in 0..=SCOPE_VERTICAL_DIVISIONS * 5 {
        let y = egui::lerp(
            rect.y_range(),
            index as f32 / (SCOPE_VERTICAL_DIVISIONS * 5) as f32,
        );
        painter.circle_filled(Pos2::new(center.x, y), 0.8, TEXT_MUTED.gamma_multiply(0.65));
    }
}

fn scope_vertical_offset_ratio(vertical_offset: u16) -> f32 {
    let span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    f32::from(
        vertical_offset.clamp(DSCOPE_VERTICAL_OFFSET_MIN, DSCOPE_VERTICAL_OFFSET_MAX)
            - DSCOPE_VERTICAL_OFFSET_MIN,
    ) / span
}

fn scope_vertical_offset_y(rect: Rect, vertical_offset: u16) -> f32 {
    egui::lerp(rect.y_range(), scope_vertical_offset_ratio(vertical_offset))
}

fn scope_vertical_offset_from_y(rect: Rect, y: f32) -> u16 {
    let ratio = ((y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
    let span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    (f32::from(DSCOPE_VERTICAL_OFFSET_MIN) + ratio * span).round() as u16
}

fn scope_sample_y(plot: Rect, channel: ScopeChannelSettings, voltage: f32) -> f32 {
    let volts_per_div =
        f32::from(channel.volts_per_div_mv) / 1_000.0 * f32::from(channel.probe_attenuation);
    let voltage_span = volts_per_div * SCOPE_VERTICAL_DIVISIONS as f32;
    scope_vertical_offset_y(plot, channel.vertical_offset) - voltage / voltage_span * plot.height()
}

fn scope_vertical_offset_divisions(rect: Rect, vertical_offset: u16) -> f32 {
    let center_y = scope_vertical_offset_y(rect, DSCOPE_VERTICAL_OFFSET_CENTER);
    (center_y - scope_vertical_offset_y(rect, vertical_offset)) / rect.height().max(1.0)
        * SCOPE_VERTICAL_DIVISIONS as f32
}

fn scope_offset_arrow_at_pointer(
    plot: Rect,
    pointer_y: f32,
    settings: ScopeCaptureSettings,
    vertical_offsets: &[u16; 2],
) -> Option<usize> {
    const HIT_RADIUS: f32 = 18.0;
    let mut closest: Option<(usize, f32)> = None;
    for (channel, vertical_offset) in vertical_offsets.iter().copied().enumerate() {
        if !settings.channels[channel].enabled {
            continue;
        }
        let distance = (pointer_y - scope_vertical_offset_y(plot, vertical_offset)).abs();
        let is_closer = match closest {
            Some((_, closest_distance)) => distance < closest_distance,
            None => true,
        };
        if is_closer {
            closest = Some((channel, distance));
        }
    }
    closest
        .filter(|(_, distance)| *distance <= HIT_RADIUS)
        .map(|(channel, _)| channel)
}

struct ScopeOffsetArrowOptions<'a> {
    settings: ScopeCaptureSettings,
    channel_colors: [Color32; 2],
    channel_names: &'a [String; 2],
    vertical_offsets: &'a mut [u16; 2],
    controls_enabled: bool,
}

fn paint_scope_offset_arrows(ui: &egui::Ui, plot: Rect, options: ScopeOffsetArrowOptions<'_>) {
    let ScopeOffsetArrowOptions {
        settings,
        channel_colors,
        channel_names,
        vertical_offsets,
        controls_enabled,
    } = options;
    const ARROW_BODY_WIDTH: f32 = 15.0;
    const ARROW_TIP_WIDTH: f32 = 8.0;
    const ARROW_HALF_HEIGHT: f32 = 9.0;
    const DOT_SPACING: f32 = 7.0;
    let painter = ui.painter().with_clip_rect(plot);
    let arrow_x = plot.left() + 2.0;
    let arrow_width = ARROW_BODY_WIDTH + ARROW_TIP_WIDTH;
    let interaction_rect = Rect::from_min_max(
        plot.left_top(),
        Pos2::new(
            (plot.left() + arrow_width + 4.0).min(plot.right()),
            plot.bottom(),
        ),
    );
    let interaction_id = ui.make_persistent_id("scope_vertical_offset_arrows");
    let response = ui.interact(
        interaction_rect,
        interaction_id,
        if controls_enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let hovered_channel = response.hover_pos().and_then(|pointer| {
        scope_offset_arrow_at_pointer(plot, pointer.y, settings, vertical_offsets)
    });
    let response = if let Some(channel) = hovered_channel {
        response
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_text(format!(
                "{} vertical offset: {:+.2} div\nDrag to move · double-click to center",
                scope_channel_label(channel, channel_names),
                scope_vertical_offset_divisions(plot, vertical_offsets[channel])
            ))
    } else {
        response
    };

    if controls_enabled {
        if response.drag_started() {
            let channel = response.interact_pointer_pos().and_then(|pointer| {
                scope_offset_arrow_at_pointer(plot, pointer.y, settings, vertical_offsets)
            });
            ui.data_mut(|data| {
                data.remove_temp::<usize>(interaction_id);
                if let Some(channel) = channel {
                    data.insert_temp(interaction_id, channel);
                }
            });
        }
        if response.double_clicked() {
            if let Some(channel) = response.interact_pointer_pos().and_then(|pointer| {
                scope_offset_arrow_at_pointer(plot, pointer.y, settings, vertical_offsets)
            }) {
                vertical_offsets[channel] = DSCOPE_VERTICAL_OFFSET_CENTER;
            }
        } else if response.dragged() {
            let channel = ui.data(|data| data.get_temp::<usize>(interaction_id));
            if let (Some(channel), Some(pointer)) = (channel, response.interact_pointer_pos()) {
                vertical_offsets[channel] = scope_vertical_offset_from_y(plot, pointer.y);
            }
        }
        if response.drag_stopped() {
            ui.data_mut(|data| data.remove_temp::<usize>(interaction_id));
        }
    }

    for (channel, color) in channel_colors.iter().enumerate() {
        if !settings.channels[channel].enabled {
            continue;
        }
        let arrow_y = scope_vertical_offset_y(plot, vertical_offsets[channel]);
        let mut x = plot.left();
        while x <= plot.right() {
            painter.circle_filled(Pos2::new(x, arrow_y), 0.8, color.gamma_multiply(0.35));
            x += DOT_SPACING;
        }
    }

    for channel in (0..channel_colors.len()).rev() {
        if !settings.channels[channel].enabled {
            continue;
        }
        let display_color = channel_colors[channel];
        let arrow_y = scope_vertical_offset_y(plot, vertical_offsets[channel]);
        let body_right = arrow_x + ARROW_BODY_WIDTH;
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(arrow_x, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_right, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_right + ARROW_TIP_WIDTH, arrow_y),
                Pos2::new(body_right, arrow_y + ARROW_HALF_HEIGHT),
                Pos2::new(arrow_x, arrow_y + ARROW_HALF_HEIGHT),
            ],
            display_color,
            Stroke::new(1.0_f32, BACKGROUND),
        ));
        painter.text(
            Pos2::new(arrow_x + ARROW_BODY_WIDTH * 0.5, arrow_y),
            egui::Align2::CENTER_CENTER,
            channel.to_string(),
            FontId::monospace(10.0),
            marker_text_color(display_color),
        );
    }
}

fn marker_text_color(marker_color: Color32) -> Color32 {
    let luminance = (299 * u32::from(marker_color.r())
        + 587 * u32::from(marker_color.g())
        + 114 * u32::from(marker_color.b()))
        / 1_000;
    if luminance >= 140 {
        BACKGROUND
    } else {
        Color32::WHITE
    }
}

fn scope_trigger_level_y(plot: Rect, channel: ScopeChannelSettings, trigger_level: u8) -> f32 {
    scope_sample_y(
        plot,
        channel,
        scope_trigger_threshold_voltage(channel, trigger_level),
    )
}

fn scope_trigger_level_from_y(plot: Rect, y: f32, channel: ScopeChannelSettings) -> u8 {
    let zero_y = scope_vertical_offset_y(plot, channel.vertical_offset);
    let screen_divisions = (y - zero_y) / plot.height().max(1.0) * SCOPE_VERTICAL_DIVISIONS as f32;
    let reference_span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    let level = (f32::from(DSCOPE_VERTICAL_OFFSET_CENTER)
        + screen_divisions / SCOPE_ADC_VERTICAL_DIVISIONS * reference_span)
        .round()
        .clamp(
            f32::from(DSCOPE_TRIGGER_LEVEL_MIN),
            f32::from(DSCOPE_TRIGGER_LEVEL_MAX),
        );
    level as u8
}

fn scope_trigger_channels(settings: ScopeTriggerSettings) -> &'static [usize] {
    match settings.source {
        DsCopeTriggerSource::Auto if settings.auto_channel == 0 => &[0],
        DsCopeTriggerSource::Auto => &[1],
        DsCopeTriggerSource::ChannelZero => &[0],
        DsCopeTriggerSource::ChannelOne => &[1],
        DsCopeTriggerSource::ChannelZeroAndOne | DsCopeTriggerSource::ChannelZeroOrOne => &[0, 1],
    }
}

fn scope_trigger_threshold_voltage(channel: ScopeChannelSettings, trigger_level: u8) -> f32 {
    let reference_span = f32::from(DSCOPE_VERTICAL_OFFSET_MAX - DSCOPE_VERTICAL_OFFSET_MIN);
    let input_span = f32::from(channel.volts_per_div_mv) / 1_000.0
        * SCOPE_ADC_VERTICAL_DIVISIONS
        * f32::from(channel.probe_attenuation);
    (f32::from(DSCOPE_VERTICAL_OFFSET_CENTER) - f32::from(trigger_level)) / reference_span
        * input_span
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeTriggerCrossing {
    sample: f32,
    channel_index: usize,
}

fn scope_trigger_crossing_sample(
    waveforms: &WaveformStore,
    settings: ScopeCaptureSettings,
    trigger: ScopeTriggerSettings,
) -> Option<ScopeTriggerCrossing> {
    // The FPGA trigger pointer may lead or trail the analog threshold crossing
    // slightly because of the ADC/trigger pipeline and the footer byte offset.
    // Select the closest matching edge instead of requiring it to be exactly
    // one sample before the reported position.
    const TRIGGER_SEARCH_RADIUS: usize = 512;
    let hardware_trigger = waveforms
        .scope_trigger_sample_index
        .and_then(|trigger_sample| trigger_sample.checked_sub(waveforms.scope_sample_offset))
        .and_then(|trigger_sample| usize::try_from(trigger_sample).ok());
    if hardware_trigger.is_none() && trigger.source != DsCopeTriggerSource::Auto {
        return None;
    }
    let capture_boundaries = waveforms
        .scope_capture_boundaries
        .iter()
        .filter_map(|boundary| boundary.checked_sub(waveforms.scope_sample_offset))
        .filter_map(|boundary| usize::try_from(boundary).ok())
        .collect::<HashSet<_>>();
    let mut closest_crossing = None::<(ScopeTriggerCrossing, f32)>;

    for &channel_index in scope_trigger_channels(trigger) {
        let channel_settings = settings.channels[channel_index];
        if !channel_settings.enabled {
            continue;
        }
        let Some(samples) = waveforms.scope_channels.get(channel_index) else {
            continue;
        };
        if samples.len() < 2 {
            continue;
        }
        let reference_sample = match hardware_trigger {
            Some(sample) if sample < samples.len() => sample as f32,
            Some(_) => continue,
            None => f32::from(trigger.position_percent) / 100.0 * (samples.len() - 1) as f32,
        };
        let threshold =
            scope_trigger_threshold_voltage(channel_settings, trigger.levels[channel_index]);
        let (first_left, last_left) = if trigger.source == DsCopeTriggerSource::Auto {
            // Auto capture can return a free-running frame without setting the
            // hardware trigger flag. If that frame nevertheless contains the
            // requested edge, lock it to the trigger position just like a
            // hardware-triggered frame. Only free-run when no edge exists.
            (0, samples.len() - 1)
        } else {
            let sample = hardware_trigger.expect("non-auto trigger requires a hardware position");
            (
                sample.saturating_sub(TRIGGER_SEARCH_RADIUS),
                sample
                    .saturating_add(TRIGGER_SEARCH_RADIUS)
                    .min(samples.len() - 1),
            )
        };
        for left in first_left..last_left {
            let right = left + 1;
            if capture_boundaries.contains(&right) {
                continue;
            }
            let (left_value, right_value) = (samples[left], samples[right]);
            let crosses = match trigger.slope {
                DsCopeTriggerSlope::Rising => {
                    (left_value < threshold && right_value >= threshold)
                        || (left_value <= threshold && right_value > threshold)
                }
                DsCopeTriggerSlope::Falling => {
                    (left_value > threshold && right_value <= threshold)
                        || (left_value >= threshold && right_value < threshold)
                }
            };
            if !crosses {
                continue;
            }
            let delta = right_value - left_value;
            let interpolation = if delta.abs() <= f32::EPSILON {
                0.0
            } else {
                ((threshold - left_value) / delta).clamp(0.0, 1.0)
            };
            let crossing = ScopeTriggerCrossing {
                sample: waveforms.scope_sample_offset as f32 + left as f32 + interpolation,
                channel_index,
            };
            let distance = (left as f32 + interpolation - reference_sample).abs();
            if closest_crossing.map_or(true, |(_, current_distance)| distance < current_distance) {
                closest_crossing = Some((crossing, distance));
            }
        }
    }
    closest_crossing.map(|(crossing, _)| crossing)
}

fn scope_sample_fraction(sample: f32, sample_offset: u64, sample_count: usize) -> f32 {
    let local_sample = sample - sample_offset as f32;
    local_sample / sample_count.saturating_sub(1).max(1) as f32
}

fn waveform_sample_x(
    plot: Rect,
    viewport: WaveformViewport,
    sample_index: f32,
    sample_count: usize,
) -> f32 {
    let sample_fraction = sample_index / sample_count.saturating_sub(1).max(1) as f32;
    let screen_fraction = viewport.screen_fraction_at_domain(sample_fraction);
    plot.left() + screen_fraction * plot.width()
}

fn scope_plot_sample_indices(
    visible_range: Range<usize>,
    step: usize,
    channel_index: usize,
    trigger_crossing: Option<ScopeTriggerCrossing>,
    sample_offset: u64,
) -> Vec<usize> {
    let mut indices = visible_range
        .clone()
        .step_by(step.max(1))
        .collect::<Vec<_>>();
    if let Some(last) = visible_range.end.checked_sub(1) {
        indices.push(last);
    }
    if let Some(crossing) = trigger_crossing.filter(|crossing| {
        crossing.channel_index == channel_index && crossing.sample >= sample_offset as f32
    }) {
        let local_crossing = crossing.sample - sample_offset as f32;
        for index in [
            local_crossing.floor() as usize,
            local_crossing.ceil() as usize,
        ] {
            if visible_range.contains(&index) {
                indices.push(index);
            }
        }
    }
    indices.sort_unstable();
    indices.dedup();
    indices
}

fn scope_plot_sample_segments(
    visible_range: Range<usize>,
    step: usize,
    channel_index: usize,
    trigger_crossing: Option<ScopeTriggerCrossing>,
    sample_offset: u64,
    capture_boundaries: &VecDeque<u64>,
) -> Vec<Vec<usize>> {
    let mut indices = scope_plot_sample_indices(
        visible_range.clone(),
        step,
        channel_index,
        trigger_crossing,
        sample_offset,
    );
    let local_boundaries = capture_boundaries
        .iter()
        .filter_map(|boundary| boundary.checked_sub(sample_offset))
        .filter_map(|boundary| usize::try_from(boundary).ok())
        .filter(|boundary| *boundary > visible_range.start && *boundary < visible_range.end)
        .collect::<Vec<_>>();
    for boundary in &local_boundaries {
        indices.extend([boundary - 1, *boundary]);
    }
    indices.sort_unstable();
    indices.dedup();

    let mut segments = Vec::new();
    let mut current = Vec::new();
    for index in indices {
        if local_boundaries.binary_search(&index).is_ok() && !current.is_empty() {
            segments.push(std::mem::take(&mut current));
        }
        current.push(index);
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

fn scope_trigger_position_x(plot: Rect, position_percent: u8) -> f32 {
    egui::lerp(plot.x_range(), f32::from(position_percent) / 100.0)
}

fn scope_trigger_position_from_x(plot: Rect, x: f32) -> u8 {
    let ratio = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    (ratio * 100.0).round().clamp(
        f32::from(DSCOPE_TRIGGER_POSITION_MIN_PERCENT),
        f32::from(DSCOPE_TRIGGER_POSITION_MAX_PERCENT),
    ) as u8
}

fn scope_trigger_position_in_view(trigger_sample_fraction: f32, viewport: WaveformViewport) -> u8 {
    let position_percent =
        ((trigger_sample_fraction - viewport.start) / viewport.span.max(f32::EPSILON) * 100.0)
            .round() as i32;
    position_percent.clamp(
        i32::from(DSCOPE_TRIGGER_POSITION_MIN_PERCENT),
        i32::from(DSCOPE_TRIGGER_POSITION_MAX_PERCENT),
    ) as u8
}

fn finish_scope_viewport_pan(
    trigger_sample_fraction: f32,
    viewport: &mut WaveformViewport,
    trigger_settings: &mut ScopeTriggerSettings,
) {
    trigger_settings.position_percent =
        scope_trigger_position_in_view(trigger_sample_fraction, *viewport);
    // A manual waveform pan must remain detached from the trigger. Relocking
    // here causes the next frame to recenter the viewport and makes time
    // markers appear to jump away from their capture positions.
    viewport.anchor_locked = false;
}

fn paint_scope_trigger_position(
    ui: &egui::Ui,
    time_ruler: Rect,
    plot: Rect,
    settings: &mut ScopeTriggerSettings,
    controls_enabled: bool,
    viewport: &mut WaveformViewport,
) {
    const COLOR: Color32 = Color32::from_rgb(235, 72, 72);
    const ARROW_HALF_WIDTH: f32 = 10.0;
    const ARROW_HEIGHT: f32 = 19.0;
    const ARROW_SHOULDER_HEIGHT: f32 = 9.0;
    const DOT_SPACING: f32 = 7.0;
    let x = scope_trigger_position_x(plot, settings.position_percent);
    let arrow_top = plot.top() - ARROW_HEIGHT;
    let marker_rect = Rect::from_min_max(
        Pos2::new(x - ARROW_HALF_WIDTH - 4.0, arrow_top - 3.0),
        Pos2::new(x + ARROW_HALF_WIDTH + 4.0, plot.top() + 8.0),
    );
    let response = ui.interact(
        marker_rect,
        ui.make_persistent_id("scope_trigger_position"),
        if controls_enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let response = if controls_enabled {
        response.on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
    } else {
        response
    }
    .on_hover_text(format!("{}%", settings.position_percent));

    if controls_enabled {
        let old_position = settings.position_percent;
        if response.double_clicked() {
            settings.position_percent = 50;
        } else if response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                settings.position_percent = scope_trigger_position_from_x(plot, pointer.x);
            }
        }
        if settings.position_percent != old_position {
            viewport.move_screen_anchor(
                f32::from(old_position) / 100.0,
                f32::from(settings.position_percent) / 100.0,
            );
            ui.ctx().request_repaint();
        }
    }

    let x = scope_trigger_position_x(plot, settings.position_percent);
    let clip_rect = Rect::from_min_max(
        Pos2::new(plot.left(), time_ruler.top()),
        plot.right_bottom(),
    );
    let painter = ui.painter().with_clip_rect(clip_rect);
    let mut y = plot.top();
    while y <= plot.bottom() {
        painter.circle_filled(Pos2::new(x, y), 0.85, COLOR.gamma_multiply(0.62));
        y += DOT_SPACING;
    }
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(x - ARROW_HALF_WIDTH, plot.top() - ARROW_HEIGHT),
            Pos2::new(x + ARROW_HALF_WIDTH, plot.top() - ARROW_HEIGHT),
            Pos2::new(x + ARROW_HALF_WIDTH, plot.top() - ARROW_SHOULDER_HEIGHT),
            Pos2::new(x, plot.top()),
            Pos2::new(x - ARROW_HALF_WIDTH, plot.top() - ARROW_SHOULDER_HEIGHT),
        ],
        COLOR,
        Stroke::new(1.0_f32, BACKGROUND),
    ));
    painter.text(
        Pos2::new(x, plot.top() - ARROW_HEIGHT + 5.5),
        egui::Align2::CENTER_CENTER,
        "T",
        FontId::monospace(9.0),
        marker_text_color(COLOR),
    );
}

fn paint_scope_trigger_level(
    ui: &egui::Ui,
    plot: Rect,
    scope_settings: ScopeCaptureSettings,
    settings: &mut ScopeTriggerSettings,
    channel_colors: [Color32; 2],
    vertical_offsets: &[u16; 2],
    controls_enabled: bool,
) {
    const ARROW_BODY_WIDTH: f32 = 19.0;
    const ARROW_TIP_WIDTH: f32 = 8.0;
    const ARROW_HALF_HEIGHT: f32 = 9.0;
    const DOT_SPACING: f32 = 7.0;
    let painter = ui.painter().with_clip_rect(plot);
    let active_channels = scope_trigger_channels(*settings)
        .iter()
        .copied()
        .filter(|channel| scope_settings.channels[*channel].enabled)
        .collect::<Vec<_>>();

    for channel in active_channels.iter().copied() {
        let color = channel_colors[channel];
        let mut channel_settings = scope_settings.channels[channel];
        channel_settings.vertical_offset = vertical_offsets[channel];
        let trigger_voltage =
            scope_trigger_threshold_voltage(channel_settings, settings.levels[channel]);
        let arrow_x = plot.right() - 2.0;
        let arrow_width = ARROW_BODY_WIDTH + ARROW_TIP_WIDTH;
        let arrow_y = scope_trigger_level_y(plot, channel_settings, settings.levels[channel]);
        let hit_rect = Rect::from_center_size(
            Pos2::new(arrow_x - arrow_width * 0.5, arrow_y),
            Vec2::new(arrow_width + 4.0, ARROW_HALF_HEIGHT * 2.0 + 10.0),
        );
        let response = ui.interact(
            hit_rect,
            ui.make_persistent_id(("scope_trigger_level", channel)),
            if controls_enabled {
                Sense::click_and_drag()
            } else {
                Sense::hover()
            },
        );
        let response = if controls_enabled {
            response.on_hover_cursor(egui::CursorIcon::ResizeVertical)
        } else {
            response
        }
        .on_hover_text(format!(
            "Trigger level: {}",
            format_trigger_voltage(trigger_voltage)
        ));

        if controls_enabled {
            if response.double_clicked() {
                settings.levels[channel] = DSCOPE_TRIGGER_LEVEL_CENTER;
            } else if response.dragged() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    settings.levels[channel] =
                        scope_trigger_level_from_y(plot, pointer.y, channel_settings);
                }
            }
        }

        let arrow_y = scope_trigger_level_y(plot, channel_settings, settings.levels[channel]);
        let mut x = plot.left();
        while x <= plot.right() {
            painter.circle_filled(Pos2::new(x, arrow_y), 0.8, color.gamma_multiply(0.45));
            x += DOT_SPACING;
        }
        let body_left = arrow_x - ARROW_BODY_WIDTH;
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(arrow_x, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_left, arrow_y - ARROW_HALF_HEIGHT),
                Pos2::new(body_left - ARROW_TIP_WIDTH, arrow_y),
                Pos2::new(body_left, arrow_y + ARROW_HALF_HEIGHT),
                Pos2::new(arrow_x, arrow_y + ARROW_HALF_HEIGHT),
            ],
            color,
            Stroke::new(1.0_f32, BACKGROUND),
        ));
        painter.text(
            Pos2::new(arrow_x - ARROW_BODY_WIDTH * 0.5, arrow_y),
            egui::Align2::CENTER_CENTER,
            format!("T{channel}"),
            FontId::monospace(9.0),
            marker_text_color(color),
        );
    }
}

struct ScopeMeasurementPaintOptions<'a> {
    visible_range: Range<usize>,
    sample_rate_hz: u64,
    settings: ScopeFunctionSettings,
    enabled_channels: [bool; 2],
    channel_colors: [Color32; 2],
    channel_names: &'a [String; 2],
    caches: &'a mut [ScopeMeasurementCache; 2],
    window_positions: &'a mut [Vec2; 2],
}

fn paint_scope_measurements(
    ui: &egui::Ui,
    painter: &egui::Painter,
    plot: Rect,
    waveforms: &WaveformStore,
    options: ScopeMeasurementPaintOptions<'_>,
) {
    let ScopeMeasurementPaintOptions {
        visible_range,
        sample_rate_hz,
        settings,
        enabled_channels,
        channel_colors,
        channel_names,
        caches,
        window_positions,
    } = options;
    const ROW_HEIGHT: f32 = 20.0;
    const COLUMN_WIDTH: f32 = 176.0;
    const PADDING: f32 = 8.0;
    const HEADER_HEIGHT: f32 = 25.0;
    const HEADER_FONT_SIZE: f32 = 10.0;
    const LABEL_FONT_SIZE: f32 = 10.0;
    const VALUE_FONT_SIZE: f32 = 12.0;

    for channel in 0..2 {
        let channel_settings = settings.measurements.channels[channel];
        if !enabled_channels[channel]
            || !channel_settings.enabled
            || waveforms.scope_channels.get(channel).is_none()
        {
            continue;
        }
        let selected = SCOPE_MEASUREMENT_KINDS
            .into_iter()
            .filter(|kind| channel_settings.selected[kind.index()])
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let results =
            caches[channel].results(waveforms, channel, visible_range.clone(), sample_rate_hz);
        let rows = selected.len();
        let desired_size = Vec2::new(
            COLUMN_WIDTH + PADDING * 2.0,
            HEADER_HEIGHT + rows as f32 * ROW_HEIGHT + PADDING * 2.0,
        );
        let panel_size = Vec2::new(
            desired_size.x.min(plot.width()),
            desired_size.y.min(plot.height()),
        );
        let mut panel = scope_measurement_window_rect(plot, panel_size, window_positions[channel]);
        let header = Rect::from_min_size(
            panel.min,
            Vec2::new(panel.width(), HEADER_HEIGHT.min(panel.height())),
        );
        let response = ui
            .interact(
                header,
                ui.make_persistent_id(("scope_measurement_window", channel)),
                Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab)
            .on_hover_text(format!(
                "Drag the {} measurement window",
                scope_channel_label(channel, channel_names)
            ));
        if response.dragged() {
            let delta = ui.input(|input| input.pointer.delta());
            panel = panel.translate(delta);
            panel = panel.translate(Vec2::new(
                panel
                    .left()
                    .clamp(plot.left(), plot.right() - panel.width())
                    - panel.left(),
                panel
                    .top()
                    .clamp(plot.top(), plot.bottom() - panel.height())
                    - panel.top(),
            ));
            window_positions[channel] = scope_measurement_window_position(plot, panel);
            ui.ctx().request_repaint();
        }

        let color = channel_colors[channel];
        painter.rect_filled(
            panel,
            egui::Rounding::same(5.0),
            Color32::from_rgba_unmultiplied(8, 12, 18, 230),
        );
        painter.rect_filled(
            Rect::from_min_size(panel.min, Vec2::new(panel.width(), HEADER_HEIGHT)),
            egui::Rounding::same(5.0),
            color.gamma_multiply(0.22),
        );
        painter.text(
            panel.left_top() + Vec2::new(PADDING, 5.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{} MEASUREMENTS",
                scope_channel_label(channel, channel_names)
            ),
            FontId::monospace(HEADER_FONT_SIZE),
            color,
        );
        painter.text(
            panel.right_top() + Vec2::new(-PADDING, 5.0),
            egui::Align2::RIGHT_TOP,
            "⠿",
            FontId::proportional(13.0),
            color,
        );

        for (ordinal, kind) in selected.into_iter().enumerate() {
            let row = ordinal;
            let origin = panel.left_top()
                + Vec2::new(PADDING, PADDING + HEADER_HEIGHT + row as f32 * ROW_HEIGHT);
            painter.text(
                origin,
                egui::Align2::LEFT_TOP,
                kind.label(),
                FontId::monospace(LABEL_FONT_SIZE),
                TEXT_MUTED,
            );
            painter.text(
                origin + Vec2::new(COLUMN_WIDTH - PADDING * 2.0, -1.0),
                egui::Align2::RIGHT_TOP,
                results.formatted(kind),
                FontId::monospace(VALUE_FONT_SIZE),
                color,
            );
        }
    }
}

fn scope_measurement_window_rect(plot: Rect, size: Vec2, position: Vec2) -> Rect {
    let travel = Vec2::new(
        (plot.width() - size.x).max(0.0),
        (plot.height() - size.y).max(0.0),
    );
    Rect::from_min_size(
        plot.min
            + Vec2::new(
                position.x.clamp(0.0, 1.0) * travel.x,
                position.y.clamp(0.0, 1.0) * travel.y,
            ),
        size,
    )
}

fn scope_measurement_window_position(plot: Rect, panel: Rect) -> Vec2 {
    let travel = Vec2::new(
        (plot.width() - panel.width()).max(0.0),
        (plot.height() - panel.height()).max(0.0),
    );
    Vec2::new(
        if travel.x > 0.0 {
            (panel.left() - plot.left()) / travel.x
        } else {
            0.0
        },
        if travel.y > 0.0 {
            (panel.top() - plot.top()) / travel.y
        } else {
            0.0
        },
    )
}

struct ScopeMarkerPaintOptions<'a> {
    settings: &'a mut ScopeMarkerSettings,
    scope_settings: ScopeCaptureSettings,
    channel_names: &'a [String; 2],
    sample_count: usize,
    sample_rate_hz: u64,
    viewport: WaveformViewport,
}

const SCOPE_MARKER_LINE_WIDTH: f32 = 0.5;

fn paint_scope_markers(
    ui: &egui::Ui,
    painter: &egui::Painter,
    plot: Rect,
    options: ScopeMarkerPaintOptions<'_>,
) {
    let ScopeMarkerPaintOptions {
        settings: marker_settings,
        scope_settings,
        channel_names,
        sample_count,
        sample_rate_hz,
        viewport,
    } = options;
    for channel in 0..2 {
        if !scope_settings.channels[channel].enabled {
            continue;
        }
        let channel_markers = &mut marker_settings.channels[channel];
        let horizontal_count = channel_markers.horizontal_count.min(SCOPE_MARKER_LIMIT);
        for (index, position) in channel_markers.horizontal_positions[..horizontal_count]
            .iter_mut()
            .enumerate()
        {
            let color = channel_markers.horizontal_colors[index];
            let x = egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain(position.clamp(0.0, 1.0)),
            );
            if (plot.left()..=plot.right()).contains(&x) {
                let hit_rect = Rect::from_min_max(
                    Pos2::new(x - 7.0, plot.top()),
                    Pos2::new(x + 7.0, plot.bottom()),
                );
                let response = ui
                    .interact(
                        hit_rect,
                        ui.make_persistent_id(("scope_time_marker", channel, index)),
                        Sense::drag(),
                    )
                    .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                    .on_hover_text(format!(
                        "Drag {} T{} horizontally",
                        scope_channel_label(channel, channel_names),
                        index + 1
                    ));
                if response.dragged() {
                    if let Some(pointer) = response.interact_pointer_pos() {
                        let screen_fraction = (pointer.x - plot.left()) / plot.width().max(1.0);
                        *position = viewport
                            .domain_fraction_at_screen(screen_fraction)
                            .clamp(0.0, 1.0);
                    }
                }
            }
            let x = egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain(position.clamp(0.0, 1.0)),
            );
            paint_dashed_line(
                painter,
                Pos2::new(x, plot.top()),
                Pos2::new(x, plot.bottom()),
                Stroke::new(SCOPE_MARKER_LINE_WIDTH, color),
            );
            paint_marker_handle(
                painter,
                Pos2::new(x, plot.top() + 10.0),
                &format!("T{channel}{}", index + 1),
                color,
            );
        }
        for index in 0..horizontal_count {
            let Some(other) = channel_markers.horizontal_comparisons[index]
                .filter(|other| *other < horizontal_count && *other != index)
            else {
                continue;
            };
            if other < index && channel_markers.horizontal_comparisons[other] == Some(index) {
                continue;
            }
            let first = channel_markers.horizontal_positions[index];
            let second = channel_markers.horizontal_positions[other];
            let Some(delta_seconds) =
                scope_time_marker_delta(first, second, sample_count, sample_rate_hz)
            else {
                continue;
            };
            let frequency = (delta_seconds > 0.0).then(|| 1.0 / delta_seconds);
            let text = match frequency {
                Some(frequency) => format!(
                    "{} T{}–T{}  {}  ·  {}",
                    scope_channel_label(channel, channel_names),
                    index + 1,
                    other + 1,
                    format_duration(Duration::from_secs_f64(delta_seconds)),
                    format_frequency(frequency)
                ),
                None => format!(
                    "{} T{}–T{}  {}",
                    scope_channel_label(channel, channel_names),
                    index + 1,
                    other + 1,
                    format_duration(Duration::from_secs_f64(delta_seconds))
                ),
            };
            let center_x = egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain((first + second) * 0.5),
            );
            paint_marker_readout(
                painter,
                plot,
                Pos2::new(
                    center_x,
                    plot.top() + 33.0 + index as f32 * 27.0 + channel as f32 * 13.0,
                ),
                &text,
                channel_markers.horizontal_colors[index],
            );
        }

        let vertical_count = channel_markers.vertical_count.min(SCOPE_MARKER_LIMIT);
        for (index, position) in channel_markers.vertical_positions[..vertical_count]
            .iter_mut()
            .enumerate()
        {
            let color = channel_markers.vertical_colors[index];
            let y = egui::lerp(plot.y_range(), position.clamp(0.0, 1.0));
            let hit_rect = Rect::from_min_max(
                Pos2::new(plot.left(), y - 7.0),
                Pos2::new(plot.right(), y + 7.0),
            );
            let response = ui
                .interact(
                    hit_rect,
                    ui.make_persistent_id(("scope_voltage_marker", channel, index)),
                    Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeVertical)
                .on_hover_text(format!(
                    "Drag {} V{} vertically",
                    scope_channel_label(channel, channel_names),
                    index + 1
                ));
            if response.dragged() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    *position = ((pointer.y - plot.top()) / plot.height().max(1.0)).clamp(0.0, 1.0);
                }
            }
            let y = egui::lerp(plot.y_range(), position.clamp(0.0, 1.0));
            paint_dashed_line(
                painter,
                Pos2::new(plot.left(), y),
                Pos2::new(plot.right(), y),
                Stroke::new(SCOPE_MARKER_LINE_WIDTH, color),
            );
            paint_marker_handle(
                painter,
                Pos2::new(plot.left() + 17.0, y),
                &format!("V{channel}{}", index + 1),
                color,
            );
        }
        for index in 0..vertical_count {
            let Some(other) = channel_markers.vertical_comparisons[index]
                .filter(|other| *other < vertical_count && *other != index)
            else {
                continue;
            };
            if other < index && channel_markers.vertical_comparisons[other] == Some(index) {
                continue;
            }
            let first = channel_markers.vertical_positions[index];
            let second = channel_markers.vertical_positions[other];
            let delta_voltage =
                scope_voltage_marker_delta(first, second, scope_settings.channels[channel]);
            let center_y = egui::lerp(plot.y_range(), (first + second) * 0.5);
            let center_x = if channel == 0 {
                plot.left() + 125.0
            } else {
                plot.right() - 125.0
            };
            paint_marker_readout(
                painter,
                plot,
                Pos2::new(center_x, center_y),
                &format!(
                    "{} V{}–V{}  {}",
                    scope_channel_label(channel, channel_names),
                    index + 1,
                    other + 1,
                    format_marker_voltage(delta_voltage)
                ),
                channel_markers.vertical_colors[index],
            );
        }
    }
}

fn paint_scope_quick_time_measurement(
    painter: &egui::Painter,
    plot: Rect,
    measurement: ScopeQuickTimeMeasurement,
    sample_count: usize,
    sample_rate_hz: u64,
    viewport: WaveformViewport,
) {
    const COLOR: Color32 = Color32::from_rgb(255, 214, 76);

    let (first_position, second_position) = match measurement {
        ScopeQuickTimeMeasurement::Idle => return,
        ScopeQuickTimeMeasurement::AwaitingSecond { first_position } => (first_position, None),
        ScopeQuickTimeMeasurement::Complete {
            first_position,
            second_position,
        } => (first_position, Some(second_position)),
    };
    let first_x = egui::lerp(
        plot.x_range(),
        viewport.screen_fraction_at_domain(first_position),
    );
    paint_dashed_line(
        painter,
        Pos2::new(first_x, plot.top()),
        Pos2::new(first_x, plot.bottom()),
        Stroke::new(SCOPE_MARKER_LINE_WIDTH, COLOR),
    );
    paint_marker_handle(
        painter,
        Pos2::new(first_x, plot.bottom() - 11.0),
        "Δt1",
        COLOR,
    );

    let Some(second_position) = second_position else {
        paint_marker_readout(
            painter,
            plot,
            Pos2::new(first_x, plot.bottom() - 39.0),
            "Click to place Δt2",
            COLOR,
        );
        return;
    };

    let second_x = egui::lerp(
        plot.x_range(),
        viewport.screen_fraction_at_domain(second_position),
    );
    paint_dashed_line(
        painter,
        Pos2::new(second_x, plot.top()),
        Pos2::new(second_x, plot.bottom()),
        Stroke::new(SCOPE_MARKER_LINE_WIDTH, COLOR),
    );
    paint_marker_handle(
        painter,
        Pos2::new(second_x, plot.bottom() - 11.0),
        "Δt2",
        COLOR,
    );
    let Some(delta_seconds) = scope_time_marker_delta(
        first_position,
        second_position,
        sample_count,
        sample_rate_hz,
    ) else {
        return;
    };
    paint_marker_readout(
        painter,
        plot,
        Pos2::new((first_x + second_x) * 0.5, plot.bottom() - 39.0),
        &format!(
            "Δt  {}",
            format_duration(Duration::from_secs_f64(delta_seconds))
        ),
        COLOR,
    );
}

fn scope_marker_color(channel: usize, marker: usize) -> Color32 {
    const CH0_COLORS: [Color32; SCOPE_MARKER_LIMIT] = [
        Color32::from_rgb(255, 204, 74),
        Color32::from_rgb(255, 132, 172),
        Color32::from_rgb(255, 145, 77),
        Color32::from_rgb(213, 126, 255),
        Color32::from_rgb(180, 230, 80),
        Color32::from_rgb(255, 225, 120),
        Color32::from_rgb(255, 105, 105),
        Color32::from_rgb(232, 170, 255),
        Color32::from_rgb(255, 180, 95),
        Color32::from_rgb(205, 240, 120),
    ];
    const CH1_COLORS: [Color32; SCOPE_MARKER_LIMIT] = [
        Color32::from_rgb(99, 179, 255),
        Color32::from_rgb(70, 210, 185),
        Color32::from_rgb(174, 130, 255),
        Color32::from_rgb(100, 205, 255),
        Color32::from_rgb(245, 100, 100),
        Color32::from_rgb(80, 235, 205),
        Color32::from_rgb(135, 205, 255),
        Color32::from_rgb(205, 165, 255),
        Color32::from_rgb(85, 225, 240),
        Color32::from_rgb(255, 135, 180),
    ];
    if channel == 0 {
        CH0_COLORS[marker % SCOPE_MARKER_LIMIT]
    } else {
        CH1_COLORS[marker % SCOPE_MARKER_LIMIT]
    }
}

fn paint_dashed_line(painter: &egui::Painter, start: Pos2, end: Pos2, stroke: Stroke) {
    const DASH_LENGTH: f32 = 6.0;
    const GAP_LENGTH: f32 = 4.0;
    let delta = end - start;
    let length = delta.length();
    if length <= f32::EPSILON {
        return;
    }
    let direction = delta / length;
    let mut offset = 0.0;
    while offset < length {
        let dash_end = (offset + DASH_LENGTH).min(length);
        painter.line_segment(
            [start + direction * offset, start + direction * dash_end],
            stroke,
        );
        offset += DASH_LENGTH + GAP_LENGTH;
    }
}

fn paint_marker_handle(painter: &egui::Painter, center: Pos2, label: &str, color: Color32) {
    let width = (label.chars().count() as f32 * 7.0 + 10.0).max(25.0);
    let rect = Rect::from_center_size(center, Vec2::new(width, 18.0));
    painter.rect_filled(rect, egui::Rounding::same(3.0), color);
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        label,
        FontId::monospace(10.0),
        marker_text_color(color),
    );
}

fn paint_marker_readout(
    painter: &egui::Painter,
    plot: Rect,
    center: Pos2,
    text: &str,
    color: Color32,
) {
    let size = Vec2::new(
        (text.chars().count() as f32 * 7.2 + 16.0).min(plot.width()),
        24.0_f32.min(plot.height()),
    );
    let half = size * 0.5;
    let center = Pos2::new(
        center.x.clamp(plot.left() + half.x, plot.right() - half.x),
        center.y.clamp(plot.top() + half.y, plot.bottom() - half.y),
    );
    let rect = Rect::from_center_size(center, size);
    painter.rect_filled(
        rect,
        egui::Rounding::same(4.0),
        Color32::from_rgba_unmultiplied(8, 12, 18, 230),
    );
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        text,
        FontId::monospace(12.0),
        color,
    );
}

fn scope_time_marker_delta(
    first_position: f32,
    second_position: f32,
    sample_count: usize,
    sample_rate_hz: u64,
) -> Option<f64> {
    if sample_count == 0 || sample_rate_hz == 0 {
        return None;
    }
    let capture_seconds = sample_count as f64 / sample_rate_hz as f64;
    Some(f64::from((second_position - first_position).abs()) * capture_seconds)
}

fn scope_voltage_marker_delta(
    first_position: f32,
    second_position: f32,
    channel: ScopeChannelSettings,
) -> f32 {
    let volts_per_division =
        f32::from(channel.volts_per_div_mv) / 1_000.0 * f32::from(channel.probe_attenuation);
    (second_position - first_position).abs() * volts_per_division * SCOPE_VERTICAL_DIVISIONS as f32
}

fn format_marker_voltage(value: f32) -> String {
    if value >= 1.0 {
        format!("{value:.3} V")
    } else if value >= 1.0e-3 {
        format!("{:.2} mV", value * 1.0e3)
    } else {
        format!("{:.2} µV", value * 1.0e6)
    }
}

fn scope_math_value(
    waveforms: &WaveformStore,
    settings: ScopeMathSettings,
    sample_index: usize,
) -> Option<f32> {
    let left = *waveforms
        .scope_channels
        .get(settings.left_channel)?
        .get(sample_index)?;
    let right = *waveforms
        .scope_channels
        .get(settings.right_channel)?
        .get(sample_index)?;
    settings.operation.apply(left, right)
}

fn scope_math_unit(operation: ScopeMathOperation) -> &'static str {
    match operation {
        ScopeMathOperation::Add | ScopeMathOperation::Subtract => "V",
        ScopeMathOperation::Multiply => "V²",
        ScopeMathOperation::Divide => "ratio",
    }
}

struct ScopeMathPaintOptions<'a> {
    viewport: WaveformViewport,
    sample_count: usize,
    visible_range: Range<usize>,
    step: usize,
    settings: ScopeMathSettings,
    channel_names: &'a [String; 2],
}

fn paint_scope_math_trace(
    painter: &egui::Painter,
    plot: Rect,
    waveforms: &WaveformStore,
    options: ScopeMathPaintOptions<'_>,
) {
    let ScopeMathPaintOptions {
        viewport,
        sample_count,
        visible_range,
        step,
        settings,
        channel_names,
    } = options;
    if !settings.enabled || visible_range.is_empty() {
        return;
    }
    let segments = scope_plot_sample_segments(
        visible_range,
        step,
        usize::MAX,
        None,
        waveforms.scope_sample_offset,
        &waveforms.scope_capture_boundaries,
    );
    let maximum = segments
        .iter()
        .flatten()
        .filter_map(|index| scope_math_value(waveforms, settings, *index))
        .map(f32::abs)
        .fold(0.0_f32, f32::max)
        .max(f32::EPSILON);
    for segment in segments {
        let points = segment
            .into_iter()
            .filter_map(|index| {
                scope_math_value(waveforms, settings, index).map(|value| {
                    let x = waveform_sample_x(plot, viewport, index as f32, sample_count);
                    let y = plot.center().y - value / maximum * plot.height() * 0.44;
                    Pos2::new(x, y)
                })
            })
            .collect::<Vec<_>>();
        if points.len() > 1 {
            painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(4.0_f32, MATH_COLOR.gamma_multiply(0.16)),
            ));
            painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, MATH_COLOR)));
        }
    }
    painter.text(
        plot.right_top() + Vec2::new(-8.0, 7.0),
        egui::Align2::RIGHT_TOP,
        format!(
            "M  {} {} {}  auto ±{:.3} {}",
            scope_channel_label(settings.left_channel, channel_names),
            settings.operation.symbol(),
            scope_channel_label(settings.right_channel, channel_names),
            maximum,
            scope_math_unit(settings.operation),
        ),
        FontId::monospace(10.0),
        MATH_COLOR,
    );
}

fn fft_in_place(values: &mut [(f64, f64)]) {
    let count = values.len();
    let mut target = 0;
    for source in 1..count {
        let mut bit = count >> 1;
        while target & bit != 0 {
            target ^= bit;
            bit >>= 1;
        }
        target ^= bit;
        if source < target {
            values.swap(source, target);
        }
    }

    let mut width = 2;
    while width <= count {
        let angle = -std::f64::consts::TAU / width as f64;
        let base = (angle.cos(), angle.sin());
        for start in (0..count).step_by(width) {
            let mut twiddle = (1.0, 0.0);
            for offset in 0..width / 2 {
                let even = values[start + offset];
                let odd = values[start + offset + width / 2];
                let rotated = (
                    odd.0 * twiddle.0 - odd.1 * twiddle.1,
                    odd.0 * twiddle.1 + odd.1 * twiddle.0,
                );
                values[start + offset] = (even.0 + rotated.0, even.1 + rotated.1);
                values[start + offset + width / 2] = (even.0 - rotated.0, even.1 - rotated.1);
                twiddle = (
                    twiddle.0 * base.0 - twiddle.1 * base.1,
                    twiddle.0 * base.1 + twiddle.1 * base.0,
                );
            }
        }
        width *= 2;
    }
}

fn scope_fft_spectrum(
    samples: &VecDeque<f32>,
    visible_range: Range<usize>,
    requested_length: usize,
    window_type: ScopeFftWindow,
) -> Vec<f32> {
    let available = visible_range.len().min(requested_length);
    if available < 8 {
        return Vec::new();
    }
    let length = 1_usize << (usize::BITS - 1 - available.leading_zeros());
    let start = visible_range.start + (visible_range.len() - length) / 2;
    let mut values = Vec::with_capacity(length);
    let mut window_sum = 0.0;
    for index in 0..length {
        let window = window_type.coefficient(index, length);
        window_sum += window;
        values.push((f64::from(samples[start + index]) * window, 0.0));
    }
    fft_in_place(&mut values);

    values[..=length / 2]
        .iter()
        .enumerate()
        .map(|(bin, (real, imaginary))| {
            let factor = if bin == 0 || bin == length / 2 {
                1.0
            } else {
                2.0_f64.sqrt()
            };
            let magnitude = (real * real + imaginary * imaginary).sqrt() * factor / window_sum;
            (20.0 * magnitude.max(1.0e-12).log10()) as f32
        })
        .collect()
}

fn format_frequency(frequency_hz: f64) -> String {
    if frequency_hz >= 1.0e9 {
        format!("{:.2} GHz", frequency_hz / 1.0e9)
    } else if frequency_hz >= 1.0e6 {
        format!("{:.2} MHz", frequency_hz / 1.0e6)
    } else if frequency_hz >= 1.0e3 {
        format!("{:.2} kHz", frequency_hz / 1.0e3)
    } else {
        format!("{frequency_hz:.1} Hz")
    }
}

fn fft_frequency_at_x(plot: Rect, x: f32, start_frequency_hz: f64, end_frequency_hz: f64) -> f64 {
    let fraction = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    start_frequency_hz + fraction as f64 * (end_frequency_hz - start_frequency_hz)
}

fn fft_bin_at_x(plot: Rect, x: f32, first_bin: usize, last_bin: usize) -> usize {
    let fraction = ((x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    first_bin + (fraction * (last_bin - first_bin) as f32).round() as usize
}

fn update_scope_fft_viewport(
    ui: &egui::Ui,
    rect: Rect,
    viewport: &mut ScopeFftViewport,
    max_bin: usize,
) -> egui::Response {
    let minimum_span = (10.0 / max_bin as f32).clamp(0.001, 1.0);
    viewport.span = viewport.span.clamp(minimum_span, 1.0);
    viewport.start = viewport.start.clamp(0.0, 1.0 - viewport.span);

    let response = ui.interact(
        rect,
        ui.make_persistent_id("scope_fft_viewport"),
        Sense::click_and_drag(),
    );
    let cursor = if response.dragged_by(egui::PointerButton::Primary) {
        egui::CursorIcon::Grabbing
    } else {
        egui::CursorIcon::Crosshair
    };
    let response = response.on_hover_cursor(cursor);
    if response.double_clicked() {
        viewport.reset();
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        let delta_x = ui.input(|input| input.pointer.delta().x);
        viewport.pan(delta_x, rect.width());
        ui.ctx().request_repaint();
    }
    if response.hovered() {
        let scroll_delta = ui.input(|input| input.raw_scroll_delta.y);
        if scroll_delta != 0.0 {
            let pointer_fraction = response
                .hover_pos()
                .map(|pointer| (pointer.x - rect.left()) / rect.width().max(1.0))
                .unwrap_or(0.5);
            viewport.zoom(scroll_delta, pointer_fraction, minimum_span);
            ui.ctx().request_repaint();
        }
    }
    response
}

struct ScopeFftPaintOptions<'a> {
    viewport: WaveformViewport,
    fft_viewport: &'a mut ScopeFftViewport,
    settings: ScopeFftSettings,
    channel_names: &'a [String; 2],
    square_grid_cells: bool,
}

fn paint_scope_fft(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    options: ScopeFftPaintOptions<'_>,
) {
    let ScopeFftPaintOptions {
        viewport,
        fft_viewport,
        settings,
        channel_names,
        square_grid_cells,
    } = options;
    const MIN_DBV: f32 = -120.0;
    const MAX_DBV: f32 = 20.0;
    let rect = if square_grid_cells {
        fit_rect_to_grid(rect, 10, 4)
    } else {
        rect
    };
    paint_plot_background(ui, rect);
    let painter = ui.painter().with_clip_rect(rect);
    paint_plot_grid(ui, rect);
    let Some(samples) = waveforms.scope_channels.get(settings.channel) else {
        paint_empty(&painter, rect, "Waiting for FFT source samples");
        return;
    };
    let visible_range = if waveforms.scope_rolling {
        viewport.sample_range_in_domain(samples.len(), waveforms.scope_capacity)
    } else {
        viewport.sample_range(samples.len())
    };
    let spectrum = scope_fft_spectrum(samples, visible_range, settings.length, settings.window);
    if spectrum.len() < 2 || waveforms.scope_rate_hz == 0 {
        paint_empty(&painter, rect, "Waiting for FFT source samples");
        return;
    }

    let nyquist = waveforms.scope_rate_hz as f64 / 2.0;
    let last_bin = spectrum.len() - 1;
    let max_bin = last_bin;
    let response = update_scope_fft_viewport(ui, rect, fft_viewport, max_bin);
    let first_visible_bin = (fft_viewport.start * max_bin as f32).floor() as usize;
    let last_visible_bin = ((fft_viewport.start + fft_viewport.span) * max_bin as f32)
        .ceil()
        .clamp((first_visible_bin + 1) as f32, max_bin as f32) as usize;
    let first_trace_bin = first_visible_bin.max(1);
    let usable_bins = &spectrum[first_trace_bin..=last_visible_bin];
    let bins_per_pixel = (usable_bins.len() as f32 / rect.width().max(1.0))
        .ceil()
        .max(1.0) as usize;
    let points = usable_bins
        .chunks(bins_per_pixel)
        .enumerate()
        .map(|(chunk_index, chunk)| {
            let bin = first_trace_bin + chunk_index * bins_per_pixel;
            let value = chunk.iter().copied().fold(MIN_DBV, f32::max);
            let x = rect.left()
                + (bin - first_visible_bin) as f32 / (last_visible_bin - first_visible_bin) as f32
                    * rect.width();
            let y = rect.bottom()
                - ((value.clamp(MIN_DBV, MAX_DBV) - MIN_DBV) / (MAX_DBV - MIN_DBV)) * rect.height();
            Pos2::new(x, y)
        })
        .collect::<Vec<_>>();
    if points.len() > 1 {
        painter.add(egui::Shape::line(
            points.clone(),
            Stroke::new(4.0_f32, SCOPE_COLORS[settings.channel].gamma_multiply(0.16)),
        ));
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.5_f32, SCOPE_COLORS[settings.channel]),
        ));
    }

    let fft_length = (spectrum.len() - 1) * 2;
    let start_frequency_hz = nyquist * fft_viewport.start as f64;
    let end_frequency_hz = nyquist * (fft_viewport.start + fft_viewport.span) as f64;
    painter.text(
        rect.left_top() + Vec2::new(8.0, 7.0),
        egui::Align2::LEFT_TOP,
        format!(
            "FFT · {} · {} · {fft_length} pts",
            scope_channel_label(settings.channel, channel_names),
            settings.window.label(),
        ),
        FontId::monospace(10.0),
        SCOPE_COLORS[settings.channel],
    );
    painter.text(
        rect.left_bottom() + Vec2::new(5.0, -4.0),
        egui::Align2::LEFT_BOTTOM,
        format_frequency(start_frequency_hz),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.center_bottom() + Vec2::new(0.0, -4.0),
        egui::Align2::CENTER_BOTTOM,
        format_frequency((start_frequency_hz + end_frequency_hz) * 0.5),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.right_bottom() + Vec2::new(-5.0, -4.0),
        egui::Align2::RIGHT_BOTTOM,
        format_frequency(end_frequency_hz),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.right_top() + Vec2::new(-5.0, 5.0),
        egui::Align2::RIGHT_TOP,
        format!("{MAX_DBV:.0} dBV"),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );
    painter.text(
        rect.right_bottom() + Vec2::new(-5.0, -18.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{MIN_DBV:.0} dBV"),
        FontId::monospace(9.0),
        TEXT_MUTED,
    );

    if let Some(pointer) = response.hover_pos() {
        let frequency = fft_frequency_at_x(rect, pointer.x, start_frequency_hz, end_frequency_hz);
        let bin = fft_bin_at_x(rect, pointer.x, first_visible_bin, last_visible_bin);
        let magnitude_dbv = spectrum[bin];
        painter.line_segment(
            [
                Pos2::new(pointer.x, rect.top()),
                Pos2::new(pointer.x, rect.bottom()),
            ],
            Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.75)),
        );
        response.on_hover_text(format!(
            "Frequency: {}\nMagnitude: {magnitude_dbv:.2} dBV\nScroll to zoom · drag to pan · double-click to reset",
            format_frequency(frequency),
        ));
    }
}

fn scope_lissajous_position(
    plot: Rect,
    x_channel: ScopeChannelSettings,
    y_channel: ScopeChannelSettings,
    x_voltage: f32,
    y_voltage: f32,
) -> Pos2 {
    let x_volts_per_division =
        f32::from(x_channel.volts_per_div_mv) / 1_000.0 * f32::from(x_channel.probe_attenuation);
    let y_volts_per_division =
        f32::from(y_channel.volts_per_div_mv) / 1_000.0 * f32::from(y_channel.probe_attenuation);
    let x_span = x_volts_per_division * SCOPE_VERTICAL_DIVISIONS as f32;
    let y_span = y_volts_per_division * SCOPE_VERTICAL_DIVISIONS as f32;
    let x_fraction = scope_vertical_offset_ratio(x_channel.vertical_offset) + x_voltage / x_span;
    let y_fraction = scope_vertical_offset_ratio(y_channel.vertical_offset) - y_voltage / y_span;
    Pos2::new(
        plot.left() + x_fraction * plot.width(),
        plot.top() + y_fraction * plot.height(),
    )
}

fn paint_scope_xy_graticule(painter: &egui::Painter, plot: Rect) {
    for index in 0..=SCOPE_VERTICAL_DIVISIONS {
        let fraction = index as f32 / SCOPE_VERTICAL_DIVISIONS as f32;
        let x = egui::lerp(plot.x_range(), fraction);
        let y = egui::lerp(plot.y_range(), fraction);
        let center = index == SCOPE_VERTICAL_DIVISIONS / 2;
        let stroke = Stroke::new(
            if center { 1.2_f32 } else { 0.8_f32 },
            if center {
                TEXT_MUTED.gamma_multiply(0.45)
            } else {
                GRID.gamma_multiply(0.8)
            },
        );
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            stroke,
        );
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            stroke,
        );
    }
}

struct ScopeLissajousPaintOptions<'a> {
    viewport: WaveformViewport,
    settings: ScopeCaptureSettings,
    lissajous: ScopeLissajousSettings,
    channel_colors: [Color32; 2],
    channel_names: &'a [String; 2],
}

fn paint_scope_lissajous(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    options: ScopeLissajousPaintOptions<'_>,
) {
    let ScopeLissajousPaintOptions {
        viewport,
        settings,
        lissajous,
        channel_colors,
        channel_names,
    } = options;
    let panel_side = rect.width().min(rect.height()).max(1.0);
    let panel = Rect::from_center_size(rect.center(), Vec2::splat(panel_side));
    paint_plot_background(ui, panel);
    let painter = ui.painter();
    let header_bottom = (panel.top() + WAVEFORM_STATUS_HEIGHT).min(panel.bottom());
    let header = Rect::from_min_max(panel.left_top(), Pos2::new(panel.right(), header_bottom));
    painter.rect_filled(header, 4.0, PANEL.gamma_multiply(0.9));
    painter.line_segment(
        [header.left_bottom(), header.right_bottom()],
        Stroke::new(1.0_f32, GRID),
    );

    let x_index = lissajous.x_channel.min(settings.channels.len() - 1);
    let y_index = lissajous.y_channel.min(settings.channels.len() - 1);
    let x_channel = settings.channels[x_index];
    let y_channel = settings.channels[y_index];
    painter.text(
        header.left_center() + Vec2::new(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "LISSAJOUS · XY",
        FontId::monospace(10.0),
        ACCENT,
    );
    painter.text(
        header.right_center() + Vec2::new(-6.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        format!(
            "X {} · Y {}",
            scope_channel_label(x_index, channel_names),
            scope_channel_label(y_index, channel_names)
        ),
        FontId::monospace(10.0),
        TEXT_MUTED,
    );

    let available_plot = Rect::from_min_max(
        Pos2::new(panel.left() + 8.0, header.bottom() + 8.0),
        Pos2::new(panel.right() - 8.0, panel.bottom() - 8.0),
    );
    let side = available_plot.width().min(available_plot.height()).max(1.0);
    let plot = Rect::from_center_size(available_plot.center(), Vec2::splat(side));
    let plot_painter = painter.with_clip_rect(plot);
    paint_scope_xy_graticule(&plot_painter, plot);

    let Some(x_samples) = waveforms.scope_channels.get(x_index) else {
        paint_empty(&plot_painter, plot, "Waiting for X source samples");
        return;
    };
    let Some(y_samples) = waveforms.scope_channels.get(y_index) else {
        paint_empty(&plot_painter, plot, "Waiting for Y source samples");
        return;
    };
    let sample_count = x_samples.len().min(y_samples.len());
    let visible_range = if waveforms.scope_rolling {
        viewport.sample_range_in_domain(sample_count, waveforms.scope_capacity)
    } else {
        viewport.sample_range(sample_count)
    };
    if visible_range.is_empty() {
        paint_empty(&plot_painter, plot, "Waiting for two-channel samples");
        return;
    }
    let target_points = (plot.width() * 2.0).max(256.0) as usize;
    let step = (visible_range.len() / target_points).max(1);
    let segments = scope_plot_sample_segments(
        visible_range,
        step,
        usize::MAX,
        None,
        waveforms.scope_sample_offset,
        &waveforms.scope_capture_boundaries,
    );
    for segment in segments {
        let points = segment
            .into_iter()
            .filter_map(|index| {
                Some(scope_lissajous_position(
                    plot,
                    x_channel,
                    y_channel,
                    *x_samples.get(index)?,
                    *y_samples.get(index)?,
                ))
            })
            .collect::<Vec<_>>();
        if points.len() > 1 {
            plot_painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(5.0_f32, ACCENT.gamma_multiply(0.14)),
            ));
            plot_painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, ACCENT)));
        }
    }

    painter.text(
        plot.left_bottom() + Vec2::new(4.0, -4.0),
        egui::Align2::LEFT_BOTTOM,
        format!(
            "X · {} · {} · {}× probe",
            scope_channel_label(x_index, channel_names),
            format_input_scale(x_channel.volts_per_div_mv),
            x_channel.probe_attenuation
        ),
        FontId::monospace(9.0),
        channel_colors[x_index],
    );
    painter.text(
        plot.left_top() + Vec2::new(4.0, 4.0),
        egui::Align2::LEFT_TOP,
        format!(
            "Y · {} · {} · {}× probe",
            scope_channel_label(y_index, channel_names),
            format_input_scale(y_channel.volts_per_div_mv),
            y_channel.probe_attenuation
        ),
        FontId::monospace(9.0),
        channel_colors[y_index],
    );
}

fn paint_scope(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    viewport: &mut WaveformViewport,
    fft_viewport: &mut ScopeFftViewport,
    square_grid_cells: bool,
    scope_options: ScopeWaveformOptions<'_>,
) {
    let fft_settings = scope_options.function_settings.fft;
    let channel_names = scope_options.channel_names;
    if !fft_settings.enabled {
        paint_scope_time_domain(
            ui,
            rect,
            waveforms,
            viewport,
            square_grid_cells,
            scope_options,
        );
        return;
    }

    const FFT_GAP: f32 = 8.0;
    let desired_fft_height = (rect.height() * 0.36).clamp(80.0, 220.0);
    let fft_height = desired_fft_height.min((rect.height() - FFT_GAP - 64.0).max(32.0));
    let time_height = (rect.height() - FFT_GAP - fft_height).max(0.0);
    let time_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), time_height));
    let fft_rect = Rect::from_min_size(
        Pos2::new(rect.left(), time_rect.bottom() + FFT_GAP),
        Vec2::new(rect.width(), fft_height),
    );
    paint_scope_time_domain(
        ui,
        time_rect,
        waveforms,
        viewport,
        square_grid_cells,
        scope_options,
    );
    paint_scope_fft(
        ui,
        fft_rect,
        waveforms,
        ScopeFftPaintOptions {
            viewport: *viewport,
            fft_viewport,
            settings: fft_settings,
            channel_names,
            square_grid_cells,
        },
    );
}

fn scope_plot_rect(area: Rect, square_grid_cells: bool) -> Rect {
    if square_grid_cells {
        fit_rect_to_grid(area, SCOPE_HORIZONTAL_DIVISIONS, SCOPE_VERTICAL_DIVISIONS)
    } else {
        area
    }
}

fn paint_scope_time_domain(
    ui: &egui::Ui,
    rect: Rect,
    waveforms: &WaveformStore,
    viewport: &mut WaveformViewport,
    square_grid_cells: bool,
    scope_options: ScopeWaveformOptions<'_>,
) {
    let ScopeWaveformOptions {
        mut settings,
        function_settings,
        channel_names,
        measurement_caches,
        measurement_window_positions,
        marker_settings,
        quick_time_measurement,
        channel_colors,
        vertical_offsets,
        trigger_settings,
        time_per_division_seconds,
        timebase_changed,
        offset_controls_enabled,
        trigger_controls_enabled,
    } = scope_options;
    for (channel, vertical_offset) in settings.channels.iter_mut().zip(*vertical_offsets) {
        channel.vertical_offset = vertical_offset;
    }
    let sample_count = waveforms
        .scope_channels
        .iter()
        .enumerate()
        .filter(|(index, _)| settings.channels[*index].enabled)
        .map(|(_, samples)| samples)
        .map(VecDeque::len)
        .min()
        .unwrap_or(0);
    let display_sample_count = if waveforms.scope_rolling {
        waveforms.scope_capacity
    } else if sample_count == 0 {
        settings.sample_count as usize
    } else {
        sample_count
    };
    let display_sample_rate_hz = if waveforms.scope_rate_hz == 0 {
        settings.sample_rate_hz
    } else {
        waveforms.scope_rate_hz
    };
    if let Some(selected_timebase) = *time_per_division_seconds {
        apply_time_per_division(
            viewport,
            display_sample_count,
            display_sample_rate_hz,
            selected_timebase,
        );
    }
    paint_plot_background(ui, rect);
    let painter = ui.painter();
    let (status_area, ruler_area, plot_area) = split_waveform_chrome(rect);
    let plot = scope_plot_rect(plot_area, square_grid_cells);
    let (status_bar, time_ruler) = align_waveform_chrome(status_area, ruler_area, plot);
    let scope_painter = painter.with_clip_rect(plot);
    paint_scope_graticule(&scope_painter, plot);
    let trigger_crossing = scope_trigger_crossing_sample(waveforms, settings, *trigger_settings);
    let trigger_sample_fraction = trigger_crossing
        .map(|crossing| crossing.sample)
        .or_else(|| {
            waveforms
                .scope_trigger_sample_index
                .map(|sample| sample as f32)
        })
        .map(|sample| {
            scope_sample_fraction(sample, waveforms.scope_sample_offset, display_sample_count)
        })
        .unwrap_or_else(|| f32::from(settings.trigger.position_percent) / 100.0);
    let trigger_screen_fraction = f32::from(trigger_settings.position_percent) / 100.0;
    if viewport.anchor_locked && !waveforms.scope_rolling {
        viewport.align_trigger_to_screen(trigger_sample_fraction, trigger_screen_fraction);
    }
    let (viewport_panned, viewport_timebase_changed, waveform_click) = update_waveform_viewport(
        ui,
        plot,
        "scope_waveform_zoom",
        viewport,
        display_sample_count,
        display_sample_rate_hz,
        false,
    );
    if let Some(click) = waveform_click {
        match click {
            WaveformPlotClick::Primary {
                double_clicked: true,
                position,
            } => quick_time_measurement
                .start(viewport.domain_fraction_at_screen(position).clamp(0.0, 1.0)),
            WaveformPlotClick::Primary {
                double_clicked: false,
                position,
            } => quick_time_measurement
                .finish(viewport.domain_fraction_at_screen(position).clamp(0.0, 1.0)),
            WaveformPlotClick::Secondary => quick_time_measurement.clear(),
        }
    }
    *timebase_changed |= viewport_timebase_changed;
    if viewport_panned && !waveforms.scope_rolling {
        finish_scope_viewport_pan(trigger_sample_fraction, viewport, trigger_settings);
    }
    *time_per_division_seconds =
        viewport.time_per_division(display_sample_count, display_sample_rate_hz);
    for (channel, vertical_offset) in settings.channels.iter_mut().zip(*vertical_offsets) {
        channel.vertical_offset = vertical_offset;
    }
    settings.trigger = *trigger_settings;

    let visible_range = viewport.sample_range_in_domain(sample_count, display_sample_count);
    let visible_count = visible_range.len();
    if visible_count == 0 {
        paint_empty(&scope_painter, plot, "Waiting for oscilloscope samples");
        paint_scope_offset_arrows(
            ui,
            plot,
            ScopeOffsetArrowOptions {
                settings,
                channel_colors,
                channel_names,
                vertical_offsets,
                controls_enabled: offset_controls_enabled,
            },
        );
        paint_scope_trigger_level(
            ui,
            plot,
            settings,
            trigger_settings,
            channel_colors,
            vertical_offsets,
            trigger_controls_enabled,
        );
        paint_scope_markers(
            ui,
            &scope_painter,
            plot,
            ScopeMarkerPaintOptions {
                settings: marker_settings,
                scope_settings: settings,
                channel_names,
                sample_count: display_sample_count,
                sample_rate_hz: display_sample_rate_hz,
                viewport: *viewport,
            },
        );
        paint_scope_quick_time_measurement(
            &scope_painter,
            plot,
            *quick_time_measurement,
            display_sample_count,
            display_sample_rate_hz,
            *viewport,
        );
        paint_waveform_time_scale(
            painter,
            time_ruler,
            display_sample_count,
            display_sample_rate_hz,
            waveforms.scope_sample_offset,
            *viewport,
        );
        paint_waveform_status(
            painter,
            status_bar,
            display_sample_count,
            display_sample_rate_hz,
            *viewport,
        );
        paint_scope_trigger_position(
            ui,
            time_ruler,
            plot,
            trigger_settings,
            trigger_controls_enabled,
            viewport,
        );
        return;
    }

    let step = (visible_count as f32 / plot.width().max(1.0))
        .floor()
        .max(1.0) as usize;

    for (channel_index, channel) in waveforms.scope_channels.iter().enumerate() {
        let Some(channel_settings) = settings.channels.get(channel_index).copied() else {
            continue;
        };
        if !channel_settings.enabled {
            continue;
        }
        let segments = scope_plot_sample_segments(
            visible_range.clone(),
            step,
            channel_index,
            trigger_crossing,
            waveforms.scope_sample_offset,
            &waveforms.scope_capture_boundaries,
        );
        let color = channel_colors[channel_index.min(channel_colors.len() - 1)];
        for segment in segments {
            let points = segment
                .into_iter()
                .map(|index| {
                    let x = waveform_sample_x(plot, *viewport, index as f32, display_sample_count);
                    let y = scope_sample_y(plot, channel_settings, channel[index]);
                    Pos2::new(x, y)
                })
                .collect::<Vec<_>>();
            if points.len() > 1 {
                scope_painter.add(egui::Shape::line(
                    points.clone(),
                    Stroke::new(4.0_f32, color.gamma_multiply(0.16)),
                ));
                scope_painter.add(egui::Shape::line(points, Stroke::new(1.5_f32, color)));
            }
        }
    }

    paint_scope_math_trace(
        &scope_painter,
        plot,
        waveforms,
        ScopeMathPaintOptions {
            viewport: *viewport,
            sample_count: display_sample_count,
            visible_range: visible_range.clone(),
            step,
            settings: function_settings.math,
            channel_names,
        },
    );

    // Keep channel and trigger references in the foreground so dense or bright
    // waveforms never obscure their arrows, labels, or dotted level lines.
    paint_scope_offset_arrows(
        ui,
        plot,
        ScopeOffsetArrowOptions {
            settings,
            channel_colors,
            channel_names,
            vertical_offsets,
            controls_enabled: offset_controls_enabled,
        },
    );
    paint_scope_trigger_level(
        ui,
        plot,
        settings,
        trigger_settings,
        channel_colors,
        vertical_offsets,
        trigger_controls_enabled,
    );
    paint_scope_markers(
        ui,
        &scope_painter,
        plot,
        ScopeMarkerPaintOptions {
            settings: marker_settings,
            scope_settings: settings,
            channel_names,
            sample_count: display_sample_count,
            sample_rate_hz: display_sample_rate_hz,
            viewport: *viewport,
        },
    );
    paint_scope_quick_time_measurement(
        &scope_painter,
        plot,
        *quick_time_measurement,
        display_sample_count,
        display_sample_rate_hz,
        *viewport,
    );
    paint_scope_measurements(
        ui,
        &scope_painter,
        plot,
        waveforms,
        ScopeMeasurementPaintOptions {
            visible_range,
            sample_rate_hz: display_sample_rate_hz,
            settings: function_settings,
            enabled_channels: settings.channels.map(|channel| channel.enabled),
            channel_colors,
            channel_names,
            caches: measurement_caches,
            window_positions: measurement_window_positions,
        },
    );
    paint_waveform_time_scale(
        painter,
        time_ruler,
        display_sample_count,
        display_sample_rate_hz,
        waveforms.scope_sample_offset,
        *viewport,
    );
    paint_waveform_status(
        painter,
        status_bar,
        display_sample_count,
        display_sample_rate_hz,
        *viewport,
    );
    paint_scope_trigger_position(
        ui,
        time_ruler,
        plot,
        trigger_settings,
        trigger_controls_enabled,
        viewport,
    );
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

fn format_duration(duration: Duration) -> String {
    if duration >= Duration::from_secs(1) {
        format!("{:.2} s", duration.as_secs_f64())
    } else if duration >= Duration::from_millis(1) {
        format!("{:.2} ms", duration.as_secs_f64() * 1_000.0)
    } else if duration >= Duration::from_micros(1) {
        format!("{:.1} µs", duration.as_secs_f64() * 1_000_000.0)
    } else {
        format!("{:.1} ns", duration.as_secs_f64() * 1_000_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsview_core::{CaptureFrame, LogicBatch, ScopeBatch};
    use std::io::Cursor;

    #[test]
    fn logic_capture_defaults_to_stream_mode() {
        assert_eq!(
            LogicCaptureSettings::default().operation_mode,
            DsLogicOperationMode::Stream
        );
    }

    #[test]
    fn logic_capture_menu_offers_five_milliseconds_at_125_megahertz() {
        let option = logic_capture_duration_options(125_000_000)
            .into_iter()
            .find(|(duration, _)| (*duration - 0.005).abs() < f64::EPSILON)
            .expect("5 ms should be available");

        assert_eq!(option.1, 625_664);
        let settings = LogicCaptureSettings {
            sample_rate_hz: 125_000_000,
            sample_count: option.1,
            ..LogicCaptureSettings::default()
        };
        assert_eq!(logic_capture_duration_label(settings), "5.00 ms");
    }

    #[test]
    fn changing_logic_sample_rate_preserves_capture_duration() {
        let mut settings = LogicCaptureSettings {
            sample_rate_hz: 125_000_000,
            sample_count: 625_664,
            ..LogicCaptureSettings::default()
        };

        set_logic_sample_rate_preserving_duration(&mut settings, 250_000_000);

        assert_eq!(settings.sample_rate_hz, 250_000_000);
        assert_eq!(settings.sample_count, 1_250_304);
        assert_eq!(logic_capture_duration_label(settings), "5.00 ms");
    }

    #[test]
    fn logic_settings_map_every_device_option() {
        let mut enabled_channels = [false; LOGIC_CHANNEL_COUNT];
        enabled_channels[0] = true;
        enabled_channels[3] = true;
        let settings = LogicCaptureSettings {
            sample_rate_hz: 250_000_000,
            sample_count: 625_664,
            enabled_channels,
            maximum_heights: [4; LOGIC_CHANNEL_COUNT],
            operation_mode: DsLogicOperationMode::Buffer,
            stop_option: DsLogicStopOption::StopImmediately,
            threshold_voltage: 2.5,
            input_filter: DsLogicInputFilter::OneSampleClock,
            rle_enabled: true,
            external_clock: true,
            falling_clock_edge: true,
        };

        let config = settings
            .capture_config()
            .expect("logic settings should be valid");

        assert_eq!(config.operation_mode(), settings.operation_mode);
        assert_eq!(config.sample_rate_hz(), settings.sample_rate_hz);
        assert_eq!(config.sample_count(), settings.sample_count);
        assert_eq!(config.enabled_channels(), settings.enabled_channels);
        assert_eq!(config.stop_option(), settings.stop_option);
        assert_eq!(config.threshold_voltage(), settings.threshold_voltage);
        assert_eq!(config.input_filter(), settings.input_filter);
        assert!(config.rle_enabled());
        assert!(config.external_clock());
        assert!(config.falling_clock_edge());
    }

    #[test]
    fn stream_mode_ignores_buffer_only_options() {
        let settings = LogicCaptureSettings {
            operation_mode: DsLogicOperationMode::Stream,
            stop_option: DsLogicStopOption::UploadCapturedData,
            rle_enabled: true,
            ..LogicCaptureSettings::default()
        };

        let config = settings
            .capture_config()
            .expect("logic settings should be valid");

        assert_eq!(config.stop_option(), DsLogicStopOption::StopImmediately);
        assert!(!config.rle_enabled());
    }

    #[test]
    fn gui_recording_reader_restores_full_capture_and_scope_settings() {
        let descriptors = vec![
            DeviceDescriptor {
                id: DeviceId::new("logic-test"),
                name: "Logic test".to_owned(),
                kind: InstrumentKind::LogicAnalyzer,
            },
            DeviceDescriptor {
                id: DeviceId::new("scope-test"),
                name: "Scope test".to_owned(),
                kind: InstrumentKind::Oscilloscope,
            },
        ];
        let mut metadata = BTreeMap::new();
        metadata.insert("scope.sample_rate_hz".to_owned(), "1000000".to_owned());
        metadata.insert("scope.sample_count".to_owned(), "1000000".to_owned());
        metadata.insert("scope.ch0.enabled".to_owned(), "false".to_owned());
        metadata.insert("scope.ch1.enabled".to_owned(), "true".to_owned());
        metadata.insert("scope.ch1.volts_per_div_mv".to_owned(), "500".to_owned());
        metadata.insert("scope.ch1.coupling".to_owned(), "ac".to_owned());
        metadata.insert("scope.ch1.probe_attenuation".to_owned(), "10".to_owned());
        metadata.insert("logic.operation_mode".to_owned(), "stream".to_owned());
        metadata.insert("logic.sample_rate_hz".to_owned(), "250000000".to_owned());
        metadata.insert("logic.sample_count".to_owned(), "1250304".to_owned());
        for channel in 0..LOGIC_CHANNEL_COUNT {
            metadata.insert(
                format!("logic.ch{channel}.enabled"),
                matches!(channel, 2 | 9).to_string(),
            );
            metadata.insert(
                format!("logic.ch{channel}.maximum_height"),
                (channel % 5 + 1).to_string(),
            );
        }
        metadata.insert(
            "logic.stop_option".to_owned(),
            "stop_immediately".to_owned(),
        );
        metadata.insert("logic.threshold_voltage".to_owned(), "2.5".to_owned());
        metadata.insert(
            "logic.input_filter".to_owned(),
            "one_sample_clock".to_owned(),
        );
        metadata.insert("logic.rle_enabled".to_owned(), "true".to_owned());
        metadata.insert("logic.external_clock".to_owned(), "true".to_owned());
        metadata.insert("logic.falling_clock_edge".to_owned(), "true".to_owned());
        metadata.insert("logic.ch0.name".to_owned(), "Clock".to_owned());
        metadata.insert("logic.ch1.name".to_owned(), "  Data  ".to_owned());
        metadata.insert("scope.ch0.name".to_owned(), "Input".to_owned());
        metadata.insert("scope.ch1.name".to_owned(), "Reference".to_owned());
        let mut bytes = Vec::new();
        {
            let mut writer = RecordingWriter::new(&mut bytes, &descriptors, metadata)
                .expect("recording header is valid");
            writer
                .write_event(&CaptureEvent::Frame(CaptureFrame {
                    device_id: DeviceId::new("logic-test"),
                    sequence: 0,
                    captured_at: Duration::ZERO,
                    payload: CaptureBatch::Logic(LogicBatch {
                        sample_rate_hz: 2_000_000,
                        channel_count: 4,
                        samples: vec![1, 2, 3],
                    }),
                }))
                .expect("logic frame is valid");
            writer
                .write_event(&CaptureEvent::Frame(CaptureFrame {
                    device_id: DeviceId::new("scope-test"),
                    sequence: 0,
                    captured_at: Duration::ZERO,
                    payload: CaptureBatch::Scope(ScopeBatch {
                        sample_rate_hz: 1_000_000,
                        trigger_sample_index: Some(1),
                        channels: vec![vec![0.25, -0.5]],
                    }),
                }))
                .expect("scope frame is valid");
            writer
                .finish(CompletionStatus::Complete)
                .expect("recording footer is valid");
        }

        let loaded = read_gui_recording(Cursor::new(bytes)).expect("recording should reopen");

        assert_eq!(loaded.completion, CompletionStatus::Complete);
        assert!(loaded.capture.has_logic);
        assert!(loaded.capture.has_scope);
        assert_eq!(loaded.capture.waveforms.logic_rate_hz, 2_000_000);
        assert_eq!(
            loaded.capture.waveforms.logic_samples,
            VecDeque::from(vec![1, 2, 3])
        );
        assert!(loaded.capture.waveforms.scope_channels[0].is_empty());
        assert_eq!(
            loaded.capture.waveforms.scope_channels[1],
            VecDeque::from(vec![0.25, -0.5])
        );
        let scope_settings = loaded.scope_settings.expect("scope metadata should load");
        assert!(!scope_settings.channels[0].enabled);
        assert!(scope_settings.channels[1].enabled);
        assert_eq!(scope_settings.channels[1].volts_per_div_mv, 500);
        assert_eq!(scope_settings.channels[1].coupling, DsCopeCoupling::Ac);
        assert_eq!(scope_settings.channels[1].probe_attenuation, 10);
        let logic_settings = loaded.logic_settings.expect("logic metadata should load");
        assert_eq!(logic_settings.operation_mode, DsLogicOperationMode::Stream);
        assert_eq!(logic_settings.sample_rate_hz, 250_000_000);
        assert_eq!(logic_settings.sample_count, 1_250_304);
        assert_eq!(
            logic_settings.enabled_channels,
            [
                false, false, true, false, false, false, false, false, false, true, false, false,
                false, false, false, false,
            ]
        );
        assert_eq!(
            logic_settings.stop_option,
            DsLogicStopOption::StopImmediately
        );
        assert_eq!(logic_settings.threshold_voltage, 2.5);
        assert_eq!(
            logic_settings.input_filter,
            DsLogicInputFilter::OneSampleClock
        );
        assert_eq!(
            logic_settings.maximum_heights,
            [1, 2, 3, 4, 5, 1, 2, 3, 4, 5, 1, 2, 3, 4, 5, 1]
        );
        assert!(logic_settings.rle_enabled);
        assert!(logic_settings.external_clock);
        assert!(logic_settings.falling_clock_edge);
        assert_eq!(loaded.logic_channel_names[0], "Clock");
        assert_eq!(loaded.logic_channel_names[1], "Data");
        assert_eq!(loaded.scope_channel_names, ["Input", "Reference"]);
    }

    #[test]
    fn recordings_without_channel_names_use_hardware_identifiers() {
        let (logic_names, scope_names) = channel_names_from_recording_metadata(&BTreeMap::new());

        assert!(logic_names.iter().all(String::is_empty));
        assert!(scope_names.iter().all(String::is_empty));
        assert_eq!(channel_label("D", 0, &logic_names[0]), "D0");
        assert_eq!(scope_channel_label(1, &scope_names), "CH1");
    }

    #[test]
    fn recording_metadata_saves_normalized_channel_names() {
        let mut logic_names = empty_logic_channel_names();
        logic_names[0] = "  Clock  ".to_owned();
        let mut scope_names = empty_scope_channel_names();
        scope_names[1] = "Reference".to_owned();
        let mut metadata = BTreeMap::new();

        insert_channel_name_metadata(&mut metadata, &logic_names, &scope_names);

        assert_eq!(
            metadata.get("logic.ch0.name").map(String::as_str),
            Some("Clock")
        );
        assert_eq!(metadata.get("logic.ch1.name").map(String::as_str), Some(""));
        assert_eq!(
            metadata.get("scope.ch1.name").map(String::as_str),
            Some("Reference")
        );
    }

    #[test]
    fn recording_paths_use_the_dsv_extension() {
        assert_eq!(
            normalize_recording_path(PathBuf::from("capture")),
            PathBuf::from("capture.dsv")
        );
        assert_eq!(
            normalize_recording_path(PathBuf::from("capture.DSV")),
            PathBuf::from("capture.DSV")
        );
        assert_eq!(
            normalize_recording_path(PathBuf::from("capture.txt")),
            PathBuf::from("capture.dsv")
        );
    }

    #[test]
    fn snapshot_crop_uses_plot_bounds_and_display_scale() {
        let mut screenshot = egui::ColorImage::new([8, 6], Color32::BLACK);
        for (index, pixel) in screenshot.pixels.iter_mut().enumerate() {
            *pixel = Color32::from_rgb(index as u8, 0, 0);
        }

        let snapshot = crop_snapshot_image(
            &screenshot,
            Rect::from_min_max(Pos2::new(1.0, 0.5), Pos2::new(3.0, 2.5)),
            2.0,
        )
        .expect("the plot is inside the screenshot");

        assert_eq!(snapshot.width, 4);
        assert_eq!(snapshot.height, 4);
        let red_values = snapshot
            .rgba
            .chunks_exact(4)
            .map(|pixel| pixel[0])
            .collect::<Vec<_>>();
        assert_eq!(
            red_values,
            vec![10, 11, 12, 13, 18, 19, 20, 21, 26, 27, 28, 29, 34, 35, 36, 37]
        );
    }

    #[test]
    fn snapshot_crop_rejects_invalid_bounds_and_scale() {
        let screenshot = egui::ColorImage::new([4, 3], Color32::BLACK);

        assert!(crop_snapshot_image(
            &screenshot,
            Rect::from_min_max(Pos2::new(5.0, 1.0), Pos2::new(6.0, 2.0)),
            1.0,
        )
        .is_err());
        assert!(crop_snapshot_image(
            &screenshot,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(2.0, 2.0)),
            0.0,
        )
        .is_err());
    }

    #[test]
    fn acquisition_modes_rearm_only_successful_continuous_runs() {
        for mode in [AcquisitionMode::Repetitive, AcquisitionMode::Run] {
            assert!(should_rearm_acquisition(
                mode,
                true,
                CompletionStatus::Complete,
                false,
            ));
        }
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Loop,
            true,
            CompletionStatus::Complete,
            false,
        ));
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Run,
            false,
            CompletionStatus::Complete,
            false,
        ));
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Run,
            true,
            CompletionStatus::Failed,
            true,
        ));
        assert!(!should_rearm_acquisition(
            AcquisitionMode::Single,
            true,
            CompletionStatus::Complete,
            false,
        ));
    }

    #[test]
    fn repetitive_interval_matches_original_stream_and_buffer_behavior() {
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Stream, 7.5),
            Duration::from_millis(100)
        );
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Buffer, 1.5),
            Duration::from_millis(1_500)
        );
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Buffer, 0.0),
            Duration::from_millis(100)
        );
        assert_eq!(
            repetitive_interval(DsLogicOperationMode::Buffer, 20.0),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn trigger_controls_stay_enabled_while_continuous_capture_rearms() {
        assert!(scope_trigger_controls_are_enabled(true, true, true, None));
        assert!(scope_trigger_controls_are_enabled(
            false,
            true,
            true,
            Some(true)
        ));
        assert!(!scope_trigger_controls_are_enabled(true, false, true, None));
        assert!(!scope_trigger_controls_are_enabled(
            false,
            true,
            false,
            Some(false)
        ));
        assert!(scope_frontend_controls_are_enabled(true, true, true));
        assert!(!scope_frontend_controls_are_enabled(true, false, true));
        assert!(scope_frontend_controls_are_enabled(false, false, false));
    }

    #[test]
    fn manual_calibration_stays_available_during_continuous_capture() {
        assert!(manual_calibration_control_is_enabled(
            false,
            true,
            false,
            Some(AcquisitionMode::Run)
        ));
        assert!(manual_calibration_control_is_enabled(
            false, true, false, None
        ));
        assert!(!manual_calibration_control_is_enabled(
            true,
            true,
            false,
            Some(AcquisitionMode::Run)
        ));
        assert!(!manual_calibration_control_is_enabled(
            false,
            true,
            false,
            Some(AcquisitionMode::Single)
        ));
        assert!(!manual_calibration_control_is_enabled(
            false,
            true,
            true,
            Some(AcquisitionMode::Run)
        ));
    }

    #[test]
    fn waveform_store_keeps_newest_samples_within_bound() {
        let mut store = WaveformStore::default();
        store.ingest(
            &CaptureBatch::Logic(LogicBatch {
                sample_rate_hz: 1_000,
                channel_count: 2,
                samples: (0..MAX_WAVEFORM_SAMPLES as u32 + 10).collect(),
            }),
            [true, true],
        );

        assert_eq!(store.logic_samples.len(), MAX_WAVEFORM_SAMPLES);
        assert_eq!(store.logic_samples.front(), Some(&10));
        assert_eq!(store.logic_sample_offset, 10);
    }

    #[test]
    fn waveform_store_tracks_scope_channels() {
        let mut store = WaveformStore::default();
        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000,
                trigger_sample_index: Some(1),
                channels: vec![vec![0.1, 0.2], vec![-0.1, -0.2]],
            }),
            [true, true],
        );

        assert_eq!(store.scope_rate_hz, 1_000_000);
        assert_eq!(store.scope_trigger_sample_index, Some(1));
        assert_eq!(store.scope_channels[0].back(), Some(&0.2));
        assert_eq!(store.scope_channels[1].back(), Some(&-0.2));
    }

    #[test]
    fn waveform_store_keeps_trigger_coordinates_across_appended_captures() {
        let mut store = WaveformStore {
            scope_capacity: 3,
            ..WaveformStore::default()
        };
        for (capture_index, (trigger_sample_index, samples)) in
            [(Some(1), vec![0.0, 1.0]), (Some(0), vec![2.0, 3.0])]
                .into_iter()
                .enumerate()
        {
            if capture_index > 0 {
                store.mark_scope_capture_start([true, true]);
            }
            store.ingest(
                &CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 1_000_000,
                    trigger_sample_index,
                    channels: vec![samples.clone(), samples],
                }),
                [true, true],
            );
        }

        assert_eq!(store.scope_channels[0], VecDeque::from(vec![1.0, 2.0, 3.0]));
        assert_eq!(store.scope_trigger_sample_index, Some(2));
        assert_eq!(store.scope_sample_offset, 1);
        assert_eq!(store.scope_capture_boundaries, VecDeque::from([2]));
    }

    #[test]
    fn scope_plot_segments_do_not_join_independent_captures() {
        let segments = scope_plot_sample_segments(0..10, 4, 0, None, 100, &VecDeque::from([105]));

        assert_eq!(segments, vec![vec![0, 4], vec![5, 8, 9]]);
    }

    #[test]
    fn rolling_scope_discards_boundaries_evicted_from_the_ring() {
        let mut store = WaveformStore {
            scope_capacity: 3,
            scope_rolling: true,
            ..WaveformStore::default()
        };
        for samples in [vec![0.0, 1.0], vec![2.0, 3.0], vec![4.0, 5.0]] {
            store.mark_scope_capture_start([true, true]);
            store.ingest(
                &CaptureBatch::Scope(ScopeBatch {
                    sample_rate_hz: 1_000,
                    trigger_sample_index: None,
                    channels: vec![samples.clone(), samples],
                }),
                [true, true],
            );
        }

        assert_eq!(store.scope_sample_offset, 3);
        assert_eq!(store.scope_capture_boundaries, VecDeque::from([4]));
    }

    #[test]
    fn waveform_store_preserves_single_scope_channel_identity() {
        let mut store = WaveformStore::default();
        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000_000_000,
                trigger_sample_index: None,
                channels: vec![vec![0.1, 0.2]],
            }),
            [false, true],
        );

        assert!(store.scope_channels[0].is_empty());
        assert_eq!(store.scope_channels[1].back(), Some(&0.2));
    }

    #[test]
    fn scope_measurement_cache_tracks_new_samples_and_visible_ranges() {
        let mut store = WaveformStore {
            scope_capacity: 4,
            ..WaveformStore::default()
        };
        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000,
                trigger_sample_index: None,
                channels: vec![vec![-1.0, 1.0], vec![0.0, 0.0]],
            }),
            [true, true],
        );
        let mut cache = ScopeMeasurementCache::default();

        let first = cache.results(&store, 0, 0..2, 1_000);
        assert_eq!(
            first.get(measurements::ScopeMeasurementKind::Mean),
            Some(0.0)
        );
        assert_eq!(cache.key.unwrap().range_end, 2);

        store.ingest(
            &CaptureBatch::Scope(ScopeBatch {
                sample_rate_hz: 1_000,
                trigger_sample_index: None,
                channels: vec![vec![3.0, 3.0], vec![0.0, 0.0]],
            }),
            [true, true],
        );
        let updated = cache.results(&store, 0, 0..4, 1_000);
        assert_eq!(
            updated.get(measurements::ScopeMeasurementKind::Mean),
            Some(1.5)
        );

        let zoomed = cache.results(&store, 0, 2..4, 1_000);
        assert_eq!(
            zoomed.get(measurements::ScopeMeasurementKind::Mean),
            Some(3.0)
        );
    }

    #[test]
    fn scope_math_operations_use_captured_channel_values() {
        assert_eq!(ScopeMathOperation::Add.apply(6.0, 2.0), Some(8.0));
        assert_eq!(ScopeMathOperation::Subtract.apply(6.0, 2.0), Some(4.0));
        assert_eq!(ScopeMathOperation::Multiply.apply(6.0, 2.0), Some(12.0));
        assert_eq!(ScopeMathOperation::Divide.apply(6.0, 2.0), Some(3.0));
        assert_eq!(ScopeMathOperation::Divide.apply(6.0, 0.0), None);
    }

    #[test]
    fn scope_function_sources_follow_enabled_channels() {
        let mut settings = ScopeFunctionSettings {
            math: ScopeMathSettings {
                enabled: true,
                operation: ScopeMathOperation::Subtract,
                left_channel: 0,
                right_channel: 0,
            },
            fft: ScopeFftSettings {
                enabled: true,
                channel: 0,
                length: 4_096,
                window: ScopeFftWindow::Blackman,
            },
            lissajous: ScopeLissajousSettings {
                enabled: true,
                x_channel: 0,
                y_channel: 0,
            },
            measurements: ScopeMeasurementSettings::default(),
        };

        settings.clamp_to_channels([false, true]);

        assert_eq!(settings.math.left_channel, 1);
        assert_eq!(settings.math.right_channel, 1);
        assert_eq!(settings.fft.channel, 1);
        assert_eq!(settings.fft.window, ScopeFftWindow::Blackman);
        assert_eq!(settings.lissajous.x_channel, 1);
        assert_eq!(settings.lissajous.y_channel, 1);
        assert!(!settings.lissajous.enabled);
        assert!(settings
            .measurements
            .channels
            .iter()
            .all(|channel| channel.enabled));
    }

    #[test]
    fn lissajous_coordinates_use_each_channels_voltage_scale() {
        let plot = Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 800.0));
        let x_channel = ScopeChannelSettings {
            volts_per_div_mv: 1_000,
            ..ScopeChannelSettings::default()
        };
        let y_channel = ScopeChannelSettings {
            volts_per_div_mv: 500,
            ..ScopeChannelSettings::default()
        };

        let zero = scope_lissajous_position(plot, x_channel, y_channel, 0.0, 0.0);
        let expected_zero = plot.left_top()
            + Vec2::splat(
                scope_vertical_offset_ratio(DSCOPE_VERTICAL_OFFSET_CENTER) * plot.width(),
            );
        assert!((zero.x - expected_zero.x).abs() < 0.001);
        assert!((zero.y - expected_zero.y).abs() < 0.001);
        let point = scope_lissajous_position(plot, x_channel, y_channel, 1.0, 0.5);
        assert!((point.x - zero.x - 100.0).abs() < 0.001);
        assert!((zero.y - point.y - 100.0).abs() < 0.001);
    }

    #[test]
    fn waveform_grids_fit_square_division_cells() {
        let wide_area = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_600.0, 800.0));
        let scope_grid = fit_rect_to_grid(wide_area, 10, 8);
        assert!((scope_grid.width() - 1_000.0).abs() < 0.001);
        assert!((scope_grid.height() - 800.0).abs() < 0.001);
        assert!((scope_grid.center().x - wide_area.center().x).abs() < 0.001);
        assert!((scope_grid.width() / 10.0 - scope_grid.height() / 8.0).abs() < 0.001);
        assert_eq!(scope_plot_rect(wide_area, false), wide_area);
        assert_eq!(scope_plot_rect(wide_area, true), scope_grid);

        let square_area = Rect::from_min_size(Pos2::ZERO, Vec2::splat(800.0));
        let fft_grid = fit_rect_to_grid(square_area, 10, 4);
        assert!((fft_grid.width() - 800.0).abs() < 0.001);
        assert!((fft_grid.height() - 320.0).abs() < 0.001);
        assert!((fft_grid.width() / 10.0 - fft_grid.height() / 4.0).abs() < 0.001);
    }

    #[test]
    fn scope_fft_finds_the_input_tone() {
        let length = 1_024;
        let tone_bin = 64;
        let samples = (0..length)
            .map(|index| {
                (std::f32::consts::TAU * tone_bin as f32 * index as f32 / length as f32).sin()
            })
            .collect::<VecDeque<_>>();

        let spectrum = scope_fft_spectrum(&samples, 0..length, length, ScopeFftWindow::Hann);
        let peak_bin = spectrum
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(bin, _)| bin)
            .unwrap();

        assert_eq!(peak_bin, tone_bin);
        assert!((spectrum[peak_bin] + 3.0).abs() < 0.1);
    }

    #[test]
    fn scope_fft_supports_vendor_window_types() {
        let samples = (0..1_024)
            .map(|index| (std::f32::consts::TAU * 32.0 * index as f32 / 1_024.0).sin())
            .collect::<VecDeque<_>>();

        for window in [
            ScopeFftWindow::Rectangular,
            ScopeFftWindow::Hann,
            ScopeFftWindow::Hamming,
            ScopeFftWindow::Blackman,
            ScopeFftWindow::FlatTop,
        ] {
            let spectrum = scope_fft_spectrum(&samples, 0..1_024, 1_024, window);
            assert_eq!(spectrum.len(), 513);
            assert!(spectrum.iter().all(|magnitude| magnitude.is_finite()));
            assert_eq!(
                spectrum
                    .iter()
                    .enumerate()
                    .skip(1)
                    .max_by(|left, right| left.1.total_cmp(right.1))
                    .map(|(bin, _)| bin),
                Some(32),
            );
        }
    }

    #[test]
    fn scope_fft_mouse_position_maps_to_frequency() {
        let plot = Rect::from_min_size(Pos2::new(100.0, 20.0), Vec2::new(800.0, 200.0));

        assert_eq!(fft_frequency_at_x(plot, 100.0, 0.0, 500_000.0), 0.0);
        assert_eq!(fft_frequency_at_x(plot, 500.0, 0.0, 500_000.0), 250_000.0);
        assert_eq!(fft_frequency_at_x(plot, 900.0, 0.0, 500_000.0), 500_000.0);
        assert_eq!(fft_frequency_at_x(plot, 950.0, 0.0, 500_000.0), 500_000.0);
        assert_eq!(
            fft_frequency_at_x(plot, 100.0, 100_000.0, 200_000.0),
            100_000.0
        );
        assert_eq!(
            fft_frequency_at_x(plot, 900.0, 100_000.0, 200_000.0),
            200_000.0
        );
        assert_eq!(fft_bin_at_x(plot, 100.0, 128, 256), 128);
        assert_eq!(fft_bin_at_x(plot, 500.0, 128, 256), 192);
        assert_eq!(fft_bin_at_x(plot, 900.0, 128, 256), 256);
        assert_eq!(fft_bin_at_x(plot, 950.0, 128, 256), 256);
    }

    #[test]
    fn scope_fft_viewport_zooms_at_pointer_and_pans() {
        let mut viewport = ScopeFftViewport::default();

        viewport.zoom(1.0, 0.25, 0.01);
        assert!((viewport.span - 0.8).abs() < f32::EPSILON);
        assert!((viewport.start - 0.05).abs() < f32::EPSILON);

        viewport.pan(-100.0, 1_000.0);
        assert!((viewport.start - 0.13).abs() < f32::EPSILON);

        viewport.zoom(-1.0, 0.5, 0.01);
        assert_eq!(viewport, ScopeFftViewport::default());
    }

    #[test]
    fn waveform_tabs_require_their_connected_devices() {
        assert!(waveform_tabs(false, false, true).is_empty());
        assert_eq!(
            waveform_tabs(true, false, true),
            vec![WaveformTab::LogicAnalyzer]
        );
        assert_eq!(
            waveform_tabs(false, true, false),
            vec![WaveformTab::Oscilloscope]
        );
        assert_eq!(
            waveform_tabs(false, true, true),
            vec![WaveformTab::Oscilloscope, WaveformTab::Lissajous]
        );
        assert_eq!(
            waveform_tabs(true, true, false),
            vec![
                WaveformTab::LogicAnalyzer,
                WaveformTab::Oscilloscope,
                WaveformTab::Combined,
            ]
        );
        assert_eq!(
            waveform_tabs(true, true, true),
            vec![
                WaveformTab::LogicAnalyzer,
                WaveformTab::Oscilloscope,
                WaveformTab::Lissajous,
                WaveformTab::Combined,
            ]
        );
    }

    #[test]
    fn waveform_tabs_target_their_matching_instruments() {
        assert_eq!(
            WaveformTab::LogicAnalyzer.capture_target(),
            CaptureTarget::LogicAnalyzer
        );
        assert_eq!(
            WaveformTab::Oscilloscope.capture_target(),
            CaptureTarget::Oscilloscope
        );
        assert_eq!(
            WaveformTab::Lissajous.capture_target(),
            CaptureTarget::Oscilloscope
        );
        assert_eq!(
            WaveformTab::Combined.capture_target(),
            CaptureTarget::Combined
        );

        assert_eq!(
            WaveformTab::LogicAnalyzer.capture_settings_tab(),
            Some(CaptureSettingsTab::DsLogic)
        );
        assert_eq!(
            WaveformTab::Oscilloscope.capture_settings_tab(),
            Some(CaptureSettingsTab::DsCope)
        );
        assert_eq!(
            WaveformTab::Lissajous.capture_settings_tab(),
            Some(CaptureSettingsTab::DsCope)
        );
        assert_eq!(WaveformTab::Combined.capture_settings_tab(), None);

        assert!(CaptureTarget::LogicAnalyzer.includes_model(DeviceModel::DsLogicU3Pro16));
        assert!(!CaptureTarget::LogicAnalyzer.includes_model(DeviceModel::DsCopeU3P100));
        assert!(CaptureTarget::Oscilloscope.includes_model(DeviceModel::DsCopeU3P100));
        assert!(!CaptureTarget::Oscilloscope.includes_model(DeviceModel::DsLogicU3Pro16));
        assert!(CaptureTarget::Combined.includes_model(DeviceModel::DsLogicU3Pro16));
        assert!(CaptureTarget::Combined.includes_model(DeviceModel::DsCopeU3P100));
    }

    #[test]
    fn combined_capture_requires_both_instrument_types() {
        assert!(CaptureTarget::LogicAnalyzer.is_available(true, false));
        assert!(CaptureTarget::Oscilloscope.is_available(false, true));
        assert!(!CaptureTarget::Combined.is_available(true, false));
        assert!(!CaptureTarget::Combined.is_available(false, true));
        assert!(CaptureTarget::Combined.is_available(true, true));
    }

    #[test]
    fn scope_tools_panel_is_limited_to_scope_views() {
        assert!(scope_tools_panel_is_visible(Some(
            WaveformTab::Oscilloscope
        )));
        assert!(scope_tools_panel_is_visible(Some(WaveformTab::Lissajous)));
        assert!(!scope_tools_panel_is_visible(Some(
            WaveformTab::LogicAnalyzer
        )));
        assert!(!scope_tools_panel_is_visible(Some(WaveformTab::Combined)));
        assert!(!scope_tools_panel_is_visible(None));
    }

    #[test]
    fn capture_setup_tabs_require_their_connected_devices() {
        assert!(capture_settings_tabs(false, false).is_empty());
        assert_eq!(
            capture_settings_tabs(true, false),
            vec![CaptureSettingsTab::DsLogic]
        );
        assert_eq!(
            capture_settings_tabs(false, true),
            vec![CaptureSettingsTab::DsCope]
        );
        let tabs = capture_settings_tabs(true, true);
        assert_eq!(
            tabs,
            vec![CaptureSettingsTab::DsLogic, CaptureSettingsTab::DsCope]
        );
        assert_eq!(
            tabs.iter()
                .copied()
                .map(CaptureSettingsTab::label)
                .collect::<Vec<_>>(),
            ["DSLogic", "DSCope"]
        );
    }

    #[test]
    fn usb_speed_warning_only_lists_devices_below_usb_three() {
        let device = |platform_path: &str, speed| DiscoveredDevice {
            profile: &dsview_usb::DSLOGIC_U3PRO16,
            location: dsview_usb::DeviceLocation {
                platform_path: platform_path.to_owned(),
            },
            speed,
        };
        let devices = [
            device("usb-2", UsbSpeedClass::HighSpeed),
            device("usb-3", UsbSpeedClass::SuperSpeed),
            device("unknown", UsbSpeedClass::Unknown),
        ];

        let warning = usb_speed_warning(&devices).unwrap();

        assert_eq!(warning.devices.len(), 2);
        assert_eq!(warning.devices[0].speed, UsbSpeedClass::HighSpeed);
        assert_eq!(warning.devices[1].speed, UsbSpeedClass::Unknown);
        assert!(usb_speed_warning(&devices[1..2]).is_none());
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
        let minimum_scale = 300.0 / (MIN_LOGIC_PANE_HEIGHT + MIN_SCOPE_PANE_HEIGHT);
        assert!((logic - MIN_LOGIC_PANE_HEIGHT * minimum_scale).abs() < 0.001);
        assert!((scope - MIN_SCOPE_PANE_HEIGHT * minimum_scale).abs() < 0.001);
    }

    #[test]
    fn floating_measurement_windows_stay_relative_to_the_plot() {
        let plot = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 400.0));
        let size = Vec2::new(200.0, 100.0);

        let [channel_zero, channel_one] = default_scope_measurement_window_positions();
        let bottom_left = scope_measurement_window_rect(plot, size, channel_zero);
        assert_eq!(bottom_left.left_bottom(), plot.left_bottom());
        let bottom_right = scope_measurement_window_rect(plot, size, channel_one);
        assert_eq!(bottom_right.right_bottom(), plot.right_bottom());

        let top_right = scope_measurement_window_rect(plot, size, Vec2::new(1.0, 0.0));
        assert_eq!(top_right.right(), plot.right());
        assert_eq!(top_right.top(), plot.top());
        assert_eq!(
            scope_measurement_window_position(plot, top_right),
            Vec2::new(1.0, 0.0)
        );

        let center = scope_measurement_window_rect(plot, size, Vec2::new(0.5, 0.5));
        assert_eq!(center.center(), plot.center());
        assert_eq!(
            scope_measurement_window_position(plot, center),
            Vec2::new(0.5, 0.5)
        );
    }

    #[test]
    fn scope_markers_measure_horizontal_time_and_vertical_amplitude() {
        let markers = ScopeMarkerSettings::default();

        assert!(
            (scope_time_marker_delta(
                markers.channels[0].horizontal_positions[0],
                markers.channels[0].horizontal_positions[1],
                1_000,
                1_000,
            )
            .unwrap()
                - 0.1)
                .abs()
                < 0.000_001
        );
        assert!(
            (scope_voltage_marker_delta(
                markers.channels[0].vertical_positions[0],
                markers.channels[0].vertical_positions[1],
                ScopeCaptureSettings::default().channels[0]
            ) - 0.8)
                .abs()
                < 0.000_001
        );

        let mut scope_settings = ScopeCaptureSettings::default();
        scope_settings.channels[1].volts_per_div_mv = 500;
        scope_settings.channels[1].probe_attenuation = 10;
        assert!(
            (scope_voltage_marker_delta(
                markers.channels[1].vertical_positions[0],
                markers.channels[1].vertical_positions[1],
                scope_settings.channels[1]
            ) - 4.0)
                .abs()
                < 0.000_001
        );

        assert_ne!(
            markers.channels[0].horizontal_positions,
            markers.channels[1].horizontal_positions
        );
        assert_eq!(
            markers.channels[0].horizontal_positions.len(),
            SCOPE_MARKER_LIMIT
        );
        assert_eq!(
            markers.channels[1].vertical_positions.len(),
            SCOPE_MARKER_LIMIT
        );
        assert_eq!(format_marker_voltage(0.125), "125.00 mV");
    }

    #[test]
    fn quick_time_measurement_starts_then_finishes_with_clamped_positions() {
        let mut measurement = ScopeQuickTimeMeasurement::default();

        measurement.finish(0.8);
        assert_eq!(measurement, ScopeQuickTimeMeasurement::Idle);

        measurement.start(-0.2);
        assert_eq!(
            measurement,
            ScopeQuickTimeMeasurement::AwaitingSecond {
                first_position: 0.0,
            }
        );

        measurement.finish(1.2);
        assert_eq!(
            measurement,
            ScopeQuickTimeMeasurement::Complete {
                first_position: 0.0,
                second_position: 1.0,
            }
        );

        measurement.finish(0.4);
        assert_eq!(
            measurement,
            ScopeQuickTimeMeasurement::Complete {
                first_position: 0.0,
                second_position: 1.0,
            }
        );

        measurement.start(0.25);
        assert_eq!(
            measurement,
            ScopeQuickTimeMeasurement::AwaitingSecond {
                first_position: 0.25,
            }
        );

        measurement.clear();
        assert_eq!(measurement, ScopeQuickTimeMeasurement::Idle);
    }

    #[test]
    fn new_scope_markers_start_in_the_visible_pane_center() {
        let settings = ScopeMarkerSettings::default();
        let viewport = WaveformViewport {
            start: 0.6,
            span: 0.2,
            ..WaveformViewport::default()
        };

        let time = ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Time);
        let voltage = ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Voltage);

        assert!((time.position - 0.7).abs() < f32::EPSILON);
        assert!((voltage.position - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn scope_markers_are_saved_and_compared_individually() {
        let mut settings = ScopeMarkerSettings::default();
        let viewport = WaveformViewport::default();
        let mut first = ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Time);
        first.position = 0.2;
        first.color = Color32::RED;
        assert!(first.save(&mut settings));

        let mut second = ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Time);
        second.position = 0.7;
        second.comparison = Some(0);
        assert!(second.save(&mut settings));

        let channel = settings.channels[0];
        assert_eq!(channel.horizontal_count, 2);
        assert_eq!(channel.marker(ScopeMarkerKind::Time, 0).unwrap().0, 0.2);
        assert_eq!(
            channel.marker(ScopeMarkerKind::Time, 0).unwrap().2,
            Color32::RED
        );
        assert_eq!(channel.horizontal_comparisons[1], Some(0));
        assert!(
            (scope_time_marker_delta(
                channel.horizontal_positions[1],
                channel.horizontal_positions[0],
                1_000,
                1_000,
            )
            .unwrap()
                - 0.5)
                .abs()
                < 0.000_001
        );

        assert!(settings.channels[0].remove_marker(ScopeMarkerKind::Time, 0));
        assert_eq!(settings.channels[0].horizontal_count, 1);
        assert_eq!(settings.channels[0].horizontal_comparisons[0], None);
        assert_eq!(settings.channels[0].horizontal_positions[0], 0.7);
    }

    #[test]
    fn scope_marker_limit_is_shared_across_types_and_markers_can_change_type() {
        let mut settings = ScopeMarkerSettings::default();
        let viewport = WaveformViewport::default();
        for index in 0..SCOPE_MARKER_LIMIT {
            let kind = if index % 2 == 0 {
                ScopeMarkerKind::Time
            } else {
                ScopeMarkerKind::Voltage
            };
            let editor = ScopeMarkerEditor::for_new(settings, viewport, 0, kind);
            assert!(editor.save(&mut settings));
        }
        assert_eq!(settings.channels[0].total_count(), SCOPE_MARKER_LIMIT);
        assert!(
            !ScopeMarkerEditor::for_new(settings, viewport, 0, ScopeMarkerKind::Time)
                .save(&mut settings)
        );

        let key = ScopeMarkerKey {
            channel: 0,
            kind: ScopeMarkerKind::Time,
            index: 0,
        };
        let mut editor = ScopeMarkerEditor::for_existing(settings, key).unwrap();
        editor.kind = ScopeMarkerKind::Voltage;
        assert!(editor.save(&mut settings));
        assert_eq!(settings.channels[0].total_count(), SCOPE_MARKER_LIMIT);
        assert_eq!(settings.channels[0].count(ScopeMarkerKind::Time), 4);
        assert_eq!(settings.channels[0].count(ScopeMarkerKind::Voltage), 6);
    }

    #[test]
    fn waveform_status_is_positioned_above_the_time_ruler() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 300.0));
        let (status, ruler, plot) = split_waveform_chrome(rect);

        assert_eq!(status.top(), rect.top());
        assert_eq!(status.bottom(), ruler.top());
        assert_eq!(ruler.bottom(), plot.top());
        assert_eq!(plot.bottom(), rect.bottom());
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
    fn logic_channel_markers_stay_compact_and_inside_their_lanes() {
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 320.0));
        let lane_height = 20.0;
        let marker = logic_channel_marker_rect(plot, lane_height * 0.75, lane_height, "15");
        let named_marker =
            logic_channel_marker_rect(plot, lane_height * 0.75, lane_height, "CLOCKCLK");

        assert_eq!(marker.width(), 26.0);
        assert_eq!(named_marker.width(), 62.0);
        assert!(marker.height() <= 13.0);
        assert!(marker.top() >= plot.top());
        assert!(marker.bottom() <= plot.top() + lane_height);
    }

    #[test]
    fn logic_maximum_height_uses_the_selected_multiple() {
        let mut maximum_heights = [1; LOGIC_CHANNEL_COUNT];

        for multiple in 1..=5 {
            maximum_heights[8] = multiple;
            let layout = logic_signal_layout(0.0, 320.0, &maximum_heights);
            let (high, low) = layout.levels[8];
            assert!((low - high - layout.base_height * f32::from(multiple)).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn logic_maximum_height_reflows_channels_inside_the_plot() {
        let height_patterns = [
            [1; LOGIC_CHANNEL_COUNT],
            [5; LOGIC_CHANNEL_COUNT],
            [5, 5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 5, 5],
            [5, 1, 5, 1, 5, 1, 5, 1, 5, 1, 5, 1, 5, 1, 5, 1],
        ];

        for maximum_heights in height_patterns {
            let layout = logic_signal_layout(40.0, 320.0, &maximum_heights);
            assert!(layout.levels[0].0 >= 40.0 + layout.base_height);
            assert!(layout.levels[LOGIC_CHANNEL_COUNT - 1].1 <= 360.0 - layout.base_height);
            for channel in 1..LOGIC_CHANNEL_COUNT {
                assert!(
                    layout.levels[channel - 1].1 + layout.base_height
                        <= layout.levels[channel].0 + f32::EPSILON
                );
            }
        }
    }

    #[test]
    fn logic_reflow_moves_neighbors_in_base_height_steps() {
        let defaults = logic_signal_layout(0.0, 320.0, &[1; LOGIC_CHANNEL_COUNT]);
        let mut maximum_heights = [1; LOGIC_CHANNEL_COUNT];
        maximum_heights[0] = 5;
        let enlarged_first = logic_signal_layout(0.0, 320.0, &maximum_heights);
        assert!((defaults.base_height - enlarged_first.base_height).abs() < f32::EPSILON);
        assert!(
            (enlarged_first.levels[1].0 - defaults.levels[1].0 - defaults.base_height).abs()
                < f32::EPSILON
        );

        maximum_heights = [1; LOGIC_CHANNEL_COUNT];
        maximum_heights[LOGIC_CHANNEL_COUNT - 1] = 5;
        let enlarged_last = logic_signal_layout(0.0, 320.0, &maximum_heights);
        let movement = (defaults.levels[14].0 - enlarged_last.levels[14].0) / defaults.base_height;
        assert!((movement - movement.round()).abs() < f32::EPSILON);
        assert!(movement >= 1.0);
    }

    #[test]
    fn logic_channel_marker_uses_name_limited_to_eight_characters() {
        assert_eq!(logic_channel_marker_text(3, ""), "3");
        assert_eq!(logic_channel_marker_text(3, "  CLOCK  "), "CLOCK");
        assert_eq!(logic_channel_marker_text(3, "DATA-LONG-NAME"), "DATA-LON");
    }

    #[test]
    fn logic_decimation_preserves_one_kilohertz_pulses_across_a_one_second_span() {
        let mut samples = VecDeque::from(vec![0_u32; 1_000_000]);
        for pulse in 0..1_000 {
            let start = pulse * 1_000 + 100;
            for sample in samples.range_mut(start..start + 20) {
                *sample = 1;
            }
        }
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 320.0));
        let mut enabled_channels = [false; LOGIC_CHANNEL_COUNT];
        enabled_channels[0] = true;
        let levels = logic_signal_layout(plot.top(), plot.height(), &[1; LOGIC_CHANNEL_COUNT]);

        let points = logic_waveform_points(
            &samples,
            0..samples.len(),
            plot,
            WaveformViewport::default(),
            &levels.levels,
            &enabled_channels,
        );
        let (high, low) = levels.levels[0];
        let pulses_in_first_200_ms = points[0]
            .windows(2)
            .filter(|edge| {
                edge[0].x < 200.0
                    && (edge[0].x - edge[1].x).abs() < f32::EPSILON
                    && edge[0].y == low
                    && edge[1].y == high
            })
            .count();

        assert_eq!(pulses_in_first_200_ms, 200);
    }

    #[test]
    fn waveform_zoom_preserves_the_sample_under_the_pointer() {
        let mut viewport = WaveformViewport::default();

        assert!(viewport.step_timebase(1.0, 1_000_000, 1_000_000, 0.25));

        assert!((viewport.span - 0.5).abs() < f32::EPSILON);
        assert!((viewport.start + viewport.span * 0.25 - 0.25).abs() < 0.000_001);
        assert!(!viewport.anchor_locked);
        assert!(viewport.sample_range(1_000).len() < 1_000);
        assert_eq!(viewport.time_per_division(1_000_000, 1_000_000), Some(0.05));
    }

    #[test]
    fn waveform_zoom_out_is_clamped_to_the_full_capture() {
        let mut viewport = WaveformViewport::default();
        viewport.step_timebase(1.0, 1_000_000, 1_000_000, 0.75);

        viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.75);

        assert_eq!(viewport.start, 0.0);
        assert_eq!(viewport.span, 1.0);
        assert!(!viewport.anchor_locked);
        assert_eq!(viewport.sample_range(1_000), 0..1_000);
    }

    #[test]
    fn scope_timebase_does_not_change_with_capture_length() {
        let mut viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            ..WaveformViewport::default()
        };
        let selected_timebase = viewport.time_per_division(1_000_000, 1_000_000).unwrap();

        apply_time_per_division(&mut viewport, 250_000, 1_000_000, selected_timebase);

        assert!((viewport.span - 2.0).abs() < f32::EPSILON);
        assert!(
            (viewport.time_per_division(250_000, 1_000_000).unwrap() - selected_timebase).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn rolling_scope_uses_the_full_span_as_its_sample_domain() {
        let viewport = WaveformViewport::default();
        let range = viewport.sample_range_in_domain(200_000, 1_000_000);
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 400.0));

        assert_eq!(range, 0..200_000);
        assert!((waveform_sample_x(plot, viewport, 199_999.0, 1_000_000) - 200.0).abs() < 0.01);
    }

    #[test]
    fn only_auto_trigger_run_uses_the_rolling_scope_buffer() {
        assert!(uses_rolling_scope_buffer(
            AcquisitionMode::Run,
            true,
            DsCopeTriggerSource::Auto,
        ));
        assert!(!uses_rolling_scope_buffer(
            AcquisitionMode::Single,
            true,
            DsCopeTriggerSource::Auto,
        ));
        assert!(!uses_rolling_scope_buffer(
            AcquisitionMode::Run,
            true,
            DsCopeTriggerSource::ChannelZero,
        ));

        let config = ScopeCaptureSettings::default().capture_config().unwrap();
        assert_eq!(scope_rolling_sample_capacity(config, Some(0.1)), 1_000_000);
        assert_eq!(
            scope_rolling_sample_capacity(config, Some(2.0)),
            SCOPE_ROLLING_MEMORY_BUDGET_BYTES / 2 / std::mem::size_of::<f32>()
        );
    }

    #[test]
    fn time_scale_selects_readable_units_for_the_visible_window() {
        assert_eq!(time_scale_unit(2.0), ("s", 1.0));
        assert_eq!(time_scale_unit(0.002), ("ms", 1.0e3));
        assert_eq!(time_scale_unit(0.000_002), ("µs", 1.0e6));
        assert_eq!(time_scale_unit(0.000_000_002), ("ns", 1.0e9));
        assert_eq!(time_scale_precision(12.0), 0);
        assert_eq!(time_scale_precision(1.2), 1);
        assert_eq!(time_scale_precision(0.008), 4);
        assert_eq!(time_scale_label_stride(1_000.0), 1);
        assert_eq!(time_scale_label_stride(500.0), 2);
        assert_eq!(time_scale_label_stride(250.0), 5);
        assert_eq!(format_duration(Duration::from_nanos(2)), "2.0 ns");
    }

    #[test]
    fn timebase_steps_follow_the_vendor_one_two_five_sequence() {
        assert_eq!(TIMEBASE_STEPS_SECONDS.first(), Some(&2.0e-9));
        assert_eq!(TIMEBASE_STEPS_SECONDS.last(), Some(&10.0));
        assert!(TIMEBASE_STEPS_SECONDS.contains(&DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS));
        assert_eq!(adjacent_timebase(10.0e-9, true), Some(5.0e-9));
        assert_eq!(adjacent_timebase(10.0e-9, false), Some(20.0e-9));
        assert_eq!(adjacent_timebase(2.0e-9, true), None);
        assert_eq!(adjacent_timebase(10.0, false), None);

        let mut default_viewport = WaveformViewport::default();
        apply_time_per_division(
            &mut default_viewport,
            SAMPLE_COUNT as usize,
            SAMPLE_RATE_HZ,
            DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS,
        );
        assert!(
            (default_viewport
                .time_per_division(SAMPLE_COUNT as usize, SAMPLE_RATE_HZ)
                .unwrap()
                - DEFAULT_SCOPE_TIME_PER_DIVISION_SECONDS)
                .abs()
                < 1.0e-9
        );

        let mut viewport = WaveformViewport::default();
        for _ in 0..6 {
            viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5);
        }
        assert!((viewport.time_per_division(1_000_000, 1_000_000).unwrap() - 10.0).abs() < 0.001);
        assert!((viewport.span - 100.0).abs() < 0.001);
        assert_eq!(viewport.sample_range(1_000_000), 0..1_000_000);
        assert!(!viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5));
    }

    #[test]
    fn scope_timebase_selects_the_vendor_floor_sample_rate() {
        let mut settings = ScopeCaptureSettings::default();

        assert_eq!(settings.sample_rate_for_timebase(1.0e-3), 100_000_000);
        assert_eq!(settings.sample_rate_for_timebase(500.0e-6), 125_000_000);
        assert_eq!(settings.sample_rate_for_timebase(200.0e-6), 500_000_000);
        assert_eq!(settings.sample_rate_for_timebase(10.0), 10_000);
        assert_eq!(
            settings.sample_rate_for_timebase(f64::NAN),
            settings.sample_rate_hz
        );

        settings.channels[1].enabled = false;
        settings.sample_count = 2_000_000;
        assert_eq!(settings.sample_rate_for_timebase(200.0e-6), 1_000_000_000);
    }

    #[test]
    fn idle_time_scale_uses_the_configured_display_window() {
        let viewport = WaveformViewport::default();
        let (first, last) = time_scale_sample_window(SAMPLE_COUNT as usize, 0, viewport).unwrap();

        assert_eq!(first, 0.0);
        assert_eq!(last, SAMPLE_COUNT as f64 - 1.0);

        let mut zoomed = viewport;
        zoomed.step_timebase(1.0, SAMPLE_COUNT as usize, SAMPLE_RATE_HZ, 0.5);
        let (zoomed_first, zoomed_last) =
            time_scale_sample_window(SAMPLE_COUNT as usize, 0, zoomed).unwrap();
        assert!(zoomed_last - zoomed_first < last - first);
    }

    #[test]
    fn scope_settings_map_common_and_per_channel_configuration() {
        let settings = ScopeCaptureSettings {
            sample_rate_hz: 2_000_000,
            sample_count: 250_000,
            channels: [
                ScopeChannelSettings {
                    enabled: true,
                    volts_per_div_mv: 100,
                    coupling: DsCopeCoupling::Ac,
                    probe_attenuation: 100,
                    vertical_offset: 64,
                },
                ScopeChannelSettings {
                    enabled: false,
                    volts_per_div_mv: 2_000,
                    coupling: DsCopeCoupling::Dc,
                    probe_attenuation: 1,
                    vertical_offset: 192,
                },
            ],
            trigger: ScopeTriggerSettings {
                position_percent: 75,
                source: DsCopeTriggerSource::ChannelZero,
                auto_channel: 0,
                slope: DsCopeTriggerSlope::Falling,
                holdoff_ns: 250_000,
                holdoff_unit: TriggerHoldoffUnit::Microseconds,
                noise_sensitivity: 12,
                levels: [64, 192],
            },
            operation_mode: DsCopeOperationMode::InternalTest,
            bandwidth_limit: DsCopeBandwidthLimit::Mhz20,
        };

        let config = settings.capture_config().unwrap();
        assert_eq!(config.sample_rate_hz(), 2_000_000);
        assert_eq!(config.sample_count(), 250_000);
        assert_eq!(config.channels()[0].volts_per_div_mv(), 100);
        assert_eq!(config.channels()[0].coupling(), DsCopeCoupling::Ac);
        assert_eq!(config.channels()[0].probe_attenuation(), 100);
        assert_eq!(
            config.channels()[0].vertical_offset(),
            DSCOPE_VERTICAL_OFFSET_CENTER
        );
        assert_eq!(config.channels()[1].volts_per_div_mv(), 2_000);
        assert_eq!(config.channels()[1].coupling(), DsCopeCoupling::Dc);
        assert_eq!(config.channels()[1].probe_attenuation(), 1);
        assert_eq!(
            config.channels()[1].vertical_offset(),
            DSCOPE_VERTICAL_OFFSET_CENTER
        );
        assert!(!config.channels()[1].enabled());
        assert_eq!(config.trigger().position_percent(), 75);
        assert_eq!(config.trigger().source(), DsCopeTriggerSource::ChannelZero);
        assert_eq!(config.trigger().slope(), DsCopeTriggerSlope::Falling);
        assert_eq!(config.trigger().holdoff_ns(), 250_000);
        assert_eq!(config.trigger().noise_sensitivity(), 12);
        assert_eq!(config.trigger().levels(), [64, 192]);
        assert_eq!(config.operation_mode(), DsCopeOperationMode::InternalTest);
        assert_eq!(config.bandwidth_limit(), DsCopeBandwidthLimit::Mhz20);
        let mut hardware_settings = settings;
        for channel in &mut hardware_settings.channels {
            channel.vertical_offset = DSCOPE_VERTICAL_OFFSET_CENTER;
        }
        assert_eq!(
            ScopeCaptureSettings::from_capture_config(config),
            hardware_settings
        );
        assert_eq!(format_sample_rate(1_000_000), "1.00 MSa/s");
        assert_eq!(format_sample_rate(1_000_000_000), "1.00 GSa/s");
    }

    #[test]
    fn scope_limits_follow_the_enabled_channel_count() {
        let mut settings = ScopeCaptureSettings::default();
        assert_eq!(settings.maximum_sample_rate_hz(), 500_000_000);
        assert_eq!(settings.maximum_sample_count(), 1_000_000);

        settings.channels[1].enabled = false;
        settings.sample_rate_hz = 1_000_000_000;
        settings.sample_count = 2_000_000;

        assert_eq!(settings.maximum_sample_rate_hz(), 1_000_000_000);
        assert_eq!(settings.maximum_sample_count(), 2_000_000);
        assert!(settings.capture_config().is_ok());
    }

    #[test]
    fn scope_frontend_change_detection_ignores_display_conversion_and_buffer() {
        let original = ScopeCaptureSettings::default();
        let mut changed = original;
        changed.sample_count = 250_000;
        changed.channels[0].probe_attenuation = 10;
        assert!(!changed.frontend_differs_from(original));

        changed = original;
        changed.channels[1].enabled = false;
        assert!(changed.frontend_differs_from(original));
        assert!(changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.channels[0].volts_per_div_mv = 100;
        assert!(changed.frontend_differs_from(original));
        assert!(changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.channels[0].vertical_offset = 64;
        assert!(!changed.frontend_differs_from(original));
        assert!(!changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.channels[0].coupling = DsCopeCoupling::Ac;
        assert!(changed.frontend_differs_from(original));
        assert!(!changed.runtime_frontend_differs_from(original));

        changed = original;
        changed.trigger.holdoff_unit = TriggerHoldoffUnit::Seconds;
        assert!(!changed.frontend_differs_from(original));

        changed.trigger.slope = DsCopeTriggerSlope::Falling;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.trigger.levels[0] = 64;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.operation_mode = DsCopeOperationMode::InternalTest;
        assert!(changed.frontend_differs_from(original));

        changed = original;
        changed.bandwidth_limit = DsCopeBandwidthLimit::Mhz20;
        assert!(changed.frontend_differs_from(original));
    }

    #[test]
    fn scope_trigger_source_tracks_enabled_channels() {
        let mut trigger = ScopeTriggerSettings {
            source: DsCopeTriggerSource::ChannelOne,
            auto_channel: 1,
            ..ScopeTriggerSettings::default()
        };

        trigger.clamp_to_channels([true, false]);

        assert_eq!(trigger.source, DsCopeTriggerSource::Auto);
        assert_eq!(trigger.auto_channel, 0);
        assert!(trigger.capture_config().is_ok());
    }

    #[test]
    fn scope_trigger_levels_use_centered_hardware_and_follow_display_offsets() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 20.0), Pos2::new(100.0, 220.0));
        let channel = ScopeChannelSettings::default();

        assert_eq!(scope_trigger_level_from_y(plot, plot.top(), channel), 34);
        assert_eq!(
            scope_trigger_level_from_y(plot, plot.bottom(), channel),
            222
        );
        assert_eq!(
            scope_trigger_level_from_y(plot, scope_trigger_level_y(plot, channel, 128), channel),
            128
        );
        assert!((scope_trigger_threshold_voltage(channel, 128)).abs() < f32::EPSILON);
        assert!((scope_trigger_threshold_voltage(channel, 104) - 1.021_276_6).abs() < 0.000_001);

        let shifted_channel = ScopeChannelSettings {
            vertical_offset: 160,
            ..channel
        };
        let shifted_zero_y = scope_vertical_offset_y(plot, 160);
        assert_eq!(
            scope_trigger_level_y(plot, shifted_channel, DSCOPE_TRIGGER_LEVEL_CENTER),
            shifted_zero_y
        );
        assert_eq!(
            scope_trigger_level_from_y(plot, shifted_zero_y, shifted_channel),
            DSCOPE_TRIGGER_LEVEL_CENTER
        );
        assert_eq!(
            scope_trigger_threshold_voltage(shifted_channel, 104),
            scope_trigger_threshold_voltage(channel, 104)
        );
        assert_eq!(format_trigger_voltage(0.0), "0.00 mV");
        assert_eq!(format_trigger_voltage(0.125), "125 mV");
        assert_eq!(format_trigger_voltage(-1.25), "-1.25 V");

        let trigger = ScopeTriggerSettings {
            source: DsCopeTriggerSource::ChannelZeroAndOne,
            ..ScopeTriggerSettings::default()
        };
        assert_eq!(scope_trigger_channels(trigger), &[0, 1]);
    }

    #[test]
    fn scope_trigger_anchor_interpolates_the_threshold_crossing() {
        let mut waveforms = WaveformStore {
            scope_channels: vec![
                VecDeque::from(vec![-1.0, -1.0, 1.0, 1.0]),
                VecDeque::from(vec![0.0; 4]),
            ],
            scope_trigger_sample_index: Some(3),
            ..WaveformStore::default()
        };
        let settings = ScopeCaptureSettings::default();
        let mut trigger = ScopeTriggerSettings {
            source: DsCopeTriggerSource::ChannelZero,
            slope: DsCopeTriggerSlope::Rising,
            ..ScopeTriggerSettings::default()
        };

        assert_eq!(
            scope_trigger_crossing_sample(&waveforms, settings, trigger),
            Some(ScopeTriggerCrossing {
                sample: 1.5,
                channel_index: 0,
            })
        );

        waveforms.scope_channels[0] = VecDeque::from(vec![1.0, 1.0, -1.0, -1.0]);
        trigger.slope = DsCopeTriggerSlope::Falling;
        assert_eq!(
            scope_trigger_crossing_sample(&waveforms, settings, trigger),
            Some(ScopeTriggerCrossing {
                sample: 1.5,
                channel_index: 0,
            })
        );
    }

    #[test]
    fn scope_trigger_anchor_selects_the_nearest_matching_edge() {
        let mut samples = vec![-1.0; 32];
        samples[11..16].fill(1.0);
        samples[21..].fill(1.0);
        let waveforms = WaveformStore {
            scope_channels: vec![VecDeque::from(samples), VecDeque::from(vec![0.0; 32])],
            scope_trigger_sample_index: Some(19),
            ..WaveformStore::default()
        };
        let trigger = ScopeTriggerSettings {
            source: DsCopeTriggerSource::ChannelZero,
            slope: DsCopeTriggerSlope::Rising,
            ..ScopeTriggerSettings::default()
        };

        assert_eq!(
            scope_trigger_crossing_sample(&waveforms, ScopeCaptureSettings::default(), trigger),
            Some(ScopeTriggerCrossing {
                sample: 20.5,
                channel_index: 0,
            })
        );
    }

    #[test]
    fn scope_auto_trigger_locks_a_free_running_frame_when_it_contains_an_edge() {
        let mut samples = vec![-1.0; 32];
        samples[11..16].fill(1.0);
        samples[21..].fill(1.0);
        let mut waveforms = WaveformStore {
            scope_channels: vec![VecDeque::from(samples), VecDeque::from(vec![0.0; 32])],
            scope_trigger_sample_index: None,
            ..WaveformStore::default()
        };
        let trigger = ScopeTriggerSettings {
            position_percent: 65,
            source: DsCopeTriggerSource::Auto,
            auto_channel: 0,
            slope: DsCopeTriggerSlope::Rising,
            ..ScopeTriggerSettings::default()
        };

        assert_eq!(
            scope_trigger_crossing_sample(&waveforms, ScopeCaptureSettings::default(), trigger),
            Some(ScopeTriggerCrossing {
                sample: 20.5,
                channel_index: 0,
            })
        );

        waveforms.scope_channels[0]
            .iter_mut()
            .for_each(|sample| *sample = 0.0);
        assert_eq!(
            scope_trigger_crossing_sample(&waveforms, ScopeCaptureSettings::default(), trigger),
            None
        );
    }

    #[test]
    fn scope_trigger_detection_ignores_capture_boundaries() {
        let waveforms = WaveformStore {
            scope_channels: vec![
                VecDeque::from(vec![-1.0, -1.0, 1.0, 1.0]),
                VecDeque::from(vec![0.0; 4]),
            ],
            scope_capture_boundaries: VecDeque::from([2]),
            ..WaveformStore::default()
        };

        assert_eq!(
            scope_trigger_crossing_sample(
                &waveforms,
                ScopeCaptureSettings::default(),
                ScopeTriggerSettings::default(),
            ),
            None
        );
    }

    #[test]
    fn scope_trace_and_trigger_use_the_same_sample_coordinates() {
        let plot = Rect::from_min_max(Pos2::new(100.0, 20.0), Pos2::new(900.0, 420.0));
        let viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            ..WaveformViewport::default()
        };

        assert!((scope_sample_fraction(1_050.0, 1_000, 101) - 0.5).abs() < f32::EPSILON);
        assert!((waveform_sample_x(plot, viewport, 50.0, 101) - 500.0).abs() < f32::EPSILON);
    }

    #[test]
    fn scope_time_marker_coordinates_stay_attached_to_the_capture() {
        let plot = Rect::from_min_max(Pos2::new(100.0, 20.0), Pos2::new(900.0, 420.0));
        let mut viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            ..WaveformViewport::default()
        };
        let marker_position = viewport.domain_fraction_at_screen(0.5);

        assert!((marker_position - 0.5).abs() < f32::EPSILON);
        assert!(
            (egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain(marker_position)
            ) - 500.0)
                .abs()
                < f32::EPSILON
        );

        assert!(viewport.pan_by_screen_delta(-100.0, plot.width()));
        assert!(
            (egui::lerp(
                plot.x_range(),
                viewport.screen_fraction_at_domain(marker_position)
            ) - 400.0)
                .abs()
                < f32::EPSILON
        );
        assert!(
            (scope_time_marker_delta(0.25, 0.5, 1_000, 1_000).unwrap() - 0.25).abs() < 0.000_001
        );
    }

    #[test]
    fn scope_downsampling_preserves_trigger_crossing_samples() {
        let indices = scope_plot_sample_indices(
            0..1_000,
            100,
            0,
            Some(ScopeTriggerCrossing {
                sample: 512.25,
                channel_index: 0,
            }),
            0,
        );

        assert!(indices.contains(&512));
        assert!(indices.contains(&513));
        assert_eq!(indices.last(), Some(&999));
    }

    #[test]
    fn scope_trigger_position_maps_between_percent_and_plot_x() {
        let plot = Rect::from_min_max(Pos2::new(100.0, 20.0), Pos2::new(900.0, 420.0));

        assert!((scope_trigger_position_x(plot, 50) - 500.0).abs() < f32::EPSILON);
        assert_eq!(scope_trigger_position_from_x(plot, 500.0), 50);
        assert_eq!(
            scope_trigger_position_from_x(plot, plot.left()),
            DSCOPE_TRIGGER_POSITION_MIN_PERCENT
        );
        assert_eq!(
            scope_trigger_position_from_x(plot, plot.right()),
            DSCOPE_TRIGGER_POSITION_MAX_PERCENT
        );

        let mut viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            ..WaveformViewport::default()
        };
        assert_eq!(scope_trigger_position_in_view(0.5, viewport), 50);
        assert!(viewport.pan_by_screen_delta(-100.0, 1_000.0));
        assert_eq!(scope_trigger_position_in_view(0.5, viewport), 40);
        let panned_start = viewport.start;
        let mut trigger_settings = ScopeTriggerSettings::default();
        finish_scope_viewport_pan(0.5, &mut viewport, &mut trigger_settings);
        assert_eq!(trigger_settings.position_percent, 40);
        assert_eq!(viewport.start, panned_start);
        assert!(!viewport.anchor_locked);

        viewport.start = 0.25;
        viewport.anchor_locked = true;
        viewport.move_screen_anchor(0.5, 0.25);
        assert!((viewport.start - 0.375).abs() < f32::EPSILON);
        assert!(((0.5 - viewport.start) / viewport.span - 0.25).abs() < f32::EPSILON);

        let mut full_span = WaveformViewport::default();
        full_span.align_trigger_to_screen(0.75, 0.5);
        assert!((full_span.start - 0.25).abs() < f32::EPSILON);
        assert!(((0.75 - full_span.start) / full_span.span - 0.5).abs() < f32::EPSILON);

        let mut hardware_anchored = WaveformViewport::default();
        hardware_anchored.step_timebase_at_sample(1.0, 1_000_000, 1_000_000, 0.503, 0.5);
        assert!(hardware_anchored.span < 1.0);
        assert!(
            ((0.503 - hardware_anchored.start) / hardware_anchored.span - 0.5).abs() < f32::EPSILON
        );
    }

    #[test]
    fn waveform_pan_tracks_mouse_motion_and_clamps_to_capture() {
        let mut viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            ..WaveformViewport::default()
        };

        assert!(viewport.pan_by_screen_delta(-100.0, 1_000.0));
        assert!((viewport.start - 0.3).abs() < 0.000_001);
        assert!(!viewport.anchor_locked);

        assert!(viewport.pan_by_screen_delta(10_000.0, 1_000.0));
        assert_eq!(viewport.start, 0.0);
        assert!(!viewport.pan_by_screen_delta(1.0, 1_000.0));
    }

    #[test]
    fn scope_vertical_offset_maps_hardware_limits_to_the_plot() {
        let rect = Rect::from_min_max(Pos2::new(0.0, 20.0), Pos2::new(100.0, 220.0));

        assert_eq!(
            scope_vertical_offset_y(rect, DSCOPE_VERTICAL_OFFSET_MIN),
            rect.top()
        );
        assert_eq!(
            scope_vertical_offset_y(rect, DSCOPE_VERTICAL_OFFSET_MAX),
            rect.bottom()
        );
        assert_eq!(
            scope_vertical_offset_from_y(rect, rect.top()),
            DSCOPE_VERTICAL_OFFSET_MIN
        );
        assert_eq!(
            scope_vertical_offset_from_y(rect, rect.bottom()),
            DSCOPE_VERTICAL_OFFSET_MAX
        );
    }

    #[test]
    fn scope_input_range_rescales_captured_volts() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1_000.0, 800.0));
        let one_volt_per_div = ScopeChannelSettings {
            volts_per_div_mv: 1_000,
            ..ScopeChannelSettings::default()
        };
        let half_volt_per_div = ScopeChannelSettings {
            volts_per_div_mv: 500,
            ..one_volt_per_div
        };
        let zero_y = scope_sample_y(plot, one_volt_per_div, 0.0);
        let one_volt_y = scope_sample_y(plot, one_volt_per_div, 1.0);
        let half_volt_range_y = scope_sample_y(plot, half_volt_per_div, 1.0);

        let pixels_per_division = plot.height() / SCOPE_VERTICAL_DIVISIONS as f32;
        assert!((zero_y - one_volt_y - pixels_per_division).abs() < 0.001);
        assert!((zero_y - half_volt_range_y - pixels_per_division * 2.0).abs() < 0.001);
    }

    #[test]
    fn overlapping_scope_offset_arrows_remain_independently_selectable() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 200.0));
        let mut settings = ScopeCaptureSettings::default();
        let mut offsets = [DSCOPE_VERTICAL_OFFSET_CENTER; 2];
        let center_y = scope_vertical_offset_y(plot, DSCOPE_VERTICAL_OFFSET_CENTER);

        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            Some(0)
        );

        offsets[0] = DSCOPE_VERTICAL_OFFSET_MIN;
        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            Some(1)
        );

        settings.channels[0].enabled = false;
        offsets[0] = DSCOPE_VERTICAL_OFFSET_CENTER;
        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            Some(1)
        );

        settings.channels[1].enabled = false;
        assert_eq!(
            scope_offset_arrow_at_pointer(plot, center_y, settings, &offsets),
            None
        );
    }

    #[test]
    fn scope_input_range_encoder_steps_and_clamps_supported_ranges() {
        assert_eq!(step_scope_input_range(100, 1), 200);
        assert_eq!(step_scope_input_range(100, -1), 50);
        assert_eq!(step_scope_input_range(10, -10), 10);
        assert_eq!(step_scope_input_range(2_000, 10), 2_000);
    }
}
