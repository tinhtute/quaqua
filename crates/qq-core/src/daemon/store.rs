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
