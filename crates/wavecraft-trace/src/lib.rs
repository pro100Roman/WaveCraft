//! Device-independent system and RTOS trace events and reconstructed state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Stable target-provided identifier for a task or another execution context.
pub type ContextId = u64;

/// One decoded trace event on a target core's monotonic clock.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceEvent {
    pub sequence: u64,
    pub core_id: u32,
    pub timestamp_ticks: u64,
    pub kind: TraceEventKind,
}

/// Device-independent trace events used to reconstruct target execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TraceEventKind {
    NoOp,
    TraceStarted,
    TraceStopped,
    Overflow {
        dropped_events: u64,
    },
    TaskCreated {
        task_id: ContextId,
    },
    TaskInfo {
        task_id: ContextId,
        priority: u64,
        name: String,
    },
    TaskStarted {
        task_id: ContextId,
    },
    TaskStopped,
    TaskBecameReady {
        task_id: ContextId,
    },
    TaskStoppedReady {
        task_id: ContextId,
        cause: u64,
    },
    TaskTerminated {
        task_id: ContextId,
    },
    IsrEntered {
        isr_id: ContextId,
    },
    IsrExited,
    IsrExitedToScheduler,
    TimerEntered {
        timer_id: ContextId,
    },
    TimerExited,
    Idle,
    MarkerStarted {
        marker_id: u64,
    },
    MarkerStopped {
        marker_id: u64,
    },
    MarkerName {
        marker_id: u64,
        name: String,
    },
    SystemDescription(String),
    ClockSyncCycles {
        cycles: u64,
    },
    ClockSyncMicros {
        micros: u64,
    },
    TargetInfo {
        system_frequency_hz: u64,
        cpu_frequency_hz: u64,
        ram_base: u64,
        id_shift: u64,
    },
    StackInfo {
        task_id: ContextId,
        stack_base: u64,
        stack_size: u64,
        stack_usage: u64,
    },
    ModuleDescription {
        module_id: u64,
        event_offset: u64,
        name: String,
    },
    ModuleCount {
        count: u64,
    },
    ResourceName {
        resource_id: ContextId,
        name: String,
    },
    DataSample {
        sample_id: u64,
        value: f32,
    },
    FormattedPrint {
        format: String,
        options: u64,
        arguments: Vec<u64>,
    },
    PrintElf {
        format_address: u64,
        options: u64,
        payload: Vec<u8>,
    },
    ApiCall {
        event_id: u32,
        payload: Vec<u8>,
    },
    ApiCallEnded {
        event_id: u64,
        return_value: Option<u64>,
    },
    Unknown {
        event_id: u32,
        payload: Vec<u8>,
    },
}

/// The execution context active during a target time interval.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ExecutionContext {
    Task(ContextId),
    Isr(ContextId),
    Timer(ContextId),
    Scheduler,
    Idle,
}

/// Latest target-reported stack information for one task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskStackInfo {
    pub stack_base: u64,
    pub stack_size: u64,
    pub stack_usage: u64,
}

/// Aggregated execution statistics for one context in a trace model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextStatistics {
    pub core_id: u32,
    pub context: ExecutionContext,
    pub activations: u64,
    pub run_slices: u64,
    pub total_run_ticks: u64,
    pub last_run_ticks: Option<u64>,
    pub minimum_run_ticks: Option<u64>,
    pub maximum_run_ticks: Option<u64>,
    pub interrupted_ticks: u64,
    pub blocked_ticks: Option<u64>,
    pub stack: Option<TaskStackInfo>,
}

impl ContextStatistics {
    pub fn average_run_ticks(&self) -> Option<u64> {
        (self.run_slices > 0).then(|| self.total_run_ticks / self.run_slices)
    }
}

/// Per-context execution time accumulated inside one CPU-load time bin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuLoadBin {
    pub start_ticks: u64,
    pub end_ticks: u64,
    pub context_ticks: BTreeMap<ExecutionContext, u64>,
}

impl CpuLoadBin {
    pub fn duration_ticks(&self) -> u64 {
        self.end_ticks.saturating_sub(self.start_ticks)
    }

    pub fn busy_ticks(&self) -> u64 {
        self.context_ticks
            .iter()
            .filter(|(context, _)| **context != ExecutionContext::Idle)
            .map(|(_, ticks)| *ticks)
            .sum()
    }
}

fn context_statistics_entry(
    statistics: &mut BTreeMap<ExecutionContext, ContextStatistics>,
    core_id: u32,
    context: ExecutionContext,
) -> &mut ContextStatistics {
    statistics.entry(context).or_insert(ContextStatistics {
        core_id,
        context,
        activations: 0,
        run_slices: 0,
        total_run_ticks: 0,
        last_run_ticks: None,
        minimum_run_ticks: None,
        maximum_run_ticks: None,
        interrupted_ticks: 0,
        blocked_ticks: None,
        stack: None,
    })
}

/// A contiguous interval during which one context was executing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionInterval {
    pub context: ExecutionContext,
    pub start_ticks: u64,
    pub end_ticks: Option<u64>,
    /// False when the interval ended because trace data was lost.
    pub complete: bool,
}

/// A target performance-marker interval reconstructed from matching start and stop events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkerInterval {
    pub marker_id: u64,
    pub start_sequence: u64,
    pub start_ticks: u64,
    pub end_ticks: Option<u64>,
    /// False when the interval ended because tracing stopped or data was lost.
    pub complete: bool,
}

/// Last reconstructed lifecycle state for a task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState {
    Created,
    Ready,
    Running,
    Blocked,
    Terminated,
}

/// Reconstructed metadata and lifecycle for one target task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskTraceState {
    pub id: ContextId,
    pub name: Option<String>,
    pub priority: Option<u64>,
    pub state: TaskState,
    pub last_transition_ticks: u64,
    pub last_block_cause: Option<u64>,
    pub stack: Option<TaskStackInfo>,
    blocked_since_ticks: Option<u64>,
    total_blocked_ticks: u64,
}

impl TaskTraceState {
    fn new(id: ContextId, timestamp_ticks: u64) -> Self {
        Self {
            id,
            name: None,
            priority: None,
            state: TaskState::Created,
            last_transition_ticks: timestamp_ticks,
            last_block_cause: None,
            stack: None,
            blocked_since_ticks: None,
            total_blocked_ticks: 0,
        }
    }

    fn transition_to(&mut self, state: TaskState, timestamp_ticks: u64) {
        if self.state == TaskState::Blocked && state != TaskState::Blocked {
            self.close_blocked_interval(timestamp_ticks);
        } else if self.state != TaskState::Blocked && state == TaskState::Blocked {
            self.blocked_since_ticks = Some(timestamp_ticks);
        }
        self.state = state;
        self.last_transition_ticks = timestamp_ticks;
    }

    fn close_blocked_interval(&mut self, timestamp_ticks: u64) {
        if let Some(start_ticks) = self.blocked_since_ticks.take() {
            self.total_blocked_ticks = self
                .total_blocked_ticks
                .saturating_add(timestamp_ticks.saturating_sub(start_ticks));
        }
    }

    fn blocked_ticks_at(&self, end_ticks: u64) -> u64 {
        self.total_blocked_ticks.saturating_add(
            self.blocked_since_ticks
                .map_or(0, |start_ticks| end_ticks.saturating_sub(start_ticks)),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InterruptedContext {
    context: ExecutionContext,
    start_ticks: u64,
}

/// Reconstructed state for one target core.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoreTraceState {
    tasks: BTreeMap<ContextId, TaskTraceState>,
    intervals: Vec<ExecutionInterval>,
    marker_intervals: Vec<MarkerInterval>,
    active_markers: BTreeMap<u64, Vec<usize>>,
    interrupted_contexts: Vec<InterruptedContext>,
    activation_counts: BTreeMap<ExecutionContext, u64>,
    interrupted_ticks: BTreeMap<ExecutionContext, u64>,
    active_context: Option<ExecutionContext>,
    continuity_known: bool,
    dropped_events: u64,
}

impl CoreTraceState {
    pub fn tasks(&self) -> &BTreeMap<ContextId, TaskTraceState> {
        &self.tasks
    }

    pub fn intervals(&self) -> &[ExecutionInterval] {
        &self.intervals
    }

    pub fn marker_intervals(&self) -> &[MarkerInterval] {
        &self.marker_intervals
    }

    pub fn active_context(&self) -> Option<ExecutionContext> {
        self.active_context
    }

    pub fn continuity_known(&self) -> bool {
        self.continuity_known
    }

    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }

    fn context_statistics(&self, core_id: u32, end_ticks: u64) -> Vec<ContextStatistics> {
        let mut statistics = BTreeMap::<ExecutionContext, ContextStatistics>::new();

        for interval in &self.intervals {
            let interval_end = interval.end_ticks.unwrap_or(end_ticks).min(end_ticks);
            let run_ticks = interval_end.saturating_sub(interval.start_ticks);
            if run_ticks == 0 {
                continue;
            }
            let context = context_statistics_entry(&mut statistics, core_id, interval.context);
            context.run_slices = context.run_slices.saturating_add(1);
            context.total_run_ticks = context.total_run_ticks.saturating_add(run_ticks);
            context.last_run_ticks = Some(run_ticks);
            context.minimum_run_ticks = Some(
                context
                    .minimum_run_ticks
                    .map_or(run_ticks, |minimum| minimum.min(run_ticks)),
            );
            context.maximum_run_ticks = Some(
                context
                    .maximum_run_ticks
                    .map_or(run_ticks, |maximum| maximum.max(run_ticks)),
            );
        }
        for (context, activations) in &self.activation_counts {
            context_statistics_entry(&mut statistics, core_id, *context).activations = *activations;
        }
        for (context, interrupted_ticks) in &self.interrupted_ticks {
            context_statistics_entry(&mut statistics, core_id, *context).interrupted_ticks =
                *interrupted_ticks;
        }
        for interrupted in &self.interrupted_contexts {
            let context = context_statistics_entry(&mut statistics, core_id, interrupted.context);
            context.interrupted_ticks = context
                .interrupted_ticks
                .saturating_add(end_ticks.saturating_sub(interrupted.start_ticks));
        }
        for task in self.tasks.values() {
            let context =
                context_statistics_entry(&mut statistics, core_id, ExecutionContext::Task(task.id));
            context.blocked_ticks = Some(task.blocked_ticks_at(end_ticks));
            context.stack = task.stack;
        }
        statistics.into_values().collect()
    }

    fn cpu_load_bins(
        &self,
        start_ticks: u64,
        end_ticks: u64,
        requested_bin_count: usize,
    ) -> Vec<CpuLoadBin> {
        let duration_ticks = end_ticks.saturating_sub(start_ticks);
        if duration_ticks == 0 || requested_bin_count == 0 {
            return Vec::new();
        }
        let bin_count = if duration_ticks < requested_bin_count as u64 {
            duration_ticks as usize
        } else {
            requested_bin_count
        };
        let bin_edge = |index: usize| {
            start_ticks.saturating_add(
                (u128::from(duration_ticks) * index as u128 / bin_count as u128) as u64,
            )
        };
        let mut bins = (0..bin_count)
            .map(|index| CpuLoadBin {
                start_ticks: bin_edge(index),
                end_ticks: bin_edge(index + 1),
                context_ticks: BTreeMap::new(),
            })
            .collect::<Vec<_>>();

        for interval in &self.intervals {
            let interval_end = interval.end_ticks.unwrap_or(end_ticks);
            let overlap_start = interval.start_ticks.max(start_ticks);
            let overlap_end = interval_end.min(end_ticks);
            if overlap_start >= overlap_end {
                continue;
            }
            let first_bin = ((u128::from(overlap_start - start_ticks) * bin_count as u128)
                / u128::from(duration_ticks)) as usize;
            let last_bin = ((u128::from(overlap_end - 1 - start_ticks) * bin_count as u128)
                / u128::from(duration_ticks)) as usize;
            for bin in &mut bins[first_bin..=last_bin.min(bin_count - 1)] {
                let ticks = overlap_end
                    .min(bin.end_ticks)
                    .saturating_sub(overlap_start.max(bin.start_ticks));
                let total = bin.context_ticks.entry(interval.context).or_default();
                *total = total.saturating_add(ticks);
            }
        }
        bins
    }

    fn task_mut(&mut self, task_id: ContextId, timestamp_ticks: u64) -> &mut TaskTraceState {
        self.tasks
            .entry(task_id)
            .or_insert_with(|| TaskTraceState::new(task_id, timestamp_ticks))
    }

    fn set_task_state(&mut self, task_id: ContextId, state: TaskState, timestamp_ticks: u64) {
        let task = self.task_mut(task_id, timestamp_ticks);
        task.transition_to(state, timestamp_ticks);
    }

    fn record_activation(&mut self, context: ExecutionContext) {
        let activations = self.activation_counts.entry(context).or_default();
        *activations = activations.saturating_add(1);
    }

    fn push_interrupted_context(&mut self, context: ExecutionContext, timestamp_ticks: u64) {
        self.interrupted_contexts.push(InterruptedContext {
            context,
            start_ticks: timestamp_ticks,
        });
    }

    fn pop_interrupted_context(&mut self, timestamp_ticks: u64) -> Option<ExecutionContext> {
        let interrupted = self.interrupted_contexts.pop()?;
        let ticks = timestamp_ticks.saturating_sub(interrupted.start_ticks);
        let total = self
            .interrupted_ticks
            .entry(interrupted.context)
            .or_default();
        *total = total.saturating_add(ticks);
        Some(interrupted.context)
    }

    fn close_interrupted_contexts(&mut self, timestamp_ticks: u64) {
        while self.pop_interrupted_context(timestamp_ticks).is_some() {}
    }

    fn switch_context(
        &mut self,
        next: Option<ExecutionContext>,
        timestamp_ticks: u64,
        complete: bool,
    ) {
        if self.active_context == next {
            return;
        }
        if let Some(interval) = self
            .intervals
            .last_mut()
            .filter(|interval| interval.end_ticks.is_none())
        {
            interval.end_ticks = Some(timestamp_ticks.max(interval.start_ticks));
            interval.complete = complete;
        }
        self.active_context = next;
        if let Some(context) = next {
            self.intervals.push(ExecutionInterval {
                context,
                start_ticks: timestamp_ticks,
                end_ticks: None,
                complete: true,
            });
        }
    }

    fn invalidate_continuity(&mut self, timestamp_ticks: u64, dropped_events: u64) {
        self.switch_context(None, timestamp_ticks, false);
        self.close_active_markers(timestamp_ticks, false);
        self.close_interrupted_contexts(timestamp_ticks);
        self.continuity_known = false;
        self.dropped_events = self.dropped_events.saturating_add(dropped_events);
        for task in self.tasks.values_mut() {
            if task.state == TaskState::Running {
                task.transition_to(TaskState::Created, timestamp_ticks);
            } else if task.state == TaskState::Blocked {
                task.close_blocked_interval(timestamp_ticks);
            }
        }
    }

    fn close_active_markers(&mut self, timestamp_ticks: u64, complete: bool) {
        for interval_indices in self.active_markers.values() {
            for interval_index in interval_indices {
                if let Some(interval) = self.marker_intervals.get_mut(*interval_index) {
                    interval.end_ticks = Some(timestamp_ticks.max(interval.start_ticks));
                    interval.complete = complete;
                }
            }
        }
        self.active_markers.clear();
    }

    fn apply(&mut self, event: &TraceEvent) {
        let timestamp = event.timestamp_ticks;
        match &event.kind {
            TraceEventKind::TraceStarted => {
                self.close_active_markers(timestamp, false);
                self.continuity_known = true;
                self.interrupted_contexts.clear();
                self.switch_context(None, timestamp, true);
            }
            TraceEventKind::TraceStopped => {
                self.switch_context(None, timestamp, true);
                self.close_active_markers(timestamp, false);
                self.close_interrupted_contexts(timestamp);
                for task in self.tasks.values_mut() {
                    task.close_blocked_interval(timestamp);
                }
            }
            TraceEventKind::Overflow { dropped_events } => {
                self.invalidate_continuity(timestamp, *dropped_events);
            }
            TraceEventKind::TaskCreated { task_id } => {
                self.set_task_state(*task_id, TaskState::Created, timestamp);
            }
            TraceEventKind::TaskInfo {
                task_id,
                priority,
                name,
            } => {
                let task = self.task_mut(*task_id, timestamp);
                task.priority = Some(*priority);
                task.name = Some(name.clone());
            }
            TraceEventKind::TaskStarted { task_id } => {
                if let Some(ExecutionContext::Task(previous)) = self.active_context {
                    if previous != *task_id {
                        self.set_task_state(previous, TaskState::Created, timestamp);
                    }
                }
                self.close_interrupted_contexts(timestamp);
                self.set_task_state(*task_id, TaskState::Running, timestamp);
                self.record_activation(ExecutionContext::Task(*task_id));
                self.switch_context(Some(ExecutionContext::Task(*task_id)), timestamp, true);
                self.continuity_known = true;
            }
            TraceEventKind::TaskStopped => {
                if let Some(ExecutionContext::Task(task_id)) = self.active_context {
                    let was_running = self
                        .tasks
                        .get(&task_id)
                        .is_some_and(|task| task.state == TaskState::Running);
                    if was_running {
                        self.set_task_state(task_id, TaskState::Created, timestamp);
                    }
                }
                self.close_interrupted_contexts(timestamp);
                if self.active_context != Some(ExecutionContext::Scheduler) {
                    self.record_activation(ExecutionContext::Scheduler);
                }
                self.switch_context(Some(ExecutionContext::Scheduler), timestamp, true);
            }
            TraceEventKind::TaskBecameReady { task_id } => {
                self.set_task_state(*task_id, TaskState::Ready, timestamp);
            }
            TraceEventKind::TaskStoppedReady { task_id, cause } => {
                let task = self.task_mut(*task_id, timestamp);
                task.transition_to(TaskState::Blocked, timestamp);
                task.last_block_cause = Some(*cause);
            }
            TraceEventKind::TaskTerminated { task_id } => {
                self.set_task_state(*task_id, TaskState::Terminated, timestamp);
                if self.active_context == Some(ExecutionContext::Task(*task_id)) {
                    self.switch_context(Some(ExecutionContext::Scheduler), timestamp, true);
                }
            }
            TraceEventKind::IsrEntered { isr_id } => {
                if let Some(context) = self.active_context {
                    self.push_interrupted_context(context, timestamp);
                }
                self.record_activation(ExecutionContext::Isr(*isr_id));
                self.switch_context(Some(ExecutionContext::Isr(*isr_id)), timestamp, true);
            }
            TraceEventKind::IsrExited => {
                let resumed = self.pop_interrupted_context(timestamp);
                self.switch_context(resumed, timestamp, true);
            }
            TraceEventKind::IsrExitedToScheduler => {
                self.close_interrupted_contexts(timestamp);
                if self.active_context != Some(ExecutionContext::Scheduler) {
                    self.record_activation(ExecutionContext::Scheduler);
                }
                self.switch_context(Some(ExecutionContext::Scheduler), timestamp, true);
            }
            TraceEventKind::TimerEntered { timer_id } => {
                if let Some(context) = self.active_context {
                    self.push_interrupted_context(context, timestamp);
                }
                self.record_activation(ExecutionContext::Timer(*timer_id));
                self.switch_context(Some(ExecutionContext::Timer(*timer_id)), timestamp, true);
            }
            TraceEventKind::TimerExited => {
                let resumed = self.pop_interrupted_context(timestamp);
                self.switch_context(resumed, timestamp, true);
            }
            TraceEventKind::Idle => {
                self.close_interrupted_contexts(timestamp);
                if self.active_context != Some(ExecutionContext::Idle) {
                    self.record_activation(ExecutionContext::Idle);
                }
                self.switch_context(Some(ExecutionContext::Idle), timestamp, true);
                self.continuity_known = true;
            }
            TraceEventKind::StackInfo {
                task_id,
                stack_base,
                stack_size,
                stack_usage,
            } => {
                self.task_mut(*task_id, timestamp).stack = Some(TaskStackInfo {
                    stack_base: *stack_base,
                    stack_size: *stack_size,
                    stack_usage: *stack_usage,
                });
            }
            TraceEventKind::MarkerStarted { marker_id } => {
                let interval_index = self.marker_intervals.len();
                self.marker_intervals.push(MarkerInterval {
                    marker_id: *marker_id,
                    start_sequence: event.sequence,
                    start_ticks: timestamp,
                    end_ticks: None,
                    complete: true,
                });
                self.active_markers
                    .entry(*marker_id)
                    .or_default()
                    .push(interval_index);
            }
            TraceEventKind::MarkerStopped { marker_id } => {
                let interval_index = self.active_markers.get_mut(marker_id).and_then(Vec::pop);
                if self
                    .active_markers
                    .get(marker_id)
                    .is_some_and(Vec::is_empty)
                {
                    self.active_markers.remove(marker_id);
                }
                if let Some(interval) = interval_index
                    .and_then(|interval_index| self.marker_intervals.get_mut(interval_index))
                {
                    interval.end_ticks = Some(timestamp.max(interval.start_ticks));
                }
            }
            TraceEventKind::NoOp
            | TraceEventKind::MarkerName { .. }
            | TraceEventKind::SystemDescription(_)
            | TraceEventKind::ClockSyncCycles { .. }
            | TraceEventKind::ClockSyncMicros { .. }
            | TraceEventKind::TargetInfo { .. }
            | TraceEventKind::ModuleDescription { .. }
            | TraceEventKind::ModuleCount { .. }
            | TraceEventKind::ResourceName { .. }
            | TraceEventKind::DataSample { .. }
            | TraceEventKind::FormattedPrint { .. }
            | TraceEventKind::PrintElf { .. }
            | TraceEventKind::ApiCall { .. }
            | TraceEventKind::ApiCallEnded { .. }
            | TraceEventKind::Unknown { .. } => {}
        }
    }
}

/// Reconstructed trace state, partitioned by target core.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TraceModel {
    cores: BTreeMap<u32, CoreTraceState>,
}

impl TraceModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cores(&self) -> &BTreeMap<u32, CoreTraceState> {
        &self.cores
    }

    pub fn core(&self, core_id: u32) -> Option<&CoreTraceState> {
        self.cores.get(&core_id)
    }

    pub fn apply(&mut self, event: &TraceEvent) {
        self.cores.entry(event.core_id).or_default().apply(event);
    }

    pub fn apply_all<'a>(&mut self, events: impl IntoIterator<Item = &'a TraceEvent>) {
        for event in events {
            self.apply(event);
        }
    }

    pub fn context_statistics(&self, end_ticks: u64) -> Vec<ContextStatistics> {
        self.cores
            .iter()
            .flat_map(|(core_id, core)| core.context_statistics(*core_id, end_ticks))
            .collect()
    }

    pub fn cpu_load_bins(
        &self,
        core_id: u32,
        start_ticks: u64,
        end_ticks: u64,
        bin_count: usize,
    ) -> Vec<CpuLoadBin> {
        self.cores.get(&core_id).map_or_else(Vec::new, |core| {
            core.cpu_load_bins(start_ticks, end_ticks, bin_count)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64, ticks: u64, kind: TraceEventKind) -> TraceEvent {
        TraceEvent {
            sequence,
            core_id: 0,
            timestamp_ticks: ticks,
            kind,
        }
    }

    #[test]
    fn reconstructs_task_lifecycle_and_metadata() {
        let events = [
            event(0, 1, TraceEventKind::TraceStarted),
            event(1, 2, TraceEventKind::TaskCreated { task_id: 7 }),
            event(
                2,
                3,
                TraceEventKind::TaskInfo {
                    task_id: 7,
                    priority: 4,
                    name: "worker".to_owned(),
                },
            ),
            event(3, 5, TraceEventKind::TaskBecameReady { task_id: 7 }),
            event(4, 8, TraceEventKind::TaskStarted { task_id: 7 }),
            event(
                5,
                13,
                TraceEventKind::TaskStoppedReady {
                    task_id: 7,
                    cause: 9,
                },
            ),
            event(6, 13, TraceEventKind::TaskStopped),
            event(7, 21, TraceEventKind::TaskTerminated { task_id: 7 }),
        ];
        let mut model = TraceModel::new();
        model.apply_all(&events);

        let core = model.core(0).expect("core is created");
        let task = core.tasks().get(&7).expect("task is created");
        assert_eq!(task.name.as_deref(), Some("worker"));
        assert_eq!(task.priority, Some(4));
        assert_eq!(task.state, TaskState::Terminated);
        assert_eq!(task.last_block_cause, Some(9));
        assert_eq!(
            core.intervals(),
            &[
                ExecutionInterval {
                    context: ExecutionContext::Task(7),
                    start_ticks: 8,
                    end_ticks: Some(13),
                    complete: true,
                },
                ExecutionInterval {
                    context: ExecutionContext::Scheduler,
                    start_ticks: 13,
                    end_ticks: None,
                    complete: true,
                },
            ]
        );
    }

    #[test]
    fn reconstructs_marker_intervals_without_changing_execution_context() {
        let events = [
            event(0, 1, TraceEventKind::TraceStarted),
            event(1, 2, TraceEventKind::TaskStarted { task_id: 7 }),
            event(2, 3, TraceEventKind::MarkerStarted { marker_id: 1 }),
            event(3, 5, TraceEventKind::MarkerStopped { marker_id: 1 }),
            event(4, 7, TraceEventKind::TaskStopped),
        ];
        let mut model = TraceModel::new();
        model.apply_all(&events);
        let core = model.core(0).expect("core exists");

        assert_eq!(
            core.marker_intervals(),
            &[MarkerInterval {
                marker_id: 1,
                start_sequence: 2,
                start_ticks: 3,
                end_ticks: Some(5),
                complete: true,
            }]
        );
        assert_eq!(core.intervals()[0].context, ExecutionContext::Task(7));
        assert_eq!(core.intervals()[0].start_ticks, 2);
        assert_eq!(core.intervals()[0].end_ticks, Some(7));
    }

    #[test]
    fn nested_interrupts_resume_the_interrupted_contexts() {
        let events = [
            event(0, 1, TraceEventKind::TaskStarted { task_id: 3 }),
            event(1, 4, TraceEventKind::IsrEntered { isr_id: 15 }),
            event(2, 6, TraceEventKind::IsrEntered { isr_id: 2 }),
            event(3, 7, TraceEventKind::IsrExited),
            event(4, 9, TraceEventKind::IsrExited),
        ];
        let mut model = TraceModel::new();
        model.apply_all(&events);

        let core = model.core(0).expect("core is created");
        assert_eq!(core.active_context(), Some(ExecutionContext::Task(3)));
        assert_eq!(
            core.intervals(),
            &[
                ExecutionInterval {
                    context: ExecutionContext::Task(3),
                    start_ticks: 1,
                    end_ticks: Some(4),
                    complete: true,
                },
                ExecutionInterval {
                    context: ExecutionContext::Isr(15),
                    start_ticks: 4,
                    end_ticks: Some(6),
                    complete: true,
                },
                ExecutionInterval {
                    context: ExecutionContext::Isr(2),
                    start_ticks: 6,
                    end_ticks: Some(7),
                    complete: true,
                },
                ExecutionInterval {
                    context: ExecutionContext::Isr(15),
                    start_ticks: 7,
                    end_ticks: Some(9),
                    complete: true,
                },
                ExecutionInterval {
                    context: ExecutionContext::Task(3),
                    start_ticks: 9,
                    end_ticks: None,
                    complete: true,
                },
            ]
        );
    }

    #[test]
    fn overflow_closes_the_active_interval_as_incomplete() {
        let events = [
            event(0, 10, TraceEventKind::TaskStarted { task_id: 1 }),
            event(1, 20, TraceEventKind::Overflow { dropped_events: 12 }),
        ];
        let mut model = TraceModel::new();
        model.apply_all(&events);

        let core = model.core(0).expect("core is created");
        assert!(!core.continuity_known());
        assert_eq!(core.dropped_events(), 12);
        assert_eq!(core.active_context(), None);
        assert_eq!(core.intervals()[0].end_ticks, Some(20));
        assert!(!core.intervals()[0].complete);
    }

    #[test]
    fn stopping_execution_preserves_a_preceding_blocked_transition() {
        let events = [
            event(0, 1, TraceEventKind::TaskStarted { task_id: 7 }),
            event(
                1,
                5,
                TraceEventKind::TaskStoppedReady {
                    task_id: 7,
                    cause: 4,
                },
            ),
            event(2, 5, TraceEventKind::TaskStopped),
        ];
        let mut model = TraceModel::new();
        model.apply_all(&events);

        let task = model
            .core(0)
            .and_then(|core| core.tasks().get(&7))
            .expect("task is reconstructed");
        assert_eq!(task.state, TaskState::Blocked);
        assert_eq!(task.last_block_cause, Some(4));
    }

    #[test]
    fn target_cores_are_reconstructed_independently() {
        let mut model = TraceModel::new();
        model.apply(&event(0, 3, TraceEventKind::TaskStarted { task_id: 1 }));
        model.apply(&TraceEvent {
            sequence: 0,
            core_id: 1,
            timestamp_ticks: 5,
            kind: TraceEventKind::Idle,
        });

        assert_eq!(
            model.core(0).and_then(CoreTraceState::active_context),
            Some(ExecutionContext::Task(1))
        );
        assert_eq!(
            model.core(1).and_then(CoreTraceState::active_context),
            Some(ExecutionContext::Idle)
        );
    }

    #[test]
    fn aggregates_context_runtime_blocking_interruptions_and_stack_usage() {
        let events = [
            event(
                0,
                0,
                TraceEventKind::TaskInfo {
                    task_id: 7,
                    priority: 2,
                    name: "worker".to_owned(),
                },
            ),
            event(
                1,
                0,
                TraceEventKind::StackInfo {
                    task_id: 7,
                    stack_base: 0x2000_1000,
                    stack_size: 1024,
                    stack_usage: 640,
                },
            ),
            event(2, 0, TraceEventKind::TaskStarted { task_id: 7 }),
            event(3, 10, TraceEventKind::IsrEntered { isr_id: 15 }),
            event(4, 15, TraceEventKind::IsrExited),
            event(
                5,
                20,
                TraceEventKind::TaskStoppedReady {
                    task_id: 7,
                    cause: 1,
                },
            ),
            event(6, 20, TraceEventKind::TaskStopped),
            event(7, 30, TraceEventKind::TaskBecameReady { task_id: 7 }),
            event(8, 35, TraceEventKind::TaskStarted { task_id: 7 }),
            event(9, 45, TraceEventKind::Idle),
        ];
        let mut model = TraceModel::new();
        model.apply_all(&events);

        let statistics = model.context_statistics(50);
        let task = statistics
            .iter()
            .find(|statistics| statistics.context == ExecutionContext::Task(7))
            .expect("task statistics exist");
        assert_eq!(task.activations, 2);
        assert_eq!(task.run_slices, 3);
        assert_eq!(task.total_run_ticks, 25);
        assert_eq!(task.last_run_ticks, Some(10));
        assert_eq!(task.minimum_run_ticks, Some(5));
        assert_eq!(task.average_run_ticks(), Some(8));
        assert_eq!(task.maximum_run_ticks, Some(10));
        assert_eq!(task.interrupted_ticks, 5);
        assert_eq!(task.blocked_ticks, Some(10));
        assert_eq!(
            task.stack,
            Some(TaskStackInfo {
                stack_base: 0x2000_1000,
                stack_size: 1024,
                stack_usage: 640,
            })
        );
        let isr = statistics
            .iter()
            .find(|statistics| statistics.context == ExecutionContext::Isr(15))
            .expect("ISR statistics exist");
        assert_eq!(isr.activations, 1);
        assert_eq!(isr.total_run_ticks, 5);
        assert_eq!(isr.blocked_ticks, None);
    }

    #[test]
    fn cpu_load_bins_clip_context_intervals_to_the_requested_range() {
        let events = [
            event(0, 0, TraceEventKind::TaskStarted { task_id: 7 }),
            event(1, 10, TraceEventKind::IsrEntered { isr_id: 15 }),
            event(2, 15, TraceEventKind::IsrExited),
            event(3, 20, TraceEventKind::TaskStopped),
            event(4, 35, TraceEventKind::TaskStarted { task_id: 7 }),
            event(5, 45, TraceEventKind::Idle),
        ];
        let mut model = TraceModel::new();
        model.apply_all(&events);

        let bins = model.cpu_load_bins(0, 0, 50, 5);

        assert_eq!(bins.len(), 5);
        assert_eq!(bins[0].context_ticks[&ExecutionContext::Task(7)], 10);
        assert_eq!(bins[1].context_ticks[&ExecutionContext::Isr(15)], 5);
        assert_eq!(bins[1].context_ticks[&ExecutionContext::Task(7)], 5);
        assert_eq!(bins[4].context_ticks[&ExecutionContext::Task(7)], 5);
        assert_eq!(bins[4].context_ticks[&ExecutionContext::Idle], 5);
        assert_eq!(bins[4].duration_ticks(), 10);
        assert_eq!(bins[4].busy_ticks(), 5);
    }

    #[test]
    fn normalized_events_have_a_stable_tagged_representation() {
        let event = TraceEvent {
            sequence: 17,
            core_id: 2,
            timestamp_ticks: 99,
            kind: TraceEventKind::FormattedPrint {
                format: "value=%u".to_owned(),
                options: 3,
                arguments: vec![4, 5],
            },
        };

        let value = serde_json::to_value(&event).expect("event serializes");
        assert_eq!(
            value,
            serde_json::json!({
                "sequence": 17,
                "core_id": 2,
                "timestamp_ticks": 99,
                "kind": {
                    "type": "formatted_print",
                    "data": {
                        "format": "value=%u",
                        "options": 3,
                        "arguments": [4, 5]
                    }
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<TraceEvent>(value).expect("event deserializes"),
            event
        );
    }
}
