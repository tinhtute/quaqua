//! The one spawn path. A Lead and a Peer go through the same function; they
//! differ only by data (`role`, `base`, `project_id`).
//!
//! Steps, each safe to repeat: resolve the kind from SQLite, find the driver
//! of its harness, `herdr agent start` (herdr converges on the same name,
//! kind and pane), then record the agent unless an open one with that name
//! exists. A pane is an input: herdr 0.9.5's client has no pane split.

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
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::Config(e) => write!(f, "config: {e:?}"),
            SpawnError::NoDriver(h) => write!(f, "no driver for harness '{h}'"),
            SpawnError::Herdr(e) => write!(f, "herdr refused the start: {e}"),
            SpawnError::Io(e) => write!(f, "herdr could not be run: {e}"),
            SpawnError::Db(e) => write!(f, "database: {e}"),
        }
    }
}

impl std::error::Error for SpawnError {}

pub fn spawn(
    conn: &Connection,
    herdr: &dyn HerdrClient,
    drivers: &Registry,
    spec: &SpawnSpec,
    now_ms: u64,
) -> Result<Spawned, SpawnError> {
    let target = config::resolve_spawn(conn, spec.kind).map_err(SpawnError::Config)?;
    let driver = drivers
        .get(&target.harness.name)
        .ok_or_else(|| SpawnError::NoDriver(target.harness.name.clone()))?;
    let args = driver.launch_args(&target.model, target.effort.as_deref());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let reply = herdr
        .agent_start(
            spec.name,
            &driver.herdr_kind,
            spec.pane,
            START_TIMEOUT_MS,
            &arg_refs,
        )
        .map_err(SpawnError::Io)?;
    if !reply.ok {
        return Err(SpawnError::Herdr(reply.error_code().unwrap_or(reply.body)));
    }
    let open: Option<i64> = conn
        .query_row(
            "SELECT id FROM agent WHERE name = ?1 AND ended_ms IS NULL",
            [spec.name],
            |r| r.get(0),
        )
        .optional()
        .map_err(SpawnError::Db)?;
    let agent_id = match open {
        Some(id) => id,
        None => {
            conn.execute(
                "INSERT INTO agent (name, role, kind, project_id, spawner_id, started_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    spec.name,
                    spec.role.as_str(),
                    spec.kind,
                    spec.project_id,
                    spec.spawner_id,
                    now_ms as i64
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
