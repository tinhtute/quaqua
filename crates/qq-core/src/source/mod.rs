//! Source interface (R24): a source yields items, each item files one
//! proposal, once. Sources are pulled only by the schedule
//! (`daemon::schedule`); nothing here listens on a socket or takes a push.
//!
//! Two sources ship: farm test and gate failures (loop 1) and anh iu's idea
//! inbox (loop 3), one markdown file per idea under `inbox/ideas/`.

use std::path::PathBuf;

use rusqlite::{params, Connection};

use crate::backlog::{self, Change, NewProposal, Text};
use crate::cli::{Install, Registry};
use crate::daemon::Daemon;

pub const INSTALL: Install = install;

fn install(_: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    Ok(())
}

/// One thing a source found: `key` is stable across pulls, so the same thing
/// files one proposal.
#[derive(Clone, Debug)]
pub struct Item {
    pub key: String,
    pub loop_no: u8,
    pub project_id: Option<i64>,
    pub change: Change,
    pub text: Text,
}

pub trait Source {
    /// The name the schedule row and the cursor rows use.
    fn name(&self) -> &str;
    fn pull(&self, conn: &Connection) -> Result<Vec<Item>, String>;
}

/// Files the item unless its key was filed before; returns the new proposal id.
pub fn file_once(
    conn: &mut Connection,
    source: &str,
    item: &Item,
    now_ms: i64,
) -> Result<Option<i64>, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let seen: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM source_cursor WHERE source = ?1 AND key = ?2)",
            params![source, item.key],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if seen {
        return Ok(None);
    }
    let id = backlog::file(
        &tx,
        &NewProposal {
            loop_no: item.loop_no,
            project_id: item.project_id,
            change: item.change.clone(),
            protocol_floor: None,
            text: item.text.clone(),
        },
        now_ms,
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO source_cursor (source, key, proposal_id, ts_ms) VALUES (?1, ?2, ?3, ?4)",
        params![source, item.key, id, now_ms],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(Some(id))
}

/// Run outcomes that count as a farm test or gate failure.
pub const FAILURE_OUTCOMES: [&str; 3] = ["check_failed", "gate_failed", "test_failed"];

/// Loop 1: every closed run whose outcome is a test or gate failure files one
/// bug proposal. A bug fix is reversible and stays inside the run's project.
pub struct FailureSource;

pub const FAILURE_SOURCE: &str = "farm-failures";

impl Source for FailureSource {
    fn name(&self) -> &str {
        FAILURE_SOURCE
    }

    fn pull(&self, conn: &Connection) -> Result<Vec<Item>, String> {
        let mut st = conn
            .prepare(
                "SELECT r.id, r.outcome, r.workflow_id, r.candidate_sha, r.task_id, \
                 COALESCE(t.project_id, a.project_id) \
                 FROM run r JOIN agent a ON a.id = r.agent_id \
                 LEFT JOIN task t ON t.id = r.task_id \
                 WHERE r.outcome IN (?1, ?2, ?3) ORDER BY r.id",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(
                params![FAILURE_OUTCOMES[0], FAILURE_OUTCOMES[1], FAILURE_OUTCOMES[2]],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .map_err(|e| e.to_string())?;
        let mut items = Vec::new();
        for row in rows {
            let (id, outcome, wf, sha, task, project) = row.map_err(|e| e.to_string())?;
            let wf = wf.unwrap_or_else(|| "unknown".into());
            items.push(Item {
                key: format!("run:{id}"),
                loop_no: 1,
                project_id: project,
                change: Change {
                    reversible: true,
                    projects: 1,
                    surfaces: vec![],
                    external_effect: false,
                },
                text: Text {
                    observation: Some(format!("run {id} ended with {outcome}")),
                    evidence: Some(format!(
                        "workflow {wf}, task {}, candidate sha {}",
                        task.map_or("none".into(), |t| t.to_string()),
                        sha.as_deref().unwrap_or("none")
                    )),
                    recommendation: Some("find the cause of the failure and fix it".into()),
                    ..Text::default()
                },
            });
        }
        Ok(items)
    }
}

/// Loop 3: every `*.md` file directly under `inbox/ideas/` files one feature
/// proposal. The file is the idea: its first line is the title and the whole
/// text is the observation; no other field is read. A feature has no project
/// yet, so the backlog rates it high and it waits for anh iu.
pub struct IdeaInbox {
    pub dir: PathBuf,
}

pub const IDEA_SOURCE: &str = "idea-inbox";

impl IdeaInbox {
    /// `workspace` is the workspace root; ideas live in `inbox/ideas/`.
    pub fn new(workspace: &std::path::Path) -> Self {
        Self { dir: workspace.join("inbox").join("ideas") }
    }
}

impl Source for IdeaInbox {
    fn name(&self) -> &str {
        IDEA_SOURCE
    }

    fn pull(&self, _: &Connection) -> Result<Vec<Item>, String> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(format!("{}: {e}", self.dir.display())),
        };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".md") && !n.starts_with('.'))
            .collect();
        names.sort();
        let mut items = Vec::new();
        for name in names {
            let body = std::fs::read_to_string(self.dir.join(&name))
                .map_err(|e| format!("{name}: {e}"))?;
            if body.trim().is_empty() {
                continue;
            }
            let title = body
                .lines()
                .map(|l| l.trim().trim_start_matches('#').trim())
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_string();
            items.push(Item {
                key: name.clone(),
                loop_no: 3,
                project_id: None,
                change: Change {
                    reversible: true,
                    projects: 0,
                    surfaces: vec![],
                    external_effect: false,
                },
                text: Text {
                    observation: Some(body),
                    evidence: Some(format!("inbox/ideas/{name}")),
                    question: Some(format!("build: {title}?")),
                    ..Text::default()
                },
            });
        }
        Ok(items)
    }
}
