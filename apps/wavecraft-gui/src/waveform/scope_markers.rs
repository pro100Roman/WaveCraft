//! Oscilloscope marker and measurement state: channel and time markers, the
//! marker editor, quick time measurements, and the measurement cache.

use crate::{
    calculate_scope_measurements, scope_marker_color, Color32, Range, ScopeMeasurementResults,
    WaveformStore, WaveformViewport, SCOPE_MARKER_LIMIT,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScopeChannelMarkerSettings {
    pub(crate) horizontal_count: usize,
    pub(crate) horizontal_positions: [f32; SCOPE_MARKER_LIMIT],
    pub(crate) horizontal_comparisons: [Option<usize>; SCOPE_MARKER_LIMIT],
    pub(crate) horizontal_colors: [Color32; SCOPE_MARKER_LIMIT],
    pub(crate) vertical_count: usize,
    pub(crate) vertical_positions: [f32; SCOPE_MARKER_LIMIT],
    pub(crate) vertical_comparisons: [Option<usize>; SCOPE_MARKER_LIMIT],
    pub(crate) vertical_colors: [Color32; SCOPE_MARKER_LIMIT],
}

impl ScopeChannelMarkerSettings {
    pub(crate) fn new(
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

    pub(crate) fn count(self, kind: ScopeMarkerKind) -> usize {
        match kind {
            ScopeMarkerKind::Time => self.horizontal_count,
            ScopeMarkerKind::Voltage => self.vertical_count,
        }
        .min(SCOPE_MARKER_LIMIT)
    }

    pub(crate) fn total_count(self) -> usize {
        self.count(ScopeMarkerKind::Time) + self.count(ScopeMarkerKind::Voltage)
    }

    pub(crate) fn marker(
        self,
        kind: ScopeMarkerKind,
        index: usize,
    ) -> Option<(f32, Option<usize>, Color32)> {
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

    pub(crate) fn set_marker(
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

    pub(crate) fn add_marker(
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

    pub(crate) fn remove_marker(&mut self, kind: ScopeMarkerKind, index: usize) -> bool {
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

pub(crate) fn remap_marker_comparisons(comparisons: &mut [Option<usize>], removed: usize) {
    for comparison in comparisons {
        *comparison = match *comparison {
            Some(index) if index == removed => None,
            Some(index) if index > removed => Some(index - 1),
            comparison => comparison,
        };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeMarkerKind {
    Time,
    Voltage,
}

impl ScopeMarkerKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Time => "Time (Δt)",
            Self::Voltage => "Amplitude (ΔV)",
        }
    }

    pub(crate) fn short_label(self) -> &'static str {
        match self {
            Self::Time => "T",
            Self::Voltage => "V",
        }
    }
}

pub(crate) fn scope_marker_default_position(
    kind: ScopeMarkerKind,
    viewport: WaveformViewport,
) -> f32 {
    match kind {
        ScopeMarkerKind::Time => viewport.domain_fraction_at_screen(0.5).clamp(0.0, 1.0),
        ScopeMarkerKind::Voltage => 0.5,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeMarkerKey {
    pub(crate) channel: usize,
    pub(crate) kind: ScopeMarkerKind,
    pub(crate) index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScopeMarkerEditor {
    pub(crate) original: Option<ScopeMarkerKey>,
    pub(crate) channel: usize,
    pub(crate) kind: ScopeMarkerKind,
    pub(crate) position: f32,
    pub(crate) comparison: Option<usize>,
    pub(crate) color: Color32,
}

impl ScopeMarkerEditor {
    pub(crate) fn for_new(
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

    pub(crate) fn for_existing(settings: ScopeMarkerSettings, key: ScopeMarkerKey) -> Option<Self> {
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

    pub(crate) fn save(self, settings: &mut ScopeMarkerSettings) -> bool {
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
pub(crate) struct ScopeMarkerSettings {
    pub(crate) channels: [ScopeChannelMarkerSettings; 2],
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
pub(crate) enum ScopeQuickTimeMeasurement {
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
    pub(crate) fn start(&mut self, position: f32) {
        *self = Self::AwaitingSecond {
            first_position: position.clamp(0.0, 1.0),
        };
    }

    pub(crate) fn finish(&mut self, position: f32) {
        if let Self::AwaitingSecond { first_position } = *self {
            *self = Self::Complete {
                first_position,
                second_position: position.clamp(0.0, 1.0),
            };
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::Idle;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeMeasurementCacheKey {
    pub(crate) waveform_revision: u64,
    pub(crate) channel: usize,
    pub(crate) range_start: usize,
    pub(crate) range_end: usize,
    pub(crate) sample_rate_hz: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ScopeMeasurementCache {
    pub(crate) key: Option<ScopeMeasurementCacheKey>,
    pub(crate) results: ScopeMeasurementResults,
}

impl ScopeMeasurementCache {
    pub(crate) fn results(
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
