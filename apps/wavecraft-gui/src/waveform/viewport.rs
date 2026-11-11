//! Zoom, pan, and timebase state shared by the waveform views, and the pointer
//! gestures that change it.

use crate::{egui, Range, Rect, Sense, SCOPE_HORIZONTAL_DIVISIONS, TIMEBASE_STEPS_SECONDS};

const TRACKPAD_TIMEBASE_STEP_POINTS: f32 = 12.0;
const TRACKPAD_SCROLL_IDLE_SECONDS: f64 = 0.15;

/// Turns one continuous trackpad scroll, including its macOS momentum tail,
/// into one timebase step. A wheel reported in lines remains one step per
/// input frame and bypasses this state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct TrackpadTimebaseScroll {
    accumulated_points: f32,
    last_event_time: f64,
    stepped: bool,
}

impl TrackpadTimebaseScroll {
    fn update(&mut self, delta: f32, time: f64) -> f32 {
        if time - self.last_event_time > TRACKPAD_SCROLL_IDLE_SECONDS {
            *self = Self::default();
        }
        if delta == 0.0 {
            return 0.0;
        }
        self.last_event_time = time;
        if self.stepped {
            return 0.0;
        }
        if self.accumulated_points != 0.0 && self.accumulated_points.signum() != delta.signum() {
            self.accumulated_points = 0.0;
        }
        self.accumulated_points += delta;
        if self.accumulated_points.abs() < TRACKPAD_TIMEBASE_STEP_POINTS {
            return 0.0;
        }
        self.stepped = true;
        self.accumulated_points.signum()
    }
}

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
    /// The wheel selects a new capture span instead of zooming within the
    /// samples already held. The scope enables this with automatic sample rate.
    pub(crate) timebase_controls_capture: bool,
}

fn apply_timebase_scroll(
    viewport: &mut WaveformViewport,
    sample_count: usize,
    sample_rate_hz: u64,
    options: WaveformViewportOptions,
    scroll_delta: f32,
    pointer_fraction: f32,
) -> (bool, Option<f64>) {
    let requested_timebase = options
        .timebase_basis
        .or_else(|| viewport.time_per_division(sample_count, sample_rate_hz))
        .and_then(|current| adjacent_timebase(current, scroll_delta > 0.0));
    if options.timebase_controls_capture {
        if requested_timebase.is_some() {
            // Auto sample rate will make the requested timebase fill the next
            // hardware frame, so keep the view on the complete capture span.
            viewport.reset();
        }
        return (requested_timebase.is_some(), requested_timebase);
    }
    let changed = match options.zoom_anchor {
        WaveformZoomAnchor::Pointer => {
            viewport.step_timebase(scroll_delta, sample_count, sample_rate_hz, pointer_fraction)
        }
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
    (changed, requested_timebase)
}

/// Consumes vertical scrolling over a waveform and converts the high-frequency
/// point events emitted by a Mac trackpad into a single detent. Without this,
/// the momentum events that arrive after the fingers lift repeatedly step the
/// discrete oscilloscope timebase.
fn take_timebase_scroll_step(ui: &egui::Ui, id: egui::Id) -> f32 {
    let (delta, discrete_wheel, time) = ui.input_mut(|input| {
        let discrete_wheel = input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page,
                    delta,
                    ..
                } if delta.y != 0.0
            )
        });
        let delta = input.raw_scroll_delta.y;
        input.raw_scroll_delta.y = 0.0;
        input.smooth_scroll_delta.y = 0.0;
        (delta, discrete_wheel, input.time)
    });
    if discrete_wheel {
        ui.data_mut(|data| data.remove_temp::<TrackpadTimebaseScroll>(id));
        return delta.signum();
    }
    ui.data_mut(|data| {
        data.get_temp_mut_or_default::<TrackpadTimebaseScroll>(id)
            .update(delta, time)
    })
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
        let scroll_delta = take_timebase_scroll_step(ui, response.id.with("timebase_scroll"));
        if scroll_delta != 0.0 {
            let pointer_fraction = response
                .hover_pos()
                .map(|pointer| (pointer.x - rect.left()) / rect.width().max(1.0))
                .unwrap_or(0.5);
            (timebase_changed, requested_timebase) = apply_timebase_scroll(
                viewport,
                sample_count,
                sample_rate_hz,
                options,
                scroll_delta,
                pointer_fraction,
            );
            ui.ctx().request_repaint();
        }
    }
    (panned, timebase_changed, click, requested_timebase)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::*;

    #[test]
    fn combined_timeline_maps_both_viewports_to_the_same_time_window() {
        let timeline = CombinedTimeline {
            shared: CaptureTimeRange {
                start_seconds: 0.020,
                end_seconds: 0.110,
            },
            logic: CaptureTimeRange {
                start_seconds: 0.010,
                end_seconds: 0.110,
            },
            scope: CaptureTimeRange {
                start_seconds: 0.020,
                end_seconds: 0.120,
            },
        };
        let shared = WaveformViewport {
            start: 0.25,
            span: 0.5,
            anchor_locked: false,
        };

        let logic = timeline.source_viewport(shared, timeline.logic);
        let scope = timeline.source_viewport(shared, timeline.scope);
        let logic_start = timeline.logic.start_seconds
            + f64::from(logic.start) * timeline.logic.duration_seconds();
        let scope_start = timeline.scope.start_seconds
            + f64::from(scope.start) * timeline.scope.duration_seconds();
        let logic_span = f64::from(logic.span) * timeline.logic.duration_seconds();
        let scope_span = f64::from(scope.span) * timeline.scope.duration_seconds();

        assert!((logic_start - scope_start).abs() < 1.0e-8);
        assert!((logic_span - scope_span).abs() < 1.0e-8);
        let round_trip = timeline.shared_viewport(logic, timeline.logic);
        assert!((round_trip.start - shared.start).abs() < 1.0e-6);
        assert!((round_trip.span - shared.span).abs() < 1.0e-6);
        assert_eq!(round_trip.anchor_locked, shared.anchor_locked);
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
    fn high_zoom_keeps_a_visible_range_for_large_logic_captures() {
        let sample_count = 1_250_000_128;
        let viewport = WaveformViewport {
            start: 0.75,
            span: 1.0 / sample_count as f32,
            anchor_locked: false,
        };
        assert_eq!(viewport.start + viewport.span, viewport.start);

        let visible_range = viewport.sample_range(sample_count);

        assert!(!visible_range.is_empty());
        assert!(visible_range.len() <= 2);
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
        assert!(!viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.25));

        assert_eq!(viewport.start, 0.0);
        assert_eq!(viewport.span, 1.0);
        assert!(!viewport.anchor_locked);
        assert_eq!(viewport.sample_range(1_000), 0..1_000);
    }

    #[test]
    fn scope_timebase_fits_when_selected_window_is_wider_than_the_capture() {
        let mut viewport = WaveformViewport {
            start: 0.25,
            span: 0.5,
            ..WaveformViewport::default()
        };
        let selected_timebase = viewport.time_per_division(1_000_000, 1_000_000).unwrap();

        apply_time_per_division(&mut viewport, 250_000, 1_000_000, selected_timebase);

        assert_eq!(viewport.start, 0.0);
        assert_eq!(viewport.span, 1.0);
        assert!(
            (viewport.time_per_division(250_000, 1_000_000).unwrap() - 0.025).abs() < f64::EPSILON,
            "fit-to-capture should replace a wider persisted timebase"
        );
    }

    #[test]
    fn scope_auto_timebase_preserves_zoom_out_request_beyond_current_capture() {
        let displayed_timebase = 2.0e-3;
        let requested_timebase = adjacent_timebase(displayed_timebase, false).unwrap();
        assert_eq!(requested_timebase, 5.0e-3);

        let selected = resolve_scope_timebase(
            Some(displayed_timebase),
            Some(displayed_timebase),
            Some(requested_timebase),
            1.0,
            true,
        );
        assert_eq!(selected, Some(requested_timebase));
        assert_eq!(
            resolve_scope_timebase(selected, Some(displayed_timebase), None, 1.0, true),
            Some(requested_timebase),
            "the request must survive while the old capture is still displayed"
        );
        assert_eq!(
            resolve_scope_timebase(selected, Some(displayed_timebase), None, 1.0, false),
            Some(displayed_timebase),
            "manual sample rate remains limited by the captured buffer"
        );

        let settings = ScopeCaptureSettings::default();
        let adjusted_rate = settings.sample_rate_for_timebase(requested_timebase);
        assert_eq!(adjusted_rate, 20_000_000);
        let mut viewport = WaveformViewport::default();
        apply_time_per_division(
            &mut viewport,
            settings.sample_count as usize,
            adjusted_rate,
            requested_timebase,
        );
        assert_eq!(
            viewport.time_per_division(settings.sample_count as usize, adjusted_rate),
            Some(requested_timebase)
        );
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
        assert!(!viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5));
        assert!(viewport.step_timebase(1.0, 1_000_000, 1_000_000, 0.5));
        assert_eq!(viewport.time_per_division(1_000_000, 1_000_000), Some(0.05));
        assert!(viewport.step_timebase(1.0, 1_000_000, 1_000_000, 0.5));
        assert!(
            (viewport.time_per_division(1_000_000, 1_000_000).unwrap() - 0.02).abs() < 0.000_001
        );
        assert!(viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5));
        assert!(viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5));
        assert_eq!(viewport.span, 1.0);
        assert_eq!(viewport.sample_range(1_000_000), 0..1_000_000);
        assert!(!viewport.step_timebase(-1.0, 1_000_000, 1_000_000, 0.5));
    }

    #[test]
    fn automatic_sample_rate_scroll_changes_capture_span_without_viewport_zoom() {
        let mut viewport = WaveformViewport {
            start: 0.2,
            span: 0.5,
            anchor_locked: false,
        };
        let (changed, requested) = apply_timebase_scroll(
            &mut viewport,
            1_000_000,
            1_000_000,
            WaveformViewportOptions {
                reset_on_double_click: false,
                primary_pan_enabled: true,
                zoom_anchor: WaveformZoomAnchor::Pointer,
                timebase_basis: Some(0.05),
                timebase_controls_capture: true,
            },
            -1.0,
            0.75,
        );

        assert!(changed);
        assert_eq!(requested, Some(0.1));
        assert_eq!(viewport, WaveformViewport::default());
    }

    #[test]
    fn one_trackpad_gesture_produces_one_timebase_step_despite_momentum() {
        let mut scroll = TrackpadTimebaseScroll::default();

        assert_eq!(scroll.update(4.0, 1.00), 0.0);
        assert_eq!(scroll.update(5.0, 1.01), 0.0);
        assert_eq!(scroll.update(4.0, 1.02), 1.0);
        assert_eq!(scroll.update(20.0, 1.03), 0.0);
        assert_eq!(scroll.update(8.0, 1.10), 0.0);
    }

    #[test]
    fn trackpad_timebase_step_rearms_after_scroll_becomes_idle() {
        let mut scroll = TrackpadTimebaseScroll::default();

        assert_eq!(scroll.update(-12.0, 1.00), -1.0);
        assert_eq!(scroll.update(-12.0, 1.05), 0.0);
        assert_eq!(scroll.update(0.0, 1.30), 0.0);
        assert_eq!(scroll.update(-12.0, 1.31), -1.0);
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
}
