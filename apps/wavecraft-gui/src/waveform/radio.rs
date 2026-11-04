use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};
use std::collections::VecDeque;
use wavecraft_decode::{DecodeDiagnostic, RawRadioMessageStart, RawRadioOutput, TimedByte};
use wavecraft_dsp::{DemodulatedBatch, SpectrumFrame, TimedAnalogSample, TimedSoftSymbol};

const DEFAULT_WATERFALL_CAPACITY: usize = 256;
const DEFAULT_ANALYSIS_CAPACITY: usize = 100_000;
const MAX_RAW_MESSAGE_COUNT: usize = 256;
const MAX_WATERFALL_TEXTURE_WIDTH: usize = 512;
const MIN_POWER_DBFS: f32 = -120.0;
const MAX_POWER_DBFS: f32 = 0.0;
const RADIO_PANE_DIVIDER_HEIGHT: f32 = 12.0;
const MIN_SPECTRUM_HEIGHT: f32 = 120.0;
const MIN_WATERFALL_HEIGHT: f32 = 80.0;
const MIN_RAW_DATA_HEIGHT: f32 = 80.0;
const WATERFALL_HISTORY_HINT: &str = "Rows are the newest spectrum snapshots retained for the visible waterfall. Dropped counts older display rows discarded after this bounded history filled; it does not indicate lost HackRF or IQ samples.";

#[derive(Clone, Debug, PartialEq)]
pub struct RawRadioMessage {
    sequence: u64,
    preamble_bytes: usize,
    bytes: Vec<TimedByte>,
    complete: bool,
}

impl RawRadioMessage {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn preamble_bytes(&self) -> usize {
        self.preamble_bytes.min(self.bytes.len())
    }

    pub fn bytes(&self) -> &[TimedByte] {
        &self.bytes
    }

    pub fn data_bytes(&self) -> &[TimedByte] {
        &self.bytes[self.preamble_bytes()..]
    }

    pub fn data_len(&self) -> usize {
        self.data_bytes().len()
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }
}

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
    raw_messages: VecDeque<RawRadioMessage>,
    next_raw_message_sequence: u64,
    raw_dropped_messages: u64,
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
            raw_messages: VecDeque::new(),
            next_raw_message_sequence: 1,
            raw_dropped_messages: 0,
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
                    self.finish_raw_message();
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
                self.raw_messages.clear();
                self.raw_synchronized = false;
                self.analog_samples.extend(batch.samples);
                self.trim_analog_samples();
            }
        }
        self.error = None;
        Ok(())
    }

    pub fn ingest_raw(&mut self, output: RawRadioOutput) {
        let RawRadioOutput {
            bytes,
            message_starts,
            diagnostics,
            synchronized,
        } = output;
        self.raw_synchronized = synchronized;
        let mut message_starts = message_starts.into_iter().peekable();
        for byte in bytes.iter().copied() {
            while message_starts
                .peek()
                .is_some_and(|start| start.sample_index <= byte.range.start)
            {
                let Some(start) = message_starts.next() else {
                    break;
                };
                self.start_raw_message(start);
            }
            if self.raw_messages.is_empty() && synchronized {
                self.start_raw_message(RawRadioMessageStart {
                    sample_index: byte.range.start,
                    preamble_bytes: 0,
                });
            }
            if let Some(message) = self.raw_messages.back_mut() {
                message.bytes.push(byte);
                let excess = message.bytes.len().saturating_sub(self.capacity);
                message.bytes.drain(..excess);
            }
        }
        for start in message_starts {
            self.start_raw_message(start);
        }
        self.raw_bytes.extend(bytes);
        let excess = self.raw_bytes.len().saturating_sub(self.capacity);
        self.raw_bytes.drain(..excess);
        self.raw_dropped_bytes = self.raw_dropped_bytes.saturating_add(excess as u64);
        self.raw_diagnostics.extend(diagnostics);
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

    pub fn raw_messages(&self) -> &VecDeque<RawRadioMessage> {
        &self.raw_messages
    }

    pub fn completed_raw_message_count(&self) -> usize {
        self.raw_messages
            .iter()
            .filter(|message| message.is_complete())
            .count()
    }

    pub fn raw_message_in_progress(&self) -> bool {
        self.raw_messages
            .back()
            .is_some_and(|message| !message.is_complete())
    }

    pub fn raw_dropped_messages(&self) -> u64 {
        self.raw_dropped_messages
    }

    pub fn raw_dropped_bytes(&self) -> u64 {
        self.raw_dropped_bytes
    }

    pub fn raw_diagnostics(&self) -> &VecDeque<DecodeDiagnostic> {
        &self.raw_diagnostics
    }

    pub fn finish_raw_capture(&mut self) {
        self.finish_raw_message();
        self.raw_synchronized = false;
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

    fn start_raw_message(&mut self, start: RawRadioMessageStart) {
        let mut carried_bytes = Vec::new();
        if let Some(current) = self.raw_messages.back_mut() {
            current.complete = true;
            if let Some(split) = current
                .bytes
                .iter()
                .position(|byte| byte.range.start >= start.sample_index)
            {
                carried_bytes = current.bytes.split_off(split);
            }
        }
        self.raw_messages.push_back(RawRadioMessage {
            sequence: self.next_raw_message_sequence,
            preamble_bytes: start.preamble_bytes,
            bytes: carried_bytes,
            complete: false,
        });
        self.next_raw_message_sequence = self.next_raw_message_sequence.saturating_add(1);
        let message_capacity = self.capacity.min(MAX_RAW_MESSAGE_COUNT);
        while self.raw_messages.len() > message_capacity {
            self.raw_messages.pop_front();
            self.raw_dropped_messages = self.raw_dropped_messages.saturating_add(1);
        }
    }

    fn finish_raw_message(&mut self) {
        if let Some(message) = self.raw_messages.back_mut() {
            message.complete = true;
        }
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

pub struct RadioViewState {
    waterfall_texture: Option<TextureHandle>,
    waterfall_signature: Option<WaterfallSignature>,
    raw_message_scroll_rows: usize,
    spectrum_height_ratio: f32,
    raw_data_height_ratio: f32,
}

impl Default for RadioViewState {
    fn default() -> Self {
        Self {
            waterfall_texture: None,
            waterfall_signature: None,
            raw_message_scroll_rows: 0,
            spectrum_height_ratio: 0.54,
            raw_data_height_ratio: 0.24,
        }
    }
}

pub struct RadioViewOptions<'a> {
    pub analysis_enabled: bool,
    pub trigger_level_dbfs_tenths: &'a mut i16,
    pub trigger_controls_enabled: bool,
}

impl RadioViewState {
    pub fn paint(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        store: &RadioDisplayStore,
        analysis: &RadioAnalysisStore,
        options: RadioViewOptions<'_>,
    ) {
        let RadioViewOptions {
            analysis_enabled,
            trigger_level_dbfs_tenths,
            trigger_controls_enabled,
        } = options;
        ui.painter()
            .rect_filled(rect, 0.0, Color32::from_rgb(12, 16, 22));
        let show_analysis = analysis_enabled
            || analysis.error().is_some()
            || !analysis.soft_symbols().is_empty()
            || !analysis.analog_samples().is_empty()
            || !analysis.raw_messages().is_empty();
        let (spectrum_height, waterfall_height, analysis_height) = radio_pane_heights(
            rect.height(),
            show_analysis,
            self.spectrum_height_ratio,
            self.raw_data_height_ratio,
        );
        let spectrum_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), spectrum_height));
        let spectrum_divider_rect = Rect::from_min_size(
            Pos2::new(rect.left(), spectrum_rect.bottom()),
            Vec2::new(rect.width(), RADIO_PANE_DIVIDER_HEIGHT),
        );
        let waterfall_rect = Rect::from_min_max(
            Pos2::new(
                rect.left(),
                spectrum_divider_rect.bottom().min(rect.bottom()),
            ),
            Pos2::new(
                rect.right(),
                (spectrum_divider_rect.bottom() + waterfall_height).min(rect.bottom()),
            ),
        );
        paint_spectrum(
            ui,
            spectrum_rect,
            store.newest_spectrum(),
            trigger_level_dbfs_tenths,
            trigger_controls_enabled,
        );
        self.paint_waterfall(ui, waterfall_rect, store);
        if let Some(delta) = paint_radio_pane_divider(
            ui,
            spectrum_divider_rect,
            "radio_spectrum_waterfall_divider",
        ) {
            let upper_height = spectrum_height + waterfall_height;
            if upper_height > 0.0 {
                self.spectrum_height_ratio =
                    ((spectrum_height + delta) / upper_height).clamp(0.0, 1.0);
            }
        }
        if show_analysis {
            let analysis_divider_rect = Rect::from_min_size(
                Pos2::new(rect.left(), waterfall_rect.bottom()),
                Vec2::new(rect.width(), RADIO_PANE_DIVIDER_HEIGHT),
            );
            let analysis_rect = Rect::from_min_max(
                Pos2::new(
                    rect.left(),
                    analysis_divider_rect.bottom().min(rect.bottom()),
                ),
                rect.max,
            );
            if let Some(delta) = paint_radio_pane_divider(
                ui,
                analysis_divider_rect,
                "radio_waterfall_raw_data_divider",
            ) {
                let usable_height = spectrum_height + waterfall_height + analysis_height;
                if usable_height > 0.0 {
                    self.raw_data_height_ratio =
                        ((analysis_height - delta) / usable_height).clamp(0.0, 1.0);
                }
            }
            paint_analysis(
                ui,
                analysis_rect,
                analysis,
                analysis_enabled,
                &mut self.raw_message_scroll_rows,
            );
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

fn radio_pane_heights(
    total_height: f32,
    show_raw_data: bool,
    spectrum_ratio: f32,
    raw_data_ratio: f32,
) -> (f32, f32, f32) {
    let divider_count = if show_raw_data { 2.0 } else { 1.0 };
    let usable_height = (total_height - divider_count * RADIO_PANE_DIVIDER_HEIGHT).max(0.0);
    if !show_raw_data {
        let minimum_scale = (usable_height / (MIN_SPECTRUM_HEIGHT + MIN_WATERFALL_HEIGHT)).min(1.0);
        let minimum_spectrum = MIN_SPECTRUM_HEIGHT * minimum_scale;
        let minimum_waterfall = MIN_WATERFALL_HEIGHT * minimum_scale;
        let spectrum = (usable_height * spectrum_ratio.clamp(0.0, 1.0)).clamp(
            minimum_spectrum,
            (usable_height - minimum_waterfall).max(minimum_spectrum),
        );
        return (spectrum, usable_height - spectrum, 0.0);
    }

    let minimum_total = MIN_SPECTRUM_HEIGHT + MIN_WATERFALL_HEIGHT + MIN_RAW_DATA_HEIGHT;
    let minimum_scale = (usable_height / minimum_total).min(1.0);
    let minimum_spectrum = MIN_SPECTRUM_HEIGHT * minimum_scale;
    let minimum_waterfall = MIN_WATERFALL_HEIGHT * minimum_scale;
    let minimum_raw_data = MIN_RAW_DATA_HEIGHT * minimum_scale;
    let raw_data = (usable_height * raw_data_ratio.clamp(0.0, 1.0)).clamp(
        minimum_raw_data,
        (usable_height - minimum_spectrum - minimum_waterfall).max(minimum_raw_data),
    );
    let upper_height = usable_height - raw_data;
    let spectrum = (upper_height * spectrum_ratio.clamp(0.0, 1.0)).clamp(
        minimum_spectrum,
        (upper_height - minimum_waterfall).max(minimum_spectrum),
    );
    (spectrum, upper_height - spectrum, raw_data)
}

fn paint_radio_pane_divider(ui: &egui::Ui, rect: Rect, id: &'static str) -> Option<f32> {
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return None;
    }
    let response = ui
        .interact(rect, ui.id().with(id), Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    let color = if response.dragged() || response.hovered() {
        Color32::from_rgb(90, 110, 135)
    } else {
        Color32::from_rgb(42, 55, 72)
    };
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(if response.hovered() { 2.0_f32 } else { 1.0_f32 }, color),
    );
    response
        .dragged()
        .then(|| ui.input(|input| input.pointer.delta().y))
}

fn paint_analysis(
    ui: &egui::Ui,
    rect: Rect,
    store: &RadioAnalysisStore,
    analysis_enabled: bool,
    raw_message_scroll_rows: &mut usize,
) {
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    ui.painter()
        .rect_filled(rect, 0.0, Color32::from_rgb(9, 13, 19));
    let title_rect = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.top() + 22.0));
    ui.painter().text(
        title_rect.left_center() + Vec2::new(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "RAW DATA",
        FontId::proportional(12.0),
        Color32::from_rgb(145, 160, 180),
    );
    let status = if let Some(error) = store.error() {
        format!("error · {error}")
    } else if !store.raw_messages().is_empty() {
        let completed_messages = store.completed_raw_message_count();
        let newest_completed_data_bytes = store
            .raw_messages()
            .iter()
            .rev()
            .find(|message| message.is_complete())
            .map_or(0, RawRadioMessage::data_len);
        let receiving = if store.raw_message_in_progress() {
            " · receiving"
        } else {
            ""
        };
        if completed_messages == 0 {
            format!(
                "0 messages{receiving} · {} dropped · {} gaps",
                store.raw_dropped_messages(),
                store.discontinuities()
            )
        } else {
            format!(
                "{completed_messages} messages · newest data {newest_completed_data_bytes} B{receiving} · {} dropped · {} gaps",
                store.raw_dropped_messages(),
                store.discontinuities()
            )
        }
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
    if !store.raw_messages().is_empty() {
        paint_raw_messages(ui, plot, store, raw_message_scroll_rows);
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

fn paint_raw_messages(
    ui: &egui::Ui,
    plot: Rect,
    store: &RadioAnalysisStore,
    scroll_rows: &mut usize,
) {
    const ROW_HEIGHT: f32 = 22.0;
    const BYTE_CELL_WIDTH: f32 = 24.0;
    const SEQUENCE_WIDTH: f32 = 48.0;
    const SECTION_WIDTH: f32 = 38.0;
    const LENGTH_WIDTH: f32 = 76.0;
    const SCROLLBAR_WIDTH: f32 = 7.0;
    const SCROLLBAR_GAP: f32 = 5.0;
    let visible_rows = (plot.height() / ROW_HEIGHT).floor().max(1.0) as usize;
    let completed_messages = store.completed_raw_message_count();
    let maximum_scroll = completed_messages.saturating_sub(visible_rows);
    *scroll_rows = (*scroll_rows).min(maximum_scroll);
    let list_response = ui.interact(
        plot,
        ui.id().with("raw_message_list_scroll"),
        Sense::hover(),
    );
    if list_response.hovered() {
        let scroll_delta = ui.input(|input| input.raw_scroll_delta.y);
        if scroll_delta < 0.0 {
            *scroll_rows = scroll_rows.saturating_add(1).min(maximum_scroll);
        } else if scroll_delta > 0.0 {
            *scroll_rows = scroll_rows.saturating_sub(1);
        }
    }
    let show_scrollbar = maximum_scroll > 0;
    let content_right = if show_scrollbar {
        plot.right() - SCROLLBAR_WIDTH - SCROLLBAR_GAP
    } else {
        plot.right()
    };
    for (row, message) in store
        .raw_messages()
        .iter()
        .rev()
        .filter(|message| message.is_complete())
        .skip(*scroll_rows)
        .take(visible_rows)
        .enumerate()
    {
        let row_rect = Rect::from_min_max(
            Pos2::new(plot.left(), plot.top() + row as f32 * ROW_HEIGHT),
            Pos2::new(
                content_right,
                (plot.top() + (row + 1) as f32 * ROW_HEIGHT).min(plot.bottom()),
            ),
        );
        ui.painter().rect_filled(
            row_rect,
            2.0,
            if row % 2 == 0 {
                Color32::from_rgb(15, 22, 31)
            } else {
                Color32::from_rgb(11, 17, 25)
            },
        );
        ui.painter().text(
            row_rect.left_center() + Vec2::new(5.0, 0.0),
            egui::Align2::LEFT_CENTER,
            format!("#{}", message.sequence()),
            FontId::monospace(10.0),
            Color32::from_rgb(105, 120, 140),
        );

        let mut x = row_rect.left() + SEQUENCE_WIDTH;
        let right_limit = row_rect.right() - LENGTH_WIDTH;
        let preamble_bytes = message.preamble_bytes();
        paint_raw_section_label(ui, row_rect, x, "PRE", Color32::from_rgb(235, 180, 80));
        x += SECTION_WIDTH;
        x = paint_raw_message_bytes(
            ui,
            row_rect,
            x,
            right_limit,
            &message.bytes()[..preamble_bytes],
            BYTE_CELL_WIDTH,
            Color32::from_rgb(245, 190, 90),
        );
        if x + SECTION_WIDTH < right_limit {
            paint_raw_section_label(ui, row_rect, x, "DATA", Color32::from_rgb(105, 150, 175));
            x += SECTION_WIDTH;
            let _ = paint_raw_message_bytes(
                ui,
                row_rect,
                x,
                right_limit,
                message.data_bytes(),
                BYTE_CELL_WIDTH,
                Color32::from_rgb(150, 220, 255),
            );
        }
        ui.painter().text(
            row_rect.right_center() - Vec2::new(5.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            format!("LEN {} B", message.data_len()),
            FontId::monospace(10.0),
            Color32::from_rgb(105, 150, 175),
        );
        let preamble = format_raw_hex(&message.bytes()[..preamble_bytes]);
        let data = format_raw_hex(message.data_bytes());
        ui.interact(
            row_rect,
            ui.id().with(("raw_message", message.sequence())),
            Sense::hover(),
        )
        .on_hover_ui(|ui| {
            ui.label(
                egui::RichText::new(format!("PRE  {preamble}"))
                    .monospace()
                    .color(Color32::from_rgb(245, 190, 90)),
            );
            ui.label(
                egui::RichText::new(format!("DATA ({} B) {data}", message.data_len()))
                    .monospace()
                    .color(Color32::from_rgb(150, 220, 255)),
            );
        });
    }
    if show_scrollbar {
        let track = Rect::from_min_max(
            Pos2::new(plot.right() - SCROLLBAR_WIDTH, plot.top()),
            plot.right_bottom(),
        );
        let thumb_height = (track.height() * visible_rows as f32 / completed_messages as f32)
            .clamp(20.0_f32.min(track.height()), track.height());
        let travel = (track.height() - thumb_height).max(0.0);
        let thumb_top = track.top()
            + travel * (*scroll_rows as f32 / maximum_scroll.max(1) as f32).clamp(0.0, 1.0);
        let thumb = Rect::from_min_size(
            Pos2::new(track.left(), thumb_top),
            Vec2::new(track.width(), thumb_height),
        );
        let response = ui
            .interact(
                track,
                ui.id().with("raw_message_scrollbar"),
                Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeVertical);
        if response.clicked() || response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos().filter(|_| travel > 0.0) {
                let fraction =
                    ((pointer.y - track.top() - thumb_height / 2.0) / travel).clamp(0.0, 1.0);
                *scroll_rows = (fraction * maximum_scroll as f32).round() as usize;
            }
        }
        ui.painter()
            .rect_filled(track, 3.5, Color32::from_rgb(18, 26, 36));
        ui.painter().rect_filled(
            thumb,
            3.5,
            if response.hovered() || response.dragged() {
                Color32::from_rgb(105, 130, 160)
            } else {
                Color32::from_rgb(65, 82, 105)
            },
        );
    }
    if let Some(diagnostic) = store.raw_diagnostics().back() {
        ui.painter().text(
            Pos2::new(content_right, plot.bottom()),
            egui::Align2::RIGHT_BOTTOM,
            &diagnostic.message,
            FontId::proportional(9.0),
            Color32::from_rgb(230, 180, 95),
        );
    } else if completed_messages == 0 && store.raw_message_in_progress() {
        ui.painter().text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            "Receiving raw bytes; the next preamble or capture stop closes the message",
            FontId::proportional(11.0),
            Color32::from_rgb(105, 120, 140),
        );
    }
}

fn format_raw_hex(bytes: &[TimedByte]) -> String {
    const TOOLTIP_BYTE_LIMIT: usize = 512;
    let mut text = bytes
        .iter()
        .take(TOOLTIP_BYTE_LIMIT)
        .map(|byte| format!("{:02X}", byte.value))
        .collect::<Vec<_>>()
        .join(" ");
    if bytes.len() > TOOLTIP_BYTE_LIMIT {
        text.push_str(&format!(" … (+{} B)", bytes.len() - TOOLTIP_BYTE_LIMIT));
    }
    if text.is_empty() {
        text.push('—');
    }
    text
}

fn paint_raw_section_label(ui: &egui::Ui, row: Rect, x: f32, label: &str, color: Color32) {
    ui.painter().text(
        Pos2::new(x, row.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::monospace(9.0),
        color,
    );
}

fn paint_raw_message_bytes(
    ui: &egui::Ui,
    row: Rect,
    mut x: f32,
    right_limit: f32,
    bytes: &[TimedByte],
    cell_width: f32,
    color: Color32,
) -> f32 {
    let visible = ((right_limit - x) / cell_width).floor().max(0.0) as usize;
    for byte in bytes.iter().take(visible) {
        ui.painter().text(
            Pos2::new(x, row.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{:02X}", byte.value),
            FontId::monospace(11.0),
            color,
        );
        x += cell_width;
    }
    if bytes.len() > visible && x < right_limit {
        ui.painter().text(
            Pos2::new(x, row.center().y),
            egui::Align2::LEFT_CENTER,
            "…",
            FontId::monospace(11.0),
            color,
        );
        x += cell_width;
    }
    x
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

fn paint_spectrum(
    ui: &egui::Ui,
    rect: Rect,
    spectrum: Option<&SpectrumFrame>,
    trigger_level_dbfs_tenths: &mut i16,
    trigger_controls_enabled: bool,
) {
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
        paint_radio_trigger_level(
            ui,
            plot,
            trigger_level_dbfs_tenths,
            trigger_controls_enabled,
        );
        return;
    };
    if spectrum.power_dbfs.len() < 2 {
        paint_radio_trigger_level(
            ui,
            plot,
            trigger_level_dbfs_tenths,
            trigger_controls_enabled,
        );
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
    paint_radio_trigger_level(
        ui,
        plot,
        trigger_level_dbfs_tenths,
        trigger_controls_enabled,
    );
}

fn radio_trigger_level_y(plot: Rect, level_dbfs_tenths: i16) -> f32 {
    let level = f32::from(level_dbfs_tenths) / 10.0;
    let normalized = ((level - MIN_POWER_DBFS) / (MAX_POWER_DBFS - MIN_POWER_DBFS)).clamp(0.0, 1.0);
    egui::lerp(plot.y_range(), 1.0 - normalized)
}

fn radio_trigger_level_from_y(plot: Rect, y: f32) -> i16 {
    let normalized = 1.0 - ((y - plot.top()) / plot.height().max(1.0)).clamp(0.0, 1.0);
    let level = MIN_POWER_DBFS + normalized * (MAX_POWER_DBFS - MIN_POWER_DBFS);
    (level * 10.0).round().clamp(-1_200.0, 0.0) as i16
}

fn paint_radio_trigger_level(
    ui: &egui::Ui,
    plot: Rect,
    level_dbfs_tenths: &mut i16,
    controls_enabled: bool,
) {
    const COLOR: Color32 = Color32::from_rgb(235, 72, 72);
    const ARROW_BODY_WIDTH: f32 = 19.0;
    const ARROW_TIP_WIDTH: f32 = 8.0;
    const ARROW_HALF_HEIGHT: f32 = 9.0;
    const DOT_SPACING: f32 = 7.0;
    let arrow_x = plot.right() - 2.0;
    let arrow_width = ARROW_BODY_WIDTH + ARROW_TIP_WIDTH;
    let arrow_y = radio_trigger_level_y(plot, *level_dbfs_tenths);
    let hit_rect = Rect::from_center_size(
        Pos2::new(arrow_x - arrow_width * 0.5, arrow_y),
        Vec2::new(arrow_width + 4.0, ARROW_HALF_HEIGHT * 2.0 + 10.0),
    );
    let response = ui.interact(
        hit_rect,
        ui.make_persistent_id("radio_trigger_level"),
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
        "RF trigger level: {:.1} dBFS",
        f32::from(*level_dbfs_tenths) / 10.0
    ));
    if controls_enabled {
        if response.double_clicked() {
            *level_dbfs_tenths = -500;
        } else if response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                *level_dbfs_tenths = radio_trigger_level_from_y(plot, pointer.y);
                ui.ctx().request_repaint();
            }
        }
    }

    let arrow_y = radio_trigger_level_y(plot, *level_dbfs_tenths);
    let painter = ui.painter().with_clip_rect(plot);
    let mut x = plot.left();
    while x <= plot.right() {
        painter.circle_filled(Pos2::new(x, arrow_y), 0.8, COLOR.gamma_multiply(0.45));
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
        COLOR,
        Stroke::new(1.0_f32, Color32::from_rgb(12, 16, 22)),
    ));
    painter.text(
        Pos2::new(arrow_x - ARROW_BODY_WIDTH * 0.5, arrow_y),
        egui::Align2::CENTER_CENTER,
        "T",
        FontId::monospace(9.0),
        Color32::WHITE,
    );
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
    fn radio_pane_heights_fill_the_view_and_respect_minimums() {
        let (spectrum, waterfall, raw_data) = radio_pane_heights(600.0, true, 0.54, 0.24);

        assert!(spectrum >= MIN_SPECTRUM_HEIGHT);
        assert!(waterfall >= MIN_WATERFALL_HEIGHT);
        assert!(raw_data >= MIN_RAW_DATA_HEIGHT);
        assert!(
            (spectrum + waterfall + raw_data + RADIO_PANE_DIVIDER_HEIGHT * 2.0 - 600.0).abs()
                < f32::EPSILON
        );

        let (spectrum, waterfall, raw_data) = radio_pane_heights(600.0, false, 0.54, 0.24);
        assert_eq!(raw_data, 0.0);
        assert!((spectrum + waterfall + RADIO_PANE_DIVIDER_HEIGHT - 600.0).abs() < f32::EPSILON);
    }

    #[test]
    fn radio_pane_ratios_resize_spectrum_and_raw_data() {
        let default = radio_pane_heights(600.0, true, 0.54, 0.24);
        let resized = radio_pane_heights(600.0, true, 0.70, 0.35);

        assert!(resized.0 > default.0);
        assert!(resized.2 > default.2);
        assert!(resized.1 < default.1);
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
    fn radio_trigger_level_maps_between_dbfs_and_plot_height() {
        let plot = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 240.0));

        assert_eq!(radio_trigger_level_y(plot, 0), plot.top());
        assert_eq!(radio_trigger_level_y(plot, -1_200), plot.bottom());
        assert_eq!(radio_trigger_level_from_y(plot, plot.center().y), -600);
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
            message_starts: vec![],
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
    fn analysis_store_lists_preamble_delimited_messages_newest_first() {
        let mut store = RadioAnalysisStore::new(32).unwrap();
        let timed_bytes = |values: &[u8], first_index: u64| {
            values
                .iter()
                .enumerate()
                .map(|(index, value)| TimedByte {
                    value: *value,
                    range: SampleRange {
                        start: first_index + index as u64 * 8,
                        end: first_index + index as u64 * 8 + 8,
                    },
                    status: ByteStatus::default(),
                })
                .collect::<Vec<_>>()
        };
        store.ingest_raw(RawRadioOutput {
            bytes: timed_bytes(&[0xaa, 0xaa, 0x12, 0xaa], 0),
            message_starts: vec![RawRadioMessageStart {
                sample_index: 0,
                preamble_bytes: 2,
            }],
            diagnostics: vec![],
            synchronized: true,
        });
        store.ingest_raw(RawRadioOutput {
            bytes: timed_bytes(&[0xaa, 0x34], 32),
            message_starts: vec![RawRadioMessageStart {
                sample_index: 24,
                preamble_bytes: 2,
            }],
            diagnostics: vec![],
            synchronized: true,
        });

        let newest_first = store.raw_messages().iter().rev().collect::<Vec<_>>();

        assert_eq!(newest_first.len(), 2);
        assert_eq!(store.completed_raw_message_count(), 1);
        assert!(store.raw_message_in_progress());
        assert_eq!(newest_first[0].sequence(), 2);
        assert_eq!(newest_first[0].preamble_bytes(), 2);
        assert_eq!(newest_first[0].data_len(), 1);
        assert_eq!(
            newest_first[0]
                .bytes()
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xaa, 0xaa, 0x34]
        );
        assert!(!newest_first[0].is_complete());
        assert_eq!(
            newest_first[1]
                .bytes()
                .iter()
                .map(|byte| byte.value)
                .collect::<Vec<_>>(),
            [0xaa, 0xaa, 0x12]
        );
        assert_eq!(newest_first[1].data_len(), 1);
        assert!(newest_first[1].is_complete());

        store.finish_raw_capture();

        assert_eq!(store.completed_raw_message_count(), 2);
        assert!(!store.raw_message_in_progress());
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
        let mut trigger_level = -500;

        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                view.paint(
                    ui,
                    ui.available_rect_before_wrap(),
                    &display,
                    &analysis,
                    RadioViewOptions {
                        analysis_enabled: true,
                        trigger_level_dbfs_tenths: &mut trigger_level,
                        trigger_controls_enabled: true,
                    },
                );
            });
        });

        assert!(!output.shapes.is_empty());
    }
}
