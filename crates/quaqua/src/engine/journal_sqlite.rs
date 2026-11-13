//! SQLite implementation of the frozen [`Journal`] trait.
//!
//! Stores workflow events and snapshots at `<workspace_root>/runs/quaqua.db`
//! using SQLite WAL mode and busy timeout.

use std::borrow::Borrow;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::engine::journal::{EventRecord, Journal, JournalError, SnapshotRecord};

/// Verification rigor level for a verdict event (§ S1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerdictLevel {
    UnitTestVerified,
    TypeCheckOnly,
    ReviewClean,
    VerifierBlocked,
    VerifierFailed,
    #[serde(other)]
    Inconclusive,
}

impl VerdictLevel {
    /// The six levels a report may name, in the order `rules/_all/brief.md`
    /// documents them for builders. The report contract quotes this list
    /// verbatim; a test fails if the two drift.
    pub const ALL: [VerdictLevel; 6] = [
        Self::UnitTestVerified,
        Self::TypeCheckOnly,
        Self::ReviewClean,
        Self::VerifierBlocked,
        Self::VerifierFailed,
        Self::Inconclusive,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UnitTestVerified => "unit-test-verified",
            Self::TypeCheckOnly => "type-check-only",
            Self::ReviewClean => "review-clean",
            Self::VerifierBlocked => "verifier-blocked",
            Self::VerifierFailed => "verifier-failed",
            Self::Inconclusive => "inconclusive",
        }
    }

    /// The recognised level tokens, backticked and comma-joined, exactly as the
    /// worker-facing report contract lists them (`rules/_all/brief.md`).
    pub fn vocabulary() -> String {
        Self::ALL
            .iter()
            .map(|l| format!("`{}`", l.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Parses a level token, returning `None` when the token is not one of
    /// [`Self::ALL`]. [`FromStr`] maps such a token to `Inconclusive`; callers
    /// that want to warn about it use this instead.
    pub fn parse_known(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "unit-test-verified" => Some(Self::UnitTestVerified),
            "type-check-only" => Some(Self::TypeCheckOnly),
            "review-clean" => Some(Self::ReviewClean),
            "verifier-blocked" => Some(Self::VerifierBlocked),
            "verifier-failed" => Some(Self::VerifierFailed),
            "inconclusive" => Some(Self::Inconclusive),
            _ => None,
        }
    }
}

impl std::fmt::Display for VerdictLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for VerdictLevel {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse_known(s).unwrap_or(Self::Inconclusive))
    }
}

/// Source of a verdict event (self-report by builder or independent verifier).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VerdictSource {
    #[serde(rename = "self")]
    SelfReport,
    #[serde(rename = "verifier")]
    Verifier,
}

impl VerdictSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SelfReport => "self",
            Self::Verifier => "verifier",
        }
    }
}

impl std::fmt::Display for VerdictSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for VerdictSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "self" => Ok(Self::SelfReport),
            "verifier" => Ok(Self::Verifier),
            other => Err(format!("unknown verdict source '{other}'")),
        }
    }
}

/// A verdict payload recorded under `kind = "verdict"` in the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictRecord {
    pub branch: String,
    pub sha: String,
    pub level: VerdictLevel,
    #[serde(default)]
    pub evidence: String,
    pub verifier: String,
    pub source: VerdictSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_kind: Option<String>,
    #[serde(default)]
    pub harness: String,
}

impl VerdictRecord {
    /// Relative strength score for ordering verdicts on the same branch + SHA.
    ///
    /// Verifier verdicts always score >= 100 so that an independent verification
    /// overrides any self-report.
    pub fn strength(&self) -> u32 {
        let level_score = match self.level {
            VerdictLevel::ReviewClean => 6,
            VerdictLevel::UnitTestVerified => 5,
            VerdictLevel::TypeCheckOnly => 4,
            VerdictLevel::Inconclusive => 3,
            VerdictLevel::VerifierBlocked => 2,
            VerdictLevel::VerifierFailed => 1,
        };
        let source_score = match self.source {
            VerdictSource::Verifier => 100,
            VerdictSource::SelfReport => 0,
        };
        source_score + level_score
    }

    /// Returns true if this verdict represents an independent passing verification.
    /// Self-reports and non-passing verifier verdicts are never verified.
    pub fn is_verified(&self) -> bool {
        self.source == VerdictSource::Verifier
            && matches!(
                self.level,
                VerdictLevel::ReviewClean
                    | VerdictLevel::UnitTestVerified
                    | VerdictLevel::TypeCheckOnly
            )
    }
}

/// Picks the strongest verdict from a slice of records using the S1 rules.
///
/// This is the single implementation of the rule; the journal lookup and
/// `quaqua status` both select verdicts through it.
///
/// Rules:
/// - Verifier verdicts always override self-reports.
/// - Between verifier verdicts on the same branch and SHA (e.g. dual-lens or re-review),
///   any failed, blocked, or inconclusive verifier verdict dominates over a pass (F4).
/// - A self-report alone is never presented as verified (F6); its effective level
///   is inconclusive and `is_verified()` is false.
pub(crate) fn strongest_verdict_from_slice<T: Borrow<VerdictRecord>>(
    records: &[T],
) -> Option<VerdictRecord> {
    let verifier_verdicts: Vec<&VerdictRecord> = records
        .iter()
        .map(|r| r.borrow())
        .filter(|v| v.source == VerdictSource::Verifier)
        .collect();

    if !verifier_verdicts.is_empty() {
        // F4 rule: Any failed, blocked, or inconclusive verifier verdict dominates
        if let Some(v) = verifier_verdicts
            .iter()
            .find(|v| v.level == VerdictLevel::VerifierFailed)
        {
            return Some((*v).clone());
        }
        if let Some(v) = verifier_verdicts
            .iter()
            .find(|v| v.level == VerdictLevel::VerifierBlocked)
        {
            return Some((*v).clone());
        }
        if let Some(v) = verifier_verdicts
            .iter()
            .find(|v| v.level == VerdictLevel::Inconclusive)
        {
            return Some((*v).clone());
        }
        // All verifier verdicts are clean/pass; return the strongest pass
        let best = verifier_verdicts
            .into_iter()
            .max_by_key(|v| v.strength())
            .unwrap();
        return Some(best.clone());
    }

    // F6 rule: Only self-reports exist; a self-report alone is never presented as verified.
    let mut self_report = records.last()?.borrow().clone();
    self_report.level = VerdictLevel::Inconclusive;
    Some(self_report)
}

/// SQLite-backed persistent journal implementing [`Journal`].
pub struct SqliteJournal {
    conn: Mutex<rusqlite::Connection>,
    path: PathBuf,
}

use crate::engine::sqlite::{open_conn, retry_while_busy};

/// The journal tables, shared with `bus::gui` so the GUI views can be created
/// on a database whichever opener ran first.
pub(crate) const JOURNAL_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    workflow_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    ts_ms INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (workflow_id, seq)
);

CREATE TABLE IF NOT EXISTS snapshots (
    workflow_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    state_json TEXT NOT NULL,
    PRIMARY KEY (workflow_id, seq)
);
"#;

impl SqliteJournal {
    /// Returns the default SQLite journal path at `<default_runs_dir>/quaqua.db`.
    pub fn default_path() -> PathBuf {
        crate::worker::cleanup::default_runs_dir().join("quaqua.db")
    }

    /// Opens or creates the SQLite journal at `<default_runs_dir>/quaqua.db`.
    pub fn new() -> Result<Self, JournalError> {
        Self::open(&Self::default_path())
    }

    /// Opens or creates the SQLite journal at the specified path.
    pub fn open(path: &Path) -> Result<Self, JournalError> {
        if crate::workspace::live_db_open_refused(path) {
            return Err(JournalError::StorageError(format!(
                "refusing to open live workspace database at {} under test",
                path.display()
            )));
        }
        let conn = open_conn(path).map_err(|e| JournalError::StorageError(e.to_string()))?;

        // Initialize schema exactly per journal.rs specification.
        retry_while_busy(|| conn.execute_batch(JOURNAL_SCHEMA_SQL))
        .map_err(|e| JournalError::StorageError(e.to_string()))?;

        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        })
    }

    /// Returns the database path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records a verdict event in the journal, deduplicated by `(branch, sha, source, verifier)`.
    ///
    /// Returns `Ok(true)` if inserted, or `Ok(false)` if skipped as duplicate.
    pub fn record_verdict(
        &mut self,
        workflow_id: &str,
        verdict: &VerdictRecord,
    ) -> Result<bool, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::StorageError(e.to_string()))?;

        let payload_json = match serde_json::to_string(verdict) {
            Ok(s) => s,
            Err(e) => {
                return Err(JournalError::SerializationError(e.to_string()));
            }
        };

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let timeout = crate::engine::sqlite::busy_timeout_ms();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout);

        loop {
            let _ = conn.execute_batch("ROLLBACK;");

            match conn.execute_batch("BEGIN IMMEDIATE;") {
                Ok(_) => {}
                Err(e)
                    if crate::engine::sqlite::is_lock_error(&e)
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(e) => return Err(JournalError::StorageError(e.to_string())),
            }

            // 1. Check deduplication by (branch, sha, source, verifier)
            let is_duplicate = {
                let mut stmt =
                    match conn.prepare("SELECT payload_json FROM events WHERE kind = 'verdict';") {
                        Ok(s) => s,
                        Err(e)
                            if crate::engine::sqlite::is_lock_error(&e)
                                && std::time::Instant::now() < deadline =>
                        {
                            let _ = conn.execute_batch("ROLLBACK;");
                            std::thread::sleep(std::time::Duration::from_millis(10));
                            continue;
                        }
                        Err(e) => {
                            let _ = conn.execute_batch("ROLLBACK;");
                            return Err(JournalError::StorageError(e.to_string()));
                        }
                    };
                let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
                    Ok(r) => r,
                    Err(e)
                        if crate::engine::sqlite::is_lock_error(&e)
                            && std::time::Instant::now() < deadline =>
                    {
                        let _ = conn.execute_batch("ROLLBACK;");
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => {
                        let _ = conn.execute_batch("ROLLBACK;");
                        return Err(JournalError::StorageError(e.to_string()));
                    }
                };
                let mut dup = false;
                for payload in rows.flatten() {
                    if let Ok(v) = serde_json::from_str::<VerdictRecord>(&payload) {
                        if v.branch == verdict.branch
                            && v.sha == verdict.sha
                            && v.source == verdict.source
                            && v.verifier == verdict.verifier
                        {
                            dup = true;
                            break;
                        }
                    }
                }
                dup
            };

            if is_duplicate {
                let _ = conn.execute_batch("COMMIT;");
                return Ok(false);
            }

            // 2. Assign next seq for this workflow_id
            let current_max: u64 = match conn.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM events WHERE workflow_id = ?1;",
                rusqlite::params![workflow_id],
                |row| {
                    let v: i64 = row.get(0)?;
                    Ok(v as u64)
                },
            ) {
                Ok(v) => v,
                Err(e)
                    if crate::engine::sqlite::is_lock_error(&e)
                        && std::time::Instant::now() < deadline =>
                {
                    let _ = conn.execute_batch("ROLLBACK;");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK;");
                    return Err(JournalError::StorageError(e.to_string()));
                }
            };

            let next_seq = current_max + 1;
            let insert_res = conn.execute(
                "INSERT INTO events (workflow_id, seq, ts_ms, kind, payload_json) VALUES (?1, ?2, ?3, 'verdict', ?4);",
                rusqlite::params![
                    workflow_id,
                    next_seq as i64,
                    now_ms as i64,
                    &payload_json,
                ],
            );

            match insert_res {
                Ok(_) => match conn.execute_batch("COMMIT;") {
                    Ok(_) => return Ok(true),
                    Err(e)
                        if crate::engine::sqlite::is_lock_error(&e)
                            && std::time::Instant::now() < deadline =>
                    {
                        let _ = conn.execute_batch("ROLLBACK;");
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => {
                        let _ = conn.execute_batch("ROLLBACK;");
                        return Err(JournalError::StorageError(e.to_string()));
                    }
                },
                Err(e)
                    if crate::engine::sqlite::is_lock_error(&e)
                        && std::time::Instant::now() < deadline =>
                {
                    let _ = conn.execute_batch("ROLLBACK;");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK;");
                    return Err(JournalError::StorageError(e.to_string()));
                }
            }
        }
    }

    /// Loads all verdict records matching the specified branch and SHA.
    pub fn load_verdicts(
        &self,
        branch: &str,
        sha: &str,
    ) -> Result<Vec<VerdictRecord>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::StorageError(e.to_string()))?;

        retry_while_busy(|| {
            let mut stmt = conn.prepare(
                "SELECT seq, ts_ms, payload_json FROM events WHERE kind = 'verdict' ORDER BY seq ASC;",
            )?;

            let rows = stmt.query_map([], |row| {
                let seq: i64 = row.get(0)?;
                let ts_ms: i64 = row.get(1)?;
                let payload: String = row.get(2)?;
                Ok((seq as u64, ts_ms as u64, payload))
            })?;

            let mut list = Vec::new();
            for row in rows {
                let (_seq, _ts_ms, payload) = row?;
                if let Ok(v) = serde_json::from_str::<VerdictRecord>(&payload) {
                    if v.branch == branch && v.sha == sha {
                        list.push(v);
                    }
                }
            }
            Ok(list)
        })
        .map_err(|e| JournalError::StorageError(e.to_string()))
    }

    /// Returns the strongest verdict for the specified branch and SHA, if any.
    ///
    /// Selection follows the S1 rules implemented once in
    /// [`strongest_verdict_from_slice`].
    pub fn strongest_verdict(
        &self,
        branch: &str,
        sha: &str,
    ) -> Result<Option<VerdictRecord>, JournalError> {
        let list = self.load_verdicts(branch, sha)?;
        Ok(strongest_verdict_from_slice(&list))
    }

    /// Loads all verdict records across all workflows.
    pub fn load_all_verdicts(&self) -> Result<Vec<VerdictRecord>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::StorageError(e.to_string()))?;

        retry_while_busy(|| {
            let mut stmt = conn.prepare(
                "SELECT seq, ts_ms, payload_json FROM events WHERE kind = 'verdict' ORDER BY seq ASC;",
            )?;

            let rows = stmt.query_map([], |row| row.get::<_, String>(2))?;

            let mut list = Vec::new();
            for row in rows {
                let payload = row?;
                if let Ok(v) = serde_json::from_str::<VerdictRecord>(&payload) {
                    list.push(v);
                }
            }
            Ok(list)
        })
        .map_err(|e| JournalError::StorageError(e.to_string()))
    }
}

impl Journal for SqliteJournal {
    fn append_event(
        &mut self,
        workflow_id: &str,
        event: &EventRecord,
    ) -> Result<u64, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::StorageError(e.to_string()))?;

        let timeout = crate::engine::sqlite::busy_timeout_ms();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout);

        loop {
            let _ = conn.execute_batch("ROLLBACK;");

            match conn.execute_batch("BEGIN IMMEDIATE;") {
                Ok(_) => {}
                Err(e)
                    if crate::engine::sqlite::is_lock_error(&e)
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(e) => return Err(JournalError::StorageError(e.to_string())),
            }

            let current_max: u64 = match conn.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM events WHERE workflow_id = ?1;",
                rusqlite::params![workflow_id],
                |row| {
                    let v: i64 = row.get(0)?;
                    Ok(v as u64)
                },
            ) {
                Ok(v) => v,
                Err(e)
                    if crate::engine::sqlite::is_lock_error(&e)
                        && std::time::Instant::now() < deadline =>
                {
                    let _ = conn.execute_batch("ROLLBACK;");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK;");
                    return Err(JournalError::StorageError(e.to_string()));
                }
            };

            let next_seq = current_max + 1;
            let assigned_seq = if event.seq == 0 {
                next_seq
            } else if event.seq != next_seq {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(JournalError::SequenceConflict {
                    workflow_id: workflow_id.to_string(),
                    expected_seq: next_seq,
                    actual_seq: event.seq,
                });
            } else {
                event.seq
            };

            let insert_res = conn.execute(
                "INSERT INTO events (workflow_id, seq, ts_ms, kind, payload_json) VALUES (?1, ?2, ?3, ?4, ?5);",
                rusqlite::params![
                    workflow_id,
                    assigned_seq as i64,
                    event.ts_ms as i64,
                    &event.kind,
                    &event.payload_json,
                ],
            );

            match insert_res {
                Ok(_) => match conn.execute_batch("COMMIT;") {
                    Ok(_) => return Ok(assigned_seq),
                    Err(e)
                        if crate::engine::sqlite::is_lock_error(&e)
                            && std::time::Instant::now() < deadline =>
                    {
                        let _ = conn.execute_batch("ROLLBACK;");
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => {
                        let _ = conn.execute_batch("ROLLBACK;");
                        return Err(JournalError::StorageError(e.to_string()));
                    }
                },
                Err(e)
                    if crate::engine::sqlite::is_lock_error(&e)
                        && std::time::Instant::now() < deadline =>
                {
                    let _ = conn.execute_batch("ROLLBACK;");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK;");
                    return Err(JournalError::StorageError(e.to_string()));
                }
            }
        }
    }

    fn append_snapshot(
        &mut self,
        workflow_id: &str,
        snapshot: &SnapshotRecord,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::StorageError(e.to_string()))?;

        retry_while_busy(|| {
            conn.execute(
                "INSERT INTO snapshots (workflow_id, seq, state_json) VALUES (?1, ?2, ?3);",
                rusqlite::params![workflow_id, snapshot.seq as i64, &snapshot.state_json,],
            )
        })
        .map_err(|e| JournalError::StorageError(e.to_string()))?;

        Ok(())
    }

    fn load_events(
        &self,
        workflow_id: &str,
        since_seq: u64,
    ) -> Result<Vec<EventRecord>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::StorageError(e.to_string()))?;

        retry_while_busy(|| {
            let mut stmt = conn.prepare(
                "SELECT seq, ts_ms, kind, payload_json FROM events WHERE workflow_id = ?1 AND seq >= ?2 ORDER BY seq ASC;",
            )?;

            let rows = stmt.query_map(rusqlite::params![workflow_id, since_seq as i64], |row| {
                let seq: i64 = row.get(0)?;
                let ts_ms: i64 = row.get(1)?;
                let kind: String = row.get(2)?;
                let payload_json: String = row.get(3)?;
                Ok(EventRecord::new(
                    seq as u64,
                    ts_ms as u64,
                    kind,
                    payload_json,
                ))
            })?;

            let mut events = Vec::new();
            for row in rows {
                events.push(row?);
            }

            Ok(events)
        })
        .map_err(|e| JournalError::StorageError(e.to_string()))
    }

    fn load_latest_snapshot(
        &self,
        workflow_id: &str,
    ) -> Result<Option<SnapshotRecord>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::StorageError(e.to_string()))?;

        retry_while_busy(|| {
            let mut stmt = conn.prepare(
                "SELECT seq, state_json FROM snapshots WHERE workflow_id = ?1 ORDER BY seq DESC LIMIT 1;",
            )?;

            let mut rows = stmt.query(rusqlite::params![workflow_id])?;

            if let Some(row) = rows.next()? {
                let seq: i64 = row.get(0)?;
                let state_json: String = row.get(1)?;
                Ok(Some(SnapshotRecord::new(seq as u64, state_json)))
            } else {
                Ok(None)
            }
        })
        .map_err(|e| JournalError::StorageError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn temp_db_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("quaqua-test-{name}-{nanos}.db"));
        path
    }

    struct TempDbCleaner(PathBuf);
    impl Drop for TempDbCleaner {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let shm = self.0.with_extension("db-shm");
            let wal = self.0.with_extension("db-wal");
            let _ = std::fs::remove_file(shm);
            let _ = std::fs::remove_file(wal);
        }
    }

    #[test]
    fn test_sqlite_journal_basic_append_and_load() {
        let path = temp_db_path("basic");
        let _cleaner = TempDbCleaner(path.clone());

        let mut journal = SqliteJournal::open(&path).expect("open journal");

        // Append events with seq 0 (auto-assignment)
        let ev1 = EventRecord::new(0, 1000, "workflow_started", r#"{"input": 42}"#);
        let seq1 = journal.append_event("wf-1", &ev1).expect("append ev1");
        assert_eq!(seq1, 1);

        let ev2 = EventRecord::new(0, 1050, "activity_scheduled", r#"{"id": "act-1"}"#);
        let seq2 = journal.append_event("wf-1", &ev2).expect("append ev2");
        assert_eq!(seq2, 2);

        // Append snapshot
        let snap = SnapshotRecord::new(2, r#"{"step": 2, "val": 42}"#);
        journal
            .append_snapshot("wf-1", &snap)
            .expect("append snapshot");

        // Load events inclusive
        let all_events = journal.load_events("wf-1", 0).expect("load events from 0");
        assert_eq!(all_events.len(), 2);
        assert_eq!(all_events[0].seq, 1);
        assert_eq!(all_events[0].kind, "workflow_started");
        assert_eq!(all_events[1].seq, 2);
        assert_eq!(all_events[1].kind, "activity_scheduled");

        let events_from_2 = journal.load_events("wf-1", 2).expect("load events from 2");
        assert_eq!(events_from_2.len(), 1);
        assert_eq!(events_from_2[0].seq, 2);

        // Load latest snapshot
        let loaded_snap = journal
            .load_latest_snapshot("wf-1")
            .expect("load snapshot")
            .expect("snapshot exists");
        assert_eq!(loaded_snap.seq, 2);
        assert_eq!(loaded_snap.state_json, r#"{"step": 2, "val": 42}"#);

        // Non-existent workflow
        let empty_events = journal.load_events("wf-unknown", 0).expect("load empty");
        assert!(empty_events.is_empty());
        let no_snap = journal
            .load_latest_snapshot("wf-unknown")
            .expect("load empty snap");
        assert!(no_snap.is_none());
    }

    #[test]
    fn test_sqlite_journal_sequence_conflict() {
        let path = temp_db_path("conflict");
        let _cleaner = TempDbCleaner(path.clone());

        let mut journal = SqliteJournal::open(&path).expect("open journal");

        let ev1 = EventRecord::new(0, 1000, "step", "{}");
        let seq1 = journal.append_event("wf-c", &ev1).expect("ev1");
        assert_eq!(seq1, 1);

        // Explicit seq matching expected (2) succeeds
        let ev2 = EventRecord::new(2, 1010, "step", "{}");
        let seq2 = journal.append_event("wf-c", &ev2).expect("ev2");
        assert_eq!(seq2, 2);

        // Explicit stale seq (1) fails with SequenceConflict
        let stale = EventRecord::new(1, 1020, "step", "{}");
        let err_stale = journal.append_event("wf-c", &stale).expect_err("stale seq");
        assert_eq!(
            err_stale,
            JournalError::SequenceConflict {
                workflow_id: "wf-c".to_string(),
                expected_seq: 3,
                actual_seq: 1,
            }
        );

        // Explicit future gap seq (10) fails with SequenceConflict
        let gap = EventRecord::new(10, 1030, "step", "{}");
        let err_gap = journal.append_event("wf-c", &gap).expect_err("gap seq");
        assert_eq!(
            err_gap,
            JournalError::SequenceConflict {
                workflow_id: "wf-c".to_string(),
                expected_seq: 3,
                actual_seq: 10,
            }
        );
    }

    /// Opening a rollback-journal database converts it to WAL, and that
    /// conversion needs a write lock. SQLite does not run the busy handler for
    /// `PRAGMA journal_mode`, so a concurrent writer used to fail the open with
    /// `database is locked` — one watcher dying because another one was busy.
    #[test]
    fn test_open_waits_out_a_concurrent_writer_instead_of_failing_locked() {
        let path = temp_db_path("open-busy");
        let _cleaner = TempDbCleaner(path.clone());

        // Create the schema without the WAL pragma so `open` must convert it.
        let holder = rusqlite::Connection::open(&path).expect("open holder");
        holder
            .execute_batch(
                r#"
                CREATE TABLE events (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    ts_ms INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                CREATE TABLE snapshots (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    state_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                BEGIN IMMEDIATE;
                INSERT INTO events VALUES ('wf-holder', 1, 0, 'k', '{}');
                "#,
            )
            .expect("create schema and hold the write lock");

        let release = thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(200));
            holder.execute_batch("COMMIT;").expect("commit holder tx");
        });

        let mut journal = SqliteJournal::open(&path)
            .expect("open must wait for the concurrent writer instead of failing locked");
        release.join().expect("holder thread");

        let seq = journal
            .append_event("wf-holder", &EventRecord::new(0, 1, "after", "{}"))
            .expect("journal is usable after the concurrent open");
        assert_eq!(seq, 2);
    }

    /// The watcher scenario: several runners opening the same database at once
    /// must all get a working journal (the first one converts it to WAL, the
    /// rest race that conversion).
    #[test]
    fn test_concurrent_opens_never_fail_locked() {
        let path = temp_db_path("concurrent-opens");
        let _cleaner = TempDbCleaner(path.clone());

        let writers = 6;
        let path_arc = Arc::new(path.clone());
        let barrier = Arc::new(std::sync::Barrier::new(writers));
        let mut handles = Vec::new();
        for writer_id in 0..writers {
            let p = Arc::clone(&path_arc);
            let b = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                b.wait();
                let mut journal = SqliteJournal::open(&p).expect("concurrent open");
                for i in 0..10 {
                    let ev = EventRecord::new(
                        0,
                        i as u64,
                        "concurrent_open",
                        format!(r#"{{"writer": {writer_id}, "i": {i}}}"#),
                    );
                    journal
                        .append_event("wf-open", &ev)
                        .expect("append after concurrent open");
                }
            }));
        }
        for h in handles {
            h.join().expect("thread finished");
        }

        let reader = SqliteJournal::open(&path).expect("reopen reader");
        let events = reader.load_events("wf-open", 1).expect("load events");
        assert_eq!(events.len(), writers * 10);
    }

    #[test]
    fn test_two_concurrent_watchers_on_one_journal_both_survive() {
        let path = temp_db_path("two-watchers-busy");
        let _cleaner = TempDbCleaner(path.clone());

        {
            let _init = SqliteJournal::open(&path).expect("init db");
        }

        let holder = rusqlite::Connection::open(&path).expect("open holder");
        holder.execute_batch("BEGIN IMMEDIATE;").expect("hold lock");

        let release = thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(150));
            holder.execute_batch("COMMIT;").expect("commit holder");
        });

        let mut j2 = SqliteJournal::open(&path).expect("open j2");
        // Lower j2's busy timeout to 50ms so that without journal retry,
        // it fails with "database is locked" while holder holds the lock for 150ms.
        j2.conn
            .lock()
            .unwrap()
            .pragma_update(None, "busy_timeout", 50u64)
            .unwrap();

        let ev = EventRecord::new(0, 1001, "wake", "{}");
        let res = j2.append_event("wf-watcher2", &ev);
        release.join().expect("release thread");

        assert!(
            res.is_ok(),
            "concurrent watcher append_event must survive a locked database via retry, got: {res:?}"
        );
    }

    #[test]
    fn test_concurrent_writers_monotonic_seq() {
        let path = temp_db_path("concurrent");
        let _cleaner = TempDbCleaner(path.clone());

        // Initialize schema by opening once
        {
            let _init = SqliteJournal::open(&path).expect("init db");
        }

        let num_writers = 4;
        let events_per_writer = 50;
        let path_arc = Arc::new(path.clone());

        let mut handles = Vec::new();
        for writer_id in 0..num_writers {
            let p = Arc::clone(&path_arc);
            handles.push(thread::spawn(move || {
                let mut journal = SqliteJournal::open(&p).expect("open journal in thread");
                for i in 0..events_per_writer {
                    let ev = EventRecord::new(
                        0,
                        1000 + i as u64,
                        "concurrent_event",
                        format!(r#"{{"writer": {writer_id}, "i": {i}}}"#),
                    );
                    journal
                        .append_event("wf-concurrent", &ev)
                        .expect("append concurrent event");
                }
            }));
        }

        for h in handles {
            h.join().expect("thread finished successfully");
        }

        // Verify loaded events
        let reader = SqliteJournal::open(&path).expect("open reader");
        let loaded = reader
            .load_events("wf-concurrent", 0)
            .expect("load all events");

        let total_expected = num_writers * events_per_writer;
        assert_eq!(
            loaded.len(),
            total_expected,
            "lost events detected during concurrent writing"
        );

        // Strictly monotonic check: seq must be exactly 1, 2, 3, ..., total_expected
        for (idx, record) in loaded.iter().enumerate() {
            let expected_seq = (idx + 1) as u64;
            assert_eq!(
                record.seq, expected_seq,
                "sequence gap or non-monotonic ordering at index {idx}: expected {expected_seq}, got {}",
                record.seq
            );
        }
    }

    #[test]
    fn test_crash_recovery_reopen_and_replay() {
        let path = temp_db_path("crash");
        let _cleaner = TempDbCleaner(path.clone());

        // Step 1: append 5 events and drop connection before any snapshot
        {
            let mut journal = SqliteJournal::open(&path).expect("open journal");
            for i in 1..=5 {
                let ev = EventRecord::new(
                    0,
                    1000 + i,
                    "event_before_snap",
                    format!(r#"{{"step": {i}}}"#),
                );
                let assigned = journal.append_event("wf-crash", &ev).expect("append event");
                assert_eq!(assigned, i);
            }
            // journal drops here without creating a snapshot
        }

        // Step 2: Reopen; verify zero events lost and no snapshot exists
        {
            let mut journal = SqliteJournal::open(&path).expect("reopen journal");
            let snap = journal
                .load_latest_snapshot("wf-crash")
                .expect("load snapshot");
            assert!(snap.is_none(), "expected no snapshot before checkpoint");

            let events = journal
                .load_events("wf-crash", 0)
                .expect("load events after reopen");
            assert_eq!(events.len(), 5, "expected all 5 events present on reopen");
            for (idx, ev) in events.iter().enumerate() {
                assert_eq!(ev.seq, (idx + 1) as u64);
            }

            // Step 3: Append snapshot at seq 3, and add more events (seq 4 and 5 were already appended,
            // append seq 6 and 7)
            let snap3 = SnapshotRecord::new(3, r#"{"state": "checkpoint_at_3"}"#);
            journal
                .append_snapshot("wf-crash", &snap3)
                .expect("append snapshot at seq 3");

            let ev6 = EventRecord::new(0, 1006, "event_after_snap", r#"{"step": 6}"#);
            let seq6 = journal.append_event("wf-crash", &ev6).expect("append ev6");
            assert_eq!(seq6, 6);

            let ev7 = EventRecord::new(0, 1007, "event_after_snap", r#"{"step": 7}"#);
            let seq7 = journal.append_event("wf-crash", &ev7).expect("append ev7");
            assert_eq!(seq7, 7);
            // journal drops here
        }

        // Step 4: Reopen; replay from latest snapshot + subsequent events
        {
            let journal = SqliteJournal::open(&path).expect("reopen journal again");
            let snap = journal
                .load_latest_snapshot("wf-crash")
                .expect("load snapshot")
                .expect("snapshot exists");
            assert_eq!(snap.seq, 3);
            assert_eq!(snap.state_json, r#"{"state": "checkpoint_at_3"}"#);

            // Replay loads events strictly after snapshot seq (seq >= snap.seq + 1)
            let remaining_events = journal
                .load_events("wf-crash", snap.seq + 1)
                .expect("load events since snapshot seq + 1");

            assert_eq!(remaining_events.len(), 4); // seq 4, 5, 6, 7
            assert_eq!(remaining_events[0].seq, 4);
            assert_eq!(remaining_events[1].seq, 5);
            assert_eq!(remaining_events[2].seq, 6);
            assert_eq!(remaining_events[3].seq, 7);

            // Complete history matches total 7 events
            let full_history = journal
                .load_events("wf-crash", 0)
                .expect("load full history");
            assert_eq!(full_history.len(), 7);
        }
    }

    #[test]
    fn test_journal_trait_object_safety() {
        let path = temp_db_path("obj-safe");
        let _cleaner = TempDbCleaner(path.clone());

        let mut journal: Box<dyn Journal> =
            Box::new(SqliteJournal::open(&path).expect("open journal"));

        let ev = EventRecord::new(0, 1000, "test", "{}");
        let seq = journal.append_event("wf-obj", &ev).expect("append");
        assert_eq!(seq, 1);

        let snap = SnapshotRecord::new(1, r#"{"obj": true}"#);
        journal.append_snapshot("wf-obj", &snap).expect("snap");

        let events = journal.load_events("wf-obj", 1).expect("load events");
        assert_eq!(events.len(), 1);

        let loaded_snap = journal
            .load_latest_snapshot("wf-obj")
            .expect("load snap")
            .expect("snap present");
        assert_eq!(loaded_snap.seq, 1);
    }

    #[test]
    fn test_sqlite_journal_creates_parent_directories() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir()
            .join(format!("quaqua-test-nested-dir-{nanos}"))
            .join("sub1")
            .join("sub2");
        let path = dir.join("test.db");
        let _ = std::fs::remove_dir_all(&dir);

        let journal = SqliteJournal::open(&path);
        assert!(journal.is_ok(), "must create parent directories if missing");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sqlite_journal_default_path() {
        let expected = crate::worker::cleanup::default_runs_dir().join("quaqua.db");
        assert_eq!(SqliteJournal::default_path(), expected);
    }

    #[test]
    fn test_verdict_journal_record_and_deduplicate() {
        let path = temp_db_path("verdict-dedup");
        let _cleaner = TempDbCleaner(path.clone());
        let mut journal = SqliteJournal::open(&path).expect("open journal");

        let verdict = VerdictRecord {
            branch: "quaqua/feat-1".to_string(),
            sha: "abc123456789".to_string(),
            level: VerdictLevel::UnitTestVerified,
            evidence: "tests passed".to_string(),
            verifier: "builder-1".to_string(),
            source: VerdictSource::SelfReport,
            builder_kind: Some("sude".to_string()),
            verifier_kind: None,
            harness: "omp".to_string(),
        };

        // First record -> true
        let inserted = journal.record_verdict("wf-1", &verdict).expect("record 1");
        assert!(inserted, "first insert should succeed");

        // Second record with same key -> false (deduplicated)
        let inserted2 = journal.record_verdict("wf-1", &verdict).expect("record 2");
        assert!(!inserted2, "duplicate insert should be skipped");

        // Query
        let verdicts = journal
            .load_verdicts("quaqua/feat-1", "abc123456789")
            .expect("load");
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0], verdict);
    }

    #[test]
    fn test_verdict_journal_strongest_precedence() {
        let path = temp_db_path("verdict-strongest");
        let _cleaner = TempDbCleaner(path.clone());
        let mut journal = SqliteJournal::open(&path).expect("open journal");

        let branch = "quaqua/feat-2";
        let sha = "def987654321";

        let v_builder = VerdictRecord {
            branch: branch.to_string(),
            sha: sha.to_string(),
            level: VerdictLevel::UnitTestVerified,
            evidence: "all 10 tests passed".to_string(),
            verifier: "sude-worker".to_string(),
            source: VerdictSource::SelfReport,
            builder_kind: Some("sude".to_string()),
            verifier_kind: None,
            harness: "omp".to_string(),
        };
        journal.record_verdict("wf-builder", &v_builder).unwrap();

        // Builder self-report alone cannot be presented as verified (F6)
        let cur = journal.strongest_verdict(branch, sha).unwrap().unwrap();
        assert_eq!(cur.source, VerdictSource::SelfReport);
        assert_eq!(cur.level, VerdictLevel::Inconclusive);
        assert!(!cur.is_verified());

        // Reviewer clean arrives -> overrides self-report
        let v_rev_clean = VerdictRecord {
            branch: branch.to_string(),
            sha: sha.to_string(),
            level: VerdictLevel::ReviewClean,
            evidence: "clean".to_string(),
            verifier: "suhuynh-rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("suhuynh".to_string()),
            harness: "opencode".to_string(),
        };
        journal.record_verdict("wf-rev", &v_rev_clean).unwrap();

        let strongest = journal.strongest_verdict(branch, sha).unwrap().unwrap();
        assert_eq!(strongest.source, VerdictSource::Verifier);
        assert_eq!(strongest.level, VerdictLevel::ReviewClean);
        assert_eq!(strongest.verifier, "suhuynh-rev");

        // A failing verifier verdict dominates the earlier clean one (F4)
        let v_rev_failed = VerdictRecord {
            branch: branch.to_string(),
            sha: sha.to_string(),
            level: VerdictLevel::VerifierFailed,
            evidence: "1 finding".to_string(),
            verifier: "truonglao-rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("truonglao".to_string()),
            harness: "claude".to_string(),
        };
        journal
            .record_verdict("wf-rev-failed", &v_rev_failed)
            .unwrap();

        let dominant = journal.strongest_verdict(branch, sha).unwrap().unwrap();
        assert_eq!(dominant.level, VerdictLevel::VerifierFailed);
        assert!(!dominant.is_verified());
    }
}
