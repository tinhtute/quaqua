//! Durable event-sourced workflow runner.
//!
//! Provides snapshot loading, event replay without side-effect re-execution,
//! deterministic stepping, activity dispatch, timer coordination via a clock seam,
//! event and snapshot persistence, and suspension when blocked.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::engine::activity::{Activity, ActivityError, ActivityRequest, ActivityResult};
use crate::engine::journal::{EventRecord, Journal, JournalError, SnapshotRecord};
use crate::engine::workflow::{
    ScheduledAction, StepDecision, Workflow, WorkflowContext, WorkflowEvent,
};

/// Minimal clock abstraction internal to the runner.
pub(crate) trait RunnerClock: Send {
    /// Returns current virtual timestamp in milliseconds since Unix epoch.
    fn now_ms(&self) -> u64;

    /// Schedules a timer with an ID and duration.
    fn schedule_timer(&mut self, id: &str, duration: Duration);

    /// Advances virtual time to the earliest scheduled timer, if any, and returns expired IDs.
    fn advance_to_next_timer(&mut self) -> Option<(u64, Vec<String>)>;

    /// Returns the number of pending timers waiting to fire.
    fn pending_timers_count(&self) -> usize;
}

impl RunnerClock for crate::engine::testkit::VirtualClock {
    fn now_ms(&self) -> u64 {
        self.now_ms()
    }

    fn schedule_timer(&mut self, id: &str, duration: Duration) {
        self.schedule_timer(id, duration);
    }

    fn advance_to_next_timer(&mut self) -> Option<(u64, Vec<String>)> {
        self.advance_to_next_timer()
    }

    fn pending_timers_count(&self) -> usize {
        self.pending_timers_count()
    }
}

/// Outcome of running a workflow until completion, failure, or suspension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RunnerOutcome<Output> {
    /// Workflow completed successfully with output.
    Completed(Output),
    /// Workflow suspended waiting for external events or un-fireable timers.
    Blocked(String),
    /// Workflow terminated with failure.
    Failed { code: i32, reason: String },
}

/// Configuration options for workflow execution.
#[derive(Debug, Clone)]
pub struct RunnerOptions {
    /// Maximum steps allowed before aborting with [`RunnerError::StepLimitExceeded`].
    pub max_steps: usize,
    /// Number of events between periodic snapshots (0 disables periodic snapshots).
    pub snapshot_interval: u64,
    /// Whether to always capture a state snapshot when the workflow transitions to Blocked.
    pub snapshot_at_blocked: bool,
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self {
            max_steps: 10_000,
            snapshot_interval: 20,
            snapshot_at_blocked: true,
        }
    }
}

/// Errors returned by the workflow runner.
#[derive(Debug, Clone, PartialEq)]
pub enum RunnerError {
    /// Underlying journal error.
    Journal(JournalError),
    /// Underlying activity execution error.
    Activity(ActivityError),
    /// Serialization or deserialization failure.
    Serialization(String),
    /// Workflow has no snapshot and no initial state was provided.
    NoInitialState,
    /// Execution exceeded configured max_steps.
    StepLimitExceeded(usize),
}

impl From<JournalError> for RunnerError {
    fn from(err: JournalError) -> Self {
        Self::Journal(err)
    }
}

impl From<ActivityError> for RunnerError {
    fn from(err: ActivityError) -> Self {
        Self::Activity(err)
    }
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Journal(e) => write!(f, "journal error: {e}"),
            Self::Activity(e) => write!(f, "activity error: {e}"),
            Self::Serialization(e) => write!(f, "serialization error: {e}"),
            Self::NoInitialState => write!(f, "no snapshot found and no initial state provided"),
            Self::StepLimitExceeded(n) => write!(f, "execution step limit ({n}) exceeded"),
        }
    }
}

impl std::error::Error for RunnerError {}

// ----------------------------------------------------------------------------
// Internal Event Payload Serializers / Deserializers
// ----------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct ActivityScheduledData {
    id: String,
    /// Kept as raw JSON: replay only needs the id, and old journals may name
    /// activity variants this release removed.
    #[serde(default)]
    activity: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct ActivityCompletedData {
    id: String,
    /// Raw JSON so a removed result variant in an old journal resumes as a
    /// no-op instead of failing the whole replay.
    result: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct TimerScheduledData {
    id: String,
    duration_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct TimerFiredData {
    id: String,
}

#[derive(Serialize, Deserialize)]
struct PauseForApprovalData {
    reason: String,
}

#[derive(Serialize, Deserialize)]
struct ApprovalResolvedData {
    approved: bool,
    reason: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ChildStartedData {
    id: String,
    workflow_type: String,
    input: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct ChildCompletedData {
    id: String,
    output: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct ChildFailedData {
    id: String,
    code: i32,
    reason: String,
}

#[derive(Serialize, Deserialize)]
struct WorkflowFailedData {
    code: i32,
    reason: String,
}

// ----------------------------------------------------------------------------
// Dispatch Helpers
// ----------------------------------------------------------------------------

fn dispatch_activity(
    workflow_id: &str,
    id: &str,
    activity: &ActivityRequest,
    journal: &mut dyn Journal,
    activities: &mut dyn Activity,
    clock: &impl RunnerClock,
    cursor: &mut JournalCursor,
    events_since_snapshot: &mut u64,
) -> Result<WorkflowEvent, RunnerError> {
    append_at_cursor(
        journal,
        workflow_id,
        cursor,
        clock.now_ms(),
        "activity_scheduled",
        serde_json::to_string(&ActivityScheduledData {
            id: id.to_string(),
            activity: serde_json::to_value(activity).unwrap_or_default(),
        })
        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
    )?;
    *events_since_snapshot += 1;

    let result = activities.execute(activity)?;
    append_at_cursor(
        journal,
        workflow_id,
        cursor,
        clock.now_ms(),
        "activity_completed",
        serde_json::to_string(&ActivityCompletedData {
            id: id.to_string(),
            result: serde_json::to_value(&result).unwrap_or_default(),
        })
        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
    )?;
    *events_since_snapshot += 1;
    Ok(WorkflowEvent::ActivityCompleted {
        id: id.to_string(),
        result,
    })
}

fn dispatch_timer(
    workflow_id: &str,
    id: &str,
    duration: Duration,
    journal: &mut dyn Journal,
    clock: &mut impl RunnerClock,
    cursor: &mut JournalCursor,
    events_since_snapshot: &mut u64,
) -> Result<(), RunnerError> {
    append_at_cursor(
        journal,
        workflow_id,
        cursor,
        clock.now_ms(),
        "timer_scheduled",
        serde_json::to_string(&TimerScheduledData {
            id: id.to_string(),
            duration_ms: duration.as_millis() as u64,
        })
        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
    )?;
    *events_since_snapshot += 1;
    // Format tag with the landed seq so re-armed timers don't conflict
    clock.schedule_timer(&format!("{}:{id}", cursor.seq), duration);
    Ok(())
}

/// Where this run writes next, and whether the journal still matches the state
/// it has applied.
///
/// One value, so a rebase cannot advance the write cursor without also marking
/// the run as diverged: the two facts are the same fact.
struct JournalCursor {
    /// The next sequence number to try, then the seq the journal assigned.
    seq: u64,
    /// Set on the first refused append, i.e. the first time another writer on
    /// this workflow id committed past the cursor. From that point the history
    /// holds events this run never applied, so it must not claim them.
    diverged: bool,
}

impl JournalCursor {
    fn new(head: u64) -> Self {
        Self {
            seq: head,
            diverged: false,
        }
    }
}

/// Append tries spent on a sequence conflict before giving up. Each retry
/// re-reads the head the journal reports, so this only bounds a writer storm.
const SEQUENCE_REBASE_TRIES: u32 = 16;

/// Append one event at the runner's cursor, taking the next sequence number.
///
/// The cursor is read once per process ([`run_until_blocked`]), so a second
/// writer on the same workflow id can commit past it while this run is blocked
/// inside an activity. [`JournalError::SequenceConflict`] is that writer's
/// trace, not corruption: `expected_seq` is the free slot the journal reports,
/// so rebase the cursor there and retry. The refused append inserted nothing,
/// which keeps the event exactly once in a contiguous chain.
///
/// A rebase skips events this run never applied, so it marks the cursor
/// diverged and prints one warning per run ([`warn_divergence`]).
fn append_at_cursor(
    journal: &mut dyn Journal,
    workflow_id: &str,
    cursor: &mut JournalCursor,
    ts_ms: u64,
    kind: &str,
    payload: impl Into<String>,
) -> Result<(), RunnerError> {
    let payload = payload.into();
    cursor.seq += 1;
    let mut last_conflict = None;
    for _ in 0..SEQUENCE_REBASE_TRIES {
        let event = EventRecord::new(cursor.seq, ts_ms, kind, payload.clone());
        match journal.append_event(workflow_id, &event) {
            Ok(_) => return Ok(()),
            Err(e @ JournalError::SequenceConflict { expected_seq, .. }) => {
                assert!(
                    expected_seq > cursor.seq,
                    "journal reports the free slot {expected_seq} for {workflow_id} \
                     while this run holds seq {}: a conflict can only sit past this cursor",
                    cursor.seq
                );
                if !cursor.diverged {
                    cursor.diverged = true;
                    warn_divergence(workflow_id, expected_seq - cursor.seq);
                }
                cursor.seq = expected_seq;
                last_conflict = Some(e);
            }
            Err(e) => return Err(RunnerError::Journal(e)),
        }
    }
    Err(RunnerError::Journal(
        last_conflict.expect("the loop ran at least once"),
    ))
}

/// Name the divergence once per run. Without it the rebase is invisible, and a
/// second writer on the same workflow id (two watchdogs, a stray `--resume`)
/// leaves no trace anywhere.
fn warn_divergence(workflow_id: &str, unapplied: u64) {
    eprintln!(
        "WATCHDOG journal divergence: {workflow_id}: another writer committed \
         {unapplied} event(s) this run did not apply; rebasing the cursor and \
         writing no further snapshots this run"
    );
}

/// Snapshot only while this run's state still matches the journal history.
///
/// `SnapshotRecord.seq` claims every event up to that seq is inside
/// `state_json`, and [`run_until_blocked`] replays only events past the latest
/// snapshot. After a rebase the journal holds events this run never applied
/// while `state` already carries later events of its own, so no seq is honest:
/// recording the cursor would mark foreign events incorporated forever, and
/// recording an earlier seq would replay this run's own events onto a state
/// that has them. Skip the snapshot instead; the next resume replays the gap.
fn snapshot_if_contiguous<S: Serialize>(
    journal: &mut dyn Journal,
    workflow_id: &str,
    cursor: &JournalCursor,
    state: &S,
) -> Result<(), RunnerError> {
    if cursor.diverged {
        return Ok(());
    }
    journal.append_snapshot(
        workflow_id,
        &SnapshotRecord::new(
            cursor.seq,
            serde_json::to_string(state).map_err(|e| RunnerError::Serialization(e.to_string()))?,
        ),
    )?;
    Ok(())
}

// ----------------------------------------------------------------------------
// Public Runner Entrypoints
// ----------------------------------------------------------------------------

/// Drives a workflow from initial state or prior snapshot until it completes, fails, or blocks.
pub(crate) fn run<W: Workflow, C: RunnerClock>(
    workflow: &W,
    workflow_id: &str,
    initial_state: W::State,
    journal: &mut dyn Journal,
    activities: &mut dyn Activity,
    clock: &mut C,
) -> Result<RunnerOutcome<W::Output>, RunnerError> {
    // Persist the starting state before the first event. Until the first
    // periodic snapshot, this row is the only replay base a later [`resume`]
    // has after a kill; without it the resume dies with `NoInitialState`.
    if journal.load_latest_snapshot(workflow_id)?.is_none() {
        journal.append_snapshot(
            workflow_id,
            &SnapshotRecord::new(
                0,
                serde_json::to_string(&initial_state)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?,
            ),
        )?;
    }

    run_until_blocked(
        workflow,
        workflow_id,
        Some(initial_state),
        journal,
        activities,
        clock,
        &RunnerOptions::default(),
        Vec::new(),
    )
}

/// Resumes a blocked workflow using state from the journal and delivers incoming external events.
pub(crate) fn resume<W: Workflow, C: RunnerClock>(
    workflow: &W,
    workflow_id: &str,
    journal: &mut dyn Journal,
    activities: &mut dyn Activity,
    clock: &mut C,
    incoming_events: Vec<WorkflowEvent>,
) -> Result<RunnerOutcome<W::Output>, RunnerError> {
    run_until_blocked(
        workflow,
        workflow_id,
        None,
        journal,
        activities,
        clock,
        &RunnerOptions::default(),
        incoming_events,
    )
}

/// Replay side-effect collected during deterministic history replay.
#[derive(Debug, Clone)]
enum ReplayEffect<Output> {
    DispatchAction(ScheduledAction),
    ExecuteInFlightActivity {
        id: String,
        activity: ActivityRequest,
    },
    DispatchDecision(StepDecision<Output>),
}

/// Executes or resumes a workflow until blocked, completed, or failed.
pub(crate) fn run_until_blocked<W: Workflow, C: RunnerClock>(
    workflow: &W,
    workflow_id: &str,
    initial_state: Option<W::State>,
    journal: &mut dyn Journal,
    activities: &mut dyn Activity,
    clock: &mut C,
    options: &RunnerOptions,
    incoming_events: Vec<WorkflowEvent>,
) -> Result<RunnerOutcome<W::Output>, RunnerError> {
    // 1. Inspect full history to handle terminal states and index activities/timers
    let all_events = journal.load_events(workflow_id, 1)?;
    let mut cursor = JournalCursor::new(all_events.iter().map(|e| e.seq).max().unwrap_or(0));

    // Finding 4: Resuming already completed or failed workflow returns terminal outcome immediately
    for rec in &all_events {
        if rec.kind == "workflow_completed" {
            let output: W::Output = serde_json::from_str(&rec.payload_json)
                .map_err(|e| RunnerError::Serialization(e.to_string()))?;
            return Ok(RunnerOutcome::Completed(output));
        }
        if rec.kind == "workflow_failed" {
            let data: WorkflowFailedData = serde_json::from_str(&rec.payload_json)
                .map_err(|e| RunnerError::Serialization(e.to_string()))?;
            return Ok(RunnerOutcome::Failed {
                code: data.code,
                reason: data.reason,
            });
        }
        if rec.kind == "workflow_cancelled" {
            let reason = serde_json::from_str::<serde_json::Value>(&rec.payload_json)
                .ok()
                .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string))
                .unwrap_or_else(|| "workflow cancelled".to_string());
            return Ok(RunnerOutcome::Failed {
                code: 1,
                reason: format!("workflow cancelled: {reason}"),
            });
        }
    }

    // Map scheduled_seq -> (id, scheduled_ts_ms, duration) across ALL history for timer restoration
    let mut scheduled_timer_instances: HashMap<u64, (String, u64, Duration)> = HashMap::new();
    let mut timer_id_to_scheduled_seqs: HashMap<String, Vec<u64>> = HashMap::new();
    let mut fired_timer_seqs: HashSet<u64> = HashSet::new();

    for rec in &all_events {
        match rec.kind.as_str() {
            "timer_scheduled" => {
                let data: TimerScheduledData = serde_json::from_str(&rec.payload_json)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?;
                scheduled_timer_instances.insert(
                    rec.seq,
                    (
                        data.id.clone(),
                        rec.ts_ms,
                        Duration::from_millis(data.duration_ms),
                    ),
                );
                timer_id_to_scheduled_seqs
                    .entry(data.id.clone())
                    .or_default()
                    .push(rec.seq);
            }
            "timer_fired" => {
                let data: TimerFiredData = serde_json::from_str(&rec.payload_json)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?;
                // Match fired timer with earliest unfired scheduled_seq for this timer id
                if let Some(seqs) = timer_id_to_scheduled_seqs.get_mut(&data.id) {
                    if let Some(pos) = seqs.iter().position(|s| !fired_timer_seqs.contains(s)) {
                        fired_timer_seqs.insert(seqs[pos]);
                    }
                }
            }
            _ => {}
        }
    }

    // Restore all unfired timer instances across history with remaining duration
    for (sched_seq, (id, sched_ts, orig_dur)) in &scheduled_timer_instances {
        if !fired_timer_seqs.contains(sched_seq) {
            let deadline_ms = sched_ts + orig_dur.as_millis() as u64;
            let current_now = clock.now_ms();
            let remaining = if deadline_ms > current_now {
                Duration::from_millis(deadline_ms - current_now)
            } else {
                Duration::from_millis(0)
            };
            clock.schedule_timer(&format!("{sched_seq}:{id}"), remaining);
        }
    }

    // 2. Load latest state snapshot
    let latest_snapshot = journal.load_latest_snapshot(workflow_id)?;
    let (mut state, snapshot_seq) = match latest_snapshot {
        Some(snap) => {
            let s: W::State = serde_json::from_str(&snap.state_json)
                .map_err(|e| RunnerError::Serialization(e.to_string()))?;
            (s, snap.seq)
        }
        None => {
            let s = initial_state.ok_or(RunnerError::NoInitialState)?;
            (s, 0u64)
        }
    };

    // 3. Post-snapshot occurrence indexing: count records strictly with seq > snapshot_seq
    let events_since_snapshot_rec = journal.load_events(workflow_id, snapshot_seq + 1)?;
    let mut historical_events_to_deliver: VecDeque<WorkflowEvent> = VecDeque::new();
    let mut history_activity_scheduled: HashMap<String, usize> = HashMap::new();
    let mut history_activity_completed: HashMap<String, usize> = HashMap::new();
    let mut history_timer_scheduled: HashMap<String, usize> = HashMap::new();
    let mut history_child_started: HashMap<String, usize> = HashMap::new();
    let mut history_pause_for_approval_count = 0usize;
    let mut history_continue_count = 0usize;

    for rec in &events_since_snapshot_rec {
        match rec.kind.as_str() {
            "activity_scheduled" => {
                let data: ActivityScheduledData = serde_json::from_str(&rec.payload_json)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?;
                *history_activity_scheduled.entry(data.id).or_default() += 1;
            }
            "activity_completed" => {
                let data: ActivityCompletedData = serde_json::from_str(&rec.payload_json)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?;
                *history_activity_completed
                    .entry(data.id.clone())
                    .or_default() += 1;
                let result = serde_json::from_value(data.result).unwrap_or(ActivityResult::Unknown);
                historical_events_to_deliver.push_back(WorkflowEvent::ActivityCompleted {
                    id: data.id,
                    result,
                });
            }
            "timer_scheduled" => {
                let data: TimerScheduledData = serde_json::from_str(&rec.payload_json)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?;
                *history_timer_scheduled.entry(data.id).or_default() += 1;
            }
            "timer_fired" => {
                let data: TimerFiredData = serde_json::from_str(&rec.payload_json)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?;
                historical_events_to_deliver.push_back(WorkflowEvent::TimerFired { id: data.id });
            }
            "approval_resolved" => {
                let data: ApprovalResolvedData = serde_json::from_str(&rec.payload_json)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?;
                historical_events_to_deliver.push_back(WorkflowEvent::ApprovalResolved {
                    approved: data.approved,
                    reason: data.reason,
                });
            }
            "child_started" => {
                let data: ChildStartedData = serde_json::from_str(&rec.payload_json)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?;
                *history_child_started.entry(data.id).or_default() += 1;
            }
            "child_completed" => {
                let data: ChildCompletedData = serde_json::from_str(&rec.payload_json)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?;
                historical_events_to_deliver.push_back(WorkflowEvent::ChildCompleted {
                    id: data.id,
                    output: data.output,
                });
            }
            "child_failed" => {
                let data: ChildFailedData = serde_json::from_str(&rec.payload_json)
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?;
                historical_events_to_deliver.push_back(WorkflowEvent::ChildFailed {
                    id: data.id,
                    code: data.code,
                    reason: data.reason,
                });
            }
            "pause_for_approval" => {
                history_pause_for_approval_count += 1;
            }
            "continue" => {
                history_continue_count += 1;
            }
            _ => {}
        }
    }

    // 4. Replay steps deterministically from snapshot to journal head
    let mut replay_initial_step = snapshot_seq == 0 && !all_events.is_empty();
    let mut replayed_activity_counts: HashMap<String, usize> = HashMap::new();
    let mut replayed_timer_counts: HashMap<String, usize> = HashMap::new();
    let mut replayed_child_counts: HashMap<String, usize> = HashMap::new();
    let mut replayed_continue_count = 0usize;
    let mut replayed_pause_for_approval_count = 0usize;

    let mut pending_replay_effects: Vec<ReplayEffect<W::Output>> = Vec::new();

    while replay_initial_step || !historical_events_to_deliver.is_empty() {
        let ev = if replay_initial_step {
            replay_initial_step = false;
            None
        } else if let Some(e) = historical_events_to_deliver.pop_front() {
            Some(e)
        } else {
            break;
        };

        let mut ctx = WorkflowContext::new(clock.now_ms());
        let dec = workflow.step(&mut state, &mut ctx, ev);

        // Reconcile ctx actions on this replay step in order
        for action in ctx.drain_scheduled() {
            match action {
                ScheduledAction::Activity { id, activity } => {
                    let count = replayed_activity_counts.entry(id.clone()).or_default();
                    *count += 1;
                    let sched_cnt = history_activity_scheduled.get(&id).copied().unwrap_or(0);
                    let comp_cnt = history_activity_completed.get(&id).copied().unwrap_or(0);
                    if *count <= comp_cnt {
                        // Already scheduled and completed in history
                    } else if *count <= sched_cnt {
                        // In-flight activity: scheduled in history, completion lost to kill
                        pending_replay_effects
                            .push(ReplayEffect::ExecuteInFlightActivity { id, activity });
                    } else {
                        // Unjournaled action
                        pending_replay_effects.push(ReplayEffect::DispatchAction(
                            ScheduledAction::Activity { id, activity },
                        ));
                    }
                }
                ScheduledAction::Timer { id, duration } => {
                    let count = replayed_timer_counts.entry(id.clone()).or_default();
                    *count += 1;
                    let sched_cnt = history_timer_scheduled.get(&id).copied().unwrap_or(0);
                    if *count > sched_cnt {
                        pending_replay_effects.push(ReplayEffect::DispatchAction(
                            ScheduledAction::Timer { id, duration },
                        ));
                    }
                }
                ScheduledAction::Child {
                    id,
                    workflow_type,
                    input,
                } => {
                    let count = replayed_child_counts.entry(id.clone()).or_default();
                    *count += 1;
                    let sched_cnt = history_child_started.get(&id).copied().unwrap_or(0);
                    if *count > sched_cnt {
                        pending_replay_effects.push(ReplayEffect::DispatchAction(
                            ScheduledAction::Child {
                                id,
                                workflow_type,
                                input,
                            },
                        ));
                    }
                }
            }
        }

        // Reconcile step decision on this replay step
        match dec {
            StepDecision::ScheduleActivity { id, activity } => {
                let count = replayed_activity_counts.entry(id.clone()).or_default();
                *count += 1;
                let sched_cnt = history_activity_scheduled.get(&id).copied().unwrap_or(0);
                let comp_cnt = history_activity_completed.get(&id).copied().unwrap_or(0);
                if *count <= comp_cnt {
                    // Already scheduled and completed in history
                } else if *count <= sched_cnt {
                    // In-flight activity
                    pending_replay_effects
                        .push(ReplayEffect::ExecuteInFlightActivity { id, activity });
                } else {
                    // Unjournaled decision
                    pending_replay_effects.push(ReplayEffect::DispatchDecision(
                        StepDecision::ScheduleActivity { id, activity },
                    ));
                }
            }
            StepDecision::ScheduleTimer { id, duration } => {
                let count = replayed_timer_counts.entry(id.clone()).or_default();
                *count += 1;
                let sched_cnt = history_timer_scheduled.get(&id).copied().unwrap_or(0);
                if *count > sched_cnt {
                    pending_replay_effects.push(ReplayEffect::DispatchDecision(
                        StepDecision::ScheduleTimer { id, duration },
                    ));
                }
            }
            StepDecision::StartChild {
                id,
                workflow_type,
                input,
            } => {
                let count = replayed_child_counts.entry(id.clone()).or_default();
                *count += 1;
                let sched_cnt = history_child_started.get(&id).copied().unwrap_or(0);
                if *count > sched_cnt {
                    pending_replay_effects.push(ReplayEffect::DispatchDecision(
                        StepDecision::StartChild {
                            id,
                            workflow_type,
                            input,
                        },
                    ));
                }
            }
            StepDecision::Complete(output) => {
                pending_replay_effects.push(ReplayEffect::DispatchDecision(
                    StepDecision::Complete(output),
                ));
            }
            StepDecision::Fail { code, reason } => {
                pending_replay_effects.push(ReplayEffect::DispatchDecision(StepDecision::Fail {
                    code,
                    reason,
                }));
            }
            StepDecision::PauseForApproval { reason } => {
                replayed_pause_for_approval_count += 1;
                if replayed_pause_for_approval_count > history_pause_for_approval_count {
                    pending_replay_effects.push(ReplayEffect::DispatchDecision(
                        StepDecision::PauseForApproval { reason },
                    ));
                }
            }
            StepDecision::Continue => {
                replayed_continue_count += 1;
                if replayed_continue_count > history_continue_count {
                    pending_replay_effects
                        .push(ReplayEffect::DispatchDecision(StepDecision::Continue));
                }
            }
        }
    }

    // 5. Transition to Forward Execution Queue & Dispatch Pending Replay Effects in Order
    let mut event_queue: VecDeque<WorkflowEvent> = VecDeque::new();
    let mut events_since_snapshot = 0u64;

    for effect in pending_replay_effects {
        match effect {
            ReplayEffect::ExecuteInFlightActivity { id, activity } => {
                let result = activities.execute(&activity)?;
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "activity_completed",
                    serde_json::to_string(&ActivityCompletedData {
                        id: id.clone(),
                        result: serde_json::to_value(&result).unwrap_or_default(),
                    })
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                )?;
                events_since_snapshot += 1;
                event_queue.push_back(WorkflowEvent::ActivityCompleted { id, result });
            }
            ReplayEffect::DispatchAction(action) => match action {
                ScheduledAction::Activity { id, activity } => {
                    let ev = dispatch_activity(
                        workflow_id,
                        &id,
                        &activity,
                        journal,
                        activities,
                        clock,
                        &mut cursor,
                        &mut events_since_snapshot,
                    )?;
                    event_queue.push_back(ev);
                }
                ScheduledAction::Timer { id, duration } => {
                    dispatch_timer(
                        workflow_id,
                        &id,
                        duration,
                        journal,
                        clock,
                        &mut cursor,
                        &mut events_since_snapshot,
                    )?;
                }
                ScheduledAction::Child {
                    id,
                    workflow_type,
                    input,
                } => {
                    append_at_cursor(
                        journal,
                        workflow_id,
                        &mut cursor,
                        clock.now_ms(),
                        "child_started",
                        serde_json::to_string(&ChildStartedData {
                            id,
                            workflow_type,
                            input,
                        })
                        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                    )?;
                    events_since_snapshot += 1;
                }
            },
            ReplayEffect::DispatchDecision(decision) => match decision {
                StepDecision::ScheduleActivity { id, activity } => {
                    let ev = dispatch_activity(
                        workflow_id,
                        &id,
                        &activity,
                        journal,
                        activities,
                        clock,
                        &mut cursor,
                        &mut events_since_snapshot,
                    )?;
                    event_queue.push_back(ev);
                }
                StepDecision::ScheduleTimer { id, duration } => {
                    dispatch_timer(
                        workflow_id,
                        &id,
                        duration,
                        journal,
                        clock,
                        &mut cursor,
                        &mut events_since_snapshot,
                    )?;
                }
                StepDecision::StartChild {
                    id,
                    workflow_type,
                    input,
                } => {
                    append_at_cursor(
                        journal,
                        workflow_id,
                        &mut cursor,
                        clock.now_ms(),
                        "child_started",
                        serde_json::to_string(&ChildStartedData {
                            id,
                            workflow_type,
                            input,
                        })
                        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                    )?;
                    events_since_snapshot += 1;
                }
                StepDecision::Continue => {
                    append_at_cursor(
                        journal,
                        workflow_id,
                        &mut cursor,
                        clock.now_ms(),
                        "continue",
                        "",
                    )?;
                    events_since_snapshot += 1;
                }
                StepDecision::PauseForApproval { reason } => {
                    append_at_cursor(
                        journal,
                        workflow_id,
                        &mut cursor,
                        clock.now_ms(),
                        "pause_for_approval",
                        serde_json::to_string(&PauseForApprovalData {
                            reason: reason.clone(),
                        })
                        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                    )?;
                    if options.snapshot_at_blocked {
                        snapshot_if_contiguous(journal, workflow_id, &cursor, &state)?;
                    }
                    return Ok(RunnerOutcome::Blocked(format!(
                        "pause_for_approval: {reason}"
                    )));
                }
                StepDecision::Complete(output) => {
                    append_at_cursor(
                        journal,
                        workflow_id,
                        &mut cursor,
                        clock.now_ms(),
                        "workflow_completed",
                        serde_json::to_string(&output)
                            .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                    )?;
                    snapshot_if_contiguous(journal, workflow_id, &cursor, &state)?;
                    return Ok(RunnerOutcome::Completed(output));
                }
                StepDecision::Fail { code, reason } => {
                    append_at_cursor(
                        journal,
                        workflow_id,
                        &mut cursor,
                        clock.now_ms(),
                        "workflow_failed",
                        serde_json::to_string(&WorkflowFailedData {
                            code,
                            reason: reason.clone(),
                        })
                        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                    )?;
                    return Ok(RunnerOutcome::Failed { code, reason });
                }
            },
        }
    }
    // Append and queue any new incoming external events in FIFO order
    for ev in incoming_events {
        match &ev {
            WorkflowEvent::ApprovalResolved { approved, reason } => {
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "approval_resolved",
                    serde_json::to_string(&ApprovalResolvedData {
                        approved: *approved,
                        reason: reason.clone(),
                    })
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                )?;
            }
            WorkflowEvent::ActivityCompleted { id, result } => {
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "activity_completed",
                    serde_json::to_string(&ActivityCompletedData {
                        id: id.clone(),
                        result: serde_json::to_value(&result).unwrap_or_default(),
                    })
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                )?;
            }
            WorkflowEvent::TimerFired { id } => {
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "timer_fired",
                    serde_json::to_string(&TimerFiredData { id: id.clone() })
                        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                )?;
            }
            WorkflowEvent::ChildCompleted { id, output } => {
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "child_completed",
                    serde_json::to_string(&ChildCompletedData {
                        id: id.clone(),
                        output: output.clone(),
                    })
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                )?;
            }
            WorkflowEvent::ChildFailed { id, code, reason } => {
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "child_failed",
                    serde_json::to_string(&ChildFailedData {
                        id: id.clone(),
                        code: *code,
                        reason: reason.clone(),
                    })
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                )?;
            }
        }
        event_queue.push_back(ev);
    }

    let mut initial_step_needed = snapshot_seq == 0 && all_events.is_empty();
    let mut steps = 0;

    loop {
        steps += 1;
        if steps > options.max_steps {
            return Err(RunnerError::StepLimitExceeded(options.max_steps));
        }

        let next_event = if initial_step_needed {
            initial_step_needed = false;
            None
        } else if let Some(ev) = event_queue.pop_front() {
            Some(ev)
        } else if clock.pending_timers_count() > 0 {
            if let Some((_ts, fired_tags)) = clock.advance_to_next_timer() {
                for tag in fired_tags {
                    // Tag format: "{scheduled_seq}:{user_id}" or legacy "{user_id}"
                    let (sched_seq, user_id) = if let Some((s_str, u_id)) = tag.split_once(':') {
                        (s_str.parse::<u64>().unwrap_or(0), u_id)
                    } else {
                        (0u64, tag.as_str())
                    };

                    // Findings 1 & 3: Dedup by unique instance sched_seq, never by user id!
                    if sched_seq > 0 && fired_timer_seqs.contains(&sched_seq) {
                        continue;
                    }
                    if sched_seq > 0 {
                        fired_timer_seqs.insert(sched_seq);
                    }

                    append_at_cursor(
                        journal,
                        workflow_id,
                        &mut cursor,
                        clock.now_ms(),
                        "timer_fired",
                        serde_json::to_string(&TimerFiredData {
                            id: user_id.to_string(),
                        })
                        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                    )?;
                    events_since_snapshot += 1;
                    event_queue.push_back(WorkflowEvent::TimerFired {
                        id: user_id.to_string(),
                    });
                }
            }
            if let Some(ev) = event_queue.pop_front() {
                Some(ev)
            } else {
                if options.snapshot_at_blocked {
                    snapshot_if_contiguous(journal, workflow_id, &cursor, &state)?;
                }
                return Ok(RunnerOutcome::Blocked(
                    "no pending events or timers".to_string(),
                ));
            }
        } else {
            // Quiescent point: event queue empty and no pending timers can fire
            if options.snapshot_at_blocked {
                snapshot_if_contiguous(journal, workflow_id, &cursor, &state)?;
            }
            return Ok(RunnerOutcome::Blocked(
                "no pending events or timers".to_string(),
            ));
        };

        let mut ctx = WorkflowContext::new(clock.now_ms());
        let decision = workflow.step(&mut state, &mut ctx, next_event);

        // Process actions scheduled directly on WorkflowContext FIRST
        for action in ctx.drain_scheduled() {
            match action {
                ScheduledAction::Activity { id, activity } => {
                    let ev = dispatch_activity(
                        workflow_id,
                        &id,
                        &activity,
                        journal,
                        activities,
                        clock,
                        &mut cursor,
                        &mut events_since_snapshot,
                    )?;
                    event_queue.push_back(ev);
                }
                ScheduledAction::Timer { id, duration } => {
                    dispatch_timer(
                        workflow_id,
                        &id,
                        duration,
                        journal,
                        clock,
                        &mut cursor,
                        &mut events_since_snapshot,
                    )?;
                }
                ScheduledAction::Child {
                    id,
                    workflow_type,
                    input,
                } => {
                    append_at_cursor(
                        journal,
                        workflow_id,
                        &mut cursor,
                        clock.now_ms(),
                        "child_started",
                        serde_json::to_string(&ChildStartedData {
                            id,
                            workflow_type,
                            input,
                        })
                        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                    )?;
                    events_since_snapshot += 1;
                }
            }
        }

        // Process step decision
        match decision {
            StepDecision::Continue => {
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "continue",
                    "",
                )?;
                events_since_snapshot += 1;
            }
            StepDecision::ScheduleActivity { id, activity } => {
                let ev = dispatch_activity(
                    workflow_id,
                    &id,
                    &activity,
                    journal,
                    activities,
                    clock,
                    &mut cursor,
                    &mut events_since_snapshot,
                )?;
                event_queue.push_back(ev);
            }
            StepDecision::ScheduleTimer { id, duration } => {
                dispatch_timer(
                    workflow_id,
                    &id,
                    duration,
                    journal,
                    clock,
                    &mut cursor,
                    &mut events_since_snapshot,
                )?;
            }
            StepDecision::PauseForApproval { reason } => {
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "pause_for_approval",
                    serde_json::to_string(&PauseForApprovalData {
                        reason: reason.clone(),
                    })
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                )?;
                if options.snapshot_at_blocked {
                    snapshot_if_contiguous(journal, workflow_id, &cursor, &state)?;
                }
                return Ok(RunnerOutcome::Blocked(format!(
                    "pause_for_approval: {reason}"
                )));
            }
            StepDecision::Complete(output) => {
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "workflow_completed",
                    serde_json::to_string(&output)
                        .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                )?;
                snapshot_if_contiguous(journal, workflow_id, &cursor, &state)?;
                return Ok(RunnerOutcome::Completed(output));
            }
            StepDecision::Fail { code, reason } => {
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "workflow_failed",
                    serde_json::to_string(&WorkflowFailedData {
                        code,
                        reason: reason.clone(),
                    })
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                )?;
                return Ok(RunnerOutcome::Failed { code, reason });
            }
            StepDecision::StartChild {
                id,
                workflow_type,
                input,
            } => {
                append_at_cursor(
                    journal,
                    workflow_id,
                    &mut cursor,
                    clock.now_ms(),
                    "child_started",
                    serde_json::to_string(&ChildStartedData {
                        id,
                        workflow_type,
                        input,
                    })
                    .map_err(|e| RunnerError::Serialization(e.to_string()))?,
                )?;
                events_since_snapshot += 1;
            }
        }

        // Finding 1: Periodic snapshots are ONLY taken at a quiescent point!
        // (no events remaining in event_queue)
        if options.snapshot_interval > 0
            && events_since_snapshot >= options.snapshot_interval
            && event_queue.is_empty()
        {
            snapshot_if_contiguous(journal, workflow_id, &cursor, &state)?;
            events_since_snapshot = 0;
        }
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::activity::{CostRequest, CostResult, HerdrRequest, HerdrResult};
    use crate::engine::testkit::{InMemoryJournal, VirtualClock};
    use std::sync::Mutex;
    use std::time::Instant;

    /// Thread-safe mock activities implementing [`Activity`].
    #[derive(Default)]
    struct MockActivityRegistry {
        canned: Mutex<Vec<(ActivityRequest, ActivityResult)>>,
        calls: Mutex<Vec<ActivityRequest>>,
    }

    impl MockActivityRegistry {
        fn new() -> Self {
            Self::default()
        }

        fn register(&self, req: ActivityRequest, res: ActivityResult) {
            self.canned.lock().unwrap().push((req, res));
        }

        fn count_for(&self, req: &ActivityRequest) -> usize {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|r| *r == req)
                .count()
        }
    }

    impl Activity for MockActivityRegistry {
        fn execute(&self, req: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
            self.calls.lock().unwrap().push(req.clone());
            if let Some((_, res)) = self.canned.lock().unwrap().iter().find(|(r, _)| r == req) {
                Ok(res.clone())
            } else {
                Err(ActivityError::NotFound(format!(
                    "unregistered request: {req:?}"
                )))
            }
        }
    }

    // ------------------------------------------------------------------------
    // Test Workflow 1: Long 7200s Timer Workflow (Acceptance b)
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TimerWorkflowState {
        timer_completed: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TimerWorkflow {
        timer_duration_secs: u64,
    }

    impl Workflow for TimerWorkflow {
        type Input = u64;
        type State = TimerWorkflowState;
        type Output = String;

        fn step(
            &self,
            state: &mut Self::State,
            _ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => StepDecision::ScheduleTimer {
                    id: "long_timer".to_string(),
                    duration: Duration::from_secs(self.timer_duration_secs),
                },
                Some(WorkflowEvent::TimerFired { id }) => {
                    if id == "long_timer" {
                        state.timer_completed = true;
                        StepDecision::Complete("timer_finished".to_string())
                    } else {
                        StepDecision::Fail {
                            code: 1,
                            reason: format!("unexpected timer: {id}"),
                        }
                    }
                }
                _ => StepDecision::Fail {
                    code: 2,
                    reason: "unexpected event".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_virtual_clock_7200s_timer_completes_under_50ms() {
        let workflow = TimerWorkflow {
            timer_duration_secs: 7200,
        };
        let initial_state = TimerWorkflowState {
            timer_completed: false,
        };
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(1000);

        let start_instant = Instant::now();
        let outcome = run(
            &workflow,
            "wf_timer_7200",
            initial_state,
            &mut journal,
            &mut activities,
            &mut clock,
        )
        .expect("run should succeed");

        let elapsed = start_instant.elapsed();

        assert_eq!(
            outcome,
            RunnerOutcome::Completed("timer_finished".to_string())
        );
        assert!(
            elapsed < Duration::from_millis(50),
            "7200s timer took {:?}, expected <50ms",
            elapsed
        );
        assert_eq!(clock.now_ms(), 1000 + 7200 * 1000);

        let events = journal
            .load_events("wf_timer_7200", 1)
            .expect("events load");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, "timer_scheduled");
        assert_eq!(events[1].kind, "timer_fired");
        assert_eq!(events[2].kind, "workflow_completed");
    }

    // ------------------------------------------------------------------------
    // Test Workflow 2: Mid-run Kill & Resume against Uninterrupted Reference (Acceptance a & Finding 6)
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct MidrunKillState {
        act1_result: Option<f64>,
        act2_result: Option<f64>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct MidrunKillWorkflow;

    impl Workflow for MidrunKillWorkflow {
        type Input = ();
        type State = MidrunKillState;
        type Output = String;

        fn step(
            &self,
            state: &mut Self::State,
            _ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => StepDecision::ScheduleActivity {
                    id: "act_1".to_string(),
                    activity: ActivityRequest::Cost(CostRequest::QuerySpend {
                        session_pattern: "session_1".to_string(),
                    }),
                },
                Some(WorkflowEvent::ActivityCompleted { id, result }) => {
                    if id == "act_1" {
                        if let ActivityResult::Cost(CostResult::Spend(v)) = result {
                            state.act1_result = Some(v);
                            StepDecision::ScheduleActivity {
                                id: "act_2".to_string(),
                                activity: ActivityRequest::Cost(CostRequest::QuerySpend {
                                    session_pattern: "session_2".to_string(),
                                }),
                            }
                        } else {
                            StepDecision::Fail {
                                code: 1,
                                reason: "bad act_1".to_string(),
                            }
                        }
                    } else if id == "act_2" {
                        if let ActivityResult::Cost(CostResult::Spend(v)) = result {
                            state.act2_result = Some(v);
                            StepDecision::Complete(format!(
                                "total: {}",
                                state.act1_result.unwrap_or(0.0) + v
                            ))
                        } else {
                            StepDecision::Fail {
                                code: 2,
                                reason: "bad act_2".to_string(),
                            }
                        }
                    } else {
                        StepDecision::Fail {
                            code: 3,
                            reason: "unknown act".to_string(),
                        }
                    }
                }
                _ => StepDecision::Fail {
                    code: 4,
                    reason: "unexpected".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_kill_and_resume_preserves_state_and_event_sequence() {
        let workflow = MidrunKillWorkflow;
        let initial_state = MidrunKillState {
            act1_result: None,
            act2_result: None,
        };

        let req1 = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "session_1".to_string(),
        });
        let res1 = ActivityResult::Cost(CostResult::Spend(10.0));
        let req2 = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "session_2".to_string(),
        });
        let res2 = ActivityResult::Cost(CostResult::Spend(20.0));

        // --- Run 1: Uninterrupted Reference Run ---
        let mut journal_ref = InMemoryJournal::new();
        let mut activities_ref = MockActivityRegistry::new();
        activities_ref.register(req1.clone(), res1.clone());
        activities_ref.register(req2.clone(), res2.clone());
        let mut clock_ref = VirtualClock::new(5000);

        let outcome_ref = run(
            &workflow,
            "ref_wf",
            initial_state.clone(),
            &mut journal_ref,
            &mut activities_ref,
            &mut clock_ref,
        )
        .expect("ref run should succeed");

        assert_eq!(
            outcome_ref,
            RunnerOutcome::Completed("total: 30".to_string())
        );
        let ref_events = journal_ref
            .load_events("ref_wf", 1)
            .expect("load ref events");
        let ref_snap = journal_ref
            .load_latest_snapshot("ref_wf")
            .expect("load ref snap")
            .unwrap();

        // --- Run 2: True Mid-run Kill ---
        // Pre-populate journal with act_1 scheduled and completed (NO snapshot exists yet!)
        let mut journal_interrupted = InMemoryJournal::new();
        let mut activities_interrupted = MockActivityRegistry::new();
        activities_interrupted.register(req1.clone(), res1.clone());
        activities_interrupted.register(req2.clone(), res2);
        let mut clock_interrupted = VirtualClock::new(5000);

        journal_interrupted
            .append_event(
                "int_wf",
                &EventRecord::new(
                    1,
                    5000,
                    "activity_scheduled",
                    serde_json::to_string(&ActivityScheduledData {
                        id: "act_1".to_string(),
                        activity: serde_json::to_value(&req1).unwrap_or_default(),
                    })
                    .unwrap(),
                ),
            )
            .unwrap();
        journal_interrupted
            .append_event(
                "int_wf",
                &EventRecord::new(
                    2,
                    5000,
                    "activity_completed",
                    serde_json::to_string(&ActivityCompletedData {
                        id: "act_1".to_string(),
                        result: serde_json::to_value(&res1).unwrap_or_default(),
                    })
                    .unwrap(),
                ),
            )
            .unwrap();

        // Resume with fresh runner from initial_state
        let outcome_resumed = run(
            &workflow,
            "int_wf",
            initial_state,
            &mut journal_interrupted,
            &mut activities_interrupted,
            &mut clock_interrupted,
        )
        .expect("resumed run should complete");

        assert_eq!(
            outcome_resumed,
            RunnerOutcome::Completed("total: 30".to_string())
        );

        // ACT 1 WAS REPLAYED WITHOUT RE-EXECUTION!
        assert_eq!(activities_interrupted.count_for(&req1), 0);
        // Only ACT 2 was executed in forward execution!
        assert_eq!(activities_interrupted.count_for(&req2), 1);

        // Verify final state snapshot equivalence
        let int_snap = journal_interrupted
            .load_latest_snapshot("int_wf")
            .expect("load int snap")
            .unwrap();
        assert_eq!(int_snap.state_json, ref_snap.state_json);

        // Verify exact journal event sequence equivalence
        let int_events = journal_interrupted
            .load_events("int_wf", 1)
            .expect("load int events");
        assert_eq!(int_events.len(), ref_events.len());
        for (i, (int_ev, ref_ev)) in int_events.iter().zip(ref_events.iter()).enumerate() {
            assert_eq!(
                int_ev.kind, ref_ev.kind,
                "event {i} kind mismatch: int={}, ref={}",
                int_ev.kind, ref_ev.kind
            );
            assert_eq!(
                int_ev.payload_json, ref_ev.payload_json,
                "event {i} payload mismatch"
            );
            assert_eq!(int_ev.seq, ref_ev.seq, "event {i} seq mismatch");
        }
    }

    // ------------------------------------------------------------------------
    // Test Workflow 3: Continue With Ctx Activity Order & Replay
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ContinueWithCtxState {
        order: Vec<String>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ContinueWithCtxWorkflow;

    impl Workflow for ContinueWithCtxWorkflow {
        type Input = ();
        type State = ContinueWithCtxState;
        type Output = Vec<String>;

        fn step(
            &self,
            state: &mut Self::State,
            ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => {
                    state.order.push("initial_step".to_string());
                    ctx.schedule_activity(
                        "act_a",
                        ActivityRequest::Herdr(HerdrRequest::Stop {
                            name: "w1".to_string(),
                        }),
                    );
                    StepDecision::Continue
                }
                Some(WorkflowEvent::ActivityCompleted { id, .. }) => {
                    state.order.push(format!("completed_{id}"));
                    StepDecision::Complete(state.order.clone())
                }
                _ => StepDecision::Fail {
                    code: 1,
                    reason: "unexpected".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_continue_with_ctx_activity_maintains_deterministic_order_and_replay() {
        let workflow = ContinueWithCtxWorkflow;
        let initial_state = ContinueWithCtxState { order: Vec::new() };

        let req = ActivityRequest::Herdr(HerdrRequest::Stop {
            name: "w1".to_string(),
        });
        let res = ActivityResult::Herdr(HerdrResult::Stopped {
            name: "w1".to_string(),
        });

        // --- Run 1: Uninterrupted Reference Run ---
        let mut journal_ref = InMemoryJournal::new();
        let mut activities_ref = MockActivityRegistry::new();
        activities_ref.register(req.clone(), res.clone());
        let mut clock_ref = VirtualClock::new(100);

        let outcome_ref = run(
            &workflow,
            "c_ctx_ref",
            initial_state.clone(),
            &mut journal_ref,
            &mut activities_ref,
            &mut clock_ref,
        )
        .expect("ref run");

        assert_eq!(
            outcome_ref,
            RunnerOutcome::Completed(vec![
                "initial_step".to_string(),
                "completed_act_a".to_string()
            ])
        );

        let ref_events = journal_ref
            .load_events("c_ctx_ref", 1)
            .expect("load ref events");

        // --- Run 2: Interrupted Run (Kill after act_a and continue are journaled) ---
        let mut journal_int = InMemoryJournal::new();
        let mut activities_int = MockActivityRegistry::new();
        activities_int.register(req.clone(), res.clone());
        let mut clock_int = VirtualClock::new(100);

        // Pre-populate with first 3 events (act scheduled, act completed, continue)
        for ev in ref_events.iter().take(3) {
            journal_int.append_event("c_ctx_int", ev).unwrap();
        }

        let outcome_resumed = run(
            &workflow,
            "c_ctx_int",
            initial_state,
            &mut journal_int,
            &mut activities_int,
            &mut clock_int,
        )
        .expect("resumed run");

        assert_eq!(outcome_resumed, outcome_ref);
        // Activity executed 0 times during replay!
        assert_eq!(activities_int.count_for(&req), 0);

        let int_events = journal_int
            .load_events("c_ctx_int", 1)
            .expect("load int events");
        assert_eq!(int_events.len(), ref_events.len());
        for (i, (int_ev, ref_ev)) in int_events.iter().zip(ref_events.iter()).enumerate() {
            assert_eq!(int_ev.kind, ref_ev.kind, "event {i} kind mismatch");
            assert_eq!(int_ev.seq, ref_ev.seq, "event {i} seq mismatch");
        }
    }

    // ------------------------------------------------------------------------
    // Test Workflow 4: Re-armed Timers with Same ID (Finding 1)
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct PollTimerState {
        poll_count: usize,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct PollTimerWorkflow {
        target_polls: usize,
    }

    impl Workflow for PollTimerWorkflow {
        type Input = ();
        type State = PollTimerState;
        type Output = usize;

        fn step(
            &self,
            state: &mut Self::State,
            _ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => StepDecision::ScheduleTimer {
                    id: "poll".to_string(),
                    duration: Duration::from_secs(60),
                },
                Some(WorkflowEvent::TimerFired { id }) => {
                    if id == "poll" {
                        state.poll_count += 1;
                        if state.poll_count >= self.target_polls {
                            StepDecision::Complete(state.poll_count)
                        } else {
                            StepDecision::ScheduleTimer {
                                id: "poll".to_string(),
                                duration: Duration::from_secs(60),
                            }
                        }
                    } else {
                        StepDecision::Fail {
                            code: 1,
                            reason: "unknown timer".to_string(),
                        }
                    }
                }
                _ => StepDecision::Fail {
                    code: 2,
                    reason: "bad ev".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_rearmed_timer_with_same_id_fires_on_every_loop() {
        let workflow = PollTimerWorkflow { target_polls: 3 };
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(1000);

        let outcome = run(
            &workflow,
            "poll_wf",
            PollTimerState { poll_count: 0 },
            &mut journal,
            &mut activities,
            &mut clock,
        )
        .expect("should loop and complete");

        assert_eq!(outcome, RunnerOutcome::Completed(3));
        assert_eq!(clock.now_ms(), 1000 + 3 * 60 * 1000);

        let events = journal.load_events("poll_wf", 1).unwrap();
        let timer_fired_count = events.iter().filter(|e| e.kind == "timer_fired").count();
        assert_eq!(
            timer_fired_count, 3,
            "expected exactly 3 timer_fired events"
        );
    }

    #[test]
    fn test_midstep_kill_on_rearmed_timer_resumes_cleanly() {
        let workflow = PollTimerWorkflow { target_polls: 2 };
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(1000);

        // Pre-populate journal: poll 1 scheduled (seq 1), fired (seq 2)
        // Kill lands right after poll 1 fired, before poll 2 is journaled!
        journal
            .append_event(
                "poll_midstep",
                &EventRecord::new(
                    1,
                    1000,
                    "timer_scheduled",
                    serde_json::to_string(&TimerScheduledData {
                        id: "poll".to_string(),
                        duration_ms: 60 * 1000,
                    })
                    .unwrap(),
                ),
            )
            .unwrap();
        journal
            .append_event(
                "poll_midstep",
                &EventRecord::new(
                    2,
                    1060000,
                    "timer_fired",
                    serde_json::to_string(&TimerFiredData {
                        id: "poll".to_string(),
                    })
                    .unwrap(),
                ),
            )
            .unwrap();

        // Resumed runner MUST recognize the 2nd emission of "poll" was not journaled and dispatch it!
        let outcome = run(
            &workflow,
            "poll_midstep",
            PollTimerState { poll_count: 0 },
            &mut journal,
            &mut activities,
            &mut clock,
        )
        .expect("resumed run should complete target polls");

        assert_eq!(outcome, RunnerOutcome::Completed(2));
    }

    // ------------------------------------------------------------------------
    // Test Workflow 5: Kill Between Ctx Action and Decision (Finding 2)
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct CtxAndDecisionState {
        act_done: bool,
        timer_done: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct CtxAndDecisionWorkflow;

    impl Workflow for CtxAndDecisionWorkflow {
        type Input = ();
        type State = CtxAndDecisionState;
        type Output = String;

        fn step(
            &self,
            state: &mut Self::State,
            ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => {
                    ctx.schedule_activity(
                        "act_1",
                        ActivityRequest::Herdr(HerdrRequest::Stop {
                            name: "w1".to_string(),
                        }),
                    );
                    StepDecision::ScheduleTimer {
                        id: "timer_1".to_string(),
                        duration: Duration::from_secs(100),
                    }
                }
                Some(WorkflowEvent::ActivityCompleted { id, .. }) => {
                    if id == "act_1" {
                        state.act_done = true;
                    }
                    if state.act_done && state.timer_done {
                        StepDecision::Complete("both_finished".to_string())
                    } else {
                        StepDecision::Continue
                    }
                }
                Some(WorkflowEvent::TimerFired { id }) => {
                    if id == "timer_1" {
                        state.timer_done = true;
                    }
                    if state.act_done && state.timer_done {
                        StepDecision::Complete("both_finished".to_string())
                    } else {
                        StepDecision::Continue
                    }
                }
                _ => StepDecision::Fail {
                    code: 1,
                    reason: "bad ev".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_kill_between_ctx_action_and_decision_dispatches_decision_on_resume() {
        let workflow = CtxAndDecisionWorkflow;
        let initial_state = CtxAndDecisionState {
            act_done: false,
            timer_done: false,
        };

        let req = ActivityRequest::Herdr(HerdrRequest::Stop {
            name: "w1".to_string(),
        });
        let res = ActivityResult::Herdr(HerdrResult::Stopped {
            name: "w1".to_string(),
        });

        // --- Pre-populate journal: act_1 scheduled and completed, but kill before timer_1 was journaled! ---
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        activities.register(req.clone(), res.clone());
        let mut clock = VirtualClock::new(1000);

        journal
            .append_event(
                "ctx_dec_int",
                &EventRecord::new(
                    1,
                    1000,
                    "activity_scheduled",
                    serde_json::to_string(&ActivityScheduledData {
                        id: "act_1".to_string(),
                        activity: serde_json::to_value(&req).unwrap_or_default(),
                    })
                    .unwrap(),
                ),
            )
            .unwrap();
        journal
            .append_event(
                "ctx_dec_int",
                &EventRecord::new(
                    2,
                    1000,
                    "activity_completed",
                    serde_json::to_string(&ActivityCompletedData {
                        id: "act_1".to_string(),
                        result: serde_json::to_value(&res).unwrap_or_default(),
                    })
                    .unwrap(),
                ),
            )
            .unwrap();

        // Resumed runner MUST recognize timer_1 was emitted on Step 0 but never journaled,
        // and dispatch timer_1!
        let outcome = run(
            &workflow,
            "ctx_dec_int",
            initial_state,
            &mut journal,
            &mut activities,
            &mut clock,
        )
        .expect("resumed run should dispatch timer and complete");

        assert_eq!(
            outcome,
            RunnerOutcome::Completed("both_finished".to_string())
        );
        assert_eq!(clock.now_ms(), 1000 + 100 * 1000);
    }

    // ------------------------------------------------------------------------
    // Test Workflow 6: Child Started Never Duplicated on Resume (Finding 3)
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ChildWorkflowState {
        approved: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ChildWorkflow;

    impl Workflow for ChildWorkflow {
        type Input = ();
        type State = ChildWorkflowState;
        type Output = String;

        fn step(
            &self,
            state: &mut Self::State,
            ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => {
                    ctx.start_child("c1", "single_worker", serde_json::json!({ "exit": 0 }));
                    StepDecision::PauseForApproval {
                        reason: "wait approval".to_string(),
                    }
                }
                Some(WorkflowEvent::ApprovalResolved { approved, .. }) => {
                    state.approved = approved;
                    StepDecision::Complete("done_child".to_string())
                }
                _ => StepDecision::Fail {
                    code: 1,
                    reason: "bad ev".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_ctx_child_not_duplicated_on_resume() {
        let workflow = ChildWorkflow;
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(100);

        // 1. Initial run schedules child and blocks on approval
        let outcome1 = run(
            &workflow,
            "child_wf",
            ChildWorkflowState { approved: false },
            &mut journal,
            &mut activities,
            &mut clock,
        )
        .expect("run to blocked");

        assert!(matches!(outcome1, RunnerOutcome::Blocked(_)));

        let events_after_block = journal.load_events("child_wf", 1).unwrap();
        let child_starts_1 = events_after_block
            .iter()
            .filter(|e| e.kind == "child_started")
            .count();
        assert_eq!(child_starts_1, 1, "expected 1 child_started");

        // 2. Resume with approval: child_started MUST NOT be emitted again!
        let outcome2 = resume(
            &workflow,
            "child_wf",
            &mut journal,
            &mut activities,
            &mut clock,
            vec![WorkflowEvent::ApprovalResolved {
                approved: true,
                reason: None,
            }],
        )
        .expect("resume");

        assert_eq!(outcome2, RunnerOutcome::Completed("done_child".to_string()));

        let events_after_resume = journal.load_events("child_wf", 1).unwrap();
        let child_starts_2 = events_after_resume
            .iter()
            .filter(|e| e.kind == "child_started")
            .count();
        assert_eq!(child_starts_2, 1, "child_started was duplicated on resume!");
    }

    // ------------------------------------------------------------------------
    // Test Workflow 7: Resuming on Reused Clock Exercises Dedup (Finding 3)
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ReusedClockState {
        approved: bool,
        timer_fired_count: usize,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ReusedClockWorkflow;

    impl Workflow for ReusedClockWorkflow {
        type Input = ();
        type State = ReusedClockState;
        type Output = usize;

        fn step(
            &self,
            state: &mut Self::State,
            ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => {
                    ctx.schedule_timer("timeout", Duration::from_secs(600));
                    StepDecision::PauseForApproval {
                        reason: "need approval".to_string(),
                    }
                }
                Some(WorkflowEvent::ApprovalResolved { approved, .. }) => {
                    state.approved = approved;
                    StepDecision::Continue
                }
                Some(WorkflowEvent::TimerFired { id }) => {
                    if id == "timeout" {
                        state.timer_fired_count += 1;
                        StepDecision::Complete(state.timer_fired_count)
                    } else {
                        StepDecision::Fail {
                            code: 1,
                            reason: "unknown timer".to_string(),
                        }
                    }
                }
                _ => StepDecision::Fail {
                    code: 2,
                    reason: "bad ev".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_resume_on_reused_clock_does_not_duplicate_timer() {
        let workflow = ReusedClockWorkflow;
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        let mut reused_clock = VirtualClock::new(1000);

        // 1. Initial run schedules timer and blocks on approval
        let outcome1 = run(
            &workflow,
            "reused_clock_wf",
            ReusedClockState {
                approved: false,
                timer_fired_count: 0,
            },
            &mut journal,
            &mut activities,
            &mut reused_clock,
        )
        .expect("run to blocked");

        assert!(matches!(outcome1, RunnerOutcome::Blocked(_)));

        // 2. Resume on the EXACT SAME clock with approval
        let outcome2 = resume(
            &workflow,
            "reused_clock_wf",
            &mut journal,
            &mut activities,
            &mut reused_clock,
            vec![WorkflowEvent::ApprovalResolved {
                approved: true,
                reason: None,
            }],
        )
        .expect("resume call");

        // The timer fired exactly once and completed!
        assert_eq!(outcome2, RunnerOutcome::Completed(1));

        // Journal has exactly ONE timer_fired event!
        let events = journal.load_events("reused_clock_wf", 1).unwrap();
        let timer_fired_count = events.iter().filter(|e| e.kind == "timer_fired").count();
        assert_eq!(timer_fired_count, 1, "timer fired more than once!");
    }

    // ------------------------------------------------------------------------
    // Test Workflow 8: Periodic Snapshot at Quiescence and Resume
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct QuiescentTimerState {
        stage: usize,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct QuiescentTimerWorkflow;

    impl Workflow for QuiescentTimerWorkflow {
        type Input = ();
        type State = QuiescentTimerState;
        type Output = String;

        fn step(
            &self,
            state: &mut Self::State,
            ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => {
                    ctx.schedule_timer("t1", Duration::from_secs(500));
                    ctx.schedule_activity(
                        "act_q",
                        ActivityRequest::Herdr(HerdrRequest::Stop {
                            name: "wq".to_string(),
                        }),
                    );
                    StepDecision::Continue
                }
                Some(WorkflowEvent::ActivityCompleted { id, .. }) => {
                    if id == "act_q" {
                        state.stage = 1;
                        StepDecision::PauseForApproval {
                            reason: "at quiescence with pending timer".to_string(),
                        }
                    } else {
                        StepDecision::Fail {
                            code: 1,
                            reason: "unknown act".to_string(),
                        }
                    }
                }
                Some(WorkflowEvent::ApprovalResolved { .. }) => {
                    state.stage = 2;
                    StepDecision::Continue
                }
                Some(WorkflowEvent::TimerFired { id }) => {
                    if id == "t1" {
                        state.stage = 3;
                        StepDecision::Complete("finished".to_string())
                    } else {
                        StepDecision::Fail {
                            code: 2,
                            reason: "unknown timer".to_string(),
                        }
                    }
                }
                _ => StepDecision::Fail {
                    code: 3,
                    reason: "bad ev".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_periodic_snapshot_quiescent_guard_and_resume() {
        let workflow = QuiescentTimerWorkflow;
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        activities.register(
            ActivityRequest::Herdr(HerdrRequest::Stop {
                name: "wq".to_string(),
            }),
            ActivityResult::Herdr(HerdrResult::Stopped {
                name: "wq".to_string(),
            }),
        );
        let mut clock = VirtualClock::new(1000);

        let options = RunnerOptions {
            max_steps: 100,
            snapshot_interval: 2,
            snapshot_at_blocked: true,
        };

        // 1. Run until Blocked on approval
        let outcome1 = run_until_blocked(
            &workflow,
            "q_wf",
            Some(QuiescentTimerState { stage: 0 }),
            &mut journal,
            &mut activities,
            &mut clock,
            &options,
            Vec::new(),
        )
        .expect("run to blocked");

        assert!(matches!(outcome1, RunnerOutcome::Blocked(_)));

        // Verify snapshot was taken at quiescence
        let snap = journal
            .load_latest_snapshot("q_wf")
            .expect("load snap")
            .expect("must have snapshot");
        assert!(snap.seq >= 2);

        // 2. Kill and resume with fresh runner and approval
        let mut fresh_clock = VirtualClock::new(1000);
        let outcome2 = resume(
            &workflow,
            "q_wf",
            &mut journal,
            &mut activities,
            &mut fresh_clock,
            vec![WorkflowEvent::ApprovalResolved {
                approved: true,
                reason: None,
            }],
        )
        .expect("resume");

        assert_eq!(outcome2, RunnerOutcome::Completed("finished".to_string()));
        assert_eq!(fresh_clock.now_ms(), 1000 + 500 * 1000);
    }

    // ------------------------------------------------------------------------
    // Test Workflow 8: Pending Timers Across Mid-run Snapshot (Findings 2 & 5)
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TimerAndActivityState {
        timer_fired: bool,
        act_done: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TimerAndActivityWorkflow;

    impl Workflow for TimerAndActivityWorkflow {
        type Input = ();
        type State = TimerAndActivityState;
        type Output = String;

        fn step(
            &self,
            state: &mut Self::State,
            _ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => StepDecision::ScheduleTimer {
                    id: "wf_timer".to_string(),
                    duration: Duration::from_secs(1000),
                },
                Some(WorkflowEvent::TimerFired { id }) => {
                    if id == "wf_timer" {
                        state.timer_fired = true;
                        StepDecision::ScheduleActivity {
                            id: "post_timer_act".to_string(),
                            activity: ActivityRequest::Herdr(HerdrRequest::Stop {
                                name: "worker_x".to_string(),
                            }),
                        }
                    } else {
                        StepDecision::Fail {
                            code: 1,
                            reason: "unknown timer".to_string(),
                        }
                    }
                }
                Some(WorkflowEvent::ActivityCompleted { id, .. }) => {
                    if id == "post_timer_act" {
                        state.act_done = true;
                        StepDecision::Complete("done".to_string())
                    } else {
                        StepDecision::Fail {
                            code: 2,
                            reason: "unknown act".to_string(),
                        }
                    }
                }
                _ => StepDecision::Fail {
                    code: 3,
                    reason: "bad event".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_pending_timer_across_snapshot_restores_with_remaining_duration() {
        let workflow = TimerAndActivityWorkflow;
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        let req = ActivityRequest::Herdr(HerdrRequest::Stop {
            name: "worker_x".to_string(),
        });
        let res = ActivityResult::Herdr(HerdrResult::Stopped {
            name: "worker_x".to_string(),
        });
        activities.register(req, res);

        // Pre-populate journal: timer scheduled at ts=1000 with duration=1000s (deadline=1001000)
        journal
            .append_event(
                "timer_snap_wf",
                &EventRecord::new(
                    1,
                    1000,
                    "timer_scheduled",
                    serde_json::to_string(&TimerScheduledData {
                        id: "wf_timer".to_string(),
                        duration_ms: 1000 * 1000,
                    })
                    .unwrap(),
                ),
            )
            .unwrap();
        journal
            .append_snapshot(
                "timer_snap_wf",
                &SnapshotRecord::new(
                    1,
                    serde_json::to_string(&TimerAndActivityState {
                        timer_fired: false,
                        act_done: false,
                    })
                    .unwrap(),
                ),
            )
            .unwrap();

        // Advance clock to 1000 + 400s = 401000 (400s elapsed, 600s remaining)
        let mut resuming_clock = VirtualClock::new(401000);

        let outcome = run_until_blocked(
            &workflow,
            "timer_snap_wf",
            None,
            &mut journal,
            &mut activities,
            &mut resuming_clock,
            &RunnerOptions::default(),
            Vec::new(),
        )
        .expect("should advance and complete");

        assert_eq!(outcome, RunnerOutcome::Completed("done".to_string()));
        assert_eq!(resuming_clock.now_ms(), 1001000);
    }

    // ------------------------------------------------------------------------
    // Test Workflow 9: Resuming Completed Workflow Returns Completed (Finding 4)
    // ------------------------------------------------------------------------

    #[test]
    fn test_resume_after_completed_returns_completed_not_blocked() {
        let workflow = TimerWorkflow {
            timer_duration_secs: 10,
        };
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(0);

        let outcome = run(
            &workflow,
            "completed_wf",
            TimerWorkflowState {
                timer_completed: false,
            },
            &mut journal,
            &mut activities,
            &mut clock,
        )
        .expect("run to completion");

        assert_eq!(
            outcome,
            RunnerOutcome::Completed("timer_finished".to_string())
        );

        let event_count_before = journal.load_events("completed_wf", 1).unwrap().len();

        let mut clock2 = VirtualClock::new(100);
        let outcome2 = resume(
            &workflow,
            "completed_wf",
            &mut journal,
            &mut activities,
            &mut clock2,
            vec![],
        )
        .expect("resume call");

        assert_eq!(
            outcome2,
            RunnerOutcome::Completed("timer_finished".to_string())
        );
        let event_count_after = journal.load_events("completed_wf", 1).unwrap().len();
        assert_eq!(event_count_after, event_count_before);
    }

    // ------------------------------------------------------------------------
    // Test Workflow 10: Corrupted Payload Fails Fast (Finding 7)
    // ------------------------------------------------------------------------

    #[test]
    fn test_corrupt_payload_returns_serialization_error() {
        let workflow = TimerWorkflow {
            timer_duration_secs: 10,
        };
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(0);

        journal
            .append_event(
                "corrupt_wf",
                &EventRecord::new(1, 0, "activity_scheduled", "NOT_JSON"),
            )
            .unwrap();

        let res = run(
            &workflow,
            "corrupt_wf",
            TimerWorkflowState {
                timer_completed: false,
            },
            &mut journal,
            &mut activities,
            &mut clock,
        );

        assert!(matches!(res, Err(RunnerError::Serialization(_))));
    }

    // ------------------------------------------------------------------------
    // Test Workflow 11: Workflow Failure Recorded in Journal
    // ------------------------------------------------------------------------

    #[test]
    fn test_workflow_failure_recorded_in_journal() {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        struct FailingWorkflow;

        impl Workflow for FailingWorkflow {
            type Input = ();
            type State = ();
            type Output = ();

            fn step(
                &self,
                _state: &mut Self::State,
                _ctx: &mut WorkflowContext,
                _event: Option<WorkflowEvent>,
            ) -> StepDecision<Self::Output> {
                StepDecision::Fail {
                    code: 42,
                    reason: "deliberate test failure".to_string(),
                }
            }
        }

        let workflow = FailingWorkflow;
        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(0);

        let outcome = run(
            &workflow,
            "fail_wf",
            (),
            &mut journal,
            &mut activities,
            &mut clock,
        )
        .expect("run returns outcome");

        assert_eq!(
            outcome,
            RunnerOutcome::Failed {
                code: 42,
                reason: "deliberate test failure".to_string(),
            }
        );

        let events = journal.load_events("fail_wf", 1).expect("load events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "workflow_failed");
        assert!(events[0].payload_json.contains("deliberate test failure"));
    }
    // ------------------------------------------------------------------------
    // Acceptance Test (a): Mid-step kill on re-armed timer with periodic snapshot
    // ------------------------------------------------------------------------

    #[test]
    fn test_midstep_kill_on_rearmed_timer_with_periodic_snapshot_resumes_cleanly() {
        let workflow = PollTimerWorkflow { target_polls: 5 };
        let options = RunnerOptions {
            max_steps: 100,
            snapshot_interval: 2,
            snapshot_at_blocked: true,
        };

        // --- Run 1: Uninterrupted Reference Run ---
        let mut journal_ref = InMemoryJournal::new();
        let mut activities_ref = MockActivityRegistry::new();
        let mut clock_ref = VirtualClock::new(1000);

        let outcome_ref = run_until_blocked(
            &workflow,
            "poll_snap_ref",
            Some(PollTimerState { poll_count: 0 }),
            &mut journal_ref,
            &mut activities_ref,
            &mut clock_ref,
            &options,
            Vec::new(),
        )
        .expect("ref run should complete");

        assert_eq!(outcome_ref, RunnerOutcome::Completed(5));
        let ref_events = journal_ref
            .load_events("poll_snap_ref", 1)
            .expect("load ref events");
        let ref_snap = journal_ref
            .load_latest_snapshot("poll_snap_ref")
            .expect("load ref snap")
            .expect("must have final snapshot");

        // Verify reference run took at least one periodic snapshot before final completion
        assert!(ref_events.len() >= 10);

        // --- Run 2: Interrupted Run ---
        // Truncate journal right after timer_fired (seq 4), with periodic snapshot present at seq 3,
        // killing before timer_scheduled (seq 5) is recorded!
        let mut journal_int = InMemoryJournal::new();
        let mut activities_int = MockActivityRegistry::new();
        let mut clock_int = VirtualClock::new(1000 + 2 * 60 * 1000);

        for ev in ref_events.iter().take(4) {
            journal_int.append_event("poll_snap_int", ev).unwrap();
        }
        // Copy the snapshot taken at seq 3 (poll_count = 1)
        journal_int
            .append_snapshot(
                "poll_snap_int",
                &SnapshotRecord::new(
                    3,
                    serde_json::to_string(&PollTimerState { poll_count: 1 }).unwrap(),
                ),
            )
            .unwrap();

        // Resume with no initial state (must load from snapshot at seq 3)
        let outcome_resumed = run_until_blocked(
            &workflow,
            "poll_snap_int",
            None,
            &mut journal_int,
            &mut activities_int,
            &mut clock_int,
            &options,
            Vec::new(),
        )
        .expect("resumed run should complete target polls");

        assert_eq!(outcome_resumed, outcome_ref);

        let int_events = journal_int
            .load_events("poll_snap_int", 1)
            .expect("load int events");
        assert_eq!(
            int_events.len(),
            ref_events.len(),
            "event count mismatch between resumed and reference run"
        );
        for (i, (int_ev, ref_ev)) in int_events.iter().zip(ref_events.iter()).enumerate() {
            assert_eq!(int_ev.kind, ref_ev.kind, "event {i} kind mismatch");
            assert_eq!(int_ev.seq, ref_ev.seq, "event {i} seq mismatch");
            assert_eq!(
                int_ev.payload_json, ref_ev.payload_json,
                "event {i} payload mismatch"
            );
        }

        let int_snap = journal_int
            .load_latest_snapshot("poll_snap_int")
            .expect("load int snap")
            .expect("must have final snapshot");
        assert_eq!(int_snap.state_json, ref_snap.state_json);
    }

    // ------------------------------------------------------------------------
    // Acceptance Test (b): Two lost side effects in one replay dispatched in order
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TwoLostState {
        act1_done: bool,
        act2_done: bool,
        timer_fired: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TwoLostSideEffectsWorkflow;

    impl Workflow for TwoLostSideEffectsWorkflow {
        type Input = ();
        type State = TwoLostState;
        type Output = String;

        fn step(
            &self,
            state: &mut Self::State,
            ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => {
                    ctx.schedule_activity(
                        "act_1",
                        ActivityRequest::Herdr(HerdrRequest::Stop {
                            name: "w1".to_string(),
                        }),
                    );
                    StepDecision::ScheduleTimer {
                        id: "timer_1".to_string(),
                        duration: Duration::from_secs(100),
                    }
                }
                Some(WorkflowEvent::ActivityCompleted { id, .. }) => {
                    if id == "act_1" {
                        state.act1_done = true;
                        ctx.schedule_activity(
                            "act_2",
                            ActivityRequest::Herdr(HerdrRequest::Stop {
                                name: "w2".to_string(),
                            }),
                        );
                        StepDecision::Continue
                    } else if id == "act_2" {
                        state.act2_done = true;
                        if state.act1_done && state.act2_done && state.timer_fired {
                            StepDecision::Complete("all_done".to_string())
                        } else {
                            StepDecision::Continue
                        }
                    } else {
                        StepDecision::Fail {
                            code: 1,
                            reason: "unknown act".to_string(),
                        }
                    }
                }
                Some(WorkflowEvent::TimerFired { id }) => {
                    if id == "timer_1" {
                        state.timer_fired = true;
                        if state.act1_done && state.act2_done && state.timer_fired {
                            StepDecision::Complete("all_done".to_string())
                        } else {
                            StepDecision::Continue
                        }
                    } else {
                        StepDecision::Fail {
                            code: 2,
                            reason: "unknown timer".to_string(),
                        }
                    }
                }
                _ => StepDecision::Fail {
                    code: 3,
                    reason: "bad ev".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_two_lost_side_effects_in_one_replay_dispatched_in_order() {
        let workflow = TwoLostSideEffectsWorkflow;
        let initial_state = TwoLostState {
            act1_done: false,
            act2_done: false,
            timer_fired: false,
        };

        let req1 = ActivityRequest::Herdr(HerdrRequest::Stop {
            name: "w1".to_string(),
        });
        let res1 = ActivityResult::Herdr(HerdrResult::Stopped {
            name: "w1".to_string(),
        });
        let req2 = ActivityRequest::Herdr(HerdrRequest::Stop {
            name: "w2".to_string(),
        });
        let res2 = ActivityResult::Herdr(HerdrResult::Stopped {
            name: "w2".to_string(),
        });

        // --- Run 1: Uninterrupted Reference Run ---
        let mut journal_ref = InMemoryJournal::new();
        let mut activities_ref = MockActivityRegistry::new();
        activities_ref.register(req1.clone(), res1.clone());
        activities_ref.register(req2.clone(), res2.clone());
        let mut clock_ref = VirtualClock::new(1000);

        let outcome_ref = run(
            &workflow,
            "two_lost_ref",
            initial_state.clone(),
            &mut journal_ref,
            &mut activities_ref,
            &mut clock_ref,
        )
        .expect("ref run should complete");

        assert_eq!(
            outcome_ref,
            RunnerOutcome::Completed("all_done".to_string())
        );
        let ref_events = journal_ref
            .load_events("two_lost_ref", 1)
            .expect("load ref events");

        // --- Run 2: Interrupted Run ---
        // Pre-populate with only act_1 scheduled and completed.
        // Kill lands after act_1 completes, dropping Step 0 decision (timer_1) AND Step 1 ctx action (act_2)!
        let mut journal_int = InMemoryJournal::new();
        let mut activities_int = MockActivityRegistry::new();
        activities_int.register(req1, res1);
        activities_int.register(req2, res2);
        let mut clock_int = VirtualClock::new(1000);

        for ev in ref_events.iter().take(2) {
            journal_int.append_event("two_lost_int", ev).unwrap();
        }

        let outcome_resumed = run(
            &workflow,
            "two_lost_int",
            initial_state,
            &mut journal_int,
            &mut activities_int,
            &mut clock_int,
        )
        .expect("resumed run should dispatch both lost side effects in order and complete");

        assert_eq!(outcome_resumed, outcome_ref);

        let int_events = journal_int
            .load_events("two_lost_int", 1)
            .expect("load int events");
        assert_eq!(
            int_events.len(),
            ref_events.len(),
            "event count mismatch between resumed and reference run"
        );
        for (i, (int_ev, ref_ev)) in int_events.iter().zip(ref_events.iter()).enumerate() {
            assert_eq!(int_ev.kind, ref_ev.kind, "event {i} kind mismatch");
            assert_eq!(int_ev.seq, ref_ev.seq, "event {i} seq mismatch");
            assert_eq!(
                int_ev.payload_json, ref_ev.payload_json,
                "event {i} payload mismatch"
            );
        }
    }

    // ------------------------------------------------------------------------
    // Acceptance Test (c): In-flight activity #2 on a re-issued ID after #1 completed
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ReissuedActivityState {
        first_result: Option<f64>,
        second_result: Option<f64>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ReissuedActivityWorkflow;

    impl Workflow for ReissuedActivityWorkflow {
        type Input = ();
        type State = ReissuedActivityState;
        type Output = String;

        fn step(
            &self,
            state: &mut Self::State,
            _ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => StepDecision::ScheduleActivity {
                    id: "fetch".to_string(),
                    activity: ActivityRequest::Cost(CostRequest::QuerySpend {
                        session_pattern: "req_1".to_string(),
                    }),
                },
                Some(WorkflowEvent::ActivityCompleted { id, result }) => {
                    if id == "fetch" && state.first_result.is_none() {
                        if let ActivityResult::Cost(CostResult::Spend(v)) = result {
                            state.first_result = Some(v);
                            StepDecision::ScheduleActivity {
                                id: "fetch".to_string(),
                                activity: ActivityRequest::Cost(CostRequest::QuerySpend {
                                    session_pattern: "req_2".to_string(),
                                }),
                            }
                        } else {
                            StepDecision::Fail {
                                code: 1,
                                reason: "bad res 1".to_string(),
                            }
                        }
                    } else if id == "fetch" && state.first_result.is_some() {
                        if let ActivityResult::Cost(CostResult::Spend(v)) = result {
                            state.second_result = Some(v);
                            StepDecision::Complete(format!(
                                "first: {}, second: {}",
                                state.first_result.unwrap(),
                                v
                            ))
                        } else {
                            StepDecision::Fail {
                                code: 2,
                                reason: "bad res 2".to_string(),
                            }
                        }
                    } else {
                        StepDecision::Fail {
                            code: 3,
                            reason: "unknown event".to_string(),
                        }
                    }
                }
                _ => StepDecision::Fail {
                    code: 4,
                    reason: "bad ev".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_inflight_activity_on_reissued_id_after_completion() {
        let workflow = ReissuedActivityWorkflow;
        let initial_state = ReissuedActivityState {
            first_result: None,
            second_result: None,
        };

        let req1 = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "req_1".to_string(),
        });
        let res1 = ActivityResult::Cost(CostResult::Spend(10.0));
        let req2 = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "req_2".to_string(),
        });
        let res2 = ActivityResult::Cost(CostResult::Spend(20.0));

        // --- Run 1: Uninterrupted Reference Run ---
        let mut journal_ref = InMemoryJournal::new();
        let mut activities_ref = MockActivityRegistry::new();
        activities_ref.register(req1.clone(), res1.clone());
        activities_ref.register(req2.clone(), res2.clone());
        let mut clock_ref = VirtualClock::new(1000);

        let outcome_ref = run(
            &workflow,
            "reissued_act_ref",
            initial_state.clone(),
            &mut journal_ref,
            &mut activities_ref,
            &mut clock_ref,
        )
        .expect("ref run should complete");

        assert_eq!(
            outcome_ref,
            RunnerOutcome::Completed("first: 10, second: 20".to_string())
        );
        let ref_events = journal_ref
            .load_events("reissued_act_ref", 1)
            .expect("load ref events");

        // --- Run 2: Interrupted Run ---
        // Pre-populate with first 3 events:
        // 1. activity_scheduled fetch (req1)
        // 2. activity_completed fetch (res1)
        // 3. activity_scheduled fetch (req2)
        // Kill lands right after activity #2 is scheduled, before completion is written!
        let mut journal_int = InMemoryJournal::new();
        let mut activities_int = MockActivityRegistry::new();
        activities_int.register(req1, res1);
        activities_int.register(req2, res2);
        let mut clock_int = VirtualClock::new(1000);

        for ev in ref_events.iter().take(3) {
            journal_int.append_event("reissued_act_int", ev).unwrap();
        }

        let outcome_resumed = run(
            &workflow,
            "reissued_act_int",
            initial_state,
            &mut journal_int,
            &mut activities_int,
            &mut clock_int,
        )
        .expect("resumed run should complete in-flight activity #2 without ID collision");

        assert_eq!(outcome_resumed, outcome_ref);

        let int_events = journal_int
            .load_events("reissued_act_int", 1)
            .expect("load int events");
        assert_eq!(
            int_events.len(),
            ref_events.len(),
            "event count mismatch between resumed and reference run"
        );
        for (i, (int_ev, ref_ev)) in int_events.iter().zip(ref_events.iter()).enumerate() {
            assert_eq!(int_ev.kind, ref_ev.kind, "event {i} kind mismatch");
            assert_eq!(int_ev.seq, ref_ev.seq, "event {i} seq mismatch");
            assert_eq!(
                int_ev.payload_json, ref_ev.payload_json,
                "event {i} payload mismatch"
            );
        }
    }
    // ------------------------------------------------------------------------
    // Review Round 1 Finding 1: Pause For Approval Reconciliation by Occurrence Count
    // ------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct MultiPauseState {
        pause_count: usize,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct MultiPauseWorkflow;

    impl Workflow for MultiPauseWorkflow {
        type Input = ();
        type State = MultiPauseState;
        type Output = String;

        fn step(
            &self,
            state: &mut Self::State,
            _ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => {
                    state.pause_count += 1;
                    StepDecision::PauseForApproval {
                        reason: "pause_1".to_string(),
                    }
                }
                Some(WorkflowEvent::ApprovalResolved { approved, .. }) => {
                    if !approved {
                        return StepDecision::Fail {
                            code: 1,
                            reason: "rejected".to_string(),
                        };
                    }
                    if state.pause_count == 1 {
                        state.pause_count += 1;
                        StepDecision::PauseForApproval {
                            reason: "pause_2".to_string(),
                        }
                    } else {
                        StepDecision::Complete(format!("pauses: {}", state.pause_count))
                    }
                }
                _ => StepDecision::Fail {
                    code: 2,
                    reason: "unexpected event".to_string(),
                },
            }
        }
    }

    #[test]
    fn test_kill_on_second_pause_for_approval_with_snapshots_disabled_resumes_cleanly() {
        let workflow = MultiPauseWorkflow;
        let initial_state = MultiPauseState { pause_count: 0 };
        let options = RunnerOptions {
            max_steps: 100,
            snapshot_interval: 0,
            snapshot_at_blocked: false,
        };

        // --- Run 1: Uninterrupted Reference Run to Pause 2 ---
        let mut journal_ref = InMemoryJournal::new();
        let mut activities_ref = MockActivityRegistry::new();
        let mut clock_ref = VirtualClock::new(1000);

        // First step blocks on pause_1
        let outcome_1 = run_until_blocked(
            &workflow,
            "multi_pause_ref",
            Some(initial_state.clone()),
            &mut journal_ref,
            &mut activities_ref,
            &mut clock_ref,
            &options,
            Vec::new(),
        )
        .expect("step to pause 1");
        assert_eq!(
            outcome_1,
            RunnerOutcome::Blocked("pause_for_approval: pause_1".to_string())
        );

        // Resume with approval for pause 1, advances to pause_2
        let outcome_ref = run_until_blocked(
            &workflow,
            "multi_pause_ref",
            Some(initial_state.clone()),
            &mut journal_ref,
            &mut activities_ref,
            &mut clock_ref,
            &options,
            vec![WorkflowEvent::ApprovalResolved {
                approved: true,
                reason: None,
            }],
        )
        .expect("step to pause 2");
        assert_eq!(
            outcome_ref,
            RunnerOutcome::Blocked("pause_for_approval: pause_2".to_string())
        );

        let ref_events = journal_ref
            .load_events("multi_pause_ref", 1)
            .expect("load ref events");
        assert_eq!(ref_events.len(), 3);
        assert_eq!(ref_events[0].kind, "pause_for_approval");
        assert_eq!(ref_events[1].kind, "approval_resolved");
        assert_eq!(ref_events[2].kind, "pause_for_approval");

        // --- Run 2: Interrupted Run ---
        // Pre-populate with first 2 events:
        // seq 1: pause_for_approval (pause_1)
        // seq 2: approval_resolved
        // Kill lands right after approval_resolved, BEFORE pause_for_approval #2 is recorded!
        let mut journal_int = InMemoryJournal::new();
        let mut activities_int = MockActivityRegistry::new();
        let mut clock_int = VirtualClock::new(1000);

        for ev in ref_events.iter().take(2) {
            journal_int.append_event("multi_pause_int", ev).unwrap();
        }

        // Resumed runner: must NOT suppress pause_2 due to a single boolean flag!
        // Occurrence count: pause #1 <= 1 (already journaled), pause #2 > 1 (unjournaled -> dispatched!)
        let outcome_resumed = run_until_blocked(
            &workflow,
            "multi_pause_int",
            Some(initial_state.clone()),
            &mut journal_int,
            &mut activities_int,
            &mut clock_int,
            &options,
            Vec::new(),
        )
        .expect("resumed run should dispatch pause 2 and block");

        assert_eq!(outcome_resumed, outcome_ref);

        let int_events = journal_int
            .load_events("multi_pause_int", 1)
            .expect("load int events");
        assert_eq!(
            int_events.len(),
            ref_events.len(),
            "event count mismatch between resumed and reference run"
        );
        for (i, (int_ev, ref_ev)) in int_events.iter().zip(ref_events.iter()).enumerate() {
            assert_eq!(int_ev.kind, ref_ev.kind, "event {i} kind mismatch");
            assert_eq!(int_ev.seq, ref_ev.seq, "event {i} seq mismatch");
            assert_eq!(
                int_ev.payload_json, ref_ev.payload_json,
                "event {i} payload mismatch"
            );
        }

        // Final check: resume with approval for pause 2 should complete to prove end-to-end correctness
        let outcome_final = run_until_blocked(
            &workflow,
            "multi_pause_int",
            Some(initial_state),
            &mut journal_int,
            &mut activities_int,
            &mut clock_int,
            &options,
            vec![WorkflowEvent::ApprovalResolved {
                approved: true,
                reason: None,
            }],
        )
        .expect("resume to completion");
        assert_eq!(
            outcome_final,
            RunnerOutcome::Completed("pauses: 2".to_string())
        );
    }
    #[test]
    fn test_two_sequential_pause_resolved_cycles_kill_between_dispatches_nothing() {
        let workflow = MultiPauseWorkflow;
        let initial_state = MultiPauseState { pause_count: 0 };
        let options = RunnerOptions {
            max_steps: 100,
            snapshot_interval: 0,
            snapshot_at_blocked: false,
        };

        let mut journal = InMemoryJournal::new();
        let mut activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(1000);

        // 1. Initial run blocks on pause_1 (Cycle 1 start)
        let out1 = run_until_blocked(
            &workflow,
            "two_cycle_wf",
            Some(initial_state.clone()),
            &mut journal,
            &mut activities,
            &mut clock,
            &options,
            Vec::new(),
        )
        .expect("step to pause 1");
        assert_eq!(
            out1,
            RunnerOutcome::Blocked("pause_for_approval: pause_1".to_string())
        );

        // 2. Resolve pause_1 -> advances and blocks on pause_2 (Cycle 1 resolve + Cycle 2 pause)
        let out2 = run_until_blocked(
            &workflow,
            "two_cycle_wf",
            Some(initial_state.clone()),
            &mut journal,
            &mut activities,
            &mut clock,
            &options,
            vec![WorkflowEvent::ApprovalResolved {
                approved: true,
                reason: None,
            }],
        )
        .expect("step to pause 2");
        assert_eq!(
            out2,
            RunnerOutcome::Blocked("pause_for_approval: pause_2".to_string())
        );

        let events_before_kill = journal.load_events("two_cycle_wf", 1).expect("load events");
        assert_eq!(events_before_kill.len(), 3);
        assert_eq!(events_before_kill[0].kind, "pause_for_approval");
        assert_eq!(events_before_kill[1].kind, "approval_resolved");
        assert_eq!(events_before_kill[2].kind, "pause_for_approval");

        // 3. Process is killed between cycle 1 resolve and cycle 2 resolve (while blocked on pause_2).
        // A fresh runner restarts and replays from the journal with no new events.
        // Replay MUST re-dispatch nothing: no new pause_for_approval or other events emitted.
        let mut clock_resumed = VirtualClock::new(1000);
        let out_resumed = run_until_blocked(
            &workflow,
            "two_cycle_wf",
            Some(initial_state.clone()),
            &mut journal,
            &mut activities,
            &mut clock_resumed,
            &options,
            Vec::new(),
        )
        .expect("resumed replay to blocked");

        // Outcome reaches quiescent blocked point without appending duplicate events
        assert_eq!(
            out_resumed,
            RunnerOutcome::Blocked("no pending events or timers".to_string())
        );

        let events_after_replay = journal.load_events("two_cycle_wf", 1).expect("load events");
        assert_eq!(
            events_after_replay.len(),
            3,
            "replay must re-dispatch nothing; journal event count must remain 3"
        );
        for (i, (before, after)) in events_before_kill
            .iter()
            .zip(events_after_replay.iter())
            .enumerate()
        {
            assert_eq!(before.seq, after.seq, "event {i} seq mismatch");
            assert_eq!(before.kind, after.kind, "event {i} kind mismatch");
            assert_eq!(
                before.payload_json, after.payload_json,
                "event {i} payload mismatch"
            );
        }

        // 4. Finally deliver approval for pause_2 (Cycle 2 resolve) -> workflow completes cleanly
        let out_final = run_until_blocked(
            &workflow,
            "two_cycle_wf",
            Some(initial_state),
            &mut journal,
            &mut activities,
            &mut clock_resumed,
            &options,
            vec![WorkflowEvent::ApprovalResolved {
                approved: true,
                reason: None,
            }],
        )
        .expect("resume to completion");
        assert_eq!(out_final, RunnerOutcome::Completed("pauses: 2".to_string()));

        let final_events = journal
            .load_events("two_cycle_wf", 1)
            .expect("load final events");
        assert_eq!(final_events.len(), 5);
        assert_eq!(final_events[3].kind, "approval_resolved");
        assert_eq!(final_events[4].kind, "workflow_completed");
    }

    /// A watcher killed before its first periodic snapshot still left events in
    /// the journal. `resume` used to die with `NoInitialState` on that journal;
    /// a run must persist its starting state so the resume has a replay base.
    #[test]
    fn test_resume_after_kill_before_first_snapshot_finds_the_starting_state() {
        let workflow = MidrunKillWorkflow;
        let initial_state = MidrunKillState {
            act1_result: None,
            act2_result: None,
        };
        let req1 = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "session_1".to_string(),
        });
        let res1 = ActivityResult::Cost(CostResult::Spend(10.0));
        let req2 = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "session_2".to_string(),
        });
        let res2 = ActivityResult::Cost(CostResult::Spend(20.0));

        // The run dies inside its first activity: the journal has the scheduled
        // event but nothing has been checkpointed yet.
        let mut journal = InMemoryJournal::new();
        let mut dying_activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(5000);
        let err = run(
            &workflow,
            "kill_before_snapshot",
            initial_state.clone(),
            &mut journal,
            &mut dying_activities,
            &mut clock,
        )
        .expect_err("the unregistered activity fails the run");
        assert!(matches!(err, RunnerError::Activity(_)), "got {err:?}");
        assert_eq!(
            journal
                .load_events("kill_before_snapshot", 1)
                .expect("events")
                .len(),
            1,
            "the kill point has one event and no checkpoint snapshot"
        );

        // Resume with no initial state, the way `resume` runs after a kill.
        let mut activities = MockActivityRegistry::new();
        activities.register(req1.clone(), res1);
        activities.register(req2.clone(), res2);
        let resumed = resume(
            &workflow,
            "kill_before_snapshot",
            &mut journal,
            &mut activities,
            &mut clock,
            Vec::new(),
        )
        .expect("resume must find the run's starting state instead of failing NoInitialState");
        assert_eq!(resumed, RunnerOutcome::Completed("total: 30".to_string()));
        assert_eq!(activities.count_for(&req2), 1, "forward run executes act 2");
    }

    /// Journal with the production sequence contract: an append takes the next
    /// free slot or is refused with [`JournalError::SequenceConflict`] and
    /// inserts nothing. `pending_foreign` is a second writer's batch, landing
    /// just before this run's first append attempt.
    #[derive(Default)]
    struct RacingJournal {
        events: Vec<EventRecord>,
        snapshots: Vec<SnapshotRecord>,
        pending_foreign: Vec<EventRecord>,
        /// Report a free slot behind the caller's cursor: a journal bug this
        /// run must refuse to paper over.
        stale_slot: Option<u64>,
    }

    impl Journal for RacingJournal {
        fn append_event(
            &mut self,
            workflow_id: &str,
            event: &EventRecord,
        ) -> Result<u64, JournalError> {
            self.events.append(&mut self.pending_foreign);
            let expected_seq = self
                .stale_slot
                .unwrap_or_else(|| self.events.iter().map(|e| e.seq).max().unwrap_or(0) + 1);
            if event.seq != expected_seq {
                return Err(JournalError::SequenceConflict {
                    workflow_id: workflow_id.to_string(),
                    expected_seq,
                    actual_seq: event.seq,
                });
            }
            self.events.push(event.clone());
            Ok(event.seq)
        }

        fn append_snapshot(
            &mut self,
            _workflow_id: &str,
            snapshot: &SnapshotRecord,
        ) -> Result<(), JournalError> {
            self.snapshots.push(snapshot.clone());
            Ok(())
        }

        fn load_events(
            &self,
            _workflow_id: &str,
            since_seq: u64,
        ) -> Result<Vec<EventRecord>, JournalError> {
            Ok(self
                .events
                .iter()
                .filter(|e| e.seq >= since_seq)
                .cloned()
                .collect())
        }

        fn load_latest_snapshot(
            &self,
            _workflow_id: &str,
        ) -> Result<Option<SnapshotRecord>, JournalError> {
            Ok(self.snapshots.iter().max_by_key(|s| s.seq).cloned())
        }
    }

    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    struct ApprovalCountState {
        approvals_applied: u32,
    }

    struct ApprovalCountWorkflow;

    impl Workflow for ApprovalCountWorkflow {
        type Input = ();
        type State = ApprovalCountState;
        type Output = ();

        fn step(
            &self,
            state: &mut Self::State,
            _ctx: &mut WorkflowContext,
            event: Option<WorkflowEvent>,
        ) -> StepDecision<Self::Output> {
            match event {
                None => StepDecision::PauseForApproval {
                    reason: "awaiting approval".to_string(),
                },
                Some(WorkflowEvent::ApprovalResolved { .. }) => {
                    state.approvals_applied += 1;
                    StepDecision::Continue
                }
                _ => StepDecision::Continue,
            }
        }
    }

    /// Issue #191: a rebase skips events another writer committed, so this
    /// run's state never saw them. A snapshot taken after that rebase claims
    /// them incorporated, and resume replays only events past the snapshot —
    /// they would be lost for good. Pause right after the rebase, then resume:
    /// the skipped events must still reach the workflow.
    #[test]
    fn test_pause_after_a_rebase_leaves_the_skipped_events_replayable() {
        let workflow = ApprovalCountWorkflow;
        let id = "rebase_pause";
        let foreign = (1..=12)
            .map(|seq| {
                EventRecord::new(
                    seq,
                    1,
                    "approval_resolved",
                    r#"{"approved":true,"reason":null}"#,
                )
            })
            .collect();
        let mut journal = RacingJournal {
            pending_foreign: foreign,
            ..RacingJournal::default()
        };
        let mut activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(1000);

        let first = run(
            &workflow,
            id,
            ApprovalCountState::default(),
            &mut journal,
            &mut activities,
            &mut clock,
        )
        .expect("the paused run must block, not fail");
        assert_eq!(
            first,
            RunnerOutcome::Blocked("pause_for_approval: awaiting approval".to_string())
        );
        assert_eq!(
            journal
                .load_events(id, 1)
                .expect("events")
                .iter()
                .filter(|e| e.kind == "approval_resolved")
                .count(),
            12,
            "the second writer's events stay in the journal"
        );
        let paused_after = journal
            .load_latest_snapshot(id)
            .expect("snapshot")
            .expect("the starting state is snapshotted");
        assert_eq!(
            paused_after.seq, 0,
            "a pause snapshot after the rebase would claim the 12 unapplied events incorporated"
        );

        // Resume the way `prompt_worker --resume` does: no initial state, no
        // incoming events. Only replay can bring the skipped events in.
        let resumed = resume(
            &workflow,
            id,
            &mut journal,
            &mut activities,
            &mut clock,
            Vec::new(),
        )
        .expect("resume after a rebase");
        assert_eq!(
            resumed,
            RunnerOutcome::Blocked("no pending events or timers".to_string())
        );
        let latest = journal
            .load_latest_snapshot(id)
            .expect("snapshot")
            .expect("the resumed run snapshots its state");
        let state: ApprovalCountState =
            serde_json::from_str(&latest.state_json).expect("state json");
        assert_eq!(
            state.approvals_applied, 12,
            "the 12 events the first run never applied must be replayed on resume"
        );
    }

    /// The rebase only moves forward. A journal that reports a free slot behind
    /// this run's cursor is a bug, and rebasing to it would silently skip a
    /// sequence while writing an event below it.
    #[test]
    #[should_panic(expected = "a conflict can only sit past this cursor")]
    fn test_rebase_never_moves_the_cursor_backwards() {
        let mut journal = RacingJournal {
            stale_slot: Some(0),
            ..RacingJournal::default()
        };
        let mut activities = MockActivityRegistry::new();
        let mut clock = VirtualClock::new(1000);
        let _ = run(
            &ApprovalCountWorkflow,
            "rebase_backwards",
            ApprovalCountState::default(),
            &mut journal,
            &mut activities,
            &mut clock,
        );
    }
}
