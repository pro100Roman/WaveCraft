//! Zoom, pan, and timebase state shared by the waveform views, and the pointer
//! gestures that change it.

use crate::{egui, Range, Rect, Sense, SCOPE_HORIZONTAL_DIVISIONS, TIMEBASE_STEPS_SECONDS};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WaveformViewport {
    pub(crate) start: f32,
    pub(crate) span: f32,
    pub(crate) anchor_locked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CaptureTimeRange {
    pub(crate) start_seconds: f64,
    pub(crate) end_seconds: f64,
}

impl CaptureTimeRange {
    pub(crate) fn duration_seconds(self) -> f64 {
        (self.end_seconds - self.start_seconds).max(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CombinedTimeline {
    pub(crate) shared: CaptureTimeRange,
    pub(crate) logic: CaptureTimeRange,
    pub(crate) scope: CaptureTimeRange,
}

impl CombinedTimeline {
    pub(crate) fn source_viewport(
        self,
        shared_viewport: WaveformViewport,
        source: CaptureTimeRange,
    ) -> WaveformViewport {
        let source_duration = source.duration_seconds();
        let shared_duration = self.shared.duration_seconds();
        if source_duration <= 0.0 || shared_duration <= 0.0 {
            return WaveformViewport::default();
        }
        let visible_start =
            self.shared.start_seconds + f64::from(shared_viewport.start) * shared_duration;
        WaveformViewport {
            start: ((visible_start - source.start_seconds) / source_duration) as f32,
            span: (f64::from(shared_viewport.span) * shared_duration / source_duration) as f32,
            anchor_locked: shared_viewport.anchor_locked,
        }
    }

    pub(crate) fn shared_viewport(
        self,
        source_viewport: WaveformViewport,
        source: CaptureTimeRange,
    ) -> WaveformViewport {
        let source_duration = source.duration_seconds();
        let shared_duration = self.shared.duration_seconds();
        if source_duration <= 0.0 || shared_duration <= 0.0 {
            return WaveformViewport::default();
        }
        let visible_start =
            source.start_seconds + f64::from(source_viewport.start) * source_duration;
        let mut viewport = WaveformViewport {
            start: ((visible_start - self.shared.start_seconds) / shared_duration) as f32,
            span: (f64::from(source_viewport.span) * source_duration / shared_duration) as f32,
            anchor_locked: source_viewport.anchor_locked,
        };
        viewport.span = viewport.span.clamp(f32::EPSILON, 1.0);
        viewport.start = viewport.clamp_start(viewport.start);
        viewport
    }
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
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn time_per_division(self, sample_count: usize, sample_rate_hz: u64) -> Option<f64> {
        if sample_count == 0 || sample_rate_hz == 0 {
            return None;
        }
        Some(
            self.span as f64 * sample_count as f64
                / sample_rate_hz as f64
                / SCOPE_HORIZONTAL_DIVISIONS as f64,
        )
    }

    pub(crate) fn step_timebase(
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

    pub(crate) fn sample_range(self, sample_count: usize) -> Range<usize> {
        if sample_count == 0 {
            return 0..0;
        }
        let visible_start = f64::from(self.start).max(0.0);
        let visible_end = (f64::from(self.start) + f64::from(self.span)).min(1.0);
        if visible_start >= visible_end {
            return 0..0;
        }
        let start = ((visible_start * sample_count as f64).floor() as usize).min(sample_count - 1);
        let mut end = (visible_end * sample_count as f64).ceil() as usize;
        end = end.clamp(start + 1, sample_count);
        start..end
    }

    pub(crate) fn sample_range_in_domain(
        self,
        available_sample_count: usize,
        domain_sample_count: usize,
    ) -> Range<usize> {
        if available_sample_count == 0 || domain_sample_count == 0 {
            return 0..0;
        }
        let visible_start = f64::from(self.start).max(0.0);
        let visible_end = (f64::from(self.start) + f64::from(self.span)).min(1.0);
        if visible_start >= visible_end {
            return 0..0;
        }
        let start = ((visible_start * domain_sample_count as f64).floor() as usize)
            .min(available_sample_count);
        let end = ((visible_end * domain_sample_count as f64).ceil() as usize)
            .min(available_sample_count);
        start..end.max(start)
    }

    pub(crate) fn zoom_factor(self) -> f32 {
        1.0 / self.span
    }

    pub(crate) fn domain_fraction_at_screen(self, screen_fraction: f32) -> f32 {
        self.start + screen_fraction * self.span
    }

    pub(crate) fn screen_fraction_at_domain(self, domain_fraction: f32) -> f32 {
        (domain_fraction - self.start) / self.span.max(f32::EPSILON)
    }

    pub(crate) fn move_screen_anchor(&mut self, old_fraction: f32, new_fraction: f32) {
        let sample_anchor = self.start + old_fraction.clamp(0.0, 1.0) * self.span;
        self.start = self.clamp_start(sample_anchor - new_fraction.clamp(0.0, 1.0) * self.span);
        self.anchor_locked = true;
    }

    pub(crate) fn align_sample_to_screen(&mut self, sample_fraction: f32, screen_fraction: f32) {
        self.start =
            self.clamp_start(sample_fraction - screen_fraction.clamp(0.0, 1.0) * self.span);
    }

    pub(crate) fn align_trigger_to_screen(&mut self, sample_fraction: f32, screen_fraction: f32) {
        self.start =
            self.clamp_start(sample_fraction - screen_fraction.clamp(0.0, 1.0) * self.span);
    }

    pub(crate) fn clamp_start(self, start: f32) -> f32 {
        let first = 0.0_f32.min(1.0 - self.span);
        let last = 0.0_f32.max(1.0 - self.span);
        start.clamp(first, last)
    }

    pub(crate) fn pan_by_screen_delta(&mut self, delta_x: f32, plot_width: f32) -> bool {
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

    pub(crate) fn step_timebase_at_sample(
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
        let new_span = (timebase * SCOPE_HORIZONTAL_DIVISIONS as f64 / full_duration)
            .clamp(minimum_span, 1.0) as f32;
        if (new_span - self.span).abs() <= f32::EPSILON {
            return false;
        }
        self.span = new_span;
        self.align_sample_to_screen(sample_fraction, screen_fraction);
        self.anchor_locked = true;
        true
    }
}

pub(crate) fn apply_time_per_division(
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
            .clamp(1.0 / sample_count as f64, 1.0) as f32;
    if (new_span - viewport.span).abs() <= f32::EPSILON {
        return;
    }
    let center_sample_fraction = viewport.start + viewport.span * 0.5;
    viewport.span = new_span;
    viewport.start = viewport.clamp_start(center_sample_fraction - new_span * 0.5);
}

pub(crate) fn resolve_scope_timebase(
    selected_timebase: Option<f64>,
    displayed_timebase: Option<f64>,
    requested_timebase: Option<f64>,
    viewport_span: f32,
    auto_sample_rate: bool,
) -> Option<f64> {
    requested_timebase.or_else(|| {
        if auto_sample_rate
            && viewport_span >= 1.0 - f32::EPSILON
            && selected_timebase
                .zip(displayed_timebase)
                .is_some_and(|(selected, displayed)| selected > displayed * (1.0 + 1.0e-9))
        {
            selected_timebase
        } else {
            displayed_timebase
        }
    })
}

pub(crate) fn adjacent_timebase(current_seconds: f64, zoom_in: bool) -> Option<f64> {
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

pub(crate) enum WaveformPlotClick {
    Primary { double_clicked: bool, position: f32 },
    Secondary,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WaveformZoomAnchor {
    Pointer,
    Sample {
        domain_fraction: f32,
        screen_fraction: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WaveformViewportOptions {
    pub(crate) reset_on_double_click: bool,
    pub(crate) primary_pan_enabled: bool,
    pub(crate) zoom_anchor: WaveformZoomAnchor,
    pub(crate) timebase_basis: Option<f64>,
}

pub(crate) fn update_waveform_viewport(
    ui: &egui::Ui,
    rect: Rect,
    id_salt: &'static str,
    viewport: &mut WaveformViewport,
    sample_count: usize,
    sample_rate_hz: u64,
    options: WaveformViewportOptions,
) -> (bool, bool, Option<WaveformPlotClick>, Option<f64>) {
    let mut panned = false;
    let mut timebase_changed = false;
    let mut requested_timebase = None;
    let response = ui.interact(
        rect,
        ui.make_persistent_id(id_salt),
        Sense::click_and_drag(),
    );
    let cursor = if !options.primary_pan_enabled {
        egui::CursorIcon::Crosshair
    } else if response.dragged_by(egui::PointerButton::Primary) {
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
    if response.double_clicked() && options.reset_on_double_click && options.primary_pan_enabled {
        let previous = *viewport;
        viewport.reset();
        timebase_changed = *viewport != previous;
    }
    if options.primary_pan_enabled && response.dragged_by(egui::PointerButton::Primary) {
        let delta_x = ui.input(|input| input.pointer.delta().x);
        if viewport.pan_by_screen_delta(delta_x, rect.width()) {
            panned = true;
            ui.ctx().request_repaint();
        }
    }
    if response.hovered() {
        let scroll_delta = ui.input(|input| input.raw_scroll_delta.y);
        if scroll_delta != 0.0 {
            requested_timebase = options
                .timebase_basis
                .or_else(|| viewport.time_per_division(sample_count, sample_rate_hz))
                .and_then(|current| adjacent_timebase(current, scroll_delta > 0.0));
            let pointer_fraction = response
                .hover_pos()
                .map(|pointer| (pointer.x - rect.left()) / rect.width().max(1.0))
                .unwrap_or(0.5);
            timebase_changed = match options.zoom_anchor {
                WaveformZoomAnchor::Pointer => viewport.step_timebase(
                    scroll_delta,
                    sample_count,
                    sample_rate_hz,
                    pointer_fraction,
                ),
                WaveformZoomAnchor::Sample {
                    domain_fraction,
                    screen_fraction,
                } => viewport.step_timebase_at_sample(
                    scroll_delta,
                    sample_count,
                    sample_rate_hz,
                    domain_fraction,
                    screen_fraction,
                ),
            };
            ui.ctx().request_repaint();
        }
    }
    (panned, timebase_changed, click, requested_timebase)
}
