//! `worker cleanup|stop` and `lead cleanup`. Every step reads the state that
//! exists and applies only the difference, so a repeat (or a rerun after a
//! crash between steps) converges. The only panes closed are the agent's own:
//! where herdr says it lives now, else the one the spawn recorded in
//! `agent.pane_id` (tokens may narrow, never widen). A cleanup reports success
//! only when the pane, the worktree and the agent row are all gone.

use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{params, Connection, OptionalExtension};

use crate::daemon::Daemon;
use crate::grid::{HerdrPanes, PaneOps, Scope};
use crate::herdr::HerdrClient;
use crate::rpc::{self, Request, RpcError};
use serde_json::json;

/// Wait for idle after each stop key sequence.
const SETTLE_MS: u64 = 5_000;

#[derive(Debug, PartialEq, Eq)]
pub enum OpsError {
    /// No open agent of that name.
    NoAgent(String),
    /// The agent never reached idle; nothing was released.
    NotStopped(String),
    /// The harness has no verified stop sequence; nothing was sent.
    Unverified(String),
    /// The Lead still has open Peers.
    PeersOpen(Vec<String>),
    /// The worktree has uncommitted paths; nothing was removed.
    Dirty(Vec<String>),
    /// The repo does not own this worker; nothing was touched (#170 F1).
    WrongRepo(String),
    /// The cleanup ran but these still exist (`pane p1`, `worktree /x`...).
    Remains(Vec<String>),
    Git(String),
    Other(String),
}

impl std::fmt::Display for OpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpsError::NoAgent(n) => write!(f, "no open agent '{n}'"),
            OpsError::NotStopped(n) => write!(f, "'{n}' did not stop"),
            OpsError::Unverified(h) => {
                write!(
                    f,
                    "no verified stop sequence for harness '{h}'; nothing sent"
                )
            }
            OpsError::PeersOpen(p) => write!(f, "open peers: {}", p.join(", ")),
            OpsError::Dirty(p) => write!(
                f,
                "worktree is dirty (pass --allow-dirty to discard): {}",
                p.join(", ")
            ),
            OpsError::WrongRepo(m) => write!(f, "{m}"),
            OpsError::Remains(r) => write!(f, "not cleaned, still present: {}", r.join(", ")),
            OpsError::Git(m) | OpsError::Other(m) => write!(f, "{m}"),
        }
    }
}

fn other(e: impl std::fmt::Display) -> OpsError {
    OpsError::Other(e.to_string())
}

fn git(repo: &Path, args: &[&str]) -> Result<String, OpsError> {
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| OpsError::Git(e.to_string()))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim_end().to_string())
    } else {
        Err(OpsError::Git(
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ))
    }
}

/// Where a cleanup acts.
pub struct Target<'a> {
    pub repo: &'a Path,
    pub worktree: &'a Path,
    pub runs_dir: &'a Path,
    pub name: &'a str,
    /// Discard uncommitted paths instead of refusing.
    pub allow_dirty: bool,
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Where `name`'s pane is now: herdr's answer when it has the agent (a pane
/// move renumbers the pane, so the recorded id can be stale), else the id the
/// spawn recorded.
fn pane_of(
    conn: &Connection,
    live: Option<&dyn HerdrClient>,
    name: &str,
) -> Result<Option<String>, OpsError> {
    let from_herdr = live
        .and_then(|h| h.agent_get(name).ok())
        .filter(|r| r.ok)
        .and_then(|r| serde_json::from_str::<serde_json::Value>(&r.body).ok())
        .and_then(|v| {
            v.pointer("/result/agent/pane_id")?
                .as_str()
                .map(String::from)
        })
        .filter(|p| !p.is_empty());
    if from_herdr.is_some() {
        return Ok(from_herdr);
    }
    let recorded: Option<String> = conn
        .query_row(
            "SELECT pane_id FROM agent WHERE name = ?1 ORDER BY id DESC LIMIT 1",
            [name],
            |r| r.get(0),
        )
        .optional()
        .map_err(other)?
        .flatten();
    Ok(recorded.filter(|p| !p.is_empty()))
}

/// Closes `name`'s pane when herdr still has it, the scope admits it and no
/// other open agent holds it. Returns the pane that was ours to close, so the
/// caller can check it is gone.
fn release_pane(
    conn: &Connection,
    panes: &dyn PaneOps,
    live: Option<&dyn HerdrClient>,
    scope: &Scope,
    name: &str,
) -> Result<Option<String>, OpsError> {
    let Some(pane) = pane_of(conn, live, name)? else {
        return Ok(None);
    };
    let shared: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent WHERE pane_id = ?1 AND ended_ms IS NULL AND name != ?2",
            params![pane, name],
            |r| r.get(0),
        )
        .map_err(other)?;
    if shared > 0 {
        return Ok(None);
    }
    let found = panes
        .panes()
        .map_err(other)?
        .into_iter()
        .find(|p| p.id == pane);
    match found {
        Some(p) if scope.admits(&p) => {
            panes.close_pane(&p.id).map_err(other)?;
            Ok(Some(p.id))
        }
        _ => Ok(None),
    }
}

/// `pane <id>` when `closed` is still listed by herdr.
fn pane_remains(panes: &dyn PaneOps, closed: Option<String>) -> Result<Option<String>, OpsError> {
    let Some(id) = closed else { return Ok(None) };
    let alive = panes.panes().map_err(other)?.iter().any(|p| p.id == id);
    Ok(alive.then(|| format!("pane {id}")))
}

/// Whether the agent's project is `repo` (an orphan of this repo, not of
/// another one).
fn agent_in_repo(conn: &Connection, name: &str, repo: &Path) -> Result<bool, OpsError> {
    let r: Option<String> = conn
        .query_row(
            "SELECT p.repo_path FROM agent a JOIN project p ON p.id = a.project_id \
             WHERE a.name = ?1 ORDER BY a.id DESC LIMIT 1",
            [name],
            |r| r.get(0),
        )
        .optional()
        .map_err(other)?
        .flatten();
    Ok(r.is_some_and(|p| canon(Path::new(&p)) == canon(repo)))
}

/// Removes `runs/.<name>.*` and `runs/<name>.base`.
fn remove_sidecars(runs_dir: &Path, name: &str) {
    let prefix = format!(".{name}.");
    if let Ok(rd) = std::fs::read_dir(runs_dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::remove_file(runs_dir.join(format!("{name}.base")));
}

/// Removes the worker's worktree, closes its recorded pane, drops its
/// sidecars, keeps its branch and ends the agent. A dirty worktree is refused
/// (listing the paths) unless `allow_dirty`; git is never run with `--force`.
/// A repo that has neither the worktree nor the branch is refused untouched.
pub fn cleanup_worker(
    conn: &Connection,
    panes: &dyn PaneOps,
    scope: &Scope,
    t: &Target,
    now_ms: u64,
) -> Result<(), OpsError> {
    worker(conn, panes, None, scope, t, now_ms)
}

/// [`cleanup_worker`] that also asks herdr where the worker's pane is now, so a
/// moved pane (stale `agent.pane_id`) or an orphan pane whose worktree and
/// branch are gone is still closed.
pub fn cleanup_worker_live(
    conn: &Connection,
    panes: &dyn PaneOps,
    live: &dyn HerdrClient,
    scope: &Scope,
    t: &Target,
    now_ms: u64,
) -> Result<(), OpsError> {
    worker(conn, panes, Some(live), scope, t, now_ms)
}

fn worker(
    conn: &Connection,
    panes: &dyn PaneOps,
    live: Option<&dyn HerdrClient>,
    scope: &Scope,
    t: &Target,
    now_ms: u64,
) -> Result<(), OpsError> {
    let registered = git(t.repo, &["worktree", "list", "--porcelain"])?
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| canon(Path::new(p)) == canon(t.worktree));
    let branch = format!("refs/heads/quaqua/{}", t.name);
    let has_branch = git(t.repo, &["rev-parse", "--verify", "--quiet", &branch]).is_ok();
    if !t.worktree.is_dir() && !registered && !has_branch && !agent_in_repo(conn, t.name, t.repo)? {
        return Err(OpsError::WrongRepo(format!(
            "nothing to remove in {}: no worktree {} and no branch quaqua/{} (wrong repo?)",
            t.repo.display(),
            t.worktree.display(),
            t.name
        )));
    }
    let mut dirty = Vec::new();
    if t.worktree.is_dir() {
        dirty = git(
            t.worktree,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .lines()
        .map(|l| l.get(3..).unwrap_or(l).to_string())
        .collect();
        if !dirty.is_empty() && !t.allow_dirty {
            return Err(OpsError::Dirty(dirty));
        }
    }
    let closed = release_pane(conn, panes, live, scope, t.name)?;
    if t.worktree.is_dir() {
        if dirty.is_empty() {
            git(
                t.repo,
                &["worktree", "remove", &t.worktree.to_string_lossy()],
            )?;
        } else {
            std::fs::remove_dir_all(t.worktree).map_err(other)?;
        }
    }
    // `repo` may be gone (an orphan): prune is best effort then.
    let pruned = git(t.repo, &["worktree", "prune"]);
    if t.repo.is_dir() {
        pruned?;
    }
    remove_sidecars(t.runs_dir, t.name);
    let mut remains: Vec<String> = pane_remains(panes, closed)?.into_iter().collect();
    if t.worktree.exists() {
        remains.push(format!("worktree {}", t.worktree.display()));
    }
    if !remains.is_empty() {
        return Err(OpsError::Remains(remains));
    }
    end_agent(conn, t.name, now_ms).map_err(other)
}

fn end_agent(conn: &Connection, name: &str, now_ms: u64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE agent SET ended_ms = ?2 WHERE name = ?1 AND ended_ms IS NULL",
        params![name, now_ms as i64],
    )?;
    Ok(())
}

fn open_agent(conn: &Connection, name: &str) -> Result<(i64, Option<String>), OpsError> {
    conn.query_row(
        "SELECT id, kind FROM agent WHERE name = ?1 AND ended_ms IS NULL",
        [name],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(other)?
    .ok_or_else(|| OpsError::NoAgent(name.to_string()))
}

/// Interrupts a working agent with its harness's verified keys and waits for
/// idle. The worker stays alive: no pane is closed and the agent is not
/// ended (`cleanup` does that). A harness without a verified sequence is
/// refused before anything is sent, even when the agent is idle.
pub fn stop_worker(conn: &Connection, herdr: &dyn HerdrClient, name: &str) -> Result<(), OpsError> {
    let (_, kind) = open_agent(conn, name)?;
    let kind = kind.ok_or_else(|| OpsError::Other(format!("'{name}' has no kind")))?;
    let target = crate::config::resolve_spawn(conn, &kind).map_err(|e| other(format!("{e:?}")))?;
    let harness = target.harness.name.clone();
    let keys = crate::driver::Registry::native()
        .resolve(&target.harness)
        .map(|d| d.stop_keys())
        .filter(|k| !k.is_empty())
        .ok_or(OpsError::Unverified(harness))?;
    debug_assert!(keys.iter().all(|k| !k.contains("ctrl+c")));
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    let keys = keys.as_slice();
    let idle = |r: &crate::herdr::Reply| {
        serde_json::from_str::<serde_json::Value>(&r.body)
            .ok()
            .and_then(|v| {
                v.pointer("/result/agent/agent_status")?
                    .as_str()
                    .map(String::from)
            })
            .as_deref()
            == Some("idle")
    };
    if matches!(herdr.agent_get(name), Ok(r) if r.ok && idle(&r)) {
        return Ok(());
    }
    match herdr.agent_send_keys(name, keys) {
        Ok(r) if r.ok => {}
        _ => return Err(OpsError::NotStopped(name.to_string())),
    }
    match herdr.agent_wait(name, &["idle"], SETTLE_MS) {
        Ok(r) if r.ok => Ok(()),
        _ => Err(OpsError::NotStopped(name.to_string())),
    }
}

/// Ends the project's Lead and releases its recorded pane; refused while
/// Peers run.
pub fn cleanup_lead(
    conn: &Connection,
    panes: &dyn PaneOps,
    scope: &Scope,
    project: &str,
    now_ms: u64,
) -> Result<(), OpsError> {
    lead(conn, panes, None, scope, project, now_ms)
}

/// [`cleanup_lead`] that also asks herdr where the Lead's pane is now.
pub fn cleanup_lead_live(
    conn: &Connection,
    panes: &dyn PaneOps,
    live: &dyn HerdrClient,
    scope: &Scope,
    project: &str,
    now_ms: u64,
) -> Result<(), OpsError> {
    lead(conn, panes, Some(live), scope, project, now_ms)
}

/// `<repo>/../.wt/<name>`, where a worktree of the project's agent lives.
fn lead_worktree(conn: &Connection, project: &str, name: &str) -> Option<(PathBuf, PathBuf)> {
    let repo: String = conn
        .query_row(
            "SELECT repo_path FROM project WHERE slug = ?1",
            [project],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()??;
    let repo = PathBuf::from(repo);
    let wt = repo.parent().unwrap_or(&repo).join(".wt").join(name);
    wt.is_dir().then_some((repo, wt))
}

fn lead(
    conn: &Connection,
    panes: &dyn PaneOps,
    live: Option<&dyn HerdrClient>,
    scope: &Scope,
    project: &str,
    now_ms: u64,
) -> Result<(), OpsError> {
    let mut st = conn
        .prepare(
            "SELECT a.name, a.role FROM agent a JOIN project p ON p.id = a.project_id \
             WHERE p.slug = ?1 AND a.ended_ms IS NULL",
        )
        .map_err(other)?;
    let rows: Vec<(String, String)> = st
        .query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(other)?
        .collect::<Result<_, _>>()
        .map_err(other)?;
    let peers: Vec<String> = rows
        .iter()
        .filter(|r| r.1 == "peer")
        .map(|r| r.0.clone())
        .collect();
    if !peers.is_empty() {
        return Err(OpsError::PeersOpen(peers));
    }
    let leads: Vec<&String> = rows
        .iter()
        .filter(|r| r.1 == "lead")
        .map(|r| &r.0)
        .collect();
    // A dirty Lead worktree is named before anything is released.
    for name in &leads {
        if let Some((_, wt)) = lead_worktree(conn, project, name) {
            let dirty = git(&wt, &["status", "--porcelain", "--untracked-files=all"])?;
            if !dirty.is_empty() {
                return Err(OpsError::Remains(vec![format!(
                    "worktree {} (uncommitted work; commit or discard it)",
                    wt.display()
                )]));
            }
        }
    }
    for name in leads {
        let closed = release_pane(conn, panes, live, scope, name)?;
        let mut remains: Vec<String> = pane_remains(panes, closed)?.into_iter().collect();
        if let Some((repo, wt)) = lead_worktree(conn, project, name) {
            git(&repo, &["worktree", "remove", &wt.to_string_lossy()])?;
            git(&repo, &["worktree", "prune"])?;
            if wt.exists() {
                remains.push(format!("worktree {}", wt.display()));
            }
        }
        if !remains.is_empty() {
            return Err(OpsError::Remains(remains));
        }
        end_agent(conn, name, now_ms).map_err(other)?;
    }
    Ok(())
}

fn rpc_err(e: impl std::fmt::Display) -> RpcError {
    RpcError::new("failed", e.to_string())
}

fn arg<'a>(req: &'a Request, key: &str) -> Result<&'a str, RpcError> {
    req.args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::new(rpc::BAD_REQUEST, format!("missing string argument '{key}'")))
}

fn now_ms() -> u64 {
    crate::engine::clock::SystemClock.now_ms()
}

/// Registers `worker.stop`, `worker.cleanup`, `lead.cleanup`, `grid.reconcile`
/// and `grid.acquire` on the daemon, over its own herdr client.
pub fn register(daemon: &mut Daemon) -> Result<(), String> {
    let herdr = daemon.herdr();
    let runs = daemon.runs_dir().to_path_buf();
    let db_path = runs.join(crate::db::path::DB_FILE);
    let open = move |p: &std::path::PathBuf| crate::db::open(p).map_err(rpc_err);

    let (h, p, o) = (herdr.clone(), db_path.clone(), open);
    daemon.register_verb(
        "worker.stop",
        Box::new(move |_, req| {
            let conn = o(&p)?;
            stop_worker(&conn, h.as_ref(), arg(req, "name")?)
                .map_err(|e| RpcError::new("not_stopped", e.to_string()))?;
            Ok(json!({ "stopped": arg(req, "name")? }))
        }),
    )?;

    let (h, p, o, rd) = (herdr.clone(), db_path.clone(), open, runs.clone());
    daemon.register_verb(
        "worker.cleanup",
        Box::new(move |_, req| {
            let conn = o(&p)?;
            let name = arg(req, "name")?;
            let repo: Option<String> = conn
                .query_row(
                    "SELECT p.repo_path FROM agent a JOIN project p ON p.id = a.project_id \
                     WHERE a.name = ?1 ORDER BY a.id DESC LIMIT 1",
                    [name],
                    |r| r.get(0),
                )
                .optional()
                .map_err(rpc_err)?
                .flatten();
            let repo = std::path::PathBuf::from(repo.ok_or_else(|| {
                RpcError::new("no_project", format!("'{name}' has no project repo"))
            })?);
            let wt = repo.parent().unwrap_or(&repo).join(".wt").join(name);
            let allow_dirty = req.args.get("allow_dirty").and_then(|v| v.as_bool()) == Some(true);
            let target = Target {
                repo: &repo,
                worktree: &wt,
                runs_dir: &rd,
                name,
                allow_dirty,
            };
            cleanup_worker_live(
                &conn,
                &HerdrPanes(h.as_ref()),
                h.as_ref(),
                &Scope::from_runs_dir(&rd),
                &target,
                now_ms(),
            )
            .map_err(rpc_err)?;
            Ok(json!({ "cleaned": name }))
        }),
    )?;

    let (h, p, o, rd) = (herdr.clone(), db_path.clone(), open, runs.clone());
    daemon.register_verb(
        "lead.cleanup",
        Box::new(move |_, req| {
            let conn = o(&p)?;
            cleanup_lead_live(
                &conn,
                &HerdrPanes(h.as_ref()),
                h.as_ref(),
                &Scope::from_runs_dir(&rd),
                arg(req, "project")?,
                now_ms(),
            )
            .map_err(|e| RpcError::new("refused", e.to_string()))?;
            Ok(json!({ "cleaned": arg(req, "project")? }))
        }),
    )?;

    let (h, p, o, rd) = (herdr.clone(), db_path, open, runs.clone());
    daemon.register_verb(
        "grid.reconcile",
        Box::new(move |_, _| {
            let conn = o(&p)?;
            let ops =
                crate::grid::reconcile(&HerdrPanes(h.as_ref()), &conn, &Scope::from_runs_dir(&rd))
                    .map_err(rpc_err)?;
            Ok(json!({ "closed": ops.len() }))
        }),
    )?;

    let h = herdr;
    daemon.register_verb(
        "grid.acquire",
        Box::new(move |_, req| {
            let pane = crate::grid::acquire(
                &HerdrPanes(h.as_ref()),
                arg(req, "repo")?,
                &Scope::from_runs_dir(&runs),
                &runs,
            )
            .map_err(rpc_err)?;
            Ok(json!({ "pane": pane }))
        }),
    )
}
