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

use super::pipeline::{self, PipelineFlow, PipelineInput, PipelineOutput, PipelineState};
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

/// What the flow needs from the daemon's surroundings.
pub struct Deps {
    pub herdr: Arc<dyn HerdrClient>,
    pub runs_dir: PathBuf,
    pub drivers: Drivers,
}

/// The effects of one flow. Expected failures come back as results the flow
/// turns into a pause; an `Err` would stop the daemon loop.
struct Effects {
    deps: Arc<Deps>,
    input: PipelineInput,
}

impl Effects {
    fn tip(&self) -> String {
        merge::resolve(
            Path::new(&self.input.repo),
            &format!("refs/heads/{}", self.input.branch),
        )
        .unwrap_or_default()
    }

    fn spawn_reviewer(&self, name: &str, kind: &str) -> bool {
        let db = self.deps.runs_dir.join(crate::db::path::DB_FILE);
        let Ok(conn) = crate::db::open(&db) else {
            return false;
        };
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
            ActivityRequest::ReportFile(ReportFileRequest::Read { path }) => Ok(
                ActivityResult::ReportFile(match std::fs::read_to_string(path) {
                    Ok(text) => ReportFileResult::Content(text),
                    Err(_) => ReportFileResult::Exists(false),
                }),
            ),
            // The branch tip, read through git (empty when the branch is gone).
            ActivityRequest::Git(GitRequest::CurrentSha) => {
                Ok(ActivityResult::Git(GitResult::Sha(self.tip())))
            }
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

/// The input journaled as the replay base of `id` (a daemon restart).
fn journaled_input(db: &Path, id: &str) -> Result<PipelineInput, String> {
    let j = SqliteJournal::open(db).map_err(|e| e.to_string())?;
    let snap = j
        .load_latest_snapshot(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{id}: no journaled base"))?;
    let v: Value = serde_json::from_str(&snap.state_json).map_err(|e| e.to_string())?;
    pipeline::input_from(v.get("input").ok_or("snapshot has no input")?)
}

fn factory(deps: &Arc<Deps>) -> Factory {
    let deps = Arc::clone(deps);
    Arc::new(move |id, input| {
        let (input, fresh) = match input {
            Some(v) => (pipeline::input_from(v)?, true),
            None => (
                journaled_input(&deps.runs_dir.join(crate::db::path::DB_FILE), id)?,
                false,
            ),
        };
        let acts = Box::new(Effects {
            deps: Arc::clone(&deps),
            input: input.clone(),
        });
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
    for k in ["project_id", "max_rounds", "timeout_ms", "check"] {
        if let Some(x) = a.get(k) {
            v[k] = x.clone();
        }
    }
    pipeline::input_from(&v).map_err(bad)
}

fn start_verb(deps: Arc<Deps>) -> Handler {
    Box::new(move |ctx, req| {
        let input = input_of(&req.args, &deps.runs_dir)?;
        let id = format!("pipeline:{}", input.task);
        let db = deps.runs_dir.join(crate::db::path::DB_FILE);
        let conn = crate::db::open(&db).map_err(internal)?;
        check_kinds(&conn, &input.builder_kind, &input.reviewer_kind)?;

        if let Some(w) = ctx.store.workflow(&id).map_err(internal)? {
            if let Ok(old) = journaled_input(&db, &id) {
                if (
                    &old.brief,
                    &old.branch,
                    &old.builder_kind,
                    &old.reviewer_kind,
                ) != (
                    &input.brief,
                    &input.branch,
                    &input.builder_kind,
                    &input.reviewer_kind,
                ) {
                    return Err(RpcError::new(
                        "conflict",
                        format!("{id} already runs another task"),
                    ));
                }
            }
            if w.finished_ms.is_some() {
                return Ok(json!({ "workflow": id, "finished": true }));
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
        if ctx.supervisor.is_running(&id) {
            // Repeats are ignored by the flow once it is past `Armed`.
            let _ = ctx.supervisor.signal(&id, "pipeline", "start", Value::Null);
            let mut reports = vec![input.build_report()];
            for r in 1..=input.max_rounds + 1 {
                reports.push(input.review_report(r));
                reports.push(input.fix_report(r));
            }
            for path in reports {
                rpc::call(
                    &ctx.socket,
                    &Request::new("watch.report", json!({ "workflow": id, "path": path })),
                )
                .map_err(internal)?
                .into_result()
                .map_err(|e| internal(e.to_string()))?;
            }
        }
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
        Deps {
            herdr: Arc::new(CliHerdr::new()),
            runs_dir: crate::db::path::default_runs_dir(),
            drivers: Drivers::native(),
        },
    )?;
    merge::register(daemon)?;
    cli.register(Command {
        name: "pipeline",
        help: "pipeline '<json>': build -> cross-harness review -> gate -> merge for one task",
        run: |env, args| forward(env, VERB, args),
    })?;
    cli.register(Command {
        name: "merge",
        help: "merge '<json>': merge branches into the integration branch, one at a time",
        run: |env, args| forward(env, "merge", args),
    })
}

fn forward(env: &crate::cli::Env, verb: &str, args: &[String]) -> Result<String, CliError> {
    let raw = args
        .first()
        .ok_or_else(|| CliError::new(1, format!("usage: {verb} '<json>'")))?;
    let v: Value =
        serde_json::from_str(raw).map_err(|e| CliError::new(1, format!("bad json: {e}")))?;
    crate::cli::rpc_call(env, verb, v).map(|r| serde_json::to_string_pretty(&r).unwrap_or_default())
}

#[allow(dead_code)]
type _Out = PipelineOutput;
