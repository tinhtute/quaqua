//! Backlog (R23): one proposal table with a risk level, the low/high rule, the
//! CoS verdict on low risk, anh iu's verdict on high, the task an approved
//! proposal becomes, and score rows that carry their time. Nothing here
//! computes or concludes the effect of a proposal.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::cli::Install;

mod verbs;

pub const INSTALL: Install = verbs::install;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    Low,
    High,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::High => "high",
        }
    }
}

/// A surface whose change is always high risk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Rule,
    Skill,
    Config,
    Protocol,
}

/// What a proposal would change, as the three loops state it.
#[derive(Clone, Debug)]
pub struct Change {
    pub reversible: bool,
    /// Projects the change reaches; low needs exactly one.
    pub projects: u32,
    pub surfaces: Vec<Surface>,
    pub external_effect: bool,
}

/// R23: low means reversible, inside one project, no rule/skill/config/protocol
/// change and no external effect; everything else is high. A protocol floor
/// can only tighten the result.
pub fn classify(change: &Change, protocol_floor: Option<Risk>) -> Risk {
    let low = change.reversible
        && change.projects == 1
        && change.surfaces.is_empty()
        && !change.external_effect;
    let base = if low { Risk::Low } else { Risk::High };
    base.max(protocol_floor.unwrap_or(Risk::Low))
}

#[derive(Clone, Debug, Default)]
pub struct Text {
    pub observation: Option<String>,
    pub evidence: Option<String>,
    pub suspected_mechanism: Option<String>,
    pub impact: Option<String>,
    pub question: Option<String>,
    pub recommendation: Option<String>,
}

pub struct NewProposal {
    /// The loop that proposes: 1 bug fixes, 2 self-improvement, 3 product.
    pub loop_no: u8,
    pub project_id: Option<i64>,
    pub change: Change,
    pub protocol_floor: Option<Risk>,
    pub text: Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decider {
    Cos,
    Owner,
}

impl Decider {
    fn as_str(self) -> &'static str {
        match self {
            Decider::Cos => "cos",
            Decider::Owner => "owner",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BacklogError {
    NoSuchProposal(i64),
    NotOpen(i64),
    /// High risk waits for anh iu: the CoS cannot decide it.
    OwnerOnly(i64),
    NotApproved(i64),
    NoProject(i64),
    Sqlite(String),
}

impl std::fmt::Display for BacklogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BacklogError::NoSuchProposal(id) => write!(f, "no proposal {id}"),
            BacklogError::NotOpen(id) => write!(f, "proposal {id} is not open"),
            BacklogError::OwnerOnly(id) => {
                write!(f, "proposal {id} is high risk and waits for anh iu")
            }
            BacklogError::NotApproved(id) => write!(f, "proposal {id} is not approved"),
            BacklogError::NoProject(id) => write!(f, "proposal {id} names no project"),
            BacklogError::Sqlite(e) => write!(f, "sqlite: {e}"),
        }
    }
}

impl std::error::Error for BacklogError {}

impl From<rusqlite::Error> for BacklogError {
    fn from(e: rusqlite::Error) -> Self {
        BacklogError::Sqlite(e.to_string())
    }
}

/// Files an open proposal; the risk is derived, never passed in. A proposal
/// that names no project is not inside one project, so it is high.
pub fn file(conn: &Connection, p: &NewProposal, now_ms: i64) -> Result<i64, BacklogError> {
    let mut risk = classify(&p.change, p.protocol_floor);
    if p.project_id.is_none() {
        risk = Risk::High;
    }
    conn.execute(
        "INSERT INTO proposal (loop, risk, project_id, observation, evidence, \
         suspected_mechanism, impact, question, recommendation, created_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            p.loop_no,
            risk.as_str(),
            p.project_id,
            p.text.observation,
            p.text.evidence,
            p.text.suspected_mechanism,
            p.text.impact,
            p.text.question,
            p.text.recommendation,
            now_ms
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Records a verdict and moves the proposal out of 'open'. The CoS decides
/// low risk only; the owner decides any.
pub fn decide(
    conn: &mut Connection,
    proposal_id: i64,
    by: Decider,
    approve: bool,
    note: Option<&str>,
    now_ms: i64,
) -> Result<(), BacklogError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT risk, status FROM proposal WHERE id = ?1",
            [proposal_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (risk, status) = row.ok_or(BacklogError::NoSuchProposal(proposal_id))?;
    if status != "open" {
        return Err(BacklogError::NotOpen(proposal_id));
    }
    if by == Decider::Cos && risk != "low" {
        return Err(BacklogError::OwnerOnly(proposal_id));
    }
    tx.execute(
        "INSERT INTO decision (proposal_id, decider_role, verdict, note, ts_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            proposal_id,
            by.as_str(),
            if approve { "approve" } else { "reject" },
            note,
            now_ms
        ],
    )?;
    tx.execute(
        "UPDATE proposal SET status = ?2 WHERE id = ?1",
        params![proposal_id, if approve { "approved" } else { "rejected" }],
    )?;
    tx.commit()?;
    Ok(())
}

/// Turns an approved proposal into a task that carries the proposal id.
/// A second call returns the same task.
pub fn task_from_proposal(
    conn: &mut Connection,
    proposal_id: i64,
    title: &str,
    disposition: &str,
    kind: Option<&str>,
    now_ms: i64,
) -> Result<i64, BacklogError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<(String, Option<i64>)> = tx
        .query_row(
            "SELECT status, project_id FROM proposal WHERE id = ?1",
            [proposal_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (status, project) = row.ok_or(BacklogError::NoSuchProposal(proposal_id))?;
    if status != "approved" {
        return Err(BacklogError::NotApproved(proposal_id));
    }
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM task WHERE proposal_id = ?1 ORDER BY id LIMIT 1",
            [proposal_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let project = project.ok_or(BacklogError::NoProject(proposal_id))?;
    tx.execute(
        "INSERT INTO task (project_id, title, kind, disposition, state, proposal_id, created_ms) \
         VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?6)",
        params![project, title, kind, disposition, proposal_id, now_ms],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(id)
}

/// Records one rubric score; the row always carries its time.
pub fn record_score(
    conn: &Connection,
    run_id: i64,
    rubric: &str,
    value: f64,
    detail_json: Option<&str>,
    now_ms: i64,
) -> Result<i64, BacklogError> {
    conn.execute(
        "INSERT INTO score (run_id, rubric, value, detail_json, ts_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![run_id, rubric, value, detail_json, now_ms],
    )?;
    Ok(conn.last_insert_rowid())
}
