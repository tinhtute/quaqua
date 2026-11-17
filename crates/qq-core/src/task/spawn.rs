//! The one spawn path. A Lead and a Peer go through the same function; they
//! differ only by data (`role`, `base`, `project_id`).
//!
//! Steps, each safe to repeat: resolve the kind from SQLite, find the driver
//! of its harness, `herdr agent start` (herdr converges on the same name,
//! kind and pane), then record the agent unless an open one with that name
//! exists. The pane is an input; `task.start` fills a missing one from grid
//! placement (`worker_flow::place_pane`) before it gets here.

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
/// for the agent to be idle. `false`: no dialog, or the agent did not come up.
fn accept_trust_dialog(herdr: &dyn HerdrClient, name: &str, pane: &str, _ready: &Ready) -> bool {
    let up = herdr
        .pane_read(pane, 40)
        .is_ok_and(|r| r.ok && r.body.contains("trust"));
    up && herdr.agent_send_keys(name, &["down", "enter"]).is_ok_and(|r| r.ok)
        && herdr
            .agent_wait(name, &["idle"], u64::from(START_TIMEOUT_MS))
            .is_ok_and(|r| r.ok)
}

/// How long [`spawn`] waits for claude to be really ready once its trust dialog
/// is accepted: `polls` looks, `poll_ms` apart.
pub struct Ready {
    pub polls: u32,
    pub poll_ms: u64,
}

impl Ready {
    pub const DEFAULT: Ready = Ready {
        polls: 60,
        poll_ms: 500,
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

pub fn spawn_with(
    conn: &Connection,
    herdr: &dyn HerdrClient,
    drivers: &Registry,
    spec: &SpawnSpec,
    now_ms: u64,
    ready: &Ready,
) -> Result<Spawned, SpawnError> {
    let target = config::resolve_spawn(conn, spec.kind).map_err(SpawnError::Config)?;
    let driver = drivers
        .get(&target.harness.name)
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
    let args = driver.launch_args(&target.model, target.effort.as_deref());
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
    if !reply.ok {
        let code = reply.error_code();
        // A fresh worktree is a folder claude has not been trusted in: its
        // startup dialog ("Quick safety check ... trust") holds the start back
        // as `agent_not_ready`. Accept it the way a person would (one Down to
        // "Yes, I trust this folder", then Enter) and wait for the agent.
        let cleared = code.as_deref() == Some("agent_not_ready")
            && driver.herdr_kind() == "claude"
            && accept_trust_dialog(herdr, spec.name, spec.pane, ready);
        if !cleared {
            return Err(SpawnError::Herdr(code.unwrap_or(reply.body)));
        }
    }
    let agent_id = match open {
        Some((id, ..)) => id,
        None => {
            conn.execute(
                "INSERT INTO agent (name, role, kind, project_id, spawner_id, started_ms, pane_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    spec.name,
                    spec.role.as_str(),
                    spec.kind,
                    spec.project_id,
                    spec.spawner_id,
                    now_ms as i64,
                    spec.pane
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
