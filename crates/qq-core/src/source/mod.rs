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

/// The persisted gate failure: a pipeline that pauses with merge code 4 and
/// a refused `check_failed` (the project check went red after the merge) or
/// `base_broken` (it is red on the base too). The reason lands in the
/// journal as a `pause_for_approval` event, `blocked 4: <word>: <detail>`.
const GATE_WORDS: [&str; 2] = ["check_failed", "base_broken"];

/// Loop 1: a gate failure in the journal files one bug proposal. The
/// signature is the workflow and the failure word, so a retried pipeline that
/// fails the same way again files nothing more. A bug fix is reversible and
/// stays inside the workflow's project.
pub struct FailureSource;

pub const FAILURE_SOURCE: &str = "farm-failures";

impl Source for FailureSource {
    fn name(&self) -> &str {
        FAILURE_SOURCE
    }

    fn pull(&self, conn: &Connection) -> Result<Vec<Item>, String> {
        let has_journal: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'events')",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !has_journal {
            return Ok(vec![]);
        }
        let mut st = conn
            .prepare(
                "SELECT e.workflow_id, e.seq, json_extract(e.payload_json, '$.reason'), \
                 w.project_id, w.task_id \
                 FROM events e LEFT JOIN workflow w ON w.id = e.workflow_id \
                 WHERE e.kind = 'pause_for_approval' \
                 AND json_extract(e.payload_json, '$.reason') LIKE 'blocked 4: %' \
                 ORDER BY e.workflow_id, e.seq",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut items: Vec<Item> = Vec::new();
        for row in rows {
            let (wf, seq, reason, project, task) = row.map_err(|e| e.to_string())?;
            let detail = reason.trim_start_matches("blocked 4: ");
            let Some(word) = GATE_WORDS.iter().find(|w| detail.starts_with(**w)) else {
                continue;
            };
            let key = format!("{wf}:{word}");
            if items.iter().any(|i| i.key == key) {
                continue;
            }
            items.push(Item {
                key,
                loop_no: 1,
                project_id: project,
                change: Change {
                    reversible: true,
                    projects: 1,
                    surfaces: vec![],
                    external_effect: false,
                },
                text: Text {
                    observation: Some(format!("the gate of workflow {wf} failed: {word}")),
                    evidence: Some(format!(
                        "journal event {wf}#{seq}, task {}: {detail}",
                        task.map_or("none".into(), |t| t.to_string())
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
