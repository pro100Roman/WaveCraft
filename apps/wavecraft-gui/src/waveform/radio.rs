use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};
use std::collections::VecDeque;
use wavecraft_decode::{DecodeDiagnostic, RawRadioOutput, TimedByte};
use wavecraft_dsp::{DemodulatedBatch, SpectrumFrame, TimedAnalogSample, TimedSoftSymbol};

const DEFAULT_WATERFALL_CAPACITY: usize = 256;
const DEFAULT_ANALYSIS_CAPACITY: usize = 100_000;
const MAX_WATERFALL_TEXTURE_WIDTH: usize = 512;
const MIN_POWER_DBFS: f32 = -120.0;
const MAX_POWER_DBFS: f32 = 0.0;
const WATERFALL_HISTORY_HINT: &str = "Rows are the newest spectrum snapshots retained for the visible waterfall. Dropped counts older display rows discarded after this bounded history filled; it does not indicate lost HackRF or IQ samples.";

/// Bounded, modulation-neutral output retained for analyzer rendering.
#[derive(Debug)]
pub struct RadioAnalysisStore {
    soft_symbols: VecDeque<TimedSoftSymbol>,
    soft_symbol_alphabet_size: Option<u16>,
    analog_samples: VecDeque<TimedAnalogSample>,
    analog_sample_rate_hz: Option<u64>,
    capacity: usize,
    dropped_items: u64,
    discontinuities: u64,
    error: Option<String>,
    raw_bytes: VecDeque<TimedByte>,
    raw_synchronized: bool,
    raw_dropped_bytes: u64,
    raw_diagnostics: VecDeque<DecodeDiagnostic>,
}

impl Default for RadioAnalysisStore {
    fn default() -> Self {
        Self {
            soft_symbols: VecDeque::with_capacity(DEFAULT_ANALYSIS_CAPACITY),
            soft_symbol_alphabet_size: None,
            analog_samples: VecDeque::new(),
            analog_sample_rate_hz: None,
            capacity: DEFAULT_ANALYSIS_CAPACITY,
            dropped_items: 0,
            discontinuities: 0,
            error: None,
            raw_bytes: VecDeque::new(),
            raw_synchronized: false,
            raw_dropped_bytes: 0,
            raw_diagnostics: VecDeque::new(),
        }
    }
}

impl RadioAnalysisStore {
    pub fn new(capacity: usize) -> Result<Self, String> {
        if capacity == 0 {
            return Err("radio analysis capacity must be greater than zero".to_owned());
        }
        Ok(Self {
            capacity,
            ..Self::default()
        })
    }

    pub fn ingest(&mut self, batch: DemodulatedBatch) -> Result<(), String> {
        match batch {
            DemodulatedBatch::SoftSymbols(batch) => {
                batch.validate().map_err(|error| error.to_string())?;
                if batch.discontinuity {
                    self.discontinuities = self.discontinuities.saturating_add(1);
                    self.raw_bytes.clear();
                    self.raw_synchronized = false;
                }
                if self.soft_symbol_alphabet_size != Some(batch.alphabet_size) {
                    self.soft_symbols.clear();
                    self.soft_symbol_alphabet_size = Some(batch.alphabet_size);
                }
                self.analog_samples.clear();
                self.analog_sample_rate_hz = None;
                self.soft_symbols.extend(batch.symbols);
                self.trim_soft_symbols();
            }
            DemodulatedBatch::Analog(batch) => {
                batch.validate().map_err(|error| error.to_string())?;
                if batch.discontinuity {
                    self.discontinuities = self.discontinuities.saturating_add(1);
                }
                if self.analog_sample_rate_hz != Some(batch.sample_rate_hz) {
                    self.analog_samples.clear();
                    self.analog_sample_rate_hz = Some(batch.sample_rate_hz);
                }
                self.soft_symbols.clear();
                self.soft_symbol_alphabet_size = None;
                self.raw_bytes.clear();
                self.raw_synchronized = false;
                self.analog_samples.extend(batch.samples);
                self.trim_analog_samples();
            }
        }
        self.error = None;
        Ok(())
    }

    pub fn ingest_raw(&mut self, output: RawRadioOutput) {
        self.raw_synchronized = output.synchronized;
        self.raw_bytes.extend(output.bytes);
        let excess = self.raw_bytes.len().saturating_sub(self.capacity);
        self.raw_bytes.drain(..excess);
        self.raw_dropped_bytes = self.raw_dropped_bytes.saturating_add(excess as u64);
        self.raw_diagnostics.extend(output.diagnostics);
        let diagnostic_excess = self.raw_diagnostics.len().saturating_sub(32);
        self.raw_diagnostics.drain(..diagnostic_excess);
    }

    pub fn set_error(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
    }

    pub fn soft_symbols(&self) -> &VecDeque<TimedSoftSymbol> {
        &self.soft_symbols
    }

    pub fn soft_symbol_alphabet_size(&self) -> Option<u16> {
        self.soft_symbol_alphabet_size
    }

    pub fn analog_samples(&self) -> &VecDeque<TimedAnalogSample> {
        &self.analog_samples
    }

    pub fn analog_sample_rate_hz(&self) -> Option<u64> {
        self.analog_sample_rate_hz
    }

    pub fn dropped_items(&self) -> u64 {
        self.dropped_items
    }

    pub fn discontinuities(&self) -> u64 {
        self.discontinuities
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn raw_bytes(&self) -> &VecDeque<TimedByte> {
        &self.raw_bytes
    }

    pub fn raw_synchronized(&self) -> bool {
        self.raw_synchronized
    }

    pub fn raw_dropped_bytes(&self) -> u64 {
        self.raw_dropped_bytes
    }

    pub fn raw_diagnostics(&self) -> &VecDeque<DecodeDiagnostic> {
        &self.raw_diagnostics
    }

    fn trim_soft_symbols(&mut self) {
        let excess = self.soft_symbols.len().saturating_sub(self.capacity);
        self.soft_symbols.drain(..excess);
        self.dropped_items = self.dropped_items.saturating_add(excess as u64);
    }

    fn trim_analog_samples(&mut self) {
        let excess = self.analog_samples.len().saturating_sub(self.capacity);
        self.analog_samples.drain(..excess);
        self.dropped_items = self.dropped_items.saturating_add(excess as u64);
    }
}

/// Bounded derived state for the RF spectrum and waterfall views.
#[derive(Debug)]
pub struct RadioDisplayStore {
    newest_spectrum: Option<SpectrumFrame>,
    waterfall: VecDeque<SpectrumFrame>,
    waterfall_capacity: usize,
    dropped_rows: u64,
}

impl Default for RadioDisplayStore {
    fn default() -> Self {
        Self {
            newest_spectrum: None,
            waterfall: VecDeque::with_capacity(DEFAULT_WATERFALL_CAPACITY),
            waterfall_capacity: DEFAULT_WATERFALL_CAPACITY,
            dropped_rows: 0,
        }
    }
}

impl RadioDisplayStore {
    pub fn new(waterfall_capacity: usize) -> Result<Self, String> {
        if waterfall_capacity == 0 {
            return Err("waterfall capacity must be greater than zero".to_owned());
        }
        Ok(Self {
            newest_spectrum: None,
            waterfall: VecDeque::with_capacity(waterfall_capacity),
            waterfall_capacity,
            dropped_rows: 0,
        })
    }

    pub fn ingest(&mut self, frame: SpectrumFrame) {
        if self.newest_spectrum.as_ref().is_some_and(|current| {
            current.center_frequency_hz != frame.center_frequency_hz
                || current.bin_width_hz != frame.bin_width_hz
        }) {
            self.clear_for_retune();
        }
        self.newest_spectrum = Some(frame.clone());
        if self.waterfall.len() == self.waterfall_capacity {
            self.waterfall.pop_front();
            self.dropped_rows = self.dropped_rows.saturating_add(1);
        }
        self.waterfall.push_back(frame);
    }

    pub fn clear_for_retune(&mut self) {
        self.newest_spectrum = None;
        self.waterfall.clear();
    }

    pub fn newest_spectrum(&self) -> Option<&SpectrumFrame> {
        self.newest_spectrum.as_ref()
    }

    pub fn waterfall(&self) -> &VecDeque<SpectrumFrame> {
        &self.waterfall
    }

    pub fn dropped_rows(&self) -> u64 {
        self.dropped_rows
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WaterfallSignature {
    newest_sample: u64,
    row_count: usize,
    dropped_rows: u64,
    center_frequency_hz: u64,
}

#[derive(Default)]
pub struct RadioViewState {
    waterfall_texture: Option<TextureHandle>,
    waterfall_signature: Option<WaterfallSignature>,
}

impl RadioViewState {
    pub fn paint(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        store: &RadioDisplayStore,
        analysis: &RadioAnalysisStore,
        analysis_enabled: bool,
    ) {
        ui.painter()
            .rect_filled(rect, 0.0, Color32::from_rgb(12, 16, 22));
        let gap = 12.0;
        let show_analysis = analysis_enabled
            || analysis.error().is_some()
            || !analysis.soft_symbols().is_empty()
            || !analysis.analog_samples().is_empty()
            || !analysis.raw_bytes().is_empty();
        let analysis_height = if show_analysis {
            rect.height().mul_add(0.24, 0.0).clamp(110.0, 180.0)
        } else {
            0.0
        };
        let content_bottom = if show_analysis {
            (rect.bottom() - analysis_height - gap).max(rect.top())
        } else {
            rect.bottom()
        };
        let upper_height = (content_bottom - rect.top()).max(0.0);
        let spectrum_height = (upper_height * 0.54).max(120.0).min(upper_height);
        let spectrum_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), spectrum_height));
        let waterfall_rect = Rect::from_min_max(
            Pos2::new(
                rect.left(),
                (spectrum_rect.bottom() + gap).min(rect.bottom()),
            ),
            Pos2::new(rect.right(), content_bottom),
        );
        paint_spectrum(ui, spectrum_rect, store.newest_spectrum());
        self.paint_waterfall(ui, waterfall_rect, store);
        if show_analysis {
            let analysis_rect = Rect::from_min_max(
                Pos2::new(rect.left(), (content_bottom + gap).min(rect.bottom())),
                rect.max,
            );
            paint_analysis(ui, analysis_rect, analysis, analysis_enabled);
        }
    }

    fn paint_waterfall(&mut self, ui: &egui::Ui, rect: Rect, store: &RadioDisplayStore) {
        if rect.height() <= 1.0 {
            return;
        }
        ui.painter()
            .rect_filled(rect, 0.0, Color32::from_rgb(7, 10, 16));
        let title_rect = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.top() + 22.0));
        ui.painter().text(
            title_rect.left_center() + Vec2::new(8.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "WATERFALL",
            FontId::proportional(12.0),
            Color32::from_rgb(145, 160, 180),
        );
        ui.painter().text(
            title_rect.right_center() - Vec2::new(8.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            format!(
                "{} rows · {} dropped · ⓘ",
                store.waterfall().len(),
                store.dropped_rows()
            ),
            FontId::proportional(11.0),
            Color32::from_rgb(105, 120, 140),
        );
        let hint_rect = Rect::from_min_max(
            Pos2::new(title_rect.center().x, title_rect.top()),
            title_rect.max,
        );
        ui.interact(
            hint_rect,
            ui.id().with("waterfall_history_hint"),
            Sense::hover(),
        )
        .on_hover_text(WATERFALL_HISTORY_HINT);
        let image_rect = Rect::from_min_max(
            Pos2::new(rect.left() + 8.0, title_rect.bottom()),
            Pos2::new(rect.right() - 8.0, rect.bottom() - 6.0),
        );
        let Some(newest) = store.newest_spectrum() else {
            ui.painter().text(
                image_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Waiting for IQ samples",
                FontId::proportional(14.0),
                Color32::from_rgb(105, 120, 140),
            );
            return;
        };
        let signature = WaterfallSignature {
            newest_sample: newest.source_range.end,
            row_count: store.waterfall().len(),
            dropped_rows: store.dropped_rows(),
            center_frequency_hz: newest.center_frequency_hz,
        };
        if self.waterfall_signature != Some(signature) {
            if let Some(image) = waterfall_image(store.waterfall()) {
                if let Some(texture) = self.waterfall_texture.as_mut() {
                    texture.set(image, egui::TextureOptions::NEAREST);
                } else {
                    self.waterfall_texture = Some(ui.ctx().load_texture(
                        "radio-waterfall",
                        image,
                        egui::TextureOptions::NEAREST,
                    ));
                }
                self.waterfall_signature = Some(signature);
            }
        }
        if let Some(texture) = &self.waterfall_texture {
            ui.painter().image(
                texture.id(),
                image_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }
}

fn paint_analysis(ui: &egui::Ui, rect: Rect, store: &RadioAnalysisStore, analysis_enabled: bool) {
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    ui.painter()
        .rect_filled(rect, 0.0, Color32::from_rgb(9, 13, 19));
    let title_rect = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.top() + 22.0));
    ui.painter().text(
        title_rect.left_center() + Vec2::new(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "FSK RAW DATA",
        FontId::proportional(12.0),
        Color32::from_rgb(145, 160, 180),
    );
    let status = if let Some(error) = store.error() {
        format!("error · {error}")
    } else if store.raw_synchronized() {
        format!(
            "{} bytes · locked · {} dropped · {} gaps",
            store.raw_bytes().len(),
            store.raw_dropped_bytes(),
            store.discontinuities()
        )
    } else if let Some(alphabet_size) = store.soft_symbol_alphabet_size() {
        format!(
            "searching preamble · {} symbols · alphabet {} · {} gaps",
            store.soft_symbols().len(),
            alphabet_size,
            store.discontinuities()
        )
    } else if let Some(sample_rate_hz) = store.analog_sample_rate_hz() {
        format!(
            "{} samples · {} Hz · {} dropped · {} gaps",
            store.analog_samples().len(),
            sample_rate_hz,
            store.dropped_items(),
            store.discontinuities()
        )
    } else if analysis_enabled {
        "waiting for lock or symbols".to_owned()
    } else {
        "disabled".to_owned()
    };
    ui.painter().text(
        title_rect.right_center() - Vec2::new(8.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        status,
        FontId::proportional(10.0),
        if store.error().is_some() {
            Color32::LIGHT_RED
        } else {
            Color32::from_rgb(105, 120, 140)
        },
    );
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 8.0, title_rect.bottom() + 3.0),
        Pos2::new(rect.right() - 8.0, rect.bottom() - 7.0),
    );
    if plot.width() <= 1.0 || plot.height() <= 1.0 || store.error().is_some() {
        return;
    }
    if !store.raw_bytes().is_empty() {
        paint_raw_bytes(ui, plot, store);
    } else if !store.soft_symbols().is_empty() {
        paint_soft_symbols(ui, plot, store);
    } else if !store.analog_samples().is_empty() {
        paint_analog_samples(ui, plot, store);
    } else {
        ui.painter().text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            if analysis_enabled {
                "Waiting for demodulator output"
            } else {
                "Enable Signal analyzer in Capture Setup"
            },
            FontId::proportional(12.0),
            Color32::from_rgb(105, 120, 140),
        );
    }
}

fn paint_raw_bytes(ui: &egui::Ui, plot: Rect, store: &RadioAnalysisStore) {
    const BYTE_CELL_WIDTH: f32 = 25.0;
    const ROW_HEIGHT: f32 = 18.0;
    let columns = (plot.width() / BYTE_CELL_WIDTH).floor().max(1.0) as usize;
    let rows = (plot.height() / ROW_HEIGHT).floor().max(1.0) as usize;
    let visible = columns.saturating_mul(rows);
    let skip = store.raw_bytes().len().saturating_sub(visible);
    for (index, byte) in store.raw_bytes().iter().skip(skip).enumerate() {
        let row = index / columns;
        let column = index % columns;
        ui.painter().text(
            Pos2::new(
                plot.left() + column as f32 * BYTE_CELL_WIDTH,
                plot.top() + row as f32 * ROW_HEIGHT,
            ),
            egui::Align2::LEFT_TOP,
            format!("{:02X}", byte.value),
            FontId::monospace(12.0),
            Color32::from_rgb(150, 220, 255),
        );
    }
    if let Some(diagnostic) = store.raw_diagnostics().back() {
        ui.painter().text(
            plot.right_bottom(),
            egui::Align2::RIGHT_BOTTOM,
            &diagnostic.message,
            FontId::proportional(9.0),
            Color32::from_rgb(230, 180, 95),
        );
    }
}

fn paint_soft_symbols(ui: &egui::Ui, plot: Rect, store: &RadioAnalysisStore) {
    const MAX_VISIBLE_SYMBOLS: usize = 512;
    let skip = store
        .soft_symbols()
        .len()
        .saturating_sub(MAX_VISIBLE_SYMBOLS);
    let symbols = store.soft_symbols().iter().skip(skip).collect::<Vec<_>>();
    let Some(first) = symbols.first() else {
        return;
    };
    let Some(last) = symbols.last() else {
        return;
    };
    let start = first.source_range.start;
    let span = last.source_range.end.saturating_sub(start).max(1) as f64;
    for symbol in symbols {
        let left = plot.left()
            + plot.width()
                * ((symbol.source_range.start.saturating_sub(start) as f64 / span) as f32);
        let right = plot.left()
            + plot.width() * ((symbol.source_range.end.saturating_sub(start) as f64 / span) as f32);
        let symbol_rect = Rect::from_min_max(
            Pos2::new(left, plot.top()),
            Pos2::new(right.max(left + 1.0), plot.bottom()),
        )
        .intersect(plot);
        let decision = symbol.decision().unwrap_or(0);
        let confidence = symbol.confidence().unwrap_or(0.0).clamp(0.0, 1.0);
        let base = if decision % 2 == 0 {
            Color32::from_rgb(70, 135, 230)
        } else {
            Color32::from_rgb(70, 205, 145)
        };
        ui.painter().rect_filled(
            symbol_rect,
            1.0,
            base.gamma_multiply(0.25 + 0.7 * confidence),
        );
        ui.painter().line_segment(
            [symbol_rect.left_top(), symbol_rect.left_bottom()],
            Stroke::new(1.0_f32, Color32::from_rgb(25, 35, 48)),
        );
        if symbol_rect.width() >= 12.0 {
            ui.painter().text(
                symbol_rect.center(),
                egui::Align2::CENTER_CENTER,
                decision.to_string(),
                FontId::monospace(10.0),
                Color32::WHITE,
            );
        }
    }
}

fn paint_analog_samples(ui: &egui::Ui, plot: Rect, store: &RadioAnalysisStore) {
    const MAX_VISIBLE_SAMPLES: usize = 2_048;
    let skip = store
        .analog_samples()
        .len()
        .saturating_sub(MAX_VISIBLE_SAMPLES);
    let samples = store.analog_samples().iter().skip(skip).collect::<Vec<_>>();
    let Some(first) = samples.first() else {
        return;
    };
    let Some(last) = samples.last() else {
        return;
    };
    let start = first.source_range.start;
    let span = last.source_range.end.saturating_sub(start).max(1) as f64;
    let points = samples
        .into_iter()
        .map(|sample| {
            let center = sample.source_range.start.saturating_add(
                sample
                    .source_range
                    .end
                    .saturating_sub(sample.source_range.start)
                    / 2,
            );
            let x =
                plot.left() + plot.width() * ((center.saturating_sub(start) as f64 / span) as f32);
            let y = egui::lerp(plot.y_range(), (1.0 - sample.value.clamp(-1.0, 1.0)) * 0.5);
            Pos2::new(x, y)
        })
        .collect::<Vec<_>>();
    if points.len() > 1 {
        ui.painter().with_clip_rect(plot).add(egui::Shape::line(
            points,
            Stroke::new(1.35_f32, Color32::from_rgb(75, 225, 170)),
        ));
    }
}

fn paint_spectrum(ui: &egui::Ui, rect: Rect, spectrum: Option<&SpectrumFrame>) {
    ui.painter()
        .rect_filled(rect, 0.0, Color32::from_rgb(12, 16, 22));
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 48.0, rect.top() + 10.0),
        Pos2::new(rect.right() - 10.0, rect.bottom() - 24.0),
    );
    if plot.width() <= 1.0 || plot.height() <= 1.0 {
        return;
    }
    for division in 0..=10 {
        let x = egui::lerp(plot.x_range(), division as f32 / 10.0);
        ui.painter().line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.0_f32, Color32::from_rgb(35, 44, 56)),
        );
    }
    ui.painter().line_segment(
        [plot.center_top(), plot.center_bottom()],
        Stroke::new(1.5_f32, Color32::from_rgb(75, 145, 165)),
    );
    for division in 0..=6 {
        let fraction = division as f32 / 6.0;
        let y = egui::lerp(plot.y_range(), fraction);
        ui.painter().line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(1.0_f32, Color32::from_rgb(35, 44, 56)),
        );
        let power = MAX_POWER_DBFS + (MIN_POWER_DBFS - MAX_POWER_DBFS) * fraction;
        ui.painter().text(
            Pos2::new(plot.left() - 6.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("{power:.0}"),
            FontId::monospace(10.0),
            Color32::from_rgb(110, 125, 145),
        );
    }
    ui.painter().text(
        Pos2::new(rect.left() + 8.0, rect.top() + 8.0),
        egui::Align2::LEFT_TOP,
        "dBFS",
        FontId::proportional(11.0),
        Color32::from_rgb(110, 125, 145),
    );

    let Some(spectrum) = spectrum else {
        ui.painter().text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            "Waiting for IQ samples",
            FontId::proportional(14.0),
            Color32::from_rgb(105, 120, 140),
        );
        return;
    };
    if spectrum.power_dbfs.len() < 2 {
        return;
    }
    let points = spectrum
        .power_dbfs
        .iter()
        .enumerate()
        .map(|(index, power)| {
            let fraction = index as f32 / (spectrum.power_dbfs.len() - 1) as f32;
            let x = egui::lerp(plot.x_range(), fraction);
            let normalized =
                ((*power - MIN_POWER_DBFS) / (MAX_POWER_DBFS - MIN_POWER_DBFS)).clamp(0.0, 1.0);
            let y = egui::lerp(plot.y_range(), 1.0 - normalized);
            Pos2::new(x, y)
        })
        .collect::<Vec<_>>();
    ui.painter().with_clip_rect(plot).add(egui::Shape::line(
        points,
        Stroke::new(1.35_f32, Color32::from_rgb(75, 225, 170)),
    ));

    let span_hz = spectrum.bin_width_hz * spectrum.power_dbfs.len() as f64;
    let low_hz = spectrum.center_frequency_hz as f64 - span_hz / 2.0;
    for division in 0_u8..=10 {
        let fraction = f32::from(division) / 10.0;
        let x = egui::lerp(plot.x_range(), fraction);
        let frequency = low_hz + span_hz * f64::from(division) / 10.0;
        let alignment = match division {
            0 => egui::Align2::LEFT_TOP,
            10 => egui::Align2::RIGHT_TOP,
            _ => egui::Align2::CENTER_TOP,
        };
        ui.painter().text(
            Pos2::new(x, plot.bottom() + 5.0),
            alignment,
            format_frequency(frequency),
            FontId::monospace(10.0),
            Color32::from_rgb(120, 140, 160),
        );
    }

    let response = ui.interact(plot, ui.id().with("rf_spectrum_hover"), Sense::hover());
    if let Some(pointer) = response.hover_pos() {
        let fraction = ((pointer.x - plot.left()) / plot.width()).clamp(0.0, 1.0);
        if let Some((bin, frequency_hz, power_dbfs)) = spectrum_value_at(spectrum, fraction) {
            let x = egui::lerp(
                plot.x_range(),
                bin as f32 / (spectrum.power_dbfs.len() - 1) as f32,
            );
            let normalized =
                ((power_dbfs - MIN_POWER_DBFS) / (MAX_POWER_DBFS - MIN_POWER_DBFS)).clamp(0.0, 1.0);
            let point = Pos2::new(x, egui::lerp(plot.y_range(), 1.0 - normalized));
            let painter = ui.painter().with_clip_rect(plot);
            painter.line_segment(
                [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                Stroke::new(1.0_f32, Color32::from_rgb(120, 190, 220)),
            );
            painter.line_segment(
                [
                    Pos2::new(plot.left(), point.y),
                    Pos2::new(plot.right(), point.y),
                ],
                Stroke::new(0.75_f32, Color32::from_rgb(85, 115, 135)),
            );
            painter.circle_filled(point, 3.5, Color32::WHITE);
            let screen = ui.ctx().input(|input| input.screen_rect());
            let label_rect = spectrum_hover_label_rect(pointer, screen);
            let label_painter = ui.painter();
            label_painter.rect_filled(label_rect, 4.0, Color32::from_rgb(24, 31, 42));
            label_painter.rect_stroke(
                label_rect,
                4.0,
                Stroke::new(1.0_f32, Color32::from_rgb(70, 105, 125)),
            );
            label_painter.text(
                label_rect.left_top() + Vec2::new(8.0, 5.0),
                egui::Align2::LEFT_TOP,
                format_frequency(frequency_hz),
                FontId::monospace(11.0),
                Color32::WHITE,
            );
            label_painter.text(
                label_rect.left_bottom() + Vec2::new(8.0, -5.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{power_dbfs:.1} dBFS"),
                FontId::monospace(11.0),
                Color32::from_rgb(150, 220, 255),
            );
        }
    }
}

fn spectrum_hover_label_rect(pointer: Pos2, bounds: Rect) -> Rect {
    const GAP: f32 = 12.0;
    let size = Vec2::new(150.0, 40.0);
    let mut min = Pos2::new(pointer.x + GAP, pointer.y - GAP - size.y);
    if min.x + size.x > bounds.right() {
        min.x = pointer.x - GAP - size.x;
    }
    if min.y < bounds.top() {
        min.y = pointer.y + GAP;
    }
    min.x = min
        .x
        .clamp(bounds.left(), (bounds.right() - size.x).max(bounds.left()));
    min.y = min
        .y
        .clamp(bounds.top(), (bounds.bottom() - size.y).max(bounds.top()));
    Rect::from_min_size(min, size)
}

fn spectrum_value_at(spectrum: &SpectrumFrame, fraction: f32) -> Option<(usize, f64, f32)> {
    let last_bin = spectrum.power_dbfs.len().checked_sub(1)?;
    let bin = (fraction.clamp(0.0, 1.0) * last_bin as f32).round() as usize;
    let span_hz = spectrum.bin_width_hz * spectrum.power_dbfs.len() as f64;
    let low_hz = spectrum.center_frequency_hz as f64 - span_hz / 2.0;
    Some((
        bin,
        low_hz + bin as f64 * spectrum.bin_width_hz,
        spectrum.power_dbfs[bin],
    ))
}

fn waterfall_image(frames: &VecDeque<SpectrumFrame>) -> Option<egui::ColorImage> {
    let bin_count = frames.front()?.power_dbfs.len();
    if bin_count == 0
        || frames
            .iter()
            .any(|frame| frame.power_dbfs.len() != bin_count)
    {
        return None;
    }
    let width = bin_count.min(MAX_WATERFALL_TEXTURE_WIDTH);
    let height = frames.len();
    let mut image = egui::ColorImage::new([width, height], Color32::BLACK);
    // New rows enter at the top; older history moves down the image.
    for (row, frame) in frames.iter().rev().enumerate() {
        for column in 0..width {
            let start = column * bin_count / width;
            let end = ((column + 1) * bin_count / width).max(start + 1);
            let power = frame.power_dbfs[start..end]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            image[(column, row)] = waterfall_color(power);
        }
    }
    Some(image)
}

fn waterfall_color(power_dbfs: f32) -> Color32 {
    let value = ((power_dbfs - MIN_POWER_DBFS) / (MAX_POWER_DBFS - MIN_POWER_DBFS)).clamp(0.0, 1.0);
    let stops = [
        (0.0, [3, 6, 16]),
        (0.22, [14, 35, 91]),
        (0.46, [0, 155, 190]),
        (0.70, [235, 215, 55]),
        (1.0, [255, 65, 32]),
    ];
    let (left, right) = stops
        .windows(2)
        .find_map(|window| (value <= window[1].0).then_some((window[0], window[1])))
        .unwrap_or((stops[stops.len() - 2], stops[stops.len() - 1]));
    let amount = ((value - left.0) / (right.0 - left.0)).clamp(0.0, 1.0);
    let channel = |index| {
        (left.1[index] as f32 + (right.1[index] as f32 - left.1[index] as f32) * amount).round()
            as u8
    };
    Color32::from_rgb(channel(0), channel(1), channel(2))
}

fn format_frequency(frequency_hz: f64) -> String {
    if frequency_hz.abs() >= 1.0e9 {
        format_compact_decimal(frequency_hz / 1.0e9, 4, "GHz")
    } else if frequency_hz.abs() >= 1.0e6 {
        format_compact_decimal(frequency_hz / 1.0e6, 3, "MHz")
    } else if frequency_hz.abs() >= 1.0e3 {
        format_compact_decimal(frequency_hz / 1.0e3, 2, "kHz")
    } else {
        format!("{frequency_hz:.0} Hz")
    }
}

fn format_compact_decimal(value: f64, precision: usize, unit: &str) -> String {
    let value = format!("{value:.precision$}");
    let value = value.trim_end_matches('0').trim_end_matches('.');
    format!("{value} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_decode::{ByteStatus, SampleRange};
    use wavecraft_dsp::{TimedAnalogBatch, TimedSoftSymbolBatch};

    fn frame(index: u64, frequency: u64) -> SpectrumFrame {
        SpectrumFrame {
            source_range: index..index + 8,
            center_frequency_hz: frequency,
            bin_width_hz: 1_000.0,
            power_dbfs: vec![-80.0; 8],
        }
    }

    #[test]
    fn waterfall_keeps_only_the_newest_bounded_rows() {
        let mut store = RadioDisplayStore::new(2).unwrap();
        store.ingest(frame(0, 100_000_000));
        store.ingest(frame(8, 100_000_000));
        store.ingest(frame(16, 100_000_000));

        assert_eq!(store.waterfall().len(), 2);
        assert_eq!(store.waterfall()[0].source_range, 8..16);
        assert_eq!(store.newest_spectrum().unwrap().source_range, 16..24);
        assert_eq!(store.dropped_rows(), 1);
    }

    #[test]
    fn retune_clears_rows_that_no_longer_share_a_frequency_axis() {
        let mut store = RadioDisplayStore::new(4).unwrap();
        store.ingest(frame(0, 100_000_000));
        store.ingest(frame(8, 101_000_000));

        assert_eq!(store.waterfall().len(), 1);
        assert_eq!(
            store.newest_spectrum().unwrap().center_frequency_hz,
            101_000_000
        );
    }

    #[test]
    fn waterfall_image_downsamples_wide_spectra_and_preserves_rows() {
        let frames = VecDeque::from([
            SpectrumFrame {
                source_range: 0..1_024,
                center_frequency_hz: 100_000_000,
                bin_width_hz: 1_000.0,
                power_dbfs: vec![-100.0; 1_024],
            },
            SpectrumFrame {
                source_range: 1_024..2_048,
                center_frequency_hz: 100_000_000,
                bin_width_hz: 1_000.0,
                power_dbfs: vec![-40.0; 1_024],
            },
        ]);

        let image = waterfall_image(&frames).unwrap();

        assert_eq!(image.size, [MAX_WATERFALL_TEXTURE_WIDTH, 2]);
        assert_eq!(image[(0, 0)], waterfall_color(-40.0));
        assert_eq!(image[(0, 1)], waterfall_color(-100.0));
    }

    #[test]
    fn waterfall_color_clamps_outside_the_display_range() {
        assert_eq!(waterfall_color(-200.0), Color32::from_rgb(3, 6, 16));
        assert_eq!(waterfall_color(20.0), Color32::from_rgb(255, 65, 32));
    }

    #[test]
    fn spectrum_frequency_labels_trim_redundant_zeroes() {
        assert_eq!(format_frequency(100_000_000.0), "100 MHz");
        assert_eq!(format_frequency(100_500_000.0), "100.5 MHz");
        assert_eq!(format_frequency(1_250_000_000.0), "1.25 GHz");
        assert_eq!(format_frequency(12_340.0), "12.34 kHz");
    }

    #[test]
    fn spectrum_hover_uses_the_nearest_plotted_bin() {
        let spectrum = SpectrumFrame {
            source_range: 0..4,
            center_frequency_hz: 868_000_000,
            bin_width_hz: 1_000.0,
            power_dbfs: vec![-80.0, -70.0, -60.0, -50.0],
        };

        assert_eq!(
            spectrum_value_at(&spectrum, 0.0),
            Some((0, 867_998_000.0, -80.0))
        );
        assert_eq!(
            spectrum_value_at(&spectrum, 0.5),
            Some((2, 868_000_000.0, -60.0))
        );
        assert_eq!(
            spectrum_value_at(&spectrum, 1.0),
            Some((3, 868_001_000.0, -50.0))
        );
    }

    #[test]
    fn spectrum_hover_label_prefers_right_and_above_the_pointer() {
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let label = spectrum_hover_label_rect(Pos2::new(300.0, 300.0), bounds);

        assert_eq!(label.left(), 312.0);
        assert_eq!(label.bottom(), 288.0);
    }

    #[test]
    fn analysis_store_bounds_soft_symbols_and_preserves_diagnostics() {
        let mut store = RadioAnalysisStore::new(2).unwrap();
        store
            .ingest(DemodulatedBatch::SoftSymbols(TimedSoftSymbolBatch {
                alphabet_size: 2,
                discontinuity: true,
                symbols: (0..3)
                    .map(|index| TimedSoftSymbol {
                        likelihoods: vec![0.25, 0.75],
                        source_range: index..index + 1,
                    })
                    .collect(),
            }))
            .unwrap();

        assert_eq!(store.soft_symbols().len(), 2);
        assert_eq!(store.soft_symbols()[0].source_range, 1..2);
        assert_eq!(store.soft_symbol_alphabet_size(), Some(2));
        assert_eq!(store.dropped_items(), 1);
        assert_eq!(store.discontinuities(), 1);
    }

    #[test]
    fn analysis_store_switches_cleanly_between_output_families() {
        let mut store = RadioAnalysisStore::new(4).unwrap();
        store
            .ingest(DemodulatedBatch::SoftSymbols(TimedSoftSymbolBatch {
                alphabet_size: 2,
                discontinuity: false,
                symbols: vec![TimedSoftSymbol {
                    likelihoods: vec![0.75, 0.25],
                    source_range: 0..1,
                }],
            }))
            .unwrap();
        store
            .ingest(DemodulatedBatch::Analog(TimedAnalogBatch {
                sample_rate_hz: 48_000,
                discontinuity: false,
                samples: vec![TimedAnalogSample {
                    value: 0.25,
                    source_range: 1..2,
                }],
            }))
            .unwrap();

        assert!(store.soft_symbols().is_empty());
        assert_eq!(store.soft_symbol_alphabet_size(), None);
        assert_eq!(store.analog_samples().len(), 1);
        assert_eq!(store.analog_sample_rate_hz(), Some(48_000));
    }

    #[test]
    fn analysis_store_bounds_synchronized_raw_bytes() {
        let mut store = RadioAnalysisStore::new(2).unwrap();
        store.ingest_raw(RawRadioOutput {
            bytes: [0xaa, 0xaa, 0x42]
                .into_iter()
                .enumerate()
                .map(|(index, value)| TimedByte {
                    value,
                    range: SampleRange {
                        start: index as u64 * 8,
                        end: index as u64 * 8 + 8,
                    },
                    status: ByteStatus::default(),
                })
                .collect(),
            diagnostics: vec![],
            synchronized: true,
        });

        assert!(store.raw_synchronized());
        assert_eq!(
            store
                .raw_bytes()
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xaa, 0x42]
        );
        assert_eq!(store.raw_dropped_bytes(), 1);
    }

    #[test]
    fn radio_view_renders_soft_symbol_analysis_lane() {
        let context = egui::Context::default();
        let mut display = RadioDisplayStore::new(4).unwrap();
        display.ingest(frame(0, 100_000_000));
        let mut analysis = RadioAnalysisStore::new(4).unwrap();
        analysis
            .ingest(DemodulatedBatch::SoftSymbols(TimedSoftSymbolBatch {
                alphabet_size: 2,
                discontinuity: false,
                symbols: vec![
                    TimedSoftSymbol {
                        likelihoods: vec![0.9, 0.1],
                        source_range: 0..8,
                    },
                    TimedSoftSymbol {
                        likelihoods: vec![0.1, 0.9],
                        source_range: 8..16,
                    },
                ],
            }))
            .unwrap();
        let mut view = RadioViewState::default();

        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                view.paint(
                    ui,
                    ui.available_rect_before_wrap(),
                    &display,
                    &analysis,
                    true,
                );
            });
        });

        assert!(!output.shapes.is_empty());
    }
}
