//! Bounded GUI state for decoded system trace events.

use std::collections::VecDeque;

use wavecraft_trace::{TraceEvent, TraceEventKind, TraceModel};

const DEFAULT_EVENT_CAPACITY: usize = 100_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TraceEventView {
    #[default]
    Events,
    Logs,
}

/// Display state is intentionally independent of a debug-probe backend. A
/// future capture worker can feed this type events from any supported probe.
#[derive(Debug)]
pub(crate) struct SystemTraceDisplayState {
    events: VecDeque<TraceEvent>,
    model: TraceModel,
    event_capacity: usize,
    pub(crate) selected_sequence: Option<u64>,
    pub(crate) event_view: TraceEventView,
    pub(crate) event_filter: String,
    pub(crate) follow_latest: bool,
}

impl Default for SystemTraceDisplayState {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            model: TraceModel::new(),
            event_capacity: DEFAULT_EVENT_CAPACITY,
            selected_sequence: None,
            event_view: TraceEventView::Events,
            event_filter: String::new(),
            follow_latest: true,
        }
    }
}

impl SystemTraceDisplayState {
    pub(crate) fn events(&self) -> &VecDeque<TraceEvent> {
        &self.events
    }

    pub(crate) fn model(&self) -> &TraceModel {
        &self.model
    }

    pub(crate) fn ingest(&mut self, event: TraceEvent) {
        self.model.apply(&event);
        if self.events.len() == self.event_capacity
            && self
                .events
                .pop_front()
                .is_some_and(|discarded| Some(discarded.sequence) == self.selected_sequence)
        {
            self.selected_sequence = None;
        }
        self.events.push_back(event);
    }

    #[cfg(test)]
    pub(crate) fn ingest_all(&mut self, events: impl IntoIterator<Item = TraceEvent>) {
        for event in events {
            self.ingest(event);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.events.clear();
        self.model = TraceModel::new();
        self.selected_sequence = None;
    }

    pub(crate) fn time_bounds(&self) -> Option<(u64, u64)> {
        let mut timestamps = self.events.iter().map(|event| event.timestamp_ticks);
        let first = timestamps.next()?;
        Some(
            timestamps.fold((first, first), |(minimum, maximum), timestamp| {
                (minimum.min(timestamp), maximum.max(timestamp))
            }),
        )
    }

    pub(crate) fn dropped_event_count(&self) -> u64 {
        self.model()
            .cores()
            .values()
            .map(|core| core.dropped_events())
            .sum()
    }

    pub(crate) fn selected_event(&self) -> Option<&TraceEvent> {
        let selected_sequence = self.selected_sequence?;
        self.events
            .iter()
            .find(|event| event.sequence == selected_sequence)
    }

    pub(crate) fn log_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event.kind, TraceEventKind::FormattedPrint { .. }))
            .count()
    }

    #[cfg(test)]
    fn with_capacity(event_capacity: usize) -> Self {
        Self {
            event_capacity,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64, timestamp_ticks: u64, kind: TraceEventKind) -> TraceEvent {
        TraceEvent {
            sequence,
            core_id: 0,
            timestamp_ticks,
            kind,
        }
    }

    #[test]
    fn bounds_display_events_without_losing_reconstructed_state() {
        let mut state = SystemTraceDisplayState::with_capacity(2);
        state.ingest_all([
            event(
                0,
                1,
                TraceEventKind::TaskInfo {
                    task_id: 7,
                    priority: 3,
                    name: "worker".to_owned(),
                },
            ),
            event(1, 2, TraceEventKind::TaskStarted { task_id: 7 }),
            event(2, 5, TraceEventKind::TaskStopped),
        ]);

        assert_eq!(
            state
                .events()
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            state
                .model()
                .core(0)
                .and_then(|core| core.tasks().get(&7))
                .and_then(|task| task.name.as_deref()),
            Some("worker")
        );
    }

    #[test]
    fn reports_time_bounds_logs_and_transport_loss() {
        let mut state = SystemTraceDisplayState::default();
        state.ingest_all([
            event(
                0,
                20,
                TraceEventKind::FormattedPrint {
                    format: "temperature=%u".to_owned(),
                    options: 0,
                    arguments: vec![42],
                },
            ),
            event(1, 10, TraceEventKind::Overflow { dropped_events: 8 }),
        ]);

        assert_eq!(state.time_bounds(), Some((10, 20)));
        assert_eq!(state.log_count(), 1);
        assert_eq!(state.dropped_event_count(), 8);
    }

    #[test]
    fn clear_starts_a_fresh_trace_session() {
        let mut state = SystemTraceDisplayState::default();
        state.ingest(event(0, 1, TraceEventKind::TaskStarted { task_id: 1 }));
        state.selected_sequence = Some(0);

        state.clear();

        assert!(state.events().is_empty());
        assert!(state.model().cores().is_empty());
        assert_eq!(state.selected_sequence, None);
    }
}
