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
use crate::grid::{HerdrPanes, Pane, PaneOps, Scope};
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
    /// No project with that slug; nothing was touched.
    NoProject(String),
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
            OpsError::NoProject(p) => write!(f, "no project '{p}'"),
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

/// What releasing an agent's pane found.
#[derive(Default)]
struct Released {
    /// Ours and closed; the caller proves it is gone.
    closed: Option<String>,
    /// Ours but could not be located or checked: it must be named, never
    /// counted as gone. (A pane held by another open agent, or one the scope
    /// does not admit, is not this agent's and is left out on purpose.)
    unverified: Option<String>,
}

/// A pane the scope admits; a Lead cleanup also admits the project's own
/// `grid_role=lead` pane (owner and farm still have to match).
fn admitted(scope: &Scope, p: &Pane, lead: bool) -> bool {
    if scope.admits(p) {
        return true;
    }
    if lead && p.role.as_deref() == Some("lead") {
        let mut q = p.clone();
        q.role = None;
        return scope.admits(&q);
    }
    false
}

/// Closes `name`'s pane. Where it is: herdr's answer when it has the agent (a
/// pane move renumbers it, so the recorded id can be stale), else the id the
/// spawn recorded. When herdr cannot answer (not a clean `agent_not_found`)
/// and the recorded id is not listed, the pane is unverified.
fn release_pane(
    conn: &Connection,
    panes: &dyn PaneOps,
    live: Option<&dyn HerdrClient>,
    scope: &Scope,
    name: &str,
    lead: bool,
) -> Result<Released, OpsError> {
    let (recorded, row_ended): (Option<String>, bool) = conn
        .query_row(
            "SELECT pane_id, ended_ms IS NOT NULL FROM agent WHERE name = ?1 \
             ORDER BY id DESC LIMIT 1",
            [name],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(other)?
        .unwrap_or((None, false));
    // An ended row's id is the stalest in the DB and herdr may have reused
    // it: only herdr's live answer is trusted for it.
    let recorded = recorded.filter(|p| !p.is_empty() && !row_ended);
    let mut lookup_failed = false;
    let mut from_herdr = None;
    if let Some(h) = live {
        match h.agent_get(name) {
            Ok(r) if r.ok => {
                from_herdr = serde_json::from_str::<serde_json::Value>(&r.body)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/result/agent/pane_id")?
                            .as_str()
                            .map(String::from)
                    })
                    .filter(|p| !p.is_empty());
                lookup_failed = from_herdr.is_none();
            }
            Ok(r) if r.error_code().as_deref() == Some("agent_not_found") => {}
            _ => lookup_failed = true,
        }
    }
    let from_live = from_herdr.is_some();
    let Some(pane) = from_herdr.or(recorded) else {
        return Ok(Released {
            unverified: lookup_failed.then(|| "pane unknown (herdr lookup failed)".to_string()),
            ..Released::default()
        });
    };
    let shared: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent WHERE pane_id = ?1 AND ended_ms IS NULL AND name != ?2",
            params![pane, name],
            |r| r.get(0),
        )
        .map_err(other)?;
    if shared > 0 {
        return Ok(Released::default());
    }
    let found = panes
        .panes()
        .map_err(other)?
        .into_iter()
        .find(|p| p.id == pane);
    Ok(match found {
        Some(p) if admitted(scope, &p, lead) => {
            panes.close_pane(&p.id).map_err(other)?;
            Released {
                closed: Some(p.id),
                ..Released::default()
            }
        }
        // herdr says this is the agent's pane and the scope refuses it: ours
        // but not closed. A recorded id that names such a pane is just a
        // wrong record (a CoS pane): left alone, silently.
        Some(p) if from_live => Released {
            unverified: Some(format!(
                "pane {} (out of scope: owner {}, farm {}, role {})",
                p.id,
                p.owner.as_deref().unwrap_or("-"),
                p.farm.as_deref().unwrap_or("-"),
                p.role.as_deref().unwrap_or("-")
            )),
            ..Released::default()
        },
        Some(_) => Released::default(),
        None if lookup_failed => Released {
            unverified: Some(format!(
                "pane {pane} (herdr lookup failed; cannot tell whether it is gone)"
            )),
            ..Released::default()
        },
        None => Released::default(),
    })
}

/// What of a release is still there: the closed pane when herdr still lists
/// it, plus any unverified pane.
fn pane_remains(panes: &dyn PaneOps, rel: Released) -> Result<Vec<String>, OpsError> {
    let mut out = Vec::new();
    if let Some(id) = rel.closed {
        if panes.panes().map_err(other)?.iter().any(|p| p.id == id) {
            out.push(format!("pane {id}"));
        }
    }
    out.extend(rel.unverified);
    Ok(out)
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
    // A repo that is gone cannot be asked: the worktree directory (if any)
    // is then reported as remaining, never guessed away.
    let repo_ok = t.repo.is_dir();
    let (registered, has_branch) = if repo_ok {
        let registered = git(t.repo, &["worktree", "list", "--porcelain"])?
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .any(|p| canon(Path::new(p)) == canon(t.worktree));
        let branch = format!("refs/heads/quaqua/{}", t.name);
        (
            registered,
            git(t.repo, &["rev-parse", "--verify", "--quiet", &branch]).is_ok(),
        )
    } else {
        (false, false)
    };
    if !t.worktree.is_dir() && !registered && !has_branch && !agent_in_repo(conn, t.name, t.repo)? {
        return Err(OpsError::WrongRepo(format!(
            "nothing to remove in {}: no worktree {} and no branch quaqua/{} (wrong repo?)",
            t.repo.display(),
            t.worktree.display(),
            t.name
        )));
    }
    let mut dirty = Vec::new();
    if t.worktree.is_dir() && repo_ok {
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
    // A peer from `worker spawn` has no run row; record its spend while its
    // pane still holds the session. Closed below once the cleanup is whole.
    let spend = match live {
        Some(h) => crate::run::capture::open_spawned(conn, h, t.name, now_ms).unwrap_or(None),
        None => None,
    };
    let rel = release_pane(conn, panes, live, scope, t.name, false)?;
    if t.worktree.is_dir() && repo_ok {
        if dirty.is_empty() {
            git(
                t.repo,
                &["worktree", "remove", &t.worktree.to_string_lossy()],
            )?;
        } else {
            std::fs::remove_dir_all(t.worktree).map_err(other)?;
        }
    }
    if repo_ok {
        git(t.repo, &["worktree", "prune"])?;
    }
    remove_sidecars(t.runs_dir, t.name);
    let mut remains = pane_remains(panes, rel)?;
    if t.worktree.exists() {
        remains.push(format!("worktree {}", t.worktree.display()));
    }
    if !remains.is_empty() {
        return Err(OpsError::Remains(remains));
    }
    if let Some(wf) = spend {
        crate::run::close(conn, &wf, "cleaned_up", None, now_ms).map_err(other)?;
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

fn open_agent(conn: &Connection, name: &str) -> Result<i64, OpsError> {
    conn.query_row(
        "SELECT id FROM agent WHERE name = ?1 AND ended_ms IS NULL",
        [name],
        |r| r.get(0),
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
    let id = open_agent(conn, name)?;
    // The harness pinned at spawn; a row from before the pin resolves its kind.
    let h = super::spawn::live_harness(conn, id)
        .ok_or_else(|| OpsError::Other(format!("cannot resolve the harness of '{name}'")))?;
    let harness = h.name.clone();
    let keys = crate::driver::Registry::native()
        .resolve(&h)
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

/// The worktrees of Lead `name`: the ones git lists on branch
/// `quaqua/<name>` (wherever they are) plus the conventional
/// `<repo>/../.wt/<name>` when that directory exists.
fn lead_worktrees(repo: &Path, name: &str) -> Result<Vec<PathBuf>, OpsError> {
    let mut out: Vec<PathBuf> = Vec::new();
    if repo.is_dir() {
        let branch = format!("branch refs/heads/quaqua/{name}");
        for block in git(repo, &["worktree", "list", "--porcelain"])?.split("\n\n") {
            let path = block.lines().find_map(|l| l.strip_prefix("worktree "));
            if let Some(path) = path.filter(|_| block.lines().any(|l| l == branch)) {
                if canon(Path::new(path)) != canon(repo) {
                    out.push(PathBuf::from(path));
                }
            }
        }
    }
    let conv = repo.parent().unwrap_or(repo).join(".wt").join(name);
    if conv.is_dir() && !out.iter().any(|p| canon(p) == canon(&conv)) {
        out.push(conv);
    }
    Ok(out)
}

fn lead(
    conn: &Connection,
    panes: &dyn PaneOps,
    live: Option<&dyn HerdrClient>,
    scope: &Scope,
    project: &str,
    now_ms: u64,
) -> Result<(), OpsError> {
    let (pid, repo): (i64, Option<String>) = conn
        .query_row(
            "SELECT id, repo_path FROM project WHERE slug = ?1",
            [project],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(other)?
        .ok_or_else(|| OpsError::NoProject(project.to_string()))?;
    let peers: Vec<String> = conn
        .prepare(
            "SELECT name FROM agent WHERE project_id = ?1 AND role = 'peer' AND ended_ms IS NULL",
        )
        .map_err(other)?
        .query_map([pid], |r| r.get(0))
        .map_err(other)?
        .collect::<Result<_, _>>()
        .map_err(other)?;
    if !peers.is_empty() {
        return Err(OpsError::PeersOpen(peers));
    }
    // Every Lead row of the project, ended or not: a rerun finishes what an
    // older cleanup left behind.
    let leads: Vec<String> = conn
        .prepare("SELECT DISTINCT name FROM agent WHERE project_id = ?1 AND role = 'lead'")
        .map_err(other)?
        .query_map([pid], |r| r.get(0))
        .map_err(other)?
        .collect::<Result<_, _>>()
        .map_err(other)?;
    let repo = repo.map(PathBuf::from);
    let mut trees: Vec<(String, Vec<PathBuf>)> = Vec::new();
    for name in &leads {
        let wts = match &repo {
            Some(r) => lead_worktrees(r, name)?,
            None => Vec::new(),
        };
        // A dirty worktree is refused before anything is released.
        for wt in wts.iter().filter(|w| w.is_dir()) {
            if repo.as_deref().is_some_and(Path::is_dir) {
                let dirty: Vec<String> =
                    git(wt, &["status", "--porcelain", "--untracked-files=all"])?
                        .lines()
                        .map(|l| l.get(3..).unwrap_or(l).to_string())
                        .collect();
                if !dirty.is_empty() {
                    return Err(OpsError::Dirty(dirty));
                }
            }
        }
        trees.push((name.clone(), wts));
    }
    for (name, wts) in trees {
        let rel = release_pane(conn, panes, live, scope, &name, true)?;
        let mut remains = pane_remains(panes, rel)?;
        if let Some(r) = repo.as_deref().filter(|r| r.is_dir()) {
            for wt in wts.iter().filter(|w| w.is_dir()) {
                git(r, &["worktree", "remove", &wt.to_string_lossy()])?;
            }
            git(r, &["worktree", "prune"])?;
        }
        remains.extend(
            wts.iter()
                .filter(|w| w.exists())
                .map(|w| format!("worktree {}", w.display())),
        );
        if !remains.is_empty() {
            return Err(OpsError::Remains(remains));
        }
        end_agent(conn, &name, now_ms).map_err(other)?;
    }
    Ok(())
}

/// The `worker.cleanup` verb. A worker with no project repo (an orphan: its
/// worktree gone, its row never linked) has nothing to remove but its pane, so
/// only that pane is released, found by herdr's exact-name `agent get` (never a
/// listing): the same scope, shared-pane and still-present checks apply.
fn worker_cleanup_verb(
    conn: &Connection,
    h: &dyn HerdrClient,
    rd: &Path,
    name: &str,
    allow_dirty: bool,
) -> Result<serde_json::Value, RpcError> {
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
    let scope = Scope::from_runs_dir(rd);
    let Some(repo) = repo.map(PathBuf::from) else {
        cleanup_orphan_live(conn, &HerdrPanes(h), h, &scope, rd, name, now_ms()).map_err(|e| {
            match e {
                OpsError::NoAgent(_) => {
                    RpcError::new("no_project", format!("'{name}' has no project repo"))
                }
                e => rpc_err(e),
            }
        })?;
        return Ok(json!({
            "cleaned": name,
            "journal": null,
            "raw": null,
            "journal_skipped": "no project repo",
        }));
    };
    let wt = repo.parent().unwrap_or(&repo).join(".wt").join(name);
    let target = Target {
        repo: &repo,
        worktree: &wt,
        runs_dir: rd,
        name,
        allow_dirty,
    };
    cleanup_worker_live(conn, &HerdrPanes(h), h, &scope, &target, now_ms()).map_err(rpc_err)?;
    let sha = git(&repo, &["rev-parse", "--short", &format!("quaqua/{name}")])
        .unwrap_or_else(|_| "-".into());
    let today = crate::memory::journal::today();
    let j = crate::memory::journal::cleanup(rd, &repo, name, &sha, &today);
    Ok(json!({
        "cleaned": name,
        "journal": j.line,
        "raw": j.raw,
        "journal_skipped": j.skipped,
    }))
}

/// Releases the pane of a worker that has no project (so no repo, worktree or
/// branch to remove). Only `name`'s own pane is released (see
/// [`release_pane`]); when neither herdr nor the row knows a pane it is
/// `NoAgent`, and the row is ended only once the pane is gone. A name with no
/// agent row at all is `NoAgent` too: nothing in the DB claims an untagged
/// pane of that name, so none is touched. Like [`cleanup_worker_live`] it
/// records a spawned peer's spend (no repo needed) and removes its sidecars.
pub fn cleanup_orphan_live(
    conn: &Connection,
    panes: &dyn PaneOps,
    live: &dyn HerdrClient,
    scope: &Scope,
    runs_dir: &Path,
    name: &str,
    now_ms: u64,
) -> Result<(), OpsError> {
    let has_row: bool = conn
        .query_row(
            "SELECT 1 FROM agent WHERE name = ?1 LIMIT 1",
            [name],
            |_| Ok(true),
        )
        .optional()
        .map_err(other)?
        .is_some();
    if !has_row {
        return Err(OpsError::NoAgent(name.to_string()));
    }
    let spend = crate::run::capture::open_spawned(conn, live, name, now_ms).unwrap_or(None);
    let rel = release_pane(conn, panes, Some(live), scope, name, false)?;
    if rel.closed.is_none() && rel.unverified.is_none() {
        return Err(OpsError::NoAgent(name.to_string()));
    }
    remove_sidecars(runs_dir, name);
    let remains = pane_remains(panes, rel)?;
    if !remains.is_empty() {
        return Err(OpsError::Remains(remains));
    }
    if let Some(wf) = spend {
        crate::run::close(conn, &wf, "cleaned_up", None, now_ms).map_err(other)?;
    }
    end_agent(conn, name, now_ms).map_err(other)
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
            let allow_dirty = req.args.get("allow_dirty").and_then(|v| v.as_bool()) == Some(true);
            worker_cleanup_verb(&conn, h.as_ref(), &rd, arg(req, "name")?, allow_dirty)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::FakeHerdr;

    fn conn(tag: &str) -> (PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("qq-cleanup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("runs")).unwrap();
        let mut c = crate::db::open(&dir.join("runs").join(crate::db::path::DB_FILE)).unwrap();
        let team = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
        )
        .unwrap();
        crate::config::seed(&mut c, &team).unwrap();
        (dir, c)
    }

    fn row(c: &Connection, name: &str, pane: Option<&str>) {
        c.execute(
            "INSERT INTO agent (name, role, kind, project_id, pane_id, started_ms) \
             VALUES (?1, 'peer', 'sude', NULL, ?2, 1)",
            params![name, pane],
        )
        .unwrap();
    }

    fn ended(c: &Connection, name: &str) -> bool {
        c.query_row(
            "SELECT ended_ms IS NOT NULL FROM agent WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// sude-u5: live in herdr, its worktree gone, its agent row has no project.
    #[test]
    fn worker_cleanup_closes_an_orphan_pane_with_no_project_and_only_that_one() {
        let (dir, c) = conn("orphan");
        let h = FakeHerdr::new();
        let mine = h.seed_pane("main", None, &[]);
        let other = h.seed_pane("main", None, &[]);
        let human = h.seed_pane("main", None, &[]);
        h.agent_start("sude-u5", "pi", &mine, 1000, &[]).unwrap();
        h.agent_start("sude-u6", "pi", &other, 1000, &[]).unwrap();
        row(&c, "sude-u5", Some("p-stale"));
        row(&c, "sude-u6", Some(&other));
        let r = worker_cleanup_verb(&c, &h, &dir.join("runs"), "sude-u5", false);
        assert!(r.is_ok(), "{r:?}");
        assert!(!h.has_pane(&mine), "the orphan pane is closed");
        assert!(ended(&c, "sude-u5"));
        assert!(h.has_pane(&other) && h.has_pane(&human), "nothing widened");
        assert!(!ended(&c, "sude-u6"));
    }

    /// No agent row: nothing claims an untagged herdr agent of that name.
    #[test]
    fn worker_cleanup_with_no_row_and_a_live_untagged_agent_touches_nothing() {
        let (dir, c) = conn("norow");
        let h = FakeHerdr::new();
        let p = h.seed_pane("main", None, &[]);
        h.agent_start("stranger", "pi", &p, 1000, &[]).unwrap();
        let e = worker_cleanup_verb(&c, &h, &dir.join("runs"), "stranger", false).unwrap_err();
        assert_eq!(e.code, "no_project");
        assert!(h.has_pane(&p), "the foreign pane is left alone");
    }

    #[test]
    fn worker_cleanup_with_no_project_and_no_live_pane_still_says_no_project() {
        let (dir, c) = conn("nopane");
        let h = FakeHerdr::new();
        let e = worker_cleanup_verb(&c, &h, &dir.join("runs"), "ghost", false).unwrap_err();
        assert_eq!(e.code, "no_project");
    }
}
