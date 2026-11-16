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

use super::spawn::{spawn, Role, SpawnError, SpawnSpec};
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
    pub kind: String,
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

/// Levels of a `Verified: <level> <sha>` line that count as a pass. Any other
/// level (verifier-failed, verifier-blocked, inconclusive, unknown) does not.
const PASSING_LEVELS: [&str; 3] = ["unit-test-verified", "type-check-only", "review-clean"];

/// `verified` = `Outcome: COMPLETE` plus a `Verified:` line with a passing
/// level and a sha: the worker reported a passing level, nothing more. The sha
/// is the worker's claim; no git runs here. `Outcome: no changes` counts as
/// NO_CHANGES.
fn judge(report: &str) -> Result<WorkerOutput, (i32, String)> {
    let value = field(report, "Outcome:").unwrap_or("");
    let whole = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
        .to_uppercase();
    let word = value.split_whitespace().next().unwrap_or("");
    if whole == "NO_CHANGES" {
        return Ok(WorkerOutput {
            outcome: "no_changes".into(),
            candidate_sha: None,
        });
    }
    match word {
        "COMPLETE" => {
            let mut parts = field(report, "Verified:").unwrap_or("").split_whitespace();
            let (level, sha) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            if !PASSING_LEVELS.contains(&level) {
                return Err((
                    UNVERIFIED,
                    format!("no passing Verified level (got '{level}')"),
                ));
            }
            if !is_sha(sha) {
                return Err((UNVERIFIED, "COMPLETE report names no candidate sha".into()));
            }
            Ok(WorkerOutput {
                outcome: "verified".into(),
                candidate_sha: Some(sha.to_string()),
            })
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

/// The pane for a spawn that named none. A worker herdr already knows keeps
/// its pane (a retry places nothing new); otherwise grid placement picks one
/// by the project's repo, four agents to a tab.
pub fn place_pane(
    conn: &rusqlite::Connection,
    herdr: &dyn HerdrClient,
    name: &str,
    project_id: Option<i64>,
) -> Result<String, RpcError> {
    if let Ok(r) = herdr.agent_get(name) {
        if let Some(p) = serde_json::from_str::<Value>(&r.body).ok().and_then(|v| {
            v.pointer("/result/agent/pane_id")?
                .as_str()
                .map(String::from)
        }) {
            return Ok(p);
        }
    }
    let id = project_id.ok_or_else(|| bad("a spawn needs 'pane' or a 'project_id' to place it"))?;
    let repo: String = conn
        .query_row("SELECT repo_path FROM project WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .map_err(|_| bad(format!("no project {id} to place '{name}'")))?;
    let repo = std::path::Path::new(&repo)
        .file_name()
        .map_or_else(|| repo.clone(), |f| f.to_string_lossy().into_owned());
    let runs = conn
        .path()
        .and_then(|p| Path::new(p).parent())
        .map(Path::to_path_buf)
        .ok_or_else(|| bad("no runs dir to place '".to_string() + name + "' in"))?;
    crate::grid::acquire(
        &crate::grid::HerdrPanes(herdr),
        &repo,
        &crate::grid::Scope::from_runs_dir(&runs),
        &runs,
    )
    .map_err(|e| RpcError::new("place_failed", e.to_string()))
}

/// The input `task.start` journaled for `id`, read from the replay base.
fn journaled_input(
    store: &crate::daemon::store::Store,
    id: &str,
) -> Result<Option<WorkerInput>, String> {
    let snap = store
        .journal()
        .load_latest_snapshot(id)
        .map_err(|e| e.to_string())?;
    Ok(snap
        .and_then(|s| serde_json::from_str::<Value>(&s.state_json).ok())
        .and_then(|v| v.get("input").cloned())
        .and_then(|v| serde_json::from_value(v).ok()))
}

/// Makes sure the run of `id` exists (agent looked up by name) and is closed
/// when the workflow already ended. Both steps repeat safely.
fn settle_run(
    conn: &mut rusqlite::Connection,
    journal: &dyn Journal,
    db_path: &Path,
    id: &str,
    input: &WorkerInput,
    base: Option<&str>,
    task_id: Option<i64>,
) -> Result<(), String> {
    let agent_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM agent WHERE name = ?1 ORDER BY id DESC LIMIT 1",
            [&input.name],
            |r| r.get(0),
        )
        .ok();
    if let Some(agent_id) = agent_id {
        let t = crate::config::resolve_spawn(conn, &input.kind).map_err(|e| format!("{e:?}"))?;
        crate::run::open(
            conn,
            &crate::run::NewRun {
                workflow_id: id,
                agent_id,
                task_id,
                kind: &input.kind,
                harness: &t.harness.name,
                model: &t.model,
                effort: t.effort.as_deref(),
                base,
                now_ms: now_ms(),
            },
        )
        .map_err(|e| e.to_string())?;
    }
    record_end(journal, db_path, id)
}

/// `task.start {name, kind, role, [pane: else grid-placed from project_id], prompt, report, [timeout_ms, base,
/// project_id, task_id]}`: runs one task on one agent. The workflow id is
/// `worker:<name>`. A repeat with the same input converges; a different input
/// for an existing id is a `conflict`; a new task whose report path already
/// holds a file is refused (`report_exists`), so an old report cannot verify it.
fn start_verb(deps: Arc<Deps>) -> Handler {
    Box::new(move |ctx, req| {
        let a = &req.args;
        let name = text(a, "name")?;
        let kind = text(a, "kind")?;
        let role = Role::parse(text(a, "role")?).ok_or_else(|| bad("role must be lead or peer"))?;
        let id = format!("worker:{name}");
        let project_id = a.get("project_id").and_then(Value::as_i64);
        let task_id = a.get("task_id").and_then(Value::as_i64);
        let base = a.get("base").and_then(Value::as_str);
        let report = deps.runs_dir.join(text(a, "report")?);
        let input = WorkerInput {
            name: name.to_string(),
            kind: kind.to_string(),
            prompt: text(a, "prompt")?.to_string(),
            report: report.to_string_lossy().into_owned(),
            timeout_ms: a
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_MS),
        };
        let db_path = deps.runs_dir.join(crate::db::path::DB_FILE);
        let mut conn = crate::db::open(&db_path).map_err(internal)?;

        let existing = ctx.store.workflow(&id).map_err(internal)?;
        match &existing {
            Some(w) => {
                if let Some(old) = journaled_input(&ctx.store, &id).map_err(internal)? {
                    if (&old.prompt, &old.report, &old.kind)
                        != (&input.prompt, &input.report, &input.kind)
                    {
                        return Err(RpcError::new(
                            "conflict",
                            format!("{id} already runs a different task"),
                        ));
                    }
                }
                if w.finished_ms.is_some() {
                    settle_run(
                        &mut conn,
                        &ctx.store.journal(),
                        &db_path,
                        &id,
                        &input,
                        base,
                        task_id,
                    )
                    .map_err(internal)?;
                    return Ok(json!({ "workflow": id, "finished": true }));
                }
            }
            None if report.exists() => {
                return Err(RpcError::new(
                    "report_exists",
                    format!("{} already exists; archive it first", report.display()),
                ));
            }
            None => {}
        }

        // Gate and attribute the spawn. The gate is skipped only for the open
        // agent's own spawner retrying (it converges and spawns nothing new);
        // any other caller goes through it. A request with no caller is not
        // gated (identity is client-asserted).
        let caller = req.caller.as_deref();
        let open = crate::authority::resolve(&conn, name).map_err(internal)?;
        let own_retry = match (&open, caller) {
            (Some(o), Some(c)) => {
                let who = crate::authority::resolve(&conn, c).map_err(internal)?;
                o.spawner_id.is_some() && who.map(|w| w.id) == o.spawner_id
            }
            _ => false,
        };
        if let (Some(caller), false) = (caller, own_retry) {
            let target = match role {
                Role::Lead => crate::authority::Role::Lead,
                Role::Peer => crate::authority::Role::Peer,
            };
            if let crate::authority::Decision::Refused(r) =
                crate::authority::check_spawn(&conn, caller, target, project_id)
                    .map_err(internal)?
            {
                return Err(RpcError::new(
                    "refused",
                    format!("{} -> {}: {}", r.caller, r.target, r.reason),
                ));
            }
        }
        let spawner_id = match caller {
            Some(c) => crate::authority::resolve(&conn, c)
                .map_err(internal)?
                .map(|a| a.id),
            None => None,
        };
        let pane = match a.get("pane").and_then(Value::as_str) {
            Some(p) => p.to_string(),
            None => place_pane(&conn, deps.herdr.as_ref(), name, project_id)?,
        };
        let spawned = spawn(
            &conn,
            deps.herdr.as_ref(),
            &deps.drivers,
            &SpawnSpec {
                name,
                kind,
                role,
                pane: &pane,
                project_id,
                spawner_id,
            },
            now_ms(),
        )
        .map_err(|e| match e {
            SpawnError::Conflict(m) => RpcError::new("conflict", m),
            e => RpcError::new("spawn_failed", e.to_string()),
        })?;
        ctx.supervisor
            .start(KIND, &id, &json!(input))
            .map_err(|e| RpcError::new("workflow_start_refused", e))?;
        // Opens the run, and closes it when the flow already ended.
        settle_run(
            &mut conn,
            &ctx.store.journal(),
            &db_path,
            &id,
            &input,
            base,
            task_id,
        )
        .map_err(internal)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123abc4567def890123abc4567def890123abcd";

    fn complete(verified: &str) -> String {
        format!("Outcome: COMPLETE\n{verified}\n\n## Blockers\nnone\n")
    }

    /// (outcome name, candidate sha) of a verdict; failures map through
    /// `outcome_of`.
    fn verdict(report: &str) -> (String, Option<String>) {
        match judge(report) {
            Ok(o) => (o.outcome, o.candidate_sha),
            Err((code, _)) => (outcome_of(code).to_string(), None),
        }
    }

    #[test]
    fn judge_maps_every_end_state() {
        let ok = |l: &str| complete(&format!("Verified: {l} {SHA}"));
        let cases: Vec<(String, &str, Option<&str>)> = vec![
            (ok("unit-test-verified"), "verified", Some(SHA)),
            (ok("type-check-only"), "verified", Some(SHA)),
            (ok("review-clean"), "verified", Some(SHA)),
            (ok("verifier-failed"), "unverified", None),
            (ok("verifier-blocked"), "unverified", None),
            (ok("inconclusive"), "unverified", None),
            (ok("made-up-level"), "unverified", None),
            // no Verified line, a bad sha, or the level alone
            (
                complete(&format!("Snapshot: {SHA} on b")),
                "unverified",
                None,
            ),
            (
                complete("Verified: unit-test-verified zzzz"),
                "unverified",
                None,
            ),
            (complete("Verified: unit-test-verified"), "unverified", None),
            ("Outcome: NO_CHANGES\n".into(), "no_changes", None),
            ("Outcome: no changes\n".into(), "no_changes", None),
            ("Outcome:   No   Changes \n".into(), "no_changes", None),
            ("Outcome: BLOCKED\n".into(), "reported_blocked", None),
            ("Outcome: DISPUTED\n".into(), "reported_blocked", None),
            ("Outcome: REOPEN_REQUEST\n".into(), "reported_blocked", None),
            (
                "Outcome: DEPENDENCY_REQUEST\n".into(),
                "reported_blocked",
                None,
            ),
            ("Outcome: DONE\n".into(), "unverified", None),
            ("no outcome line\n".into(), "unverified", None),
        ];
        for (report, outcome, sha) in cases {
            assert_eq!(
                verdict(&report),
                (outcome.to_string(), sha.map(String::from)),
                "{report}"
            );
        }
    }
}
