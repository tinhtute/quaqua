//! The daemon's one database door. Every write, the workflow table and the
//! journal alike, goes through [`Store`]; no other file holds daemon state.
//!
//! `SqliteJournal` owns a private connection, so the store holds two
//! connections to `runs/qq.db` (migrated entities, journal tables) and
//! serializes both behind the daemon's single writer path: loops write the
//! journal only through [`SharedJournal`], everything else through [`Store`].

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::engine::journal::{EventRecord, Journal, JournalError, SnapshotRecord};
use crate::engine::journal_sqlite::SqliteJournal;

/// Clone-able journal handle; every workflow loop writes through one connection.
#[derive(Clone)]
pub struct SharedJournal(Arc<Mutex<SqliteJournal>>);

impl SharedJournal {
    fn lock(&self) -> MutexGuard<'_, SqliteJournal> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Journal for SharedJournal {
    fn append_event(&mut self, id: &str, ev: &EventRecord) -> Result<u64, JournalError> {
        self.lock().append_event(id, ev)
    }

    fn append_snapshot(&mut self, id: &str, snap: &SnapshotRecord) -> Result<(), JournalError> {
        self.lock().append_snapshot(id, snap)
    }

    fn load_events(&self, id: &str, since: u64) -> Result<Vec<EventRecord>, JournalError> {
        self.lock().load_events(id, since)
    }

    fn load_latest_snapshot(&self, id: &str) -> Result<Option<SnapshotRecord>, JournalError> {
        self.lock().load_latest_snapshot(id)
    }
}

/// Journal stream prefixes of remembered watches (never a workflow id); the
/// prefixes differ so an agent and a report watch can never share a row.
const WATCH_STREAM: &str = "watch:";
const REPORT_STREAM: &str = "watch-report:";

pub struct Store {
    conn: Mutex<Connection>,
    journal: SharedJournal,
}

/// One row of the `workflow` table.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRow {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub started_ms: u64,
    pub updated_ms: u64,
    pub finished_ms: Option<u64>,
}

impl WorkflowRow {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "state": self.state,
            "started_ms": self.started_ms, "updated_ms": self.updated_ms,
            "finished_ms": self.finished_ms,
        })
    }
}

impl Store {
    /// Opens (and migrates) `db_path`, then attaches the journal to the same file.
    pub fn open(db_path: &Path) -> Result<Store, String> {
        let conn = crate::db::open(db_path).map_err(|e| e.to_string())?;
        let journal = SqliteJournal::open(db_path).map_err(|e| e.to_string())?;
        Ok(Store {
            conn: Mutex::new(conn),
            journal: SharedJournal(Arc::new(Mutex::new(journal))),
        })
    }

    pub fn journal(&self) -> SharedJournal {
        self.journal.clone()
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The SQL door for installs: runs `f` on the store's one migrated
    /// connection (the journal tables sit in the same file). The connection
    /// is held for the whole call and the lock is not re-entrant, so `f` must
    /// not call back into the store, the supervisor or a watcher.
    pub fn with_conn<T, E>(&self, f: impl FnOnce(&mut Connection) -> Result<T, E>) -> Result<T, E> {
        f(&mut self.conn())
    }

    /// Remembers that `agent` is watched for `workflow`, so a restart can
    /// re-arm the watch. One row per pair; a repeat changes nothing.
    pub fn remember_agent_watch(
        &self,
        workflow: &str,
        agent: &str,
        now_ms: u64,
    ) -> Result<(), String> {
        self.conn()
            .execute(
                "INSERT INTO events (workflow_id, seq, ts_ms, kind, payload_json) \
                 VALUES (?1, 1, ?2, 'watch_agent', ?3) ON CONFLICT DO NOTHING",
                params![
                    format!("{WATCH_STREAM}{workflow}:{agent}"),
                    now_ms as i64,
                    json!({ "workflow": workflow, "agent": agent }).to_string()
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Remembers that the report file `path` is watched for `workflow`, so a
    /// restart can re-arm it. One row per pair; a repeat changes nothing.
    pub fn remember_report_watch(
        &self,
        workflow: &str,
        path: &Path,
        now_ms: u64,
    ) -> Result<(), String> {
        let path = path.to_string_lossy();
        self.conn()
            .execute(
                "INSERT INTO events (workflow_id, seq, ts_ms, kind, payload_json) \
                 VALUES (?1, 1, ?2, 'watch_report', ?3) ON CONFLICT DO NOTHING",
                params![
                    format!("{REPORT_STREAM}{workflow}:{path}"),
                    now_ms as i64,
                    json!({ "workflow": workflow, "path": path }).to_string()
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Every remembered `(workflow, report path)` watch, oldest first.
    pub fn report_watches(&self) -> Result<Vec<(String, std::path::PathBuf)>, String> {
        Ok(self
            .watch_payloads("watch_report", REPORT_STREAM)?
            .iter()
            .filter_map(|v| {
                Some((
                    v["workflow"].as_str()?.to_string(),
                    v["path"].as_str()?.into(),
                ))
            })
            .collect())
    }

    fn watch_payloads(&self, kind: &str, prefix: &str) -> Result<Vec<Value>, String> {
        let conn = self.conn();
        let mut st = conn
            .prepare(
                "SELECT payload_json FROM events WHERE kind = ?1 \
                 AND substr(workflow_id, 1, length(?2)) = ?2 ORDER BY ts_ms, workflow_id",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![kind, prefix], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows
            .iter()
            .filter_map(|p| serde_json::from_str(p).ok())
            .collect())
    }

    /// Every remembered `(workflow, agent)` watch, oldest first.
    pub fn agent_watches(&self) -> Result<Vec<(String, String)>, String> {
        Ok(self
            .watch_payloads("watch_agent", WATCH_STREAM)?
            .iter()
            .filter_map(|v| {
                Some((
                    v["workflow"].as_str()?.to_string(),
                    v["agent"].as_str()?.to_string(),
                ))
            })
            .collect())
    }

    /// Records a workflow once; a repeat of the same id changes nothing.
    /// Returns true when the row is new.
    pub fn register_workflow(&self, id: &str, kind: &str, now_ms: u64) -> Result<bool, String> {
        let n = self
            .conn()
            .execute(
                "INSERT INTO workflow (id, kind, state, started_ms, updated_ms) \
                 VALUES (?1, ?2, 'running', ?3, ?3) ON CONFLICT(id) DO NOTHING",
                params![id, kind, now_ms as i64],
            )
            .map_err(|e| e.to_string())?;
        Ok(n == 1)
    }

    /// Moves a workflow to a terminal `state`; a repeat keeps the first finish time.
    pub fn finish_workflow(&self, id: &str, state: &str, now_ms: u64) -> Result<(), String> {
        self.conn()
            .execute(
                "UPDATE workflow SET state = ?2, updated_ms = ?3, \
                 finished_ms = COALESCE(finished_ms, ?3) WHERE id = ?1",
                params![id, state, now_ms as i64],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn workflow(&self, id: &str) -> Result<Option<WorkflowRow>, String> {
        Ok(self.workflows()?.into_iter().find(|w| w.id == id))
    }

    pub fn workflows(&self) -> Result<Vec<WorkflowRow>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, state, started_ms, updated_ms, finished_ms \
                 FROM workflow ORDER BY started_ms, id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WorkflowRow {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    state: r.get(2)?,
                    started_ms: r.get::<_, i64>(3)? as u64,
                    updated_ms: r.get::<_, i64>(4)? as u64,
                    finished_ms: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
}
