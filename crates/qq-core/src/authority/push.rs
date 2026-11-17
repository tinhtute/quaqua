//! The `push` verb: the one door for `git push` (R11). The handler asks
//! [`check_orchestration`](super::check_orchestration) first, so a Peer or an
//! unfenced driver is refused and journaled by the daemon before git runs.
//! The fence comes from the caller's kind -> harness -> driver; a caller with
//! no resolvable kind counts as unfenced (fail closed), and a request with no
//! caller is refused.

use std::path::PathBuf;
use std::process::Command as Proc;

use serde_json::{json, Value};

use super::{check_orchestration, fence_for, resolve, Decision, Fence, Op};
use crate::cli::{CliError, Command, Env, Registry};
use crate::config;
use crate::daemon::Daemon;
use crate::rpc::{self, RpcError};

pub const VERB: &str = "push";

fn bad(m: impl Into<String>) -> RpcError {
    RpcError::new(rpc::BAD_REQUEST, m)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new("internal", e.to_string())
}

/// A ref or remote name git could read as an option is refused.
fn word<'a>(a: &'a Value, k: &str) -> Result<&'a str, RpcError> {
    a.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
        .ok_or_else(|| bad(format!("missing or invalid argument '{k}'")))
}

fn fence_of(conn: &rusqlite::Connection, caller: &str) -> Fence {
    let kind: Option<String> = resolve(conn, caller)
        .ok()
        .flatten()
        .and_then(|a| {
            conn.query_row("SELECT kind FROM agent WHERE id = ?1", [a.id], |r| r.get(0))
                .ok()
        })
        .flatten();
    kind.and_then(|k| config::resolve_spawn(conn, &k).ok())
        .map_or(Fence::Unfenced, |t| fence_for(t.harness.driver))
}

/// Registers `push {repo, remote, branch}` -> `{pushed: true}`.
pub fn register(daemon: &mut Daemon, runs_dir: PathBuf) -> Result<(), String> {
    daemon.register_verb(
        VERB,
        Box::new(move |_, req| {
            let a = &req.args;
            let repo = word(a, "repo").or_else(|_| {
                a.get("repo")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| bad("missing argument 'repo'"))
            })?;
            let (remote, branch) = (word(a, "remote")?, word(a, "branch")?);
            let conn =
                crate::db::open(&runs_dir.join(crate::db::path::DB_FILE)).map_err(internal)?;
            let Some(caller) = req.caller.as_deref() else {
                return Err(RpcError::new("refused", "push needs an identified caller"));
            };
            let fence = fence_of(&conn, caller);
            match check_orchestration(&conn, caller, Op::Push, fence).map_err(internal)? {
                Decision::Allowed => {}
                Decision::Refused(r) => {
                    return Err(RpcError::new(
                        "refused",
                        format!("{} -> {}: {}", r.caller, r.target, r.reason),
                    ))
                }
            }
            let out = Proc::new("git")
                .arg("-C")
                .arg(repo)
                .args(["push", "--", remote, branch])
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .map_err(internal)?;
            if !out.status.success() {
                return Err(RpcError::new(
                    "push_failed",
                    String::from_utf8_lossy(&out.stderr).trim().to_string(),
                ));
            }
            Ok(json!({ "pushed": true, "remote": remote, "branch": branch }))
        }),
    )
}

fn push_cli(env: &Env, args: &[String]) -> Result<String, CliError> {
    let [repo, remote, branch] = args else {
        return Err(CliError::new(2, "usage: qq push <repo> <remote> <branch>"));
    };
    crate::cli::rpc_call(
        env,
        VERB,
        json!({ "repo": repo, "remote": remote, "branch": branch }),
    )
    .map(|v| serde_json::to_string_pretty(&v).unwrap_or_default())
}

pub(super) fn install(registry: &mut Registry, daemon: &mut Daemon) -> Result<(), String> {
    register(daemon, crate::db::path::default_runs_dir())?;
    registry.register(Command {
        name: "push",
        help: "push <repo> <remote> <branch>: git push through the daemon (fenced Lead only)",
        run: push_cli,
    })
}
