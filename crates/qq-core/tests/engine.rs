//! Engine integration: a reducer workflow driven only by pushed Signals and one
//! idle timer. No activity, no poll, no wake row ever reaches the journal.

use qq_core::engine::journal_sqlite::SqliteJournal;
use qq_core::engine::runner::{resume, run_until_blocked, RunnerOptions, RunnerOutcome};
use qq_core::engine::{
    Activity, ActivityError, ActivityRequest, ActivityResult, Journal, StepDecision, VirtualClock,
    Workflow, WorkflowContext, WorkflowEvent,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WF: &str = "wf_stub_reducer";
const IDLE_SECS: u64 = 7200;
const START_MS: u64 = 1_000_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct ReducerState {
    statuses: Vec<String>,
}

/// Reducer: each `herdr`/`status` Signal folds into state; the idle timer ends it.
struct StubReducer;

impl Workflow for StubReducer {
    type Input = ();
    type State = ReducerState;
    type Output = Vec<String>;

    fn step(
        &self,
        state: &mut Self::State,
        _ctx: &mut WorkflowContext,
        event: Option<WorkflowEvent>,
    ) -> StepDecision<Self::Output> {
        match event {
            None => StepDecision::ScheduleTimer {
                id: "idle".to_string(),
                duration: Duration::from_secs(IDLE_SECS),
            },
            Some(WorkflowEvent::Signal {
                source,
                name,
                payload,
            }) if source == "herdr" && name == "status" => {
                let status = payload["status"].as_str().unwrap_or("?").to_string();
                state.statuses.push(status);
                StepDecision::Continue
            }
            Some(WorkflowEvent::TimerFired { id }) if id == "idle" => {
                StepDecision::Complete(state.statuses.clone())
            }
            Some(other) => StepDecision::Fail {
                code: 1,
                reason: format!("unexpected event: {other:?}"),
            },
        }
    }
}

/// Any activity call is a bug in this test.
struct NoActivities;

impl Activity for NoActivities {
    fn execute(&self, _req: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        Err(ActivityError::ExecutionFailed(
            "no activity may run".to_string(),
        ))
    }
}

fn status(s: &str) -> WorkflowEvent {
    WorkflowEvent::Signal {
        source: "herdr".to_string(),
        name: "status".to_string(),
        payload: serde_json::json!({ "status": s }),
    }
}

/// Temp DB path that removes the db, -wal and -shm files on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "qq-core-engine-{tag}-{}-{nanos}.db",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut p = self.0.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }
}

#[test]
fn engine_stub_idle_no_poll_rows() {
    let db = TempDb::new("idle");
    let mut activities = NoActivities;
    let expected: Vec<String> = ["working", "blocked", "idle"].map(String::from).to_vec();

    // The driver owns waiting, so virtual time must not jump on its own.
    let hold = RunnerOptions {
        max_steps: 100,
        snapshot_interval: 0,
        snapshot_at_blocked: false,
        advance_virtual_time: false,
    };

    // Start without idling, then push 3 signals, killing the process (drop
    // journal + clock) between each.
    {
        let mut journal = SqliteJournal::open(db.path()).expect("open journal");
        let mut clock = VirtualClock::new(START_MS);
        let outcome = run_until_blocked(
            &StubReducer,
            WF,
            Some(ReducerState::default()),
            &mut journal,
            &mut activities,
            &mut clock,
            // The first block writes the replay base a later kill resumes from.
            &RunnerOptions {
                snapshot_at_blocked: true,
                ..hold.clone()
            },
            Vec::new(),
        )
        .expect("start");
        assert!(matches!(outcome, RunnerOutcome::Blocked(_)), "{outcome:?}");
        assert_eq!(clock.next_deadline(), Some(START_MS + IDLE_SECS * 1000));
    }

    for s in &expected {
        let mut journal = SqliteJournal::open(db.path()).expect("reopen journal");
        let mut clock = VirtualClock::new(START_MS + 1);
        let outcome = run_until_blocked(
            &StubReducer,
            WF,
            None,
            &mut journal,
            &mut activities,
            &mut clock,
            &hold,
            vec![status(s)],
        )
        .expect("resume with signal");
        assert!(matches!(outcome, RunnerOutcome::Blocked(_)), "{outcome:?}");
        // The idle timer survives the kill: replay re-arms it on the fresh clock.
        assert_eq!(clock.pending_timers_count(), 1);
    }

    // Idle two virtual hours: the driver moves time, the runner fires the timer.
    let started = Instant::now();
    let outcome = {
        let mut journal = SqliteJournal::open(db.path()).expect("reopen journal");
        let mut clock = VirtualClock::new(START_MS + 1);
        // The driver slept until the journaled deadline; only then does it resume.
        clock.set_now(START_MS + IDLE_SECS * 1000 + 1);
        run_until_blocked(
            &StubReducer,
            WF,
            None,
            &mut journal,
            &mut activities,
            &mut clock,
            &hold,
            Vec::new(),
        )
        .expect("resume idle")
    };
    assert_eq!(outcome, RunnerOutcome::Completed(expected.clone()));
    assert!(started.elapsed() < Duration::from_secs(1));

    // Journal content: reducer rows only.
    let journal = SqliteJournal::open(db.path()).expect("reopen journal");
    let rows = journal.load_events(WF, 1).expect("load events");
    let count = |kind: &str| rows.iter().filter(|r| r.kind == kind).count();
    assert!(
        rows.iter().all(|r| !r.kind.starts_with("activity_")),
        "activity rows: {rows:?}"
    );
    assert!(
        rows.iter().all(|r| !r.payload_json.contains("wake_")),
        "wake rows: {rows:?}"
    );
    assert_eq!(count("signal"), 3);
    assert_eq!(count("timer_scheduled"), 1);
    assert_eq!(count("timer_fired"), 1);
    assert_eq!(count("workflow_completed"), 1);
    assert!(
        rows.iter().all(|r| matches!(
            r.kind.as_str(),
            "signal" | "continue" | "timer_scheduled" | "timer_fired" | "workflow_completed"
        )),
        "unexpected kinds: {rows:?}"
    );

    // Terminal: another resume repeats the outcome and writes nothing.
    let before = rows.len();
    let mut journal = SqliteJournal::open(db.path()).expect("reopen journal");
    let again = resume(
        &StubReducer,
        WF,
        &mut journal,
        &mut activities,
        &mut VirtualClock::new(START_MS),
        Vec::new(),
    )
    .expect("resume terminal");
    assert_eq!(again, RunnerOutcome::Completed(expected));
    assert_eq!(journal.load_events(WF, 1).unwrap().len(), before);
}
