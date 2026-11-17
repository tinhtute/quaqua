//! The effects and verbs of the per-task flow: the activities that talk to
//! herdr, the report files and git, the `pipeline` verb that starts a flow,
//! and the `INSTALL` seam that also registers the `merge` verb.
//!
//! `pipeline {task, builder_kind, reviewer_kind, brief, repo, branch, integ,
//! pane, [project_id, max_rounds, timeout_ms, check]}` refuses a reviewer on
//! the builder's harness (review is cross-harness) and a kind that cannot
//! review. A repeat with the same input converges; another input for the same
//! task is a `conflict`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::pipeline::{self, PipelineFlow, PipelineInput, PipelineState};
use super::worker_flow::spawn_cmd::{existing_pane, spawn_placed, Ctx, Placed, WorkerSpawned};
use crate::authority::{self, Role as AuthRole};
use crate::cli::{CliError, Command, Install, Registry as CliRegistry};
use crate::config;
use crate::daemon::supervisor::{Factory, Machine};
use crate::daemon::{Daemon, Handler};
use crate::driver::Registry as Drivers;
use crate::engine::activity::{
    GitRequest, GitResult, HerdrRequest, HerdrResult, ReportFileRequest, ReportFileResult,
};
use crate::engine::clock::SystemClock;
use crate::engine::journal::Journal;
use crate::engine::journal_sqlite::SqliteJournal;
use crate::engine::{Activity, ActivityError, ActivityRequest, ActivityResult};
use crate::herdr::{CliHerdr, HerdrClient};
use crate::merge;
use crate::rpc::{self, Request, RpcError};

pub const VERB: &str = "pipeline";

/// Registers a report watcher: `(workflow id, report path)`.
pub type Watch = Arc<dyn Fn(&str, &str) -> Result<(), String> + Send + Sync>;

/// The daemon's `watch.report` verb, over the workspace socket.
pub fn rpc_watch(runs_dir: &Path) -> Watch {
    let socket = rpc::socket_path(runs_dir);
    Arc::new(move |wf, path| {
        rpc::call(
            &socket,
            &Request::new("watch.report", json!({ "workflow": wf, "path": path })),
        )
        .map_err(|e| e.to_string())?
        .into_result()
        .map(|_| ())
        .map_err(|e| e.to_string())
    })
}

/// What the flow needs from the daemon's surroundings.
pub struct Deps {
    pub herdr: Arc<dyn HerdrClient>,
    pub runs_dir: PathBuf,
    pub drivers: Drivers,
    /// How a report watcher is registered; tests plug a closure in.
    pub watch: Watch,
}

impl Deps {
    /// The real wiring: watchers go through the daemon socket.
    pub fn new(herdr: Arc<dyn HerdrClient>, runs_dir: PathBuf, drivers: Drivers) -> Deps {
        let watch = rpc_watch(&runs_dir);
        Deps {
            herdr,
            runs_dir,
            drivers,
            watch,
        }
    }
}

/// The effects of one flow. Expected failures come back as results the flow
/// turns into a pause; an `Err` would stop the daemon loop.
pub struct Effects {
    deps: Arc<Deps>,
    input: PipelineInput,
}

impl Effects {
    pub fn new(deps: Arc<Deps>, input: PipelineInput) -> Effects {
        Effects { deps, input }
    }

    fn id(&self) -> String {
        format!("pipeline:{}", self.input.task)
    }

    /// Moves a report that is already at `path` aside, then registers the
    /// watcher: the wait can only be answered by a file written after the prompt.
    fn arm(&self, path: &str) -> bool {
        if Path::new(path).exists() {
            let stale = format!("{path}.stale-{}", SystemClock.now_ms());
            if std::fs::rename(path, stale).is_err() {
                return false;
            }
        }
        (self.deps.watch)(&self.id(), path).is_ok()
    }

    fn tip(&self) -> String {
        merge::resolve(
            Path::new(&self.input.repo),
            &format!("refs/heads/{}", self.input.branch),
        )
        .unwrap_or_default()
    }

    fn base(&self) -> String {
        merge::resolve(
            Path::new(&self.input.repo),
            &format!("refs/heads/{}", self.input.integ),
        )
        .unwrap_or_default()
    }

    fn spawn_reviewer(&self, name: &str, kind: &str) -> bool {
        let db = self.deps.runs_dir.join(crate::db::path::DB_FILE);
        let Ok(conn) = crate::db::open(&db) else {
            return false;
        };
        // A restart re-enters this step: an agent already open under this name
        // and kind is reused, herdr is not asked to start it again.
        let open: Option<String> = conn
            .query_row(
                "SELECT kind FROM agent WHERE name = ?1 AND ended_ms IS NULL",
                [name],
                |row| row.get(0),
            )
            .ok();
        if open.as_deref() == Some(kind) {
            return true;
        }
        // Its own pane, split off the builder's, and its own worktree on a
        // branch started at the candidate (git holds a branch in one worktree).
        let herdr = self.deps.herdr.as_ref();
        let Some(anchor) = existing_pane(herdr, &self.input.builder())
            .or_else(|| Some(self.input.pane.clone()).filter(|p| !p.is_empty()))
        else {
            return false;
        };
        let candidate = Some(self.tip()).filter(|t| !t.is_empty());
        let branch = format!("quaqua/{name}");
        let repo = Path::new(&self.input.repo);
        placed(
            &conn,
            &self.deps,
            &anchor,
            &Placed {
                name,
                kind,
                repo,
                branch: &branch,
                base: candidate.as_deref().unwrap_or(&self.input.branch),
                record_base: candidate.as_deref(),
                review: true,
                project_id: self.input.project_id,
                spawner_id: None,
                owner: "cos",
                slug: &slug_of(&conn, self.input.project_id),
            },
        )
        .is_ok()
    }
}

fn slug_of(conn: &rusqlite::Connection, project_id: Option<i64>) -> String {
    project_id
        .and_then(|id| {
            conn.query_row("SELECT slug FROM project WHERE id = ?1", [id], |r| r.get(0))
                .ok()
        })
        .unwrap_or_else(|| "pipeline".to_string())
}

/// [`spawn_placed`] against the flow's herdr, drivers and runs dir.
fn placed(
    conn: &rusqlite::Connection,
    deps: &Deps,
    anchor: &str,
    p: &Placed,
) -> Result<WorkerSpawned, crate::cli::CliError> {
    spawn_placed(
        &Ctx {
            conn,
            herdr: deps.herdr.as_ref(),
            drivers: &deps.drivers,
            runs_dir: deps.runs_dir.clone(),
            caller: None,
            anchor: Some(anchor),
            now_ms: SystemClock.now_ms(),
        },
        p,
    )
}

impl Activity for Effects {
    fn execute(&self, req: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        match req {
            ActivityRequest::Herdr(HerdrRequest::Prompt { name, task }) => {
                let sent = matches!(self.deps.herdr.agent_prompt(name, task, None), Ok(r) if r.ok);
                Ok(ActivityResult::Herdr(if sent {
                    HerdrResult::Prompted { name: name.clone() }
                } else {
                    HerdrResult::Unknown
                }))
            }
            ActivityRequest::Herdr(HerdrRequest::Spawn { name, kind, .. }) => {
                Ok(ActivityResult::Herdr(if self.spawn_reviewer(name, kind) {
                    HerdrResult::Spawned { name: name.clone() }
                } else {
                    HerdrResult::Unknown
                }))
            }
            ActivityRequest::ReportFile(ReportFileRequest::Exists { path }) => Ok(
                ActivityResult::ReportFile(ReportFileResult::Exists(self.arm(path))),
            ),
            ActivityRequest::ReportFile(ReportFileRequest::Read { path }) => Ok(
                ActivityResult::ReportFile(match std::fs::read_to_string(path) {
                    Ok(text) => ReportFileResult::Content(text),
                    Err(_) => ReportFileResult::Exists(false),
                }),
            ),
            // `"<branch tip> <integration tip>"`, read through git (a part is
            // empty when its branch is gone).
            ActivityRequest::Git(GitRequest::CurrentSha) => Ok(ActivityResult::Git(
                GitResult::Sha(format!("{} {}", self.tip(), self.base())),
            )),
            // `branch` carries the reviewed sha: the queue merges exactly that
            // commit and refuses when the branch tip is no longer it. A refusal
            // is a `Sha("<code>: <why>")`: `GitResult` has no failure variant
            // and an `Err` would stop the loop.
            ActivityRequest::Git(GitRequest::Merge { branch: sha }) => {
                let r = merge::run(&merge::Request {
                    repo: PathBuf::from(&self.input.repo),
                    integ: self.input.integ.clone(),
                    entries: vec![merge::Entry {
                        branch: self.input.branch.clone(),
                        sha: Some(sha.clone()),
                    }],
                    check: self.input.check.clone(),
                });
                Ok(ActivityResult::Git(match r {
                    Ok(m) => GitResult::Merged {
                        sha: m.last().map(|e| e.head.clone()).unwrap_or_default(),
                    },
                    Err(e) => GitResult::Sha(format!("{}: {e}", e.code())),
                }))
            }
            other => Err(ActivityError::ExecutionFailed(format!(
                "pipeline runs no {other:?}"
            ))),
        }
    }
}

/// The state journaled as the replay base of `id` (a daemon restart).
fn journaled_state(db: &Path, id: &str) -> Result<Value, String> {
    let j = SqliteJournal::open(db).map_err(|e| e.to_string())?;
    let snap = j
        .load_latest_snapshot(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{id}: no journaled base"))?;
    serde_json::from_str(&snap.state_json).map_err(|e| e.to_string())
}

fn journaled_input(db: &Path, id: &str) -> Result<PipelineInput, String> {
    let v = journaled_state(db, id)?;
    pipeline::input_from(v.get("input").ok_or("snapshot has no input")?)
}

/// The report path the flow armed last, read from the journal: the latest
/// snapshot can be older than the flow's state (the runner skips a snapshot it
/// cannot place), the armed events are always there.
fn last_armed(db: &Path, id: &str) -> Option<String> {
    let j = SqliteJournal::open(db).ok()?;
    j.load_events(id, 0)
        .ok()?
        .iter()
        .rev()
        .filter(|e| e.kind == "activity_scheduled")
        .filter_map(|e| serde_json::from_str::<Value>(&e.payload_json).ok())
        .find_map(|v| {
            v.pointer("/activity/ReportFile/Exists/path")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// A revived flow needs its report watcher again (the old one died with the
/// daemon). Re-arming the last armed path is always safe: a report the flow
/// already took is ignored because a later stage awaits another path. The
/// daemon is not serving yet while workflows are revived, so the registration
/// retries in the background.
fn rearm_in_background(deps: &Arc<Deps>, id: &str) {
    let db = deps.runs_dir.join(crate::db::path::DB_FILE);
    let Some(report) = last_armed(&db, id) else {
        return;
    };
    let (watch, id) = (Arc::clone(&deps.watch), id.to_string());
    std::thread::spawn(move || {
        for _ in 0..150 {
            if watch(&id, &report).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
}

fn factory(deps: &Arc<Deps>) -> Factory {
    let deps = Arc::clone(deps);
    Arc::new(move |id, input| {
        let (input, fresh) = match input {
            Some(v) => (pipeline::input_from(v)?, true),
            None => {
                let state = journaled_state(&deps.runs_dir.join(crate::db::path::DB_FILE), id)?;
                rearm_in_background(&deps, id);
                (
                    pipeline::input_from(state.get("input").ok_or("snapshot has no input")?)?,
                    false,
                )
            }
        };
        let acts = Box::new(Effects::new(Arc::clone(&deps), input.clone()));
        let machine = if fresh {
            Machine::new(PipelineFlow, id, PipelineState::new(input), acts)
        } else {
            Machine::resume_only(PipelineFlow, id, acts)
        };
        Ok(Box::new(machine))
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

/// Review is cross-harness: the reviewer kind must be allowed to review and
/// must not run on the builder's harness.
fn check_kinds(conn: &rusqlite::Connection, builder: &str, reviewer: &str) -> Result<(), RpcError> {
    let b =
        config::resolve_spawn(conn, builder).map_err(|e| bad(format!("builder kind: {e:?}")))?;
    let r =
        config::resolve_spawn(conn, reviewer).map_err(|e| bad(format!("reviewer kind: {e:?}")))?;
    let can_review: i64 = conn
        .query_row(
            "SELECT can_review FROM kind WHERE name = ?1",
            [reviewer],
            |row| row.get(0),
        )
        .map_err(internal)?;
    if can_review != 1 {
        return Err(RpcError::new(
            "not_a_reviewer",
            format!("kind '{reviewer}' cannot review"),
        ));
    }
    if b.harness.name == r.harness.name {
        return Err(RpcError::new(
            "same_harness",
            format!(
                "review must run on another harness than '{}'",
                b.harness.name
            ),
        ));
    }
    Ok(())
}

fn input_of(a: &Value, runs_dir: &Path) -> Result<PipelineInput, RpcError> {
    let mut v = json!({
        "task": text(a, "task")?,
        "builder_kind": text(a, "builder_kind")?,
        "reviewer_kind": text(a, "reviewer_kind")?,
        "brief": text(a, "brief")?,
        "repo": text(a, "repo")?,
        "branch": text(a, "branch")?,
        "integ": text(a, "integ")?,
        "pane": a.get("pane").and_then(Value::as_str).unwrap_or(""),
        "runs_dir": runs_dir,
    });
    // A new start plans first (the serde default stays false so a journaled
    // flow replays as it started).
    v["plan"] = json!(true);
    for k in ["project_id", "max_rounds", "timeout_ms", "check", "plan"] {
        if let Some(x) = a.get(k) {
            v[k] = x.clone();
        }
    }
    pipeline::input_from(&v).map_err(bad)
}

/// Who may approve a paused flow: the CoS and supervisors, and the Lead of the
/// flow's own project. A peer (the builder included) and a caller the daemon
/// could not name never approve their own pause.
fn approve_resume(
    ctx: &crate::daemon::Ctx,
    runs_dir: &Path,
    id: &str,
    caller: Option<&str>,
) -> Result<(), RpcError> {
    let refuse = |why: String| Err(RpcError::new("refused", format!("resume {id}: {why}")));
    let Some(caller) = caller else {
        return refuse("no caller named".into());
    };
    let actor = ctx
        .store
        .with_conn(|c| {
            authority::resolve(c, caller).map_err(|e| rusqlite::Error::InvalidParameterName(e.0))
        })
        .map_err(internal)?;
    let Some(actor) = actor else {
        return refuse(format!("{caller} is not an active agent"));
    };
    match actor.role {
        AuthRole::Cos | AuthRole::Supervisor => Ok(()),
        AuthRole::Lead => {
            let db = runs_dir.join(crate::db::path::DB_FILE);
            let project = journaled_input(&db, id)
                .map_err(|e| internal(format!("{id}: cannot read the flow's project: {e}")))?
                .project_id;
            if project.is_some() && project == actor.project_id {
                Ok(())
            } else {
                refuse(format!("{caller} leads another project"))
            }
        }
        AuthRole::Peer => refuse(format!("{caller} is a peer; only the Lead approves")),
    }
}

fn start_verb(deps: Arc<Deps>) -> Handler {
    Box::new(move |ctx, req| {
        if let Some(task) = req.args.get("cancel").and_then(Value::as_str) {
            let id = format!("pipeline:{task}");
            if !ctx.supervisor.is_running(&id) {
                return Err(RpcError::new("not_found", format!("{id} is not running")));
            }
            ctx.supervisor
                .signal(&id, "pipeline", "cancel", Value::Null)
                .map_err(internal)?;
            return Ok(json!({ "workflow": id, "cancelled": true }));
        }
        if let Some(task) = req.args.get("resume").and_then(Value::as_str) {
            let id = format!("pipeline:{task}");
            if !ctx.supervisor.is_running(&id) {
                return Err(RpcError::new("not_found", format!("{id} is not running")));
            }
            approve_resume(ctx, &deps.runs_dir, &id, req.caller.as_deref())?;
            let payload = match req.args.get("rounds").and_then(Value::as_u64) {
                Some(n) if n >= 1 => json!({ "rounds": n }),
                Some(_) => return Err(bad("--rounds must be an integer >= 1".to_string())),
                None => Value::Null,
            };
            ctx.supervisor
                .signal(&id, "pipeline", "resume", payload)
                .map_err(internal)?;
            return Ok(json!({ "workflow": id, "resumed": true }));
        }
        let input = input_of(&req.args, &deps.runs_dir)?;
        let id = format!("pipeline:{}", input.task);
        let db = deps.runs_dir.join(crate::db::path::DB_FILE);
        let conn = crate::db::open(&db).map_err(internal)?;
        check_kinds(&conn, &input.builder_kind, &input.reviewer_kind)?;

        if let Some(w) = ctx.store.workflow(&id).map_err(internal)? {
            // Fail closed: when the journaled input cannot be read the repeat
            // is not known to be the same task.
            let old = journaled_input(&db, &id).map_err(|e| {
                internal(format!(
                    "{id}: cannot compare with the journaled input: {e}"
                ))
            })?;
            let same = old.brief == input.brief
                && old.branch == input.branch
                && old.builder_kind == input.builder_kind
                && old.reviewer_kind == input.reviewer_kind
                && old.repo == input.repo
                && old.integ == input.integ
                && old.check == input.check
                && old.plan == input.plan
                && old.pane == input.pane;
            if !same {
                return Err(RpcError::new(
                    "conflict",
                    format!("{id} already runs another task"),
                ));
            }
            if w.finished_ms.is_some() {
                return Ok(json!({ "workflow": id, "finished": true }));
            }
            if ctx.supervisor.is_running(&id) {
                // Converged: the flow is live, nothing is spawned a second time.
                return Ok(json!({ "workflow": id, "running": true }));
            }
        }
        // The builder's own pane (split off the anchor: `pane`, else the pane
        // the daemon mapped the caller to) and its own worktree on the task
        // branch. The mapped pane is placement only, never authority.
        let anchor = Some(input.pane.clone())
            .filter(|p| !p.is_empty())
            .or_else(|| req.caller_pane.clone())
            .ok_or_else(|| bad("no anchor pane: pass 'pane' or call from a herdr pane"))?;
        let name = input.builder();
        placed(
            &conn,
            &deps,
            &anchor,
            &Placed {
                name: &name,
                kind: &input.builder_kind,
                repo: Path::new(&input.repo),
                branch: &input.branch,
                base: &input.integ,
                record_base: Some(&input.integ),
                review: false,
                project_id: input.project_id,
                spawner_id: None,
                owner: req.caller.as_deref().unwrap_or("cos"),
                slug: &slug_of(&conn, input.project_id),
            },
        )
        .map_err(|e| RpcError::new("spawn_failed", e.message))?;
        ctx.supervisor
            .start(pipeline::KIND, &id, &json!(input))
            .map_err(|e| RpcError::new("workflow_start_refused", e))?;
        // The flow arms its own report watchers as it enters each attempt.
        let _ = ctx.supervisor.signal(&id, "pipeline", "start", Value::Null);
        Ok(json!({ "workflow": id }))
    })
}

/// Registers the kind and the `pipeline` verb on `daemon` against `deps`.
pub fn register(daemon: &mut Daemon, deps: Deps) -> Result<(), String> {
    let deps = Arc::new(deps);
    daemon.register_kind(pipeline::KIND, factory(&deps))?;
    daemon.register_verb(VERB, start_verb(deps))
}

/// The seam: the real herdr and the default workspace. Registers the
/// `pipeline` and `merge` verbs and a CLI command for each (the argument is
/// the JSON the verb takes). The Lead adds this to `cli::MODULES`.
pub const INSTALL: Install = install;

fn install(cli: &mut CliRegistry, daemon: &mut Daemon) -> Result<(), String> {
    register(
        daemon,
        Deps::new(
            Arc::new(CliHerdr::new()),
            crate::db::path::default_runs_dir(),
            Drivers::native(),
        ),
    )?;
    merge::register(daemon)?;
    cli.register(Command {
        name: "pipeline",
        help: "pipeline '<json>' [--rounds <N>] [--wait] | --resume <task> [<repo>] [--rounds <N>] [--wait] | --cancel <task> [--wait]: build -> cross-harness review -> gate -> merge for one task; --wait blocks until the run ends or pauses for approval (exit 0 merged or no changes, 3 cancelled, 4 held or disputed, 5 paused: the JSON names it in `paused`, approve with --resume and wait again, 1 failed, daemon gone or loop stopped)",
        run: |env, args| {
            let (wait, rest) = split_wait(args);
            let call = pipeline_args(&rest)?;
            // A resume's wait must not return at the pause it approves.
            let after = match (wait, call.get("resume").and_then(Value::as_str)) {
                (true, Some(task)) => last_pause_seq(env, &format!("pipeline:{task}")),
                _ => 0,
            };
            let mut r = crate::cli::rpc_call(env, VERB, call)?;
            if wait {
                let id = r
                    .get("workflow")
                    .and_then(Value::as_str)
                    .ok_or_else(|| CliError::new(1, "the daemon named no workflow to wait for"))?
                    .to_string();
                let outcome = wait_finished(&mut || probe(env, &id, after), &mut || {
                    std::thread::sleep(POLL)
                })?;
                let verdict = match &outcome {
                    Wait::Done(f) => {
                        r["state"] = json!(f.state);
                        r["status"] = json!(f.status);
                        verdict(f)
                    }
                    Wait::Paused(why) => {
                        r["status"] = json!("paused");
                        r["paused"] = json!(why);
                        Err(paused_exit(why))
                    }
                };
                let out = serde_json::to_string_pretty(&r).unwrap_or_default();
                return verdict.map(|()| out.clone()).map_err(|e| {
                    CliError::new(e.code, format!("{out}\n{}", e.message))
                });
            }
            Ok(serde_json::to_string_pretty(&r).unwrap_or_default())
        },
    })?;
    cli.register(Command {
        name: "merge",
        help: "merge '<json>': merge branches into the integration branch, one at a time",
        run: |env, args| forward(env, "merge", args),
    })
}

const PIPELINE_USAGE: &str = "usage: pipeline '<json>' [--rounds <N>] [--wait] | pipeline --resume <task> [<repo>] [--rounds <N>] [--wait] | pipeline --cancel <task> [--wait]";

/// How long `--wait` sleeps between looks at the run.
const POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// A run that reached a terminal state: the workflow row's `state`
/// (`completed` or `failed`) and, when it completed, the pipeline's own
/// `status` (`merged`, `no_changes`, `cancelled`, `held`, `disputed`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Finished {
    state: String,
    status: Option<String>,
}

/// `--wait` is a client-side flag: it is taken out before the verb's request
/// is built, so the daemon never sees it.
fn split_wait(args: &[String]) -> (bool, Vec<String>) {
    let rest: Vec<String> = args.iter().filter(|a| *a != "--wait").cloned().collect();
    (rest.len() != args.len(), rest)
}

/// The terminal facts of a workflow whose row says `state`, read from its
/// journal: `workflow_completed` carries the pipeline's status.
fn finished_of(state: &str, events: &[crate::engine::journal::EventRecord]) -> Finished {
    let status = events
        .iter()
        .find(|e| e.kind == "workflow_completed")
        .and_then(|e| serde_json::from_str::<Value>(&e.payload_json).ok())
        .and_then(|v| v.get("status").and_then(Value::as_str).map(str::to_string));
    Finished {
        state: state.to_string(),
        status,
    }
}

/// What one look at a run found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Look {
    Running,
    /// Unfinished and no daemon loop drives it.
    Gone,
    /// Waiting for an approval; carries the pause's reason.
    Paused(String),
    Done(Finished),
}

/// How a wait ended without an error.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Wait {
    Done(Finished),
    Paused(String),
}

/// The reason of the run's open pause: the latest of its pause and resolution
/// events decides, and only a pause journaled after `after_seq` counts (a
/// resume's wait must not return at the pause it just approved).
fn pause_of(events: &[crate::engine::journal::EventRecord], after_seq: u64) -> Option<String> {
    let last = events
        .iter()
        .rev()
        .find(|e| matches!(e.kind.as_str(), "pause_for_approval" | "approval_resolved"))?;
    if last.kind != "pause_for_approval" || last.seq <= after_seq {
        return None;
    }
    serde_json::from_str::<Value>(&last.payload_json)
        .ok()
        .and_then(|v| v.get("reason").and_then(Value::as_str).map(str::to_string))
}

/// `--wait` returns at a pause with exit 5 so the caller can approve
/// (`pipeline --resume`) and wait again.
fn paused_exit(reason: &str) -> CliError {
    CliError::new(5, format!("pipeline paused: {reason}"))
}

/// The newest pause event's seq, 0 when there is none: the baseline a resume's
/// wait starts from.
fn last_pause_seq(env: &crate::cli::Env, id: &str) -> u64 {
    let db = env.runs_dir.join(crate::db::path::DB_FILE);
    SqliteJournal::open(&db)
        .and_then(|j| j.load_events(id, 1))
        .map(|ev| {
            ev.iter()
                .filter(|e| e.kind == "pause_for_approval")
                .map(|e| e.seq)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// One look at run `id`. Re-reading is safe at any point, so a wait started
/// after the run ended returns at once.
fn probe(env: &crate::cli::Env, id: &str, after_seq: u64) -> Result<Look, CliError> {
    let status = crate::cli::rpc_call(env, "status", Value::Null)?;
    let row = status
        .get("workflows")
        .and_then(Value::as_array)
        .and_then(|rows| rows.iter().find(|w| w["id"] == id))
        .ok_or_else(|| CliError::new(1, format!("{id}: no such workflow")))?;
    let db = env.runs_dir.join(crate::db::path::DB_FILE);
    let events = SqliteJournal::open(&db)
        .and_then(|j| j.load_events(id, 1))
        .map_err(|e| CliError::new(1, format!("{id}: cannot read the journal: {e}")))?;
    if !row["finished_ms"].is_null() {
        let state = row["state"].as_str().unwrap_or("completed");
        return Ok(Look::Done(finished_of(state, &events)));
    }
    if let Some(reason) = pause_of(&events, after_seq) {
        return Ok(Look::Paused(reason));
    }
    Ok(if row["loop"].as_bool() == Some(false) {
        Look::Gone
    } else {
        Look::Running
    })
}

/// Looks until the run is terminal or paused; `sleep` runs between looks. A
/// look that fails (the daemon is gone) ends the wait with that error, and so
/// do two looks in a row that find the run unfinished with no loop driving it
/// (one is the start/recover window).
fn wait_finished(
    probe: &mut dyn FnMut() -> Result<Look, CliError>,
    sleep: &mut dyn FnMut(),
) -> Result<Wait, CliError> {
    let mut gone = 0;
    loop {
        match probe()? {
            Look::Done(f) => return Ok(Wait::Done(f)),
            Look::Paused(r) => return Ok(Wait::Paused(r)),
            Look::Gone => {
                gone += 1;
                if gone >= 2 {
                    return Err(CliError::new(1, "pipeline loop stopped"));
                }
            }
            Look::Running => gone = 0,
        }
        sleep();
    }
}

/// The exit status of a finished run: 0 when it merged (or had nothing to
/// change), 3 cancelled, 4 ended without merging (held, disputed), 1 failed.
fn verdict(f: &Finished) -> Result<(), CliError> {
    if f.state == "failed" {
        return Err(CliError::new(1, "pipeline failed"));
    }
    match f.status.as_deref() {
        Some("merged") | Some("no_changes") => Ok(()),
        Some("cancelled") => Err(CliError::new(3, "pipeline cancelled")),
        Some(other) => Err(CliError::new(4, format!("pipeline ended: {other}"))),
        None => Err(CliError::new(1, "pipeline ended with no recorded status")),
    }
}

fn rounds_of(v: &str) -> Result<u64, CliError> {
    match v.parse::<u64>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(CliError::new(1, "--rounds must be an integer >= 1")),
    }
}

/// The `pipeline` verb's request for a command line. `--rounds` is the
/// round cap of a start and of a resume (the command line wins over the
/// brief); `--resume` continues a paused flow (the old `<repo>` is accepted
/// and ignored: the flow knows its repo).
fn pipeline_args(args: &[String]) -> Result<Value, CliError> {
    let usage = || CliError::new(1, PIPELINE_USAGE);
    let mut rounds = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--rounds" {
            rounds = Some(rounds_of(it.next().ok_or_else(usage)?)?);
        } else if let Some(v) = a.strip_prefix("--rounds=") {
            rounds = Some(rounds_of(v)?);
        } else {
            rest.push(a.as_str());
        }
    }
    match rest.as_slice() {
        ["--cancel", task] if rounds.is_none() => Ok(json!({ "cancel": task })),
        ["--resume", task] | ["--resume", task, _] => {
            let mut v = json!({ "resume": task });
            if let Some(n) = rounds {
                v["rounds"] = json!(n);
            }
            Ok(v)
        }
        [raw] if !raw.starts_with("--") => {
            let mut v: Value = serde_json::from_str(raw)
                .map_err(|e| CliError::new(1, format!("bad json: {e}")))?;
            if let Some(n) = rounds {
                v["max_rounds"] = json!(n);
            }
            Ok(v)
        }
        _ => Err(usage()),
    }
}

fn forward(env: &crate::cli::Env, verb: &str, args: &[String]) -> Result<String, CliError> {
    let raw = args
        .first()
        .ok_or_else(|| CliError::new(1, format!("usage: {verb} '<json>'")))?;
    let v: Value =
        serde_json::from_str(raw).map_err(|e| CliError::new(1, format!("bad json: {e}")))?;
    crate::cli::rpc_call(env, verb, v).map(|r| serde_json::to_string_pretty(&r).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rounds_on_the_command_line_wins_over_the_json_on_start() {
        let v =
            pipeline_args(&args(&[r#"{"task":"t1","max_rounds":9}"#, "--rounds", "3"])).unwrap();
        assert_eq!(v["max_rounds"], json!(3));
        let v = pipeline_args(&args(&["--rounds=4", r#"{"task":"t1"}"#])).unwrap();
        assert_eq!(v["max_rounds"], json!(4));
        let v = pipeline_args(&args(&[r#"{"task":"t1","max_rounds":9}"#])).unwrap();
        assert_eq!(v["max_rounds"], json!(9), "no flag leaves the JSON alone");
    }

    #[test]
    fn resume_carries_the_cap_and_ignores_the_legacy_repo() {
        let v = pipeline_args(&args(&["--resume", "t1", "/repo", "--rounds", "2"])).unwrap();
        assert_eq!(v, json!({ "resume": "t1", "rounds": 2 }));
        assert_eq!(
            pipeline_args(&args(&["--resume", "t1"])).unwrap(),
            json!({ "resume": "t1" })
        );
    }

    fn fin(state: &str, status: Option<&str>) -> Finished {
        Finished {
            state: state.into(),
            status: status.map(str::to_string),
        }
    }

    #[test]
    fn wait_flag_is_stripped_and_absent_means_unchanged() {
        let (wait, rest) = split_wait(&args(&["--cancel", "t1", "--wait"]));
        assert!(wait);
        assert_eq!(rest, args(&["--cancel", "t1"]));
        let (wait, rest) = split_wait(&args(&["{\"task\":\"t1\"}", "--rounds", "2"]));
        assert!(!wait);
        assert_eq!(rest, args(&["{\"task\":\"t1\"}", "--rounds", "2"]));
    }

    #[test]
    fn wait_polls_until_the_run_is_terminal_without_real_sleeps() {
        let mut script = vec![
            Look::Running,
            Look::Running,
            Look::Done(fin("completed", Some("merged"))),
        ]
        .into_iter();
        let mut sleeps = 0;
        let got = wait_finished(&mut || Ok(script.next().unwrap()), &mut || sleeps += 1).unwrap();
        assert_eq!(got, Wait::Done(fin("completed", Some("merged"))));
        assert_eq!(sleeps, 2, "one sleep per poll that found it still running");
        // A probe error (daemon gone) ends the wait at once, no more polling.
        let mut sleeps = 0;
        let e = wait_finished(
            &mut || Err(CliError::new(1, "daemon not running")),
            &mut || sleeps += 1,
        )
        .unwrap_err();
        assert_eq!((e.code, sleeps), (1, 0));
    }

    #[test]
    fn wait_ends_when_the_loop_is_gone_on_two_polls_in_a_row() {
        let mut script = vec![Look::Gone, Look::Gone].into_iter();
        let mut sleeps = 0;
        let e = wait_finished(&mut || Ok(script.next().unwrap()), &mut || sleeps += 1).unwrap_err();
        assert_eq!(e.code, 1);
        assert!(e.message.contains("loop stopped"), "{}", e.message);
        assert_eq!(sleeps, 1);
        // One gone look between running ones (the start/recover window) is not the end.
        let mut script = vec![
            Look::Gone,
            Look::Running,
            Look::Gone,
            Look::Done(fin("completed", Some("merged"))),
        ]
        .into_iter();
        let got = wait_finished(&mut || Ok(script.next().unwrap()), &mut || {}).unwrap();
        assert_eq!(got, Wait::Done(fin("completed", Some("merged"))));
    }

    #[test]
    fn wait_returns_at_a_pause_and_the_pause_has_its_own_exit() {
        let mut script = vec![Look::Running, Look::Paused("plan ready".into())].into_iter();
        let got = wait_finished(&mut || Ok(script.next().unwrap()), &mut || {}).unwrap();
        assert_eq!(got, Wait::Paused("plan ready".into()));
        let e = paused_exit("plan ready");
        assert_eq!(e.code, 5);
        assert!(e.message.contains("plan ready"));
    }

    #[test]
    fn a_pause_is_the_latest_unresolved_pause_event_after_the_baseline() {
        use crate::engine::journal::EventRecord;
        let ev = |seq, k: &str, p: &str| EventRecord::new(seq, 0, k, p);
        let pause = |seq| ev(seq, "pause_for_approval", r#"{"reason":"blocked 7: x"}"#);
        let resolved = |seq| {
            ev(
                seq,
                "approval_resolved",
                r#"{"approved":true,"reason":null}"#,
            )
        };
        assert_eq!(pause_of(&[pause(2)], 0), Some("blocked 7: x".to_string()));
        assert_eq!(pause_of(&[pause(2), resolved(3)], 0), None);
        assert_eq!(
            pause_of(&[pause(2), resolved(3), pause(5)], 0),
            Some("blocked 7: x".to_string())
        );
        // A pause already there before a resume was sent is not the next one.
        assert_eq!(pause_of(&[pause(2)], 2), None);
        assert_eq!(pause_of(&[], 0), None);
    }

    #[test]
    fn exit_status_tells_success_from_failure_and_cancel() {
        assert!(verdict(&fin("completed", Some("merged"))).is_ok());
        assert!(verdict(&fin("completed", Some("no_changes"))).is_ok());
        let cancelled = verdict(&fin("completed", Some("cancelled"))).unwrap_err();
        let held = verdict(&fin("completed", Some("held"))).unwrap_err();
        let disputed = verdict(&fin("completed", Some("disputed"))).unwrap_err();
        let failed = verdict(&fin("failed", None)).unwrap_err();
        assert_eq!(cancelled.code, 3);
        assert_eq!((held.code, disputed.code), (4, 4));
        assert_eq!(failed.code, 1);
        assert!(cancelled.message.contains("cancelled"));
    }

    #[test]
    fn finished_comes_from_the_journaled_terminal_event() {
        use crate::engine::journal::EventRecord;
        let ev = |k: &str, p: &str| EventRecord::new(1, 0, k, p);
        let running = [ev("activity_scheduled", "{}")];
        assert_eq!(finished_of("completed", &running), fin("completed", None));
        let done = [
            ev("activity_scheduled", "{}"),
            ev(
                "workflow_completed",
                r#"{"status":"cancelled","rounds":1,"candidate_sha":null,"merge_sha":null}"#,
            ),
        ];
        assert_eq!(
            finished_of("completed", &done),
            fin("completed", Some("cancelled"))
        );
        let failed = [ev("workflow_failed", r#"{"code":2,"reason":"x"}"#)];
        assert_eq!(finished_of("failed", &failed), fin("failed", None));
    }
}
