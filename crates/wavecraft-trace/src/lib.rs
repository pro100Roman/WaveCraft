//! Device-independent system and RTOS trace events and reconstructed state.

use std::collections::BTreeMap;

/// Stable target-provided identifier for a task or another execution context.
pub type ContextId = u64;

/// One decoded trace event on a target core's monotonic clock.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceEvent {
    pub sequence: u64,
    pub core_id: u32,
    pub timestamp_ticks: u64,
    pub kind: TraceEventKind,
}

/// Device-independent trace events used to reconstruct target execution.
#[derive(Clone, Debug, PartialEq)]
pub enum TraceEventKind {
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
    SystemDescription(String),
    Unknown {
        event_id: u32,
        payload: Vec<u8>,
    },
}

/// The execution context active during a target time interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionContext {
    Task(ContextId),
    Isr(ContextId),
    Timer(ContextId),
    Scheduler,
    Idle,
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
        }
    }
}

/// Reconstructed state for one target core.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoreTraceState {
    tasks: BTreeMap<ContextId, TaskTraceState>,
    intervals: Vec<ExecutionInterval>,
    interrupted_contexts: Vec<ExecutionContext>,
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

    pub fn active_context(&self) -> Option<ExecutionContext> {
        self.active_context
    }

    pub fn continuity_known(&self) -> bool {
        self.continuity_known
    }

    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }

    fn task_mut(&mut self, task_id: ContextId, timestamp_ticks: u64) -> &mut TaskTraceState {
        self.tasks
            .entry(task_id)
            .or_insert_with(|| TaskTraceState::new(task_id, timestamp_ticks))
    }

    fn set_task_state(&mut self, task_id: ContextId, state: TaskState, timestamp_ticks: u64) {
        let task = self.task_mut(task_id, timestamp_ticks);
        task.state = state;
        task.last_transition_ticks = timestamp_ticks;
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
        self.interrupted_contexts.clear();
        self.continuity_known = false;
        self.dropped_events = self.dropped_events.saturating_add(dropped_events);
        for task in self.tasks.values_mut() {
            if task.state == TaskState::Running {
                task.state = TaskState::Created;
                task.last_transition_ticks = timestamp_ticks;
            }
        }
    }

    fn apply(&mut self, event: &TraceEvent) {
        let timestamp = event.timestamp_ticks;
        match &event.kind {
            TraceEventKind::TraceStarted => {
                self.continuity_known = true;
                self.interrupted_contexts.clear();
                self.switch_context(None, timestamp, true);
            }
            TraceEventKind::TraceStopped => {
                self.switch_context(None, timestamp, true);
                self.interrupted_contexts.clear();
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
                self.interrupted_contexts.clear();
                self.set_task_state(*task_id, TaskState::Running, timestamp);
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
                self.interrupted_contexts.clear();
                self.switch_context(Some(ExecutionContext::Scheduler), timestamp, true);
            }
            TraceEventKind::TaskBecameReady { task_id } => {
                self.set_task_state(*task_id, TaskState::Ready, timestamp);
            }
            TraceEventKind::TaskStoppedReady { task_id, cause } => {
                let task = self.task_mut(*task_id, timestamp);
                task.state = TaskState::Blocked;
                task.last_transition_ticks = timestamp;
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
                    self.interrupted_contexts.push(context);
                }
                self.switch_context(Some(ExecutionContext::Isr(*isr_id)), timestamp, true);
            }
            TraceEventKind::IsrExited => {
                let resumed = self.interrupted_contexts.pop();
                self.switch_context(resumed, timestamp, true);
            }
            TraceEventKind::IsrExitedToScheduler => {
                self.interrupted_contexts.clear();
                self.switch_context(Some(ExecutionContext::Scheduler), timestamp, true);
            }
            TraceEventKind::TimerEntered { timer_id } => {
                if let Some(context) = self.active_context {
                    self.interrupted_contexts.push(context);
                }
                self.switch_context(Some(ExecutionContext::Timer(*timer_id)), timestamp, true);
            }
            TraceEventKind::TimerExited => {
                let resumed = self.interrupted_contexts.pop();
                self.switch_context(resumed, timestamp, true);
            }
            TraceEventKind::Idle => {
                self.interrupted_contexts.clear();
                self.switch_context(Some(ExecutionContext::Idle), timestamp, true);
                self.continuity_known = true;
            }
            TraceEventKind::MarkerStarted { .. }
            | TraceEventKind::MarkerStopped { .. }
            | TraceEventKind::SystemDescription(_)
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
}
