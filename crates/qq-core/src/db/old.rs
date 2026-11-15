//! Read-only reader for the old `runs/quaqua.db` (bus and journal rows) and
//! the per-worker sidecar files in `runs/`.
//!
//! Rows are plain strings and integers: nothing passes through a validated
//! harness or kind type, so rows that name `omp` or `sude` read back as
//! written. The database is opened read-only and never created or migrated.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use super::path::live_db_open_refused;

#[derive(Debug)]
pub enum OldDbError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// The path is a live database and the process is in test mode.
    LiveDb(PathBuf),
    /// No file at the path; a missing database is not an empty one.
    NotFound(PathBuf),
    /// The pre-bus schema (`farm_events`/`mail` tables without `triggers`).
    PreBus,
}

impl std::fmt::Display for OldDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OldDbError::Sqlite(e) => write!(f, "sqlite: {e}"),
            OldDbError::Io(e) => write!(f, "io: {e}"),
            OldDbError::LiveDb(p) => write!(f, "refused to open live database {}", p.display()),
            OldDbError::NotFound(p) => write!(f, "no database at {}", p.display()),
            OldDbError::PreBus => write!(f, "pre-bus database schema is not read"),
        }
    }
}

impl std::error::Error for OldDbError {}

impl From<rusqlite::Error> for OldDbError {
    fn from(e: rusqlite::Error) -> Self {
        OldDbError::Sqlite(e)
    }
}

impl From<std::io::Error> for OldDbError {
    fn from(e: std::io::Error) -> Self {
        OldDbError::Io(e)
    }
}

/// A journal row (`events`; a verdict is an event of kind `verdict`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OldEvent {
    pub workflow_id: String,
    pub seq: i64,
    pub ts_ms: i64,
    pub kind: String,
    pub payload_json: String,
}

impl OldEvent {
    pub fn payload(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.payload_json)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OldSnapshot {
    pub workflow_id: String,
    pub seq: i64,
    pub state_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OldThread {
    pub id: String,
    pub owner: String,
    pub state: String,
    pub last_trigger_ms: i64,
}

/// A bus row. Mail is a trigger with `source == "mail"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OldTrigger {
    pub id: i64,
    pub ts_ms: i64,
    pub source: String,
    pub kind: String,
    pub actor: String,
    pub to_name: Option<String>,
    pub thread_id: String,
    pub summary: String,
    pub reference: Option<String>,
    pub payload_json: Option<String>,
    pub dedupe_key: Option<String>,
    pub farm: String,
}

impl OldTrigger {
    pub fn payload(&self) -> Option<Result<serde_json::Value, serde_json::Error>> {
        self.payload_json.as_deref().map(serde_json::from_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OldSubscription {
    pub thread_id: String,
    pub consumer: String,
    pub until_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OldCursor {
    pub consumer: String,
    pub trigger_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OldAck {
    pub trigger_id: i64,
    pub acked_ms: i64,
    pub acked_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OldSourceState {
    pub source: String,
    pub value: String,
}

pub struct OldDb {
    conn: Connection,
}

impl OldDb {
    /// Opens the old database read-only. No directory or file is created, no
    /// `journal_mode` is set and nothing is migrated.
    pub fn open(path: &Path) -> Result<Self, OldDbError> {
        if live_db_open_refused(path) {
            return Err(OldDbError::LiveDb(path.to_path_buf()));
        }
        if !path.is_file() {
            return Err(OldDbError::NotFound(path.to_path_buf()));
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "query_only", "ON")?;
        let db = Self { conn };
        if !db.has_table("triggers")? && (db.has_table("farm_events")? || db.has_table("mail")?) {
            return Err(OldDbError::PreBus);
        }
        Ok(db)
    }

    fn has_table(&self, name: &str) -> rusqlite::Result<bool> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
    }

    /// Rows of `sql` when `table` exists, else none.
    fn rows<T>(
        &self,
        table: &str,
        sql: &str,
        map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>, OldDbError> {
        if !self.has_table(table)? {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], map)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn events(&self) -> Result<Vec<OldEvent>, OldDbError> {
        self.rows(
            "events",
            "SELECT workflow_id, seq, ts_ms, kind, payload_json FROM events ORDER BY workflow_id, seq",
            |r| {
                Ok(OldEvent {
                    workflow_id: r.get(0)?,
                    seq: r.get(1)?,
                    ts_ms: r.get(2)?,
                    kind: r.get(3)?,
                    payload_json: r.get(4)?,
                })
            },
        )
    }

    pub fn snapshots(&self) -> Result<Vec<OldSnapshot>, OldDbError> {
        self.rows(
            "snapshots",
            "SELECT workflow_id, seq, state_json FROM snapshots ORDER BY workflow_id, seq",
            |r| {
                Ok(OldSnapshot {
                    workflow_id: r.get(0)?,
                    seq: r.get(1)?,
                    state_json: r.get(2)?,
                })
            },
        )
    }

    pub fn threads(&self) -> Result<Vec<OldThread>, OldDbError> {
        self.rows(
            "threads",
            "SELECT id, owner, state, last_trigger_ms FROM threads ORDER BY id",
            |r| {
                Ok(OldThread {
                    id: r.get(0)?,
                    owner: r.get(1)?,
                    state: r.get(2)?,
                    last_trigger_ms: r.get(3)?,
                })
            },
        )
    }

    pub fn triggers(&self) -> Result<Vec<OldTrigger>, OldDbError> {
        self.rows(
            "triggers",
            "SELECT id, ts_ms, source, kind, actor, to_name, thread_id, summary, ref, \
             payload_json, dedupe_key, farm FROM triggers ORDER BY id",
            |r| {
                Ok(OldTrigger {
                    id: r.get(0)?,
                    ts_ms: r.get(1)?,
                    source: r.get(2)?,
                    kind: r.get(3)?,
                    actor: r.get(4)?,
                    to_name: r.get(5)?,
                    thread_id: r.get(6)?,
                    summary: r.get(7)?,
                    reference: r.get(8)?,
                    payload_json: r.get(9)?,
                    dedupe_key: r.get(10)?,
                    farm: r.get(11)?,
                })
            },
        )
    }

    pub fn subscriptions(&self) -> Result<Vec<OldSubscription>, OldDbError> {
        self.rows(
            "subscriptions",
            "SELECT thread_id, consumer, until_ms FROM subscriptions ORDER BY thread_id, consumer",
            |r| {
                Ok(OldSubscription {
                    thread_id: r.get(0)?,
                    consumer: r.get(1)?,
                    until_ms: r.get(2)?,
                })
            },
        )
    }

    pub fn cursors(&self) -> Result<Vec<OldCursor>, OldDbError> {
        self.rows(
            "cursors",
            "SELECT consumer, trigger_id FROM cursors ORDER BY consumer",
            |r| {
                Ok(OldCursor {
                    consumer: r.get(0)?,
                    trigger_id: r.get(1)?,
                })
            },
        )
    }

    pub fn acks(&self) -> Result<Vec<OldAck>, OldDbError> {
        self.rows(
            "trigger_acks",
            "SELECT trigger_id, acked_ms, acked_by FROM trigger_acks ORDER BY trigger_id",
            |r| {
                Ok(OldAck {
                    trigger_id: r.get(0)?,
                    acked_ms: r.get(1)?,
                    acked_by: r.get(2)?,
                })
            },
        )
    }

    pub fn source_state(&self) -> Result<Vec<OldSourceState>, OldDbError> {
        self.rows(
            "source_state",
            "SELECT source, value FROM source_state ORDER BY source",
            |r| {
                Ok(OldSourceState {
                    source: r.get(0)?,
                    value: r.get(1)?,
                })
            },
        )
    }
}

/// One sidecar file of a worker in `runs/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunsRow {
    pub worker: String,
    /// The extension: `owner`, `grid`, `repo`, `pipeline`, `review`, `base`,
    /// `layout_role`, or `state` (the hidden `.<worker>.state`).
    pub file: String,
    pub content: String,
}

const SIDECARS: &[&str] = &[
    "owner",
    "grid",
    "repo",
    "pipeline",
    "review",
    "base",
    "layout_role",
];

/// Reads the sidecars of every worker in `runs_dir`: `<worker>.<ext>` for the
/// extensions above and `.<worker>.state`. No other file is read.
pub fn read_runs_dir(runs_dir: &Path) -> Result<Vec<RunsRow>, OldDbError> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(runs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((stem, ext)) = name.rsplit_once('.') else {
            continue;
        };
        let (worker, file) = match stem.strip_prefix('.') {
            Some(w) if ext == "state" && !w.is_empty() => (w, "state"),
            None if SIDECARS.contains(&ext) && !stem.is_empty() => (stem, ext),
            _ => continue,
        };
        let content = String::from_utf8_lossy(&std::fs::read(entry.path())?).into_owned();
        out.push(RunsRow {
            worker: worker.to_string(),
            file: file.to_string(),
            content,
        });
    }
    out.sort_by(|a, b| (&a.worker, &a.file).cmp(&(&b.worker, &b.file)));
    Ok(out)
}
