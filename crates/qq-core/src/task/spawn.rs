//! The one spawn path. A Lead and a Peer go through the same function; they
//! differ only by data (`role`, `base`, `project_id`).
//!
//! Steps, each safe to repeat: resolve the kind from SQLite, find the driver
//! of its harness, `herdr agent start` (herdr converges on the same name,
//! kind and pane), then record the agent unless an open one with that name
//! exists. The pane is an input; `task.start` fills a missing one from grid
//! placement (`worker_flow::place_pane`) before it gets here.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use crate::config::{self, ConfigError};
use crate::driver::Registry;
use crate::herdr::HerdrClient;

/// Start timeout handed to `herdr agent start`.
const START_TIMEOUT_MS: u32 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Lead,
    Peer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Lead => "lead",
            Role::Peer => "peer",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "lead" => Some(Role::Lead),
            "peer" => Some(Role::Peer),
            _ => None,
        }
    }
}

pub struct SpawnSpec<'a> {
    /// The herdr agent name.
    pub name: &'a str,
    /// A roster kind; harness, model and effort come from SQLite.
    pub kind: &'a str,
    pub role: Role,
    pub pane: &'a str,
    pub project_id: Option<i64>,
    pub spawner_id: Option<i64>,
}

/// The harness a live agent was spawned with: the one pinned on its row at
/// spawn, so a later profile or kind change does not move it. A row that
/// predates the pin (no `harness`) falls back to resolving its kind. `None`
/// when neither resolves (callers fail closed).
pub fn live_harness(conn: &Connection, agent_id: i64) -> Option<config::Harness> {
    let (pinned, kind): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT harness, kind FROM agent WHERE id = ?1",
            [agent_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    match pinned {
        Some(h) => config::harness(conn, &h).ok().flatten(),
        None => config::resolve_spawn(conn, &kind?).ok().map(|t| t.harness),
    }
}

/// What a spawn resolved and recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spawned {
    pub agent_id: i64,
    pub harness: String,
    pub model: String,
    pub effort: Option<String>,
}

#[derive(Debug)]
pub enum SpawnError {
    Config(ConfigError),
    /// The kind's harness has no driver (yet).
    NoDriver(String),
    /// herdr refused the start: its error code, or the raw body.
    Herdr(String),
    Io(std::io::Error),
    Db(rusqlite::Error),
    /// An open agent of that name exists with a different role, kind or project.
    Conflict(String),
    /// The target pane already hosts another open agent: herdr is not asked to start there.
    PaneBusy(String),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::Config(e) => write!(f, "config: {e:?}"),
            SpawnError::NoDriver(h) => write!(f, "no driver for harness '{h}'"),
            SpawnError::Herdr(e) => write!(f, "herdr refused the start: {e}"),
            SpawnError::Io(e) => write!(f, "herdr could not be run: {e}"),
            SpawnError::Db(e) => write!(f, "database: {e}"),
            SpawnError::Conflict(m) => write!(f, "{m}"),
            SpawnError::PaneBusy(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for SpawnError {}

/// Accepts claude's workspace-trust dialog when `pane` shows it, then waits
/// until claude is really ready: the dialog text gone from the pane's last
/// lines, something drawn there (a blank pane is claude still booting) and
/// herdr reporting `idle`. herdr says `idle` right after the keys while claude
/// is still booting, and a prompt typed then is dropped.
fn accept_trust_dialog(herdr: &dyn HerdrClient, name: &str, pane: &str, ready: &Ready) -> Trust {
    let mut drawn = String::new();
    for i in 0..=ready.settle_polls {
        match herdr.pane_read(pane, 40) {
            Ok(r) if r.ok && !r.body.trim().is_empty() => {
                drawn = r.body;
                break;
            }
            _ => {}
        }
        if i < ready.settle_polls {
            std::thread::sleep(std::time::Duration::from_millis(ready.settle_ms));
        }
    }
    if !drawn.contains("trust") {
        return Trust::NoDialog;
    }
    if !herdr
        .agent_send_keys(name, &["down", "enter"])
        .is_ok_and(|r| r.ok)
    {
        return Trust::NotReady;
    }
    for i in 0..ready.polls {
        let gone = herdr
            .pane_read(pane, 12)
            .is_ok_and(|r| r.ok && !r.body.contains("trust") && !r.body.trim().is_empty());
        if gone && herdr.agent_wait(name, &["idle"], 1_000).is_ok_and(|r| r.ok) {
            return Trust::Ready;
        }
        if i + 1 < ready.polls {
            std::thread::sleep(std::time::Duration::from_millis(ready.poll_ms));
        }
    }
    Trust::NotReady
}

/// What [`accept_trust_dialog`] found.
#[derive(PartialEq)]
enum Trust {
    /// The pane shows no trust dialog.
    NoDialog,
    /// The dialog was accepted and claude is ready.
    Ready,
    /// The dialog was there and claude was not ready within the bound.
    NotReady,
}

/// How long [`spawn`] waits for claude to be really ready once its trust dialog
/// is accepted: `polls` looks, `poll_ms` apart.
pub struct Ready {
    pub polls: u32,
    pub poll_ms: u64,
    /// Looks (`settle_ms` apart) for claude to have drawn anything at all after
    /// the start: herdr can return before the dialog is on screen.
    pub settle_polls: u32,
    pub settle_ms: u64,
}

impl Ready {
    pub const DEFAULT: Ready = Ready {
        polls: 60,
        poll_ms: 500,
        settle_polls: 10,
        settle_ms: 100,
    };
}

pub fn spawn(
    conn: &Connection,
    herdr: &dyn HerdrClient,
    drivers: &Registry,
    spec: &SpawnSpec,
    now_ms: u64,
) -> Result<Spawned, SpawnError> {
    spawn_with(conn, herdr, drivers, spec, now_ms, &Ready::DEFAULT)
}

/// [`spawn`] for a worker whose code lives in `code_root` (its worktree):
/// the code-lookup MCP is rooted there instead of at the project repo.
pub fn spawn_rooted(
    conn: &Connection,
    herdr: &dyn HerdrClient,
    drivers: &Registry,
    spec: &SpawnSpec,
    now_ms: u64,
    code_root: &Path,
) -> Result<Spawned, SpawnError> {
    spawn_inner(conn, herdr, drivers, spec, now_ms, &Ready::DEFAULT, Some(code_root))
}

pub fn spawn_with(
    conn: &Connection,
    herdr: &dyn HerdrClient,
    drivers: &Registry,
    spec: &SpawnSpec,
    now_ms: u64,
    ready: &Ready,
) -> Result<Spawned, SpawnError> {
    spawn_inner(conn, herdr, drivers, spec, now_ms, ready, None)
}

/// Journals why a worker started without the code-lookup MCP, plus a stderr
/// line. Best effort. The stream is `lookup:<name>`, not `worker:<name>`:
/// `task.start` spawns before its workflow (id `worker:<name>`) exists, and
/// the runner reads any event already in its stream as history to replay, so a
/// row there would stop the workflow's first step from running. `run show`
/// reads `lookup:<name>` next to the worker's own events.
fn lookup_skipped(conn: &Connection, name: &str, now_ms: u64, reason: &str) {
    eprintln!("code-lookup skipped for {name}: {reason}");
    let _ = conn.execute_batch(crate::engine::journal_sqlite::JOURNAL_SCHEMA_SQL);
    let _ = conn.execute(
        "INSERT INTO events (workflow_id, seq, ts_ms, kind, payload_json) \
         SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, 'code_lookup_skipped', ?3 \
         FROM events WHERE workflow_id = ?1",
        params![
            format!("lookup:{name}"),
            now_ms as i64,
            serde_json::json!({ "worker": name, "reason": reason }).to_string()
        ],
    );
}

/// The launch args that start a Peer with the code-lookup MCP, or why not.
/// Never fails the spawn: every miss is journaled via [`lookup_skipped`].
fn lookup_launch_args(
    conn: &Connection,
    driver: &dyn crate::driver::HarnessDriver,
    spec: &SpawnSpec,
    now_ms: u64,
    code_root: Option<&Path>,
) -> Vec<String> {
    if spec.role != Role::Peer {
        return Vec::new();
    }
    let skip = |why: &str| {
        lookup_skipped(conn, spec.name, now_ms, why);
        Vec::new()
    };
    if driver.harness() != "claude" {
        return skip(&format!("harness '{}' has no lookup wiring", driver.harness()));
    }
    let root = match code_root {
        Some(r) => r.to_path_buf(),
        None => {
            let repo: Option<String> = spec.project_id.and_then(|id| {
                conn.query_row("SELECT repo_path FROM project WHERE id = ?1", [id], |r| r.get(0))
                    .optional()
                    .ok()
                    .flatten()
            });
            match repo {
                Some(r) => r.into(),
                None => return skip("no worktree and no project repo to root the lookup"),
            }
        }
    };
    let Some(db) = conn.path().filter(|p| !p.is_empty()).map(std::path::PathBuf::from) else {
        return skip("no database path to place the lookup home beside");
    };
    let home = db
        .parent()
        .unwrap_or(Path::new("."))
        .join("lookup")
        .join(spec.name);
    match crate::kb::prepare_code_lookup(&root, &home) {
        Ok(Some(l)) => crate::driver::lookup_args(driver, &l),
        Ok(None) => skip(&format!("no supported language among tracked files in {}", root.display())),
        Err(e) => skip(&format!("lookup setup failed for {}: {e}", root.display())),
    }
}

fn spawn_inner(
    conn: &Connection,
    herdr: &dyn HerdrClient,
    drivers: &Registry,
    spec: &SpawnSpec,
    now_ms: u64,
    ready: &Ready,
    code_root: Option<&Path>,
) -> Result<Spawned, SpawnError> {
    let target = config::resolve_spawn(conn, spec.kind).map_err(SpawnError::Config)?;
    let driver = drivers
        .resolve(&target.harness)
        .ok_or_else(|| SpawnError::NoDriver(target.harness.name.clone()))?;
    type Open = (i64, String, Option<String>, Option<i64>);
    let open: Option<Open> = conn
        .query_row(
            "SELECT id, role, kind, project_id FROM agent WHERE name = ?1 AND ended_ms IS NULL",
            [spec.name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(SpawnError::Db)?;
    if let Some((_, role, kind, project)) = &open {
        if (role.as_str(), kind.as_deref(), *project)
            != (spec.role.as_str(), Some(spec.kind), spec.project_id)
        {
            return Err(SpawnError::Conflict(format!(
                "agent '{}' is already open as {role}/{}/project {project:?}",
                spec.name,
                kind.as_deref().unwrap_or("-")
            )));
        }
    }
    let host: Option<String> = conn
        .query_row(
            "SELECT name FROM agent WHERE pane_id = ?1 AND name <> ?2 AND ended_ms IS NULL",
            params![spec.pane, spec.name],
            |r| r.get(0),
        )
        .optional()
        .map_err(SpawnError::Db)?;
    if let Some(host) = host {
        return Err(SpawnError::PaneBusy(format!(
            "pane {} already hosts open agent '{host}'; '{}' needs its own pane",
            spec.pane, spec.name
        )));
    }
    let mut args = driver.launch_args(&target.model, target.effort.as_deref());
    args.extend(lookup_launch_args(conn, &*driver, spec, now_ms, code_root));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let reply = herdr
        .agent_start(
            spec.name,
            driver.herdr_kind(),
            spec.pane,
            START_TIMEOUT_MS,
            &arg_refs,
        )
        .map_err(SpawnError::Io)?;
    // A fresh worktree is a folder claude has not been trusted in: its startup
    // dialog ("Quick safety check ... trust") may hold the start back as
    // `agent_not_ready`, or herdr may report the agent up while the dialog is
    // still on screen. Either way accept it the way a person would (one Down
    // to "Yes, I trust this folder", then Enter) and wait until claude is ready.
    let code = reply.error_code();
    if !reply.ok && code.as_deref() != Some("agent_not_ready") {
        return Err(SpawnError::Herdr(code.unwrap_or(reply.body)));
    }
    let trust = if driver.trusts_folder_dialog() {
        accept_trust_dialog(herdr, spec.name, spec.pane, ready)
    } else {
        Trust::NoDialog
    };
    let held = if reply.ok {
        trust == Trust::NotReady
    } else {
        trust != Trust::Ready
    };
    if held {
        return Err(SpawnError::Herdr(
            code.unwrap_or_else(|| "agent_not_ready".into()),
        ));
    }
    let agent_id = match open {
        Some((id, ..)) => id,
        None => {
            conn.execute(
                "INSERT INTO agent (name, role, kind, project_id, spawner_id, started_ms, pane_id, \
                 harness, model, effort) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    spec.name,
                    spec.role.as_str(),
                    spec.kind,
                    spec.project_id,
                    spec.spawner_id,
                    now_ms as i64,
                    spec.pane,
                    target.harness.name,
                    target.model,
                    target.effort
                ],
            )
            .map_err(SpawnError::Db)?;
            conn.last_insert_rowid()
        }
    };
    Ok(Spawned {
        agent_id,
        harness: target.harness.name,
        model: target.model,
        effort: target.effort,
    })
}
