//! `worker cleanup|stop` and `lead cleanup`. Every step reads the state that
//! exists and applies only the difference, so a repeat (or a rerun after a
//! crash between steps) converges.

use std::path::Path;
use std::process::Command;

use rusqlite::{params, Connection, OptionalExtension};

use crate::daemon::Daemon;
use crate::driver::{self, Registry as Drivers};
use crate::grid::{HerdrPanes, PaneOps};
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
    /// The Lead still has open Peers.
    PeersOpen(Vec<String>),
    Git(String),
    Other(String),
}

impl std::fmt::Display for OpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpsError::NoAgent(n) => write!(f, "no open agent '{n}'"),
            OpsError::NotStopped(n) => write!(f, "'{n}' did not stop; pane kept"),
            OpsError::PeersOpen(p) => write!(f, "open peers: {}", p.join(", ")),
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
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(OpsError::Git(
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ))
    }
}

/// Removes the worker's worktree and keeps its branch, then ends the agent.
/// A worktree that is already gone is not an error.
pub fn cleanup_worker(
    conn: &Connection,
    repo: &Path,
    worktree: &Path,
    name: &str,
    now_ms: u64,
) -> Result<(), OpsError> {
    if worktree.exists() {
        git(
            repo,
            &["worktree", "remove", "--force", &worktree.to_string_lossy()],
        )?;
    }
    git(repo, &["worktree", "prune"])?;
    end_agent(conn, name, now_ms).map_err(other)
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

/// Stops the agent with its driver's key sequences (`driver::stop`), then
/// releases its pane and ends the agent. When nothing idles it, nothing is
/// released and the caller sees `NotStopped`.
pub fn stop_worker(
    conn: &Connection,
    herdr: &dyn HerdrClient,
    panes: &dyn PaneOps,
    drivers: &Drivers,
    name: &str,
    now_ms: u64,
) -> Result<(), OpsError> {
    let (_, kind) = open_agent(conn, name)?;
    let kind = kind.ok_or_else(|| OpsError::Other(format!("'{name}' has no kind")))?;
    let target = crate::config::resolve_spawn(conn, &kind).map_err(|e| other(format!("{e:?}")))?;
    let drv = drivers
        .get(&target.harness.name)
        .ok_or_else(|| OpsError::Other(format!("no driver for '{}'", target.harness.name)))?;
    if !driver::stop(herdr, drv.as_ref(), name, SETTLE_MS).map_err(other)? {
        return Err(OpsError::NotStopped(name.to_string()));
    }
    release_pane(panes, name)?;
    end_agent(conn, name, now_ms).map_err(other)
}

fn release_pane(panes: &dyn PaneOps, name: &str) -> Result<(), OpsError> {
    for p in panes.panes().map_err(other)? {
        if p.agent.as_deref() == Some(name) {
            panes.close_pane(&p.id).map_err(other)?;
        }
    }
    Ok(())
}

/// Ends the project's Lead and releases its pane; refused while Peers run.
pub fn cleanup_lead(
    conn: &Connection,
    panes: &dyn PaneOps,
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
    for (name, _) in rows.iter().filter(|r| r.1 == "lead") {
        release_pane(panes, name)?;
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
    let db_path = daemon.runs_dir().join(crate::db::path::DB_FILE);
    let open = move |p: &std::path::PathBuf| crate::db::open(p).map_err(rpc_err);

    let (h, p, o) = (herdr.clone(), db_path.clone(), open);
    daemon.register_verb(
        "worker.stop",
        Box::new(move |_, req| {
            let conn = o(&p)?;
            let drivers = Drivers::native();
            stop_worker(
                &conn,
                h.as_ref(),
                &HerdrPanes(h.as_ref()),
                &drivers,
                arg(req, "name")?,
                now_ms(),
            )
            .map_err(|e| RpcError::new("not_stopped", e.to_string()))?;
            Ok(json!({ "stopped": arg(req, "name")? }))
        }),
    )?;

    let (p, o) = (db_path.clone(), open);
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
            cleanup_worker(&conn, &repo, &wt, name, now_ms()).map_err(rpc_err)?;
            Ok(json!({ "cleaned": name }))
        }),
    )?;

    let (h, p, o) = (herdr.clone(), db_path.clone(), open);
    daemon.register_verb(
        "lead.cleanup",
        Box::new(move |_, req| {
            let conn = o(&p)?;
            cleanup_lead(
                &conn,
                &HerdrPanes(h.as_ref()),
                arg(req, "project")?,
                now_ms(),
            )
            .map_err(|e| RpcError::new("refused", e.to_string()))?;
            Ok(json!({ "cleaned": arg(req, "project")? }))
        }),
    )?;

    let (h, p, o) = (herdr.clone(), db_path, open);
    daemon.register_verb(
        "grid.reconcile",
        Box::new(move |_, _| {
            let conn = o(&p)?;
            let ops = crate::grid::reconcile(&HerdrPanes(h.as_ref()), &conn).map_err(rpc_err)?;
            Ok(json!({ "closed": ops.len() }))
        }),
    )?;

    let h = herdr;
    daemon.register_verb(
        "grid.acquire",
        Box::new(move |_, req| {
            let pane = crate::grid::acquire(&HerdrPanes(h.as_ref()), arg(req, "repo")?)
                .map_err(rpc_err)?;
            Ok(json!({ "pane": pane }))
        }),
    )
}
