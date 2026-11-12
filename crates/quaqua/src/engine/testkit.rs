//! In-memory workflow testkit for deterministic execution and testing.
//!
//! Provides virtual clock control, mock activity execution, an in-memory journal,
//! and the `run_to_completion` execution driver.

use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::engine::activity::{ActivityError, ActivityRequest, ActivityResult};
use crate::engine::journal::{EventRecord, Journal, JournalError, SnapshotRecord};
use crate::engine::workflow::{
    ScheduledAction, StepDecision, Workflow, WorkflowContext, WorkflowEvent,
};

/// Error returned by the testkit driver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TestkitError {
    WorkflowFailed { code: i32, reason: String },
    ActivityError(ActivityError),
    JournalError(JournalError),
    StepLimitExceeded(usize),
    Deadlock(String),
}

impl From<ActivityError> for TestkitError {
    fn from(err: ActivityError) -> Self {
        Self::ActivityError(err)
    }
}

impl From<JournalError> for TestkitError {
    fn from(err: JournalError) -> Self {
        Self::JournalError(err)
    }
}

impl std::fmt::Display for TestkitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkflowFailed { code, reason } => {
                write!(f, "workflow failed (code {code}): {reason}")
            }
            Self::ActivityError(err) => write!(f, "activity execution failed: {err}"),
            Self::JournalError(err) => write!(f, "journal operation failed: {err}"),
            Self::StepLimitExceeded(limit) => write!(f, "step limit exceeded ({limit} steps)"),
            Self::Deadlock(msg) => write!(f, "workflow deadlocked: {msg}"),
        }
    }
}

impl std::error::Error for TestkitError {}

/// Discrete virtual clock without real thread sleeping.
#[derive(Debug, Clone)]
pub struct VirtualClock {
    now_ms: u64,
    // (deadline_ms, id)
    timers: BinaryHeap<Reverse<(u64, String)>>,
}

impl VirtualClock {
    /// Creates a new virtual clock initialized to `start_ms` (milliseconds since Unix epoch).
    pub fn new(start_ms: u64) -> Self {
        Self {
            now_ms: start_ms,
            timers: BinaryHeap::new(),
        }
    }

    /// Current virtual timestamp in milliseconds since Unix epoch.
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// Current virtual timestamp as [`SystemTime`].
    pub fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(self.now_ms)
    }

    /// Schedules a timer with an ID and relative duration.
    pub fn schedule_timer(&mut self, id: impl Into<String>, duration: Duration) {
        let deadline = self.now_ms + duration.as_millis() as u64;
        self.timers.push(Reverse((deadline, id.into())));
    }

    /// Advances virtual time by `duration` and fires all timers reaching their deadline.
    pub fn advance(&mut self, duration: Duration) -> Vec<String> {
        self.now_ms += duration.as_millis() as u64;
        self.drain_expired()
    }

    /// Advances virtual time to the earliest scheduled timer, if any, and fires expired timers.
    pub fn advance_to_next_timer(&mut self) -> Option<(u64, Vec<String>)> {
        let next_deadline = self.timers.peek().map(|Reverse((d, _))| *d)?;
        if next_deadline > self.now_ms {
            self.now_ms = next_deadline;
        }
        let fired = self.drain_expired();
        Some((self.now_ms, fired))
    }

    /// Number of scheduled timers waiting to fire.
    pub fn pending_timers_count(&self) -> usize {
        self.timers.len()
    }

    fn drain_expired(&mut self) -> Vec<String> {
        let mut expired = Vec::new();
        while let Some(Reverse((deadline, _))) = self.timers.peek() {
            if *deadline <= self.now_ms {
                if let Some(Reverse((_, id))) = self.timers.pop() {
                    expired.push(id);
                }
            } else {
                break;
            }
        }
        expired
    }
}

/// In-memory mock activities registry and call recorder.
#[derive(Debug, Clone, Default)]
pub struct MockActivities {
    canned_by_id: HashMap<String, ActivityResult>,
    latencies: HashMap<String, Duration>,
    default_result: Option<ActivityResult>,
    call_log: Vec<(String, ActivityRequest)>,
}

impl MockActivities {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a canned result for a specific activity ID.
    pub fn register(&mut self, id: impl Into<String>, result: ActivityResult) {
        self.canned_by_id.insert(id.into(), result);
    }

    /// Registers a canned result with an explicit execution latency for virtual clock progression.
    pub fn register_with_latency(
        &mut self,
        id: impl Into<String>,
        result: ActivityResult,
        latency: Duration,
    ) {
        let id_str = id.into();
        self.latencies.insert(id_str.clone(), latency);
        self.canned_by_id.insert(id_str, result);
    }

    /// Sets a default result returned when an activity ID has no explicit canned result.
    pub fn set_default(&mut self, result: ActivityResult) {
        self.default_result = Some(result);
    }

    /// Executes the activity by recording the call and returning the canned response.
    pub fn execute(
        &mut self,
        id: &str,
        req: &ActivityRequest,
    ) -> Result<ActivityResult, ActivityError> {
        self.call_log.push((id.to_string(), req.clone()));
        if let Some(res) = self.canned_by_id.get(id) {
            return Ok(res.clone());
        }
        if let Some(def) = &self.default_result {
            return Ok(def.clone());
        }
        Err(ActivityError::NotFound(format!(
            "no mock result registered for activity ID: {id}"
        )))
    }

    /// Returns the sequence of all recorded activity calls `(id, request)`.
    pub fn calls(&self) -> &[(String, ActivityRequest)] {
        &self.call_log
    }

    /// Returns the count of calls made to mock activities.
    pub fn call_count(&self) -> usize {
        self.call_log.len()
    }
}

/// In-memory implementation of [`Journal`].
#[derive(Debug, Clone, Default)]
pub struct InMemoryJournal {
    events: HashMap<String, Vec<EventRecord>>,
    snapshots: HashMap<String, Vec<SnapshotRecord>>,
}

impl InMemoryJournal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn all_events(&self, workflow_id: &str) -> &[EventRecord] {
        self.events
            .get(workflow_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn all_snapshots(&self, workflow_id: &str) -> &[SnapshotRecord] {
        self.snapshots
            .get(workflow_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

impl Journal for InMemoryJournal {
    fn append_event(
        &mut self,
        workflow_id: &str,
        event: &EventRecord,
    ) -> Result<u64, JournalError> {
        let list = self.events.entry(workflow_id.to_string()).or_default();
        let expected_seq = (list.len() + 1) as u64;
        let assigned_seq = if event.seq == 0 {
            expected_seq
        } else {
            event.seq
        };

        let mut rec = event.clone();
        rec.seq = assigned_seq;
        list.push(rec);
        Ok(assigned_seq)
    }

    fn append_snapshot(
        &mut self,
        workflow_id: &str,
        snapshot: &SnapshotRecord,
    ) -> Result<(), JournalError> {
        let list = self.snapshots.entry(workflow_id.to_string()).or_default();
        list.push(snapshot.clone());
        Ok(())
    }

    fn load_events(
        &self,
        workflow_id: &str,
        since_seq: u64,
    ) -> Result<Vec<EventRecord>, JournalError> {
        let empty = Vec::new();
        let list = self.events.get(workflow_id).unwrap_or(&empty);
        Ok(list
            .iter()
            .filter(|e| e.seq >= since_seq)
            .cloned()
            .collect())
    }

    fn load_latest_snapshot(
        &self,
        workflow_id: &str,
    ) -> Result<Option<SnapshotRecord>, JournalError> {
        Ok(self
            .snapshots
            .get(workflow_id)
            .and_then(|list| list.last().cloned()))
    }
}

/// A blocking `AgentWait` advances the virtual clock:
/// - By the registered latency if specified on the mock activity.
/// - Or if the wait matched a target state in `until` (not a timeout), by a default transition latency (1s).
/// - Otherwise by the full `timeout_ms`.
fn advance_for_wait(
    clock: &mut VirtualClock,
    id: &str,
    activity: &ActivityRequest,
    activities: &MockActivities,
) {
    if let Some(latency) = activities.latencies.get(id) {
        clock.advance(*latency);
        return;
    }
    if let ActivityRequest::Herdr(crate::engine::activity::HerdrRequest::AgentWait {
        timeout_ms,
        until,
        ..
    }) = activity
    {
        if *timeout_ms > 0 {
            if let Some(ActivityResult::Herdr(crate::engine::activity::HerdrResult::AgentWait {
                status,
                err,
                ..
            })) = activities.canned_by_id.get(id)
            {
                if err.is_empty() && (until.is_empty() || until.contains(status)) {
                    clock.advance(Duration::from_millis(1000));
                    return;
                }
            }
            clock.advance(Duration::from_millis(*timeout_ms));
        }
    }
}

/// Drives a workflow to completion deterministically using virtual components.
pub fn run_to_completion<W: Workflow>(
    workflow: &W,
    workflow_id: &str,
    initial_state: W::State,
    clock: &mut VirtualClock,
    activities: &mut MockActivities,
    journal: &mut dyn Journal,
) -> Result<W::Output, TestkitError> {
    let mut state = initial_state;
    let mut event_queue: VecDeque<WorkflowEvent> = VecDeque::new();
    let max_steps = 10_000;
    let mut seq = 0u64;
    let mut first_step = true;

    for _step in 0..max_steps {
        // If the event queue is empty and not the initial step, advance virtual time to the next timer
        if event_queue.is_empty() && !first_step {
            if clock.pending_timers_count() > 0 {
                if let Some((_ts, fired)) = clock.advance_to_next_timer() {
                    for fired_id in fired {
                        seq += 1;
                        journal.append_event(
                            workflow_id,
                            &EventRecord::new(
                                seq,
                                clock.now_ms(),
                                "timer_fired",
                                format!("id={fired_id}"),
                            ),
                        )?;
                        event_queue.push_back(WorkflowEvent::TimerFired { id: fired_id });
                    }
                }
            } else {
                return Err(TestkitError::Deadlock(
                    "workflow awaiting events but no activities or timers are pending".to_string(),
                ));
            }
        }

        let next_event = event_queue.pop_front();
        first_step = false;

        let mut ctx = WorkflowContext::new(clock.now_ms());
        let decision = workflow.step(&mut state, &mut ctx, next_event);

        // Process any actions scheduled directly on the WorkflowContext
        for action in ctx.drain_scheduled() {
            match action {
                ScheduledAction::Activity { id, activity } => {
                    seq += 1;
                    journal.append_event(
                        workflow_id,
                        &EventRecord::new(
                            seq,
                            clock.now_ms(),
                            "activity_scheduled",
                            serde_json::to_string(&activity).unwrap_or_default(),
                        ),
                    )?;

                    advance_for_wait(clock, &id, &activity, activities);
                    let result = activities.execute(&id, &activity)?;
                    seq += 1;
                    journal.append_event(
                        workflow_id,
                        &EventRecord::new(
                            seq,
                            clock.now_ms(),
                            "activity_completed",
                            serde_json::to_string(&result).unwrap_or_default(),
                        ),
                    )?;

                    event_queue.push_back(WorkflowEvent::ActivityCompleted { id, result });
                }
                ScheduledAction::Timer { id, duration } => {
                    seq += 1;
                    journal.append_event(
                        workflow_id,
                        &EventRecord::new(
                            seq,
                            clock.now_ms(),
                            "timer_scheduled",
                            format!("id={id}; duration_ms={}", duration.as_millis()),
                        ),
                    )?;

                    clock.schedule_timer(id, duration);
                }
                ScheduledAction::Child {
                    id,
                    workflow_type,
                    input,
                } => {
                    seq += 1;
                    journal.append_event(
                        workflow_id,
                        &EventRecord::new(
                            seq,
                            clock.now_ms(),
                            "child_started",
                            format!("id={id}; type={workflow_type}"),
                        ),
                    )?;
                    seq += 1;
                    journal.append_event(
                        workflow_id,
                        &EventRecord::new(
                            seq,
                            clock.now_ms(),
                            "child_completed",
                            serde_json::to_string(&input).unwrap_or_default(),
                        ),
                    )?;
                    event_queue.push_back(WorkflowEvent::ChildCompleted { id, output: input });
                }
            }
        }

        // Process step decision
        match decision {
            StepDecision::Complete(output) => {
                seq += 1;
                journal.append_event(
                    workflow_id,
                    &EventRecord::new(
                        seq,
                        clock.now_ms(),
                        "workflow_completed",
                        serde_json::to_string(&output).unwrap_or_default(),
                    ),
                )?;
                journal.append_snapshot(
                    workflow_id,
                    &SnapshotRecord::new(seq, serde_json::to_string(&state).unwrap_or_default()),
                )?;
                return Ok(output);
            }
            StepDecision::Fail { code, reason } => {
                seq += 1;
                journal.append_event(
                    workflow_id,
                    &EventRecord::new(
                        seq,
                        clock.now_ms(),
                        "workflow_failed",
                        format!("code={code}; reason={reason}"),
                    ),
                )?;
                return Err(TestkitError::WorkflowFailed { code, reason });
            }
            StepDecision::ScheduleActivity { id, activity } => {
                seq += 1;
                journal.append_event(
                    workflow_id,
                    &EventRecord::new(
                        seq,
                        clock.now_ms(),
                        "activity_scheduled",
                        serde_json::to_string(&activity).unwrap_or_default(),
                    ),
                )?;

                advance_for_wait(clock, &id, &activity, activities);
                let result = activities.execute(&id, &activity)?;
                seq += 1;
                journal.append_event(
                    workflow_id,
                    &EventRecord::new(
                        seq,
                        clock.now_ms(),
                        "activity_completed",
                        serde_json::to_string(&result).unwrap_or_default(),
                    ),
                )?;

                event_queue.push_back(WorkflowEvent::ActivityCompleted { id, result });
            }
            StepDecision::ScheduleTimer { id, duration } => {
                seq += 1;
                journal.append_event(
                    workflow_id,
                    &EventRecord::new(
                        seq,
                        clock.now_ms(),
                        "timer_scheduled",
                        format!("id={id}; duration_ms={}", duration.as_millis()),
                    ),
                )?;

                clock.schedule_timer(id, duration);
            }
            StepDecision::Continue => {
                seq += 1;
                journal.append_event(
                    workflow_id,
                    &EventRecord::new(seq, clock.now_ms(), "continue", ""),
                )?;
            }
            StepDecision::PauseForApproval { reason } => {
                seq += 1;
                journal.append_event(
                    workflow_id,
                    &EventRecord::new(
                        seq,
                        clock.now_ms(),
                        "pause_for_approval",
                        format!("reason={reason}"),
                    ),
                )?;
                // Testkit auto-approves pauses
                seq += 1;
                journal.append_event(
                    workflow_id,
                    &EventRecord::new(seq, clock.now_ms(), "approval_resolved", "approved=true"),
                )?;
                event_queue.push_back(WorkflowEvent::ApprovalResolved {
                    approved: true,
                    reason: None,
                });
            }
            StepDecision::StartChild {
                id,
                workflow_type,
                input,
            } => {
                seq += 1;
                journal.append_event(
                    workflow_id,
                    &EventRecord::new(
                        seq,
                        clock.now_ms(),
                        "child_started",
                        format!("id={id}; type={workflow_type}"),
                    ),
                )?;
                // Testkit echoes input as child completion
                seq += 1;
                journal.append_event(
                    workflow_id,
                    &EventRecord::new(
                        seq,
                        clock.now_ms(),
                        "child_completed",
                        serde_json::to_string(&input).unwrap_or_default(),
                    ),
                )?;
                event_queue.push_back(WorkflowEvent::ChildCompleted { id, output: input });
            }
        }
    }

    Err(TestkitError::StepLimitExceeded(max_steps))
}
