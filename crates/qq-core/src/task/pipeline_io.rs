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
use super::spawn::{spawn, Role, SpawnError, SpawnSpec};
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
        spawn(
            &conn,
            self.deps.herdr.as_ref(),
            &self.deps.drivers,
            &SpawnSpec {
                name,
                kind,
                role: Role::Peer,
                pane: &self.input.pane,
                project_id: self.input.project_id,
                spawner_id: None,
            },
            SystemClock.now_ms(),
        )
        .is_ok()
    }
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
        "pane": text(a, "pane")?,
        "runs_dir": runs_dir,
    });
    for k in ["project_id", "max_rounds", "timeout_ms", "check", "plan"] {
        if let Some(x) = a.get(k) {
            v[k] = x.clone();
        }
    }
    pipeline::input_from(&v).map_err(bad)
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
        spawn(
            &conn,
            deps.herdr.as_ref(),
            &deps.drivers,
            &SpawnSpec {
                name: &input.builder(),
                kind: &input.builder_kind,
                role: Role::Peer,
                pane: &input.pane,
                project_id: input.project_id,
                spawner_id: None,
            },
            SystemClock.now_ms(),
        )
        .map_err(|e| match e {
            SpawnError::Conflict(m) => RpcError::new("conflict", m),
            e => RpcError::new("spawn_failed", e.to_string()),
        })?;
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
        help: "pipeline '<json>' [--rounds <N>] | --resume <task> [<repo>] [--rounds <N>] | --cancel <task>: build -> cross-harness review -> gate -> merge for one task",
        run: |env, args| {
            let call = pipeline_args(args)?;
            crate::cli::rpc_call(env, VERB, call)
                .map(|r| serde_json::to_string_pretty(&r).unwrap_or_default())
        },
    })?;
    cli.register(Command {
        name: "merge",
        help: "merge '<json>': merge branches into the integration branch, one at a time",
        run: |env, args| forward(env, "merge", args),
    })
}

const PIPELINE_USAGE: &str = "usage: pipeline '<json>' [--rounds <N>] | pipeline --resume <task> [<repo>] [--rounds <N>] | pipeline --cancel <task>";

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
}
