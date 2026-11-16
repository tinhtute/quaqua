//! The single-task worker flow: spawn -> prompt -> report -> verified.
//!
//! The `task.start` verb spawns the agent (the one spawn path), starts the
//! workflow, opens the run record and arms the watchers. The workflow then
//! reacts to Signals only: `task/launched` triggers the prompt activity,
//! `report/appeared` triggers reading the report, and a deadline timer ends a
//! flow that never reports. herdr status signals are recorded, never polled.
//!
//! Every step of the verb converges on repeat, so calling it again after a
//! crash re-arms what is missing and nothing twice.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::spawn::{spawn, Role, SpawnSpec};
use crate::cli::{Install, Registry as CliRegistry};
use crate::daemon::supervisor::{Factory, Machine, Progress, WorkflowDriver};
use crate::daemon::{Daemon, Handler};
use crate::driver::Registry as Drivers;
use crate::engine::activity::{HerdrRequest, HerdrResult, ReportFileRequest, ReportFileResult};
use crate::engine::clock::SystemClock;
use crate::engine::journal::Journal;
use crate::engine::{
    Activity, ActivityError, ActivityRequest, ActivityResult, StepDecision, Workflow,
    WorkflowContext, WorkflowEvent,
};
use crate::herdr::{CliHerdr, HerdrClient};
use crate::rpc::{self, Request, RpcError};

/// The workflow kind and the verb this module registers.
pub const KIND: &str = "worker_flow";
pub const VERB: &str = "task.start";

const DEFAULT_TIMEOUT_MS: u64 = 2_700_000;

/// Failure codes of the flow; the run record keeps the matching outcome name.
const TIMEOUT: i32 = 10;
const BLOCKED: i32 = 11;
const PROMPT_FAILED: i32 = 12;
const UNVERIFIED: i32 = 13;
const REPORTED_BLOCKED: i32 = 14;

fn outcome_of(code: i32) -> &'static str {
    match code {
        TIMEOUT => "timeout",
        BLOCKED => "blocked",
        PROMPT_FAILED => "prompt_failed",
        REPORTED_BLOCKED => "reported_blocked",
        UNVERIFIED => "unverified",
        _ => "failed",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerInput {
    pub name: String,
    pub prompt: String,
    /// Absolute path of the report the worker publishes.
    pub report: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Armed,
    Prompting,
    Working,
    Reading,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerState {
    input: WorkerInput,
    armed: bool,
    phase: Phase,
    report_seen: bool,
    status: Option<String>,
}

impl WorkerState {
    pub fn new(input: WorkerInput) -> Self {
        WorkerState {
            input,
            armed: false,
            phase: Phase::Armed,
            report_seen: false,
            status: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerOutput {
    /// `verified` (a candidate sha) or `no_changes`.
    pub outcome: String,
    pub candidate_sha: Option<String>,
}

fn is_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The first line that starts with `field` at column 1, minus the field.
fn field<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(field))
        .map(str::trim)
}

/// Verified = a recognised `Outcome:` and, for COMPLETE, a candidate sha taken
/// from the `Verified:` line, else the `Snapshot:` line. The sha is what the
/// worker claims; no git runs here.
fn judge(report: &str) -> Result<WorkerOutput, (i32, String)> {
    let word = field(report, "Outcome:")
        .and_then(|v| v.split_whitespace().next())
        .unwrap_or("");
    match word {
        "NO_CHANGES" => Ok(WorkerOutput {
            outcome: "no_changes".into(),
            candidate_sha: None,
        }),
        "COMPLETE" => {
            let sha = field(report, "Verified:")
                .and_then(|v| v.split_whitespace().nth(1))
                .filter(|s| is_sha(s))
                .or_else(|| {
                    field(report, "Snapshot:")
                        .and_then(|v| v.split_whitespace().find(|s| is_sha(s)))
                });
            match sha {
                Some(s) => Ok(WorkerOutput {
                    outcome: "verified".into(),
                    candidate_sha: Some(s.to_string()),
                }),
                None => Err((UNVERIFIED, "COMPLETE report names no candidate sha".into())),
            }
        }
        "BLOCKED" | "DISPUTED" | "REOPEN_REQUEST" | "DEPENDENCY_REQUEST" => {
            Err((REPORTED_BLOCKED, format!("worker reported {word}")))
        }
        _ => Err((UNVERIFIED, "report has no recognised Outcome".into())),
    }
}

pub struct WorkerFlow;

impl WorkerFlow {
    /// Reads the report once the prompt is out and the report is known.
    fn read_if_ready(state: &mut WorkerState) -> StepDecision<WorkerOutput> {
        if state.phase == Phase::Working && state.report_seen {
            state.phase = Phase::Reading;
            return StepDecision::ScheduleActivity {
                id: "report".into(),
                activity: ActivityRequest::ReportFile(ReportFileRequest::Read {
                    path: state.input.report.clone(),
                }),
            };
        }
        StepDecision::Continue
    }
}

impl Workflow for WorkerFlow {
    type Input = WorkerInput;
    type State = WorkerState;
    type Output = WorkerOutput;

    fn step(
        &self,
        state: &mut WorkerState,
        _ctx: &mut WorkflowContext,
        event: Option<WorkflowEvent>,
    ) -> StepDecision<WorkerOutput> {
        match event {
            None if !state.armed => {
                state.armed = true;
                StepDecision::ScheduleTimer {
                    id: "deadline".into(),
                    duration: Duration::from_millis(state.input.timeout_ms),
                }
            }
            Some(WorkflowEvent::Signal {
                source,
                name,
                payload,
            }) => match (source.as_str(), name.as_str()) {
                ("task", "launched") if state.phase == Phase::Armed => {
                    state.phase = Phase::Prompting;
                    StepDecision::ScheduleActivity {
                        id: "prompt".into(),
                        activity: ActivityRequest::Herdr(HerdrRequest::Prompt {
                            name: state.input.name.clone(),
                            task: state.input.prompt.clone(),
                        }),
                    }
                }
                ("herdr", "status") => {
                    state.status = payload
                        .get("status")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    StepDecision::Continue
                }
                ("report", "appeared") => {
                    state.report_seen = true;
                    Self::read_if_ready(state)
                }
                _ => StepDecision::Continue,
            },
            Some(WorkflowEvent::ActivityCompleted { id, result }) => match (id.as_str(), result) {
                ("prompt", ActivityResult::Herdr(HerdrResult::Prompted { .. })) => {
                    state.phase = Phase::Working;
                    Self::read_if_ready(state)
                }
                ("prompt", _) => StepDecision::Fail {
                    code: PROMPT_FAILED,
                    reason: "herdr did not take the prompt".into(),
                },
                ("report", ActivityResult::ReportFile(ReportFileResult::Content(text))) => {
                    match judge(&text) {
                        Ok(out) => StepDecision::Complete(out),
                        Err((code, reason)) => StepDecision::Fail { code, reason },
                    }
                }
                ("report", _) => StepDecision::Fail {
                    code: UNVERIFIED,
                    reason: "report could not be read".into(),
                },
                _ => StepDecision::Continue,
            },
            Some(WorkflowEvent::TimerFired { id }) if id == "deadline" => {
                let blocked = state.status.as_deref() == Some("blocked");
                StepDecision::Fail {
                    code: if blocked { BLOCKED } else { TIMEOUT },
                    reason: format!(
                        "no report before the deadline (last herdr status: {})",
                        state.status.as_deref().unwrap_or("none seen")
                    ),
                }
            }
            _ => StepDecision::Continue,
        }
    }
}

/// Runs the two effects of the flow. Expected failures come back as a result
/// the flow turns into a failed run; an `Err` would stop the loop.
struct WorkerActivities {
    herdr: Arc<dyn HerdrClient>,
}

impl Activity for WorkerActivities {
    fn execute(&self, req: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        match req {
            ActivityRequest::Herdr(HerdrRequest::Prompt { name, task }) => {
                let sent = matches!(self.herdr.agent_prompt(name, task, None), Ok(r) if r.ok);
                Ok(ActivityResult::Herdr(if sent {
                    HerdrResult::Prompted { name: name.clone() }
                } else {
                    HerdrResult::Unknown
                }))
            }
            ActivityRequest::ReportFile(ReportFileRequest::Read { path }) => Ok(
                ActivityResult::ReportFile(match std::fs::read_to_string(path) {
                    Ok(text) => ReportFileResult::Content(text),
                    Err(_) => ReportFileResult::Exists(false),
                }),
            ),
            other => Err(ActivityError::ExecutionFailed(format!(
                "worker flow runs no {other:?}"
            ))),
        }
    }
}

fn now_ms() -> u64 {
    SystemClock.now_ms()
}

/// Closes the run of `id` from the journal's terminal event; does nothing
/// while the workflow is still open, and nothing on a repeat.
fn record_end(journal: &dyn Journal, db_path: &Path, id: &str) -> Result<(), String> {
    let events = journal.load_events(id, 1).map_err(|e| e.to_string())?;
    let end = events.iter().find_map(|e| match e.kind.as_str() {
        "workflow_completed" => {
            let out: WorkerOutput = serde_json::from_str(&e.payload_json).ok()?;
            Some((out.outcome, out.candidate_sha))
        }
        "workflow_failed" => {
            let v: Value = serde_json::from_str(&e.payload_json).ok()?;
            let code = v.get("code").and_then(Value::as_i64)? as i32;
            Some((outcome_of(code).to_string(), None))
        }
        _ => None,
    });
    let Some((outcome, sha)) = end else {
        return Ok(());
    };
    let conn = crate::db::open(db_path).map_err(|e| e.to_string())?;
    crate::run::close(&conn, id, &outcome, sha.as_deref(), now_ms()).map_err(|e| e.to_string())?;
    Ok(())
}

/// The inner machine plus the run record: when the workflow ends, the run
/// closes before the daemon marks the workflow finished.
struct RecordingMachine {
    inner: Machine<WorkerFlow>,
    id: String,
    db_path: PathBuf,
}

impl WorkflowDriver for RecordingMachine {
    fn advance(
        &mut self,
        journal: &mut dyn Journal,
        events: Vec<WorkflowEvent>,
    ) -> Result<Progress, String> {
        let progress = self.inner.advance(journal, events)?;
        if progress != Progress::Running {
            record_end(journal, &self.db_path, &self.id)?;
        }
        Ok(progress)
    }

    fn next_deadline(&self) -> Option<u64> {
        self.inner.next_deadline()
    }
}

/// What the flow needs from the daemon's surroundings.
pub struct Deps {
    pub herdr: Arc<dyn HerdrClient>,
    pub runs_dir: PathBuf,
    pub drivers: Drivers,
}

fn factory(deps: &Arc<Deps>) -> Factory {
    let deps = Arc::clone(deps);
    Arc::new(move |id, input| {
        let acts = Box::new(WorkerActivities {
            herdr: Arc::clone(&deps.herdr),
        });
        let inner = match input {
            Some(v) => {
                let input: WorkerInput =
                    serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
                Machine::new(WorkerFlow, id, WorkerState::new(input), acts)
            }
            None => Machine::resume_only(WorkerFlow, id, acts),
        };
        Ok(Box::new(RecordingMachine {
            inner,
            id: id.to_string(),
            db_path: deps.runs_dir.join(crate::db::path::DB_FILE),
        }))
    })
}

fn bad(msg: impl Into<String>) -> RpcError {
    RpcError::new(rpc::BAD_REQUEST, msg)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new("internal", e.to_string())
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("missing string argument '{key}'")))
}

/// `task.start {name, kind, role, pane, prompt, report, [timeout_ms, base,
/// project_id, task_id]}`: runs one task on one agent. The workflow id is
/// `worker:<name>`.
fn start_verb(deps: Arc<Deps>) -> Handler {
    Box::new(move |ctx, req| {
        let a = &req.args;
        let name = text(a, "name")?;
        let role = Role::parse(text(a, "role")?).ok_or_else(|| bad("role must be lead or peer"))?;
        let id = format!("worker:{name}");
        if ctx
            .store
            .workflow(&id)
            .map_err(internal)?
            .is_some_and(|w| w.finished_ms.is_some())
        {
            return Ok(json!({ "workflow": id, "finished": true }));
        }
        let project_id = a.get("project_id").and_then(Value::as_i64);
        let task_id = a.get("task_id").and_then(Value::as_i64);
        let db_path = deps.runs_dir.join(crate::db::path::DB_FILE);
        let mut conn = crate::db::open(&db_path).map_err(internal)?;
        let spawned = spawn(
            &conn,
            deps.herdr.as_ref(),
            &deps.drivers,
            &SpawnSpec {
                name,
                kind: text(a, "kind")?,
                role,
                pane: text(a, "pane")?,
                project_id,
                spawner_id: None,
            },
            now_ms(),
        )
        .map_err(|e| RpcError::new("spawn_failed", e.to_string()))?;
        let report = deps.runs_dir.join(text(a, "report")?);
        let input = WorkerInput {
            name: name.to_string(),
            prompt: text(a, "prompt")?.to_string(),
            report: report.to_string_lossy().into_owned(),
            timeout_ms: a
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_MS),
        };
        ctx.supervisor
            .start(KIND, &id, &json!(input))
            .map_err(|e| RpcError::new("workflow_start_refused", e))?;
        crate::run::open(
            &mut conn,
            &crate::run::NewRun {
                workflow_id: &id,
                agent_id: spawned.agent_id,
                task_id,
                kind: text(a, "kind")?,
                harness: &spawned.harness,
                model: &spawned.model,
                effort: spawned.effort.as_deref(),
                base: a.get("base").and_then(Value::as_str),
                now_ms: now_ms(),
            },
        )
        .map_err(internal)?;
        // A flow that ended before its run existed closes it now.
        record_end(&ctx.store.journal(), &db_path, &id).map_err(internal)?;
        if ctx.supervisor.is_running(&id) {
            // Repeats are ignored by the flow once it is past `Armed`.
            let _ = ctx.supervisor.signal(&id, "task", "launched", Value::Null);
            for (verb, args) in [
                ("watch.agent", json!({ "workflow": id, "agent": name })),
                ("watch.report", json!({ "workflow": id, "path": report })),
            ] {
                rpc::call(&ctx.socket, &Request::new(verb, args))
                    .map_err(internal)?
                    .into_result()
                    .map_err(|e| internal(e.to_string()))?;
            }
        }
        Ok(json!({ "workflow": id, "agent_id": spawned.agent_id }))
    })
}

/// Registers the kind and the verb on `daemon` against `deps`.
pub fn register(daemon: &mut Daemon, deps: Deps) -> Result<(), String> {
    let deps = Arc::new(deps);
    daemon.register_kind(KIND, factory(&deps))?;
    daemon.register_verb(VERB, start_verb(deps))
}

/// The module's seam in [`crate::cli::MODULES`]: the real herdr and the
/// default workspace. Nothing here touches the disk.
pub const INSTALL: Install = install;

fn install(_cli: &mut CliRegistry, daemon: &mut Daemon) -> Result<(), String> {
    register(
        daemon,
        Deps {
            herdr: Arc::new(CliHerdr::new()),
            runs_dir: crate::db::path::default_runs_dir(),
            drivers: Drivers::native(),
        },
    )
}
