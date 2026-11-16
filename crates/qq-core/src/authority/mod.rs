//! Phase `switch-ready` surface authority (plan task T42).
//!
//! The role topology (R8) and the fence (R11) as decisions. Callers hold a
//! `rusqlite::Connection` on `runs/qq.db`; each `check_*` reads the topology
//! edges and the `agent` rows, answers [`Decision::Allowed`] or
//! [`Decision::Refused`], and journals every refusal as one `events` row
//! (workflow id [`JOURNAL_STREAM`], kind [`REFUSAL_KIND`]) carrying the caller
//! and the edge. A check is a read plus, on refusal, one append: repeating it
//! gives the same answer. An `Err` means the decision could not be made; the
//! daemon must treat it as a refusal (fail closed).
//!
//! Hard rules hold whatever the config says: Peer-to-Peer mail, a Peer
//! mailing anyone but its spawner, a Peer spawning or running any
//! orchestration call, a second active Lead in a project, and a push from an
//! unfenced driver.

mod gate;
mod tokenizer;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use crate::cli::{Install, Registry};
use crate::config::{self, Driver};
use crate::daemon::Daemon;
use crate::engine::journal_sqlite::JOURNAL_SCHEMA_SQL;

pub use gate::{check as gate_check, evaluate as gate_evaluate, Verdict as GateVerdict};

/// Workflow id of the stream that holds refusals in `events`.
pub const JOURNAL_STREAM: &str = "authority";
/// Event kind of a refusal row.
pub const REFUSAL_KIND: &str = "refusal";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Cos,
    Supervisor,
    Lead,
    Peer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Cos => "cos",
            Role::Supervisor => "supervisor",
            Role::Lead => "lead",
            Role::Peer => "peer",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "cos" => Some(Role::Cos),
            "supervisor" => Some(Role::Supervisor),
            "lead" => Some(Role::Lead),
            "peer" => Some(Role::Peer),
            _ => None,
        }
    }
}

/// Whether a driver's CLI has a native hook that fences it (R11). A driver
/// without one is unfenced: it builds only in its worktree and never pushes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fence {
    Fenced,
    Unfenced,
}

/// The fence a harness driver gets until T41 marks each driver itself: the
/// generic driver has no native hook; native drivers do.
pub fn fence_for(driver: Driver) -> Fence {
    match driver {
        Driver::Native => Fence::Fenced,
        Driver::Generic => Fence::Unfenced,
    }
}

/// An orchestration call R11 keeps from a Peer identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Spawn,
    Merge,
    Push,
    ConfigWrite,
}

impl Op {
    fn as_str(self) -> &'static str {
        match self {
            Op::Spawn => "spawn",
            Op::Merge => "merge",
            Op::Push => "push",
            Op::ConfigWrite => "config_write",
        }
    }
}

/// A refusal: the caller, the edge it tried (`spawn`, `mail`, `prompt`,
/// `question`, `orchestration`, `push`), the target, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub caller: String,
    pub edge: String,
    pub target: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    Refused(Refusal),
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Decision::Allowed)
    }
}

/// The decision could not be made (database or config unreadable).
#[derive(Debug)]
pub struct AuthError(pub String);

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "authority: {}", self.0)
    }
}

impl std::error::Error for AuthError {}

impl From<rusqlite::Error> for AuthError {
    fn from(e: rusqlite::Error) -> Self {
        AuthError(e.to_string())
    }
}

impl From<config::ConfigError> for AuthError {
    fn from(e: config::ConfigError) -> Self {
        AuthError(e.to_string())
    }
}

/// An active agent, resolved from the `agent` table by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub id: i64,
    pub name: String,
    pub role: Role,
    pub project_id: Option<i64>,
    pub spawner_id: Option<i64>,
}

/// The newest active (not ended) agent called `name`.
pub fn resolve(conn: &Connection, name: &str) -> Result<Option<Actor>, AuthError> {
    let row = conn
        .query_row(
            "SELECT id, name, role, project_id, spawner_id FROM agent \
             WHERE name = ?1 AND ended_ms IS NULL ORDER BY started_ms DESC, id DESC LIMIT 1",
            [name],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .optional()?;
    Ok(row.and_then(|(id, name, role, project_id, spawner_id)| {
        Role::parse(&role).map(|role| Actor {
            id,
            name,
            role,
            project_id,
            spawner_id,
        })
    }))
}

fn has_edge(conn: &Connection, from: Role, to: Role, edge: &str) -> Result<bool, AuthError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM topology_edge WHERE from_role = ?1 AND to_role = ?2 AND edge = ?3",
            params![from.as_str(), to.as_str(), edge],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Journals a refusal and returns it as a decision.
fn refuse(
    conn: &Connection,
    caller: &str,
    edge: &str,
    target: &str,
    reason: &str,
) -> Result<Decision, AuthError> {
    let r = Refusal {
        caller: caller.to_string(),
        edge: edge.to_string(),
        target: target.to_string(),
        reason: reason.to_string(),
    };
    let role = resolve(conn, caller)?.map(|a| a.role.as_str());
    let payload = json!({
        "caller": r.caller, "caller_role": role, "edge": r.edge,
        "target": r.target, "reason": r.reason,
    });
    conn.execute_batch(JOURNAL_SCHEMA_SQL)?;
    // One statement picks the next seq and inserts, so two writers cannot
    // take the same one.
    conn.execute(
        "INSERT INTO events (workflow_id, seq, ts_ms, kind, payload_json) \
         SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4 FROM events WHERE workflow_id = ?1",
        params![JOURNAL_STREAM, now_ms(), REFUSAL_KIND, payload.to_string()],
    )?;
    Ok(Decision::Refused(r))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Every journaled refusal, oldest first.
pub fn refusals(conn: &Connection) -> Result<Vec<Refusal>, AuthError> {
    conn.execute_batch(JOURNAL_SCHEMA_SQL)?;
    let mut st = conn.prepare(
        "SELECT payload_json FROM events WHERE workflow_id = ?1 AND kind = ?2 ORDER BY seq",
    )?;
    let rows = st
        .query_map(params![JOURNAL_STREAM, REFUSAL_KIND], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.iter()
        .map(|p| {
            let v: serde_json::Value =
                serde_json::from_str(p).map_err(|e| AuthError(e.to_string()))?;
            let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            Ok(Refusal {
                caller: s("caller"),
                edge: s("edge"),
                target: s("target"),
                reason: s("reason"),
            })
        })
        .collect()
}

fn unknown(conn: &Connection, caller: &str, edge: &str, who: &str) -> Result<Decision, AuthError> {
    refuse(
        conn,
        caller,
        edge,
        who,
        &format!("'{who}' is not an active agent"),
    )
}

/// May `caller` spawn an agent of `target_role` into `project`?
/// Needs a `spawn` edge; a Peer never spawns; a project keeps one active Lead.
pub fn check_spawn(
    conn: &Connection,
    caller: &str,
    target_role: Role,
    project: Option<i64>,
) -> Result<Decision, AuthError> {
    let target = target_role.as_str();
    let Some(c) = resolve(conn, caller)? else {
        return unknown(conn, caller, "spawn", caller);
    };
    if c.role == Role::Peer {
        return refuse(conn, caller, "spawn", target, "a Peer can spawn nothing");
    }
    if !has_edge(conn, c.role, target_role, "spawn")? {
        return refuse(
            conn,
            caller,
            "spawn",
            target,
            &format!("no spawn edge {} -> {target}", c.role.as_str()),
        );
    }
    if target_role == Role::Lead {
        if let Some(p) = project {
            let lead: Option<i64> = conn
                .query_row(
                    "SELECT id FROM agent WHERE project_id = ?1 AND role = 'lead' AND ended_ms IS NULL",
                    [p],
                    |r| r.get(0),
                )
                .optional()?;
            if lead.is_some() {
                return refuse(conn, caller, "spawn", target, "one Lead per project");
            }
        }
    }
    Ok(Decision::Allowed)
}

/// May `caller` mail `target` (both agent names)?
/// Peer-to-Peer is refused; a Peer mails only its spawner; any other pair
/// needs a `mail`, `spawn` or `report` edge from the caller's role to the
/// target's. A Supervisor's `question` edge does not carry mail.
pub fn check_mail(conn: &Connection, caller: &str, target: &str) -> Result<Decision, AuthError> {
    let Some(c) = resolve(conn, caller)? else {
        return unknown(conn, caller, "mail", caller);
    };
    let Some(t) = resolve(conn, target)? else {
        return unknown(conn, caller, "mail", target);
    };
    if c.role == Role::Peer {
        if t.role == Role::Peer {
            return refuse(conn, caller, "mail", target, "Peer-to-Peer mail is refused");
        }
        if c.spawner_id != Some(t.id) {
            return refuse(
                conn,
                caller,
                "mail",
                target,
                "a Peer mails only its spawner",
            );
        }
        return Ok(Decision::Allowed);
    }
    for edge in ["mail", "spawn", "report"] {
        if has_edge(conn, c.role, t.role, edge)? {
            return Ok(Decision::Allowed);
        }
    }
    refuse(
        conn,
        caller,
        "mail",
        target,
        &format!("no mail edge {} -> {}", c.role.as_str(), t.role.as_str()),
    )
}

/// May `caller` prompt `target`'s pane? The caller role's `can_prompt`
/// setting (from team.toml) must list the target's role; no setting, no edge.
pub fn check_prompt(conn: &Connection, caller: &str, target: &str) -> Result<Decision, AuthError> {
    let Some(c) = resolve(conn, caller)? else {
        return unknown(conn, caller, "prompt", caller);
    };
    let Some(t) = resolve(conn, target)? else {
        return unknown(conn, caller, "prompt", target);
    };
    let listed = config::setting(conn, &format!("roles.{}.can_prompt", c.role.as_str()))?
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|l| l.iter().any(|x| x.as_str() == Some(t.role.as_str())));
    if c.role != Role::Peer && listed {
        Ok(Decision::Allowed)
    } else {
        refuse(
            conn,
            caller,
            "prompt",
            target,
            &format!("no prompt edge {} -> {}", c.role.as_str(), t.role.as_str()),
        )
    }
}

/// May `caller` put a question to `target`? Only a `question` edge carries
/// it (the Supervisor's edge to Leads); a Peer is never a target.
pub fn check_question(
    conn: &Connection,
    caller: &str,
    target: &str,
) -> Result<Decision, AuthError> {
    let Some(c) = resolve(conn, caller)? else {
        return unknown(conn, caller, "question", caller);
    };
    let Some(t) = resolve(conn, target)? else {
        return unknown(conn, caller, "question", target);
    };
    if t.role != Role::Peer && has_edge(conn, c.role, t.role, "question")? {
        Ok(Decision::Allowed)
    } else {
        refuse(
            conn,
            caller,
            "question",
            target,
            &format!(
                "no question edge {} -> {}",
                c.role.as_str(),
                t.role.as_str()
            ),
        )
    }
}

/// May `caller` run an orchestration call (spawn, merge, push, config write)?
/// A Peer identity never may, on any driver. A push also needs a fenced driver.
pub fn check_orchestration(
    conn: &Connection,
    caller: &str,
    op: Op,
    fence: Fence,
) -> Result<Decision, AuthError> {
    let Some(c) = resolve(conn, caller)? else {
        return unknown(conn, caller, "orchestration", caller);
    };
    if c.role == Role::Peer {
        return refuse(
            conn,
            caller,
            "orchestration",
            op.as_str(),
            "a Peer identity runs no orchestration call",
        );
    }
    if op == Op::Push && fence == Fence::Unfenced {
        return refuse(
            conn,
            caller,
            "push",
            op.as_str(),
            "an unfenced driver never pushes",
        );
    }
    Ok(Decision::Allowed)
}

/// Registers the `gate` command. Called from `cli::MODULES` at wave end.
pub fn install(registry: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    registry.register(gate::command())
}

/// The entry `cli::MODULES` lists.
pub const INSTALL: Install = install;
