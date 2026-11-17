//! `task.create`: writes one `task` row so a run can carry `run.task_id`.
//!
//! `{project: <slug>, title, [kind, disposition, repo]}` -> `{task_id, created}`.
//! A repeat of the same project and title returns the row already there. An
//! unknown project is refused unless `repo` is given, which creates it the way
//! a Lead spawn does (slug + repo path). A Peer identity never creates tasks.

use std::path::PathBuf;

use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::authority::{self, Decision, Op};
use crate::cli::{CliError, Command, Env};
use crate::config;
use crate::daemon::Daemon;
use crate::rpc::{self, RpcError};

pub const VERB: &str = "task.create";
const DISPOSITIONS: &[&str] = &["engineer", "architect", "reviewer", "scout"];

fn bad(m: impl Into<String>) -> RpcError {
    RpcError::new(rpc::BAD_REQUEST, m)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new("internal", e.to_string())
}

fn text<'a>(a: &'a Value, k: &str) -> Result<&'a str, RpcError> {
    a.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| bad(format!("missing string argument '{k}'")))
}

/// Registers the verb against the database under `runs_dir`.
pub fn register(daemon: &mut Daemon, runs_dir: PathBuf) -> Result<(), String> {
    daemon.register_verb(
        VERB,
        Box::new(move |_, req| {
            let a = &req.args;
            let slug = text(a, "project")?;
            let title = text(a, "title")?;
            let kind = a.get("kind").and_then(Value::as_str);
            let disposition = a
                .get("disposition")
                .and_then(Value::as_str)
                .unwrap_or("engineer");
            if !DISPOSITIONS.contains(&disposition) {
                return Err(bad(format!("disposition must be one of {DISPOSITIONS:?}")));
            }
            let repo = a.get("repo").and_then(Value::as_str);
            let mut conn =
                crate::db::open(&runs_dir.join(crate::db::path::DB_FILE)).map_err(internal)?;

            if let Some(caller) = req.caller.as_deref() {
                let d = authority::check_orchestration(
                    &conn,
                    caller,
                    Op::TaskCreate,
                    authority::Fence::Fenced,
                )
                .map_err(internal)?;
                if let Decision::Refused(r) = d {
                    return Err(RpcError::new(
                        "refused",
                        format!("{} -> {}: {}", r.caller, r.target, r.reason),
                    ));
                }
            }
            if let Some(k) = kind {
                if config::kind(&conn, k).map_err(internal)?.is_none() {
                    return Err(RpcError::new("no_such_kind", format!("no kind '{k}'")));
                }
            }

            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(internal)?;
            let project: Option<i64> = tx
                .query_row("SELECT id FROM project WHERE slug = ?1", [slug], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(internal)?;
            let now = now_ms();
            let project = match (project, repo) {
                (Some(id), _) => id,
                (None, Some(path)) => {
                    tx.execute(
                        "INSERT INTO project (slug, repo_path, created_ms) VALUES (?1, ?2, ?3)",
                        params![slug, path, now],
                    )
                    .map_err(internal)?;
                    tx.last_insert_rowid()
                }
                (None, None) => {
                    return Err(RpcError::new(
                        "no_such_project",
                        format!("no project '{slug}' (pass repo to create it)"),
                    ))
                }
            };
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT id FROM task WHERE project_id = ?1 AND title = ?2 ORDER BY id LIMIT 1",
                    params![project, title],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            let (id, created) = match existing {
                Some(id) => (id, false),
                None => {
                    tx.execute(
                        "INSERT INTO task (project_id, title, kind, disposition, state, created_ms) \
                         VALUES (?1, ?2, ?3, ?4, 'open', ?5)",
                        params![project, title, kind, disposition, now],
                    )
                    .map_err(internal)?;
                    (tx.last_insert_rowid(), true)
                }
            };
            tx.commit().map_err(internal)?;
            Ok(json!({ "task_id": id, "project_id": project, "created": created }))
        }),
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

const USAGE: &str =
    "usage: quaqua task create <project> <title> [--kind K] [--disposition D] [--repo PATH]";

fn create_cli(env: &Env, args: &[String]) -> Result<String, CliError> {
    let usage = || CliError::new(2, USAGE);
    let mut it = args.iter();
    if it.next().map(String::as_str) != Some("create") {
        return Err(usage());
    }
    let (mut pos, mut call) = (Vec::new(), serde_json::Map::new());
    while let Some(a) = it.next() {
        match a.as_str() {
            f @ ("--kind" | "--disposition" | "--repo") => {
                call.insert(f[2..].to_string(), json!(it.next().ok_or_else(usage)?));
            }
            _ => pos.push(a),
        }
    }
    let [project, title] = pos.as_slice() else {
        return Err(usage());
    };
    call.insert("project".into(), json!(project));
    call.insert("title".into(), json!(title));
    crate::cli::rpc_call(env, VERB, Value::Object(call))
        .map(|v| serde_json::to_string_pretty(&v).unwrap_or_default())
}

/// The seam `cli::MODULES` lists: the verb on the default workspace plus `quaqua task create`.
pub const INSTALL: crate::cli::Install = install;

fn install(cli: &mut crate::cli::Registry, daemon: &mut Daemon) -> Result<(), String> {
    register(daemon, crate::db::path::default_runs_dir())?;
    cli.register(Command {
        name: "task",
        help: "task create <project> <title> [--kind K] [--disposition D] [--repo PATH]: write a task row",
        run: create_cli,
    })
}
