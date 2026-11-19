//! The `push` verb: the one door for `git push` (R11). The handler asks
//! [`check_orchestration`](super::check_orchestration) first, so a Peer or an
//! unfenced driver is refused and journaled by the daemon before git runs.
//! The fence comes from the caller's kind -> harness -> driver; a caller with
//! no resolvable kind counts as unfenced (fail closed), and a request with no
//! caller is refused.

use std::path::PathBuf;
use std::process::Command as Proc;

use serde_json::{json, Value};

use super::{check_orchestration, fence_for, refuse, resolve, Decision, Fence, Op};
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

/// The caller's project repo path, if it has one.
fn project_repo(conn: &rusqlite::Connection, caller: &str) -> Option<String> {
    let a = resolve(conn, caller).ok().flatten()?;
    conn.query_row(
        "SELECT repo_path FROM project WHERE id = ?1",
        [a.project_id?],
        |r| r.get(0),
    )
    .ok()
    .flatten()
}

/// A git command with the user's and the system's config out of the way.
pub(crate) fn git(repo: &str) -> Proc {
    let mut p = Proc::new("git");
    p.arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    p
}

/// A plain branch name: no refspec syntax, and git accepts it as a branch.
pub(crate) fn plain_branch(repo: &str, b: &str) -> bool {
    !b.is_empty()
        && !b.starts_with(['-', '+', '^', ':'])
        && !b.contains([':', '+', '^', '*', '?', '[', '\\'])
        && git(repo)
            .args(["check-ref-format", "--branch", b])
            .output()
            .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == b)
}

/// A remote configured in `repo`, by name only (never a URL or path).
pub(crate) fn configured_remote(repo: &str, name: &str) -> bool {
    git(repo).arg("remote").output().is_ok_and(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|l| l == name)
    })
}

/// Branch names compare case-insensitively: a case-insensitive remote or
/// filesystem makes `Main` and `main` the same ref.
pub(crate) fn same_branch(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// A branch the verb never publishes: `main`, `master` (any case), the
/// remote's HEAD branch (`refs/remotes/<remote>/HEAD`, local only, may be
/// unset), or the repo's configured `init.defaultBranch`. Local refs only;
/// [`remote_default`] asks the remote itself.
pub(crate) fn protected_branch(repo: &str, remote: &str, b: &str) -> bool {
    let out = |args: &[&str]| {
        git(repo)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    same_branch(b, "main")
        || same_branch(b, "master")
        || out(&[
            "symbolic-ref",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ])
        .and_then(|h| h.strip_prefix(&format!("{remote}/")).map(str::to_string))
        .is_some_and(|h| same_branch(&h, b))
        || out(&["config", "init.defaultBranch"]).is_some_and(|d| same_branch(&d, b))
}

/// What the remote itself says its default branch is.
#[derive(Debug, PartialEq)]
pub(crate) enum RemoteDefault {
    /// HEAD is a symref to this branch.
    Branch(String),
    /// The remote has no refs at all: there is no default branch yet.
    Empty,
    /// The read failed, or the remote has refs but HEAD names no branch.
    Unknown,
}

/// Asks the remote (`git ls-remote --symref`) for its default branch.
pub(crate) fn remote_default(repo: &str, remote: &str) -> RemoteDefault {
    let Ok(o) = git(repo)
        .args(["ls-remote", "--symref", "--", remote])
        .output()
    else {
        return RemoteDefault::Unknown;
    };
    if !o.status.success() {
        return RemoteDefault::Unknown;
    }
    let text = String::from_utf8_lossy(&o.stdout);
    if text.trim().is_empty() {
        return RemoteDefault::Empty;
    }
    text.lines()
        .find_map(|l| l.strip_prefix("ref: refs/heads/")?.strip_suffix("\tHEAD"))
        .map_or(RemoteDefault::Unknown, |b| {
            RemoteDefault::Branch(b.to_string())
        })
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
            let arg = |k: &str| {
                a.get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| bad(format!("missing argument '{k}'")))
            };
            let (repo, remote, branch) = (arg("repo")?, arg("remote")?, arg("branch")?);
            let conn =
                crate::db::open(&runs_dir.join(crate::db::path::DB_FILE)).map_err(internal)?;
            let Some(caller) = req.caller.as_deref() else {
                return Err(RpcError::new("refused", "push needs an identified caller"));
            };
            let fence = fence_of(&conn, caller);
            let deny = |reason: &str, target: &str| -> Result<Value, RpcError> {
                match refuse(&conn, caller, "push", target, reason).map_err(internal)? {
                    Decision::Refused(r) => Err(RpcError::new(
                        "refused",
                        format!("{} -> {}: {}", r.caller, r.target, r.reason),
                    )),
                    Decision::Allowed => unreachable!("refuse always refuses"),
                }
            };
            if let Decision::Refused(r) =
                check_orchestration(&conn, caller, Op::Push, fence).map_err(internal)?
            {
                return Err(RpcError::new(
                    "refused",
                    format!("{} -> {}: {}", r.caller, r.target, r.reason),
                ));
            }
            let own = project_repo(&conn, caller)
                .and_then(|p| std::fs::canonicalize(p).ok())
                .zip(std::fs::canonicalize(repo).ok())
                .is_some_and(|(p, r)| p == r);
            if !own {
                return deny("repo is not the caller's project repo", repo);
            }
            if !plain_branch(repo, branch) {
                return deny("branch must be a plain branch name", branch);
            }
            if !configured_remote(repo, remote) {
                return deny("remote is not configured in the repo", remote);
            }
            if protected_branch(repo, remote, branch)
                || match remote_default(repo, remote) {
                    RemoteDefault::Branch(d) => same_branch(&d, branch),
                    RemoteDefault::Empty => false,
                    RemoteDefault::Unknown => true,
                }
            {
                return deny("branch is the repo's main or default branch", branch);
            }
            let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
            let out = git(repo)
                .args(["push", "--", remote, &refspec])
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
        return Err(CliError::new(
            2,
            "usage: quaqua push <repo> <remote> <branch>",
        ));
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
