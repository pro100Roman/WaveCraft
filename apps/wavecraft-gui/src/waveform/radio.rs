use eframe::egui::{self, Color32, FontId, Pos2, Rect, Stroke, TextureHandle, Vec2};
use std::collections::VecDeque;
use wavecraft_dsp::SpectrumFrame;

const DEFAULT_WATERFALL_CAPACITY: usize = 256;
const MAX_WATERFALL_TEXTURE_WIDTH: usize = 512;
const MIN_POWER_DBFS: f32 = -120.0;
const MAX_POWER_DBFS: f32 = 0.0;
const NARROW_FREQUENCY_LABEL_DIVISIONS: &[u8] = &[0, 5, 10];
const COMPACT_FREQUENCY_LABEL_DIVISIONS: &[u8] = &[0, 2, 5, 8, 10];
const MEDIUM_FREQUENCY_LABEL_DIVISIONS: &[u8] = &[0, 2, 4, 5, 6, 8, 10];
const DENSE_FREQUENCY_LABEL_DIVISIONS: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

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
    pub fn paint(&mut self, ui: &egui::Ui, rect: Rect, store: &RadioDisplayStore) {
        ui.painter()
            .rect_filled(rect, 0.0, Color32::from_rgb(12, 16, 22));
        let gap = 12.0;
        let spectrum_height = (rect.height() * 0.54).max(120.0).min(rect.height());
        let spectrum_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), spectrum_height));
        let waterfall_rect = Rect::from_min_max(
            Pos2::new(
                rect.left(),
                (spectrum_rect.bottom() + gap).min(rect.bottom()),
            ),
            rect.max,
        );
        paint_spectrum(ui, spectrum_rect, store.newest_spectrum());
        self.paint_waterfall(ui, waterfall_rect, store);
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
                "{} rows · {} dropped",
                store.waterfall().len(),
                store.dropped_rows()
            ),
            FontId::proportional(11.0),
            Color32::from_rgb(105, 120, 140),
        );
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
    for &division in spectrum_frequency_label_divisions(plot.width()) {
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
}

fn spectrum_frequency_label_divisions(plot_width: f32) -> &'static [u8] {
    if plot_width >= 900.0 {
        DENSE_FREQUENCY_LABEL_DIVISIONS
    } else if plot_width >= 700.0 {
        MEDIUM_FREQUENCY_LABEL_DIVISIONS
    } else if plot_width >= 420.0 {
        COMPACT_FREQUENCY_LABEL_DIVISIONS
    } else {
        NARROW_FREQUENCY_LABEL_DIVISIONS
    }
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
    for (row, frame) in frames.iter().enumerate() {
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
        format!("{:.4} GHz", frequency_hz / 1.0e9)
    } else if frequency_hz.abs() >= 1.0e6 {
        format!("{:.3} MHz", frequency_hz / 1.0e6)
    } else if frequency_hz.abs() >= 1.0e3 {
        format!("{:.2} kHz", frequency_hz / 1.0e3)
    } else {
        format!("{frequency_hz:.0} Hz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        store.clear_for_retune();

        assert!(store.waterfall().is_empty());
        assert!(store.newest_spectrum().is_none());
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
        assert_ne!(image[(0, 0)], image[(0, 1)]);
    }

    #[test]
    fn waterfall_color_clamps_outside_the_display_range() {
        assert_eq!(waterfall_color(-200.0), Color32::from_rgb(3, 6, 16));
        assert_eq!(waterfall_color(20.0), Color32::from_rgb(255, 65, 32));
    }

    #[test]
    fn spectrum_frequency_labels_expand_with_available_width() {
        assert_eq!(spectrum_frequency_label_divisions(300.0), &[0, 5, 10]);
        assert_eq!(spectrum_frequency_label_divisions(500.0), &[0, 2, 5, 8, 10]);
        assert_eq!(
            spectrum_frequency_label_divisions(750.0),
            &[0, 2, 4, 5, 6, 8, 10]
        );
        assert_eq!(
            spectrum_frequency_label_divisions(1_000.0),
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        );
    }
}
