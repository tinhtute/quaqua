//! The run record (R19): one row of `run` per worker flow, opened when the
//! agent is up and closed once with its end state. Cost is `unknown` until a
//! driver reports one. Both writes converge on repeat.

use rusqlite::{params, Connection, OptionalExtension};

/// What opens a run.
pub struct NewRun<'a> {
    pub workflow_id: &'a str,
    pub agent_id: i64,
    pub task_id: Option<i64>,
    pub kind: &'a str,
    pub harness: &'a str,
    pub model: &'a str,
    pub effort: Option<&'a str>,
    /// The branch the agent's work starts from, kept as a `base` artifact.
    pub base: Option<&'a str>,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub id: i64,
    pub workflow_id: String,
    pub agent: String,
    pub role: String,
    pub kind: String,
    pub harness: String,
    pub model: String,
    pub effort: Option<String>,
    pub candidate_sha: Option<String>,
    pub ended_ms: Option<u64>,
    pub outcome: Option<String>,
    pub cost_state: String,
    pub cost_usd: Option<f64>,
}

/// Opens the run of `workflow_id`; a repeat returns the run already there.
pub fn open(conn: &mut Connection, r: &NewRun) -> rusqlite::Result<i64> {
    let tx = conn.transaction()?;
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM run WHERE workflow_id = ?1",
            [r.workflow_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    tx.execute(
        "INSERT INTO run (task_id, agent_id, kind, workflow_id, harness, model, effort, \
         started_ms, cost_state) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'unknown')",
        params![
            r.task_id,
            r.agent_id,
            r.kind,
            r.workflow_id,
            r.harness,
            r.model,
            r.effort,
            r.now_ms as i64
        ],
    )?;
    let id = tx.last_insert_rowid();
    if let Some(base) = r.base {
        tx.execute(
            "INSERT INTO run_artifact (run_id, kind, path) VALUES (?1, 'base', ?2)",
            params![id, base],
        )?;
    }
    tx.commit()?;
    Ok(id)
}

/// Closes the run of `workflow_id` once: the first end state stays, a repeat
/// changes nothing. Returns true when this call closed it.
pub fn close(
    conn: &Connection,
    workflow_id: &str,
    outcome: &str,
    candidate_sha: Option<&str>,
    now_ms: u64,
) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "UPDATE run SET ended_ms = ?2, outcome = ?3, candidate_sha = ?4 \
         WHERE workflow_id = ?1 AND ended_ms IS NULL",
        params![workflow_id, now_ms as i64, outcome, candidate_sha],
    )?;
    Ok(n == 1)
}

/// The run of `workflow_id`, with its agent's name and role.
pub fn by_workflow(conn: &Connection, workflow_id: &str) -> rusqlite::Result<Option<RunRow>> {
    conn.query_row(
        "SELECT r.id, r.workflow_id, a.name, a.role, r.kind, r.harness, r.model, r.effort, \
         r.candidate_sha, r.ended_ms, r.outcome, r.cost_state, r.cost_usd \
         FROM run r JOIN agent a ON a.id = r.agent_id WHERE r.workflow_id = ?1",
        [workflow_id],
        |row| {
            Ok(RunRow {
                id: row.get(0)?,
                workflow_id: row.get(1)?,
                agent: row.get(2)?,
                role: row.get(3)?,
                kind: row.get(4)?,
                harness: row.get(5)?,
                model: row.get(6)?,
                effort: row.get(7)?,
                candidate_sha: row.get(8)?,
                ended_ms: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                outcome: row.get(10)?,
                cost_state: row.get(11)?,
                cost_usd: row.get(12)?,
            })
        },
    )
    .optional()
}
