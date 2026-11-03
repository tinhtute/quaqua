//! Farm bus: SQLite event bus for farm coordination and monitoring.
//!
//! Stores `farm_events` and `cursors` in `runs/quaqua.db`.
//! Uses shared SQLite connection opener with WAL mode, busy timeout, and retry_while_busy.

pub mod events;
pub mod mail;
pub mod migrate;
pub use events::InboxDigest;

/// Opens or creates the bus at `path`, initializing schema idempotently.
/// Refuses a legacy database without dropping anything (returning an error).
pub fn open_bus(path: &Path) -> rusqlite::Result<FarmBus> {
    FarmBus::open(path)
}

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Returns the current timestamp in milliseconds, respecting virtual clock if set.
pub fn now_ms() -> u64 {
    if let Ok(clock_file) = std::env::var("PROMPT_WORKER_CLOCK_FILE") {
        if let Ok(c) = std::fs::read_to_string(&clock_file) {
            if let Ok(v) = c.trim().parse::<u64>() {
                return v.saturating_mul(1000);
            }
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// An event to be recorded in the `farm_events` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FarmEvent {
    pub ts_ms: u64,
    pub event_type: String,
    pub actor: Option<String>,
    pub subject: Option<String>,
    pub owner: Option<String>,
    pub project: Option<String>,
    pub incident_key: Option<String>,
    pub payload_json: String,
    pub farm: Option<String>,
}

impl FarmEvent {
    pub fn new(
        event_type: impl Into<String>,
        actor: Option<impl Into<String>>,
        subject: Option<impl Into<String>>,
        owner: Option<impl Into<String>>,
        project: Option<impl Into<String>>,
    ) -> Self {
        Self {
            ts_ms: now_ms(),
            event_type: event_type.into(),
            actor: actor.map(Into::into),
            subject: subject.map(Into::into),
            owner: owner.map(Into::into),
            project: project.map(Into::into),
            incident_key: None,
            payload_json: "{}".to_string(),
            farm: None,
        }
    }

    pub fn with_farm(mut self, farm: impl Into<String>) -> Self {
        self.farm = Some(farm.into());
        self
    }

    pub fn with_incident_key(mut self, key: impl Into<String>) -> Self {
        self.incident_key = Some(key.into());
        self
    }

    pub fn with_payload_json(mut self, payload: impl Into<String>) -> Self {
        self.payload_json = payload.into();
        self
    }
}

/// A stored farm event loaded from the database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FarmEventRecord {
    pub seq: u64,
    pub ts_ms: u64,
    pub event_type: String,
    pub actor: Option<String>,
    pub subject: Option<String>,
    pub owner: Option<String>,
    pub project: Option<String>,
    pub incident_key: Option<String>,
    pub payload_json: String,
    pub farm: Option<String>,
}

/// SQLite-backed event bus.
pub struct FarmBus {
    conn: Mutex<rusqlite::Connection>,
    path: PathBuf,
    farm: String,
    events_watermark: u64,
    mail_watermark: u64,
}
impl FarmBus {
    pub fn default_path() -> PathBuf {
        crate::worker::cleanup::default_runs_dir().join("quaqua.db")
    }

    /// Opens or creates the bus at the default path.
    pub fn new() -> rusqlite::Result<Self> {
        Self::open(&Self::default_path())
    }

    /// Opens or creates the bus at `path`, initializing schema idempotently.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if crate::workspace::live_db_open_refused(path) {
            return Err(rusqlite::Error::InvalidPath(path.to_path_buf()));
        }
        let runs_dir = path.parent().unwrap_or(path);
        let local_farm = crate::workspace::farm_of_runs_dir(runs_dir);

        let conn = crate::engine::sqlite::open_conn(path)?;
        let has_legacy: bool = crate::engine::sqlite::retry_while_busy(|| {
            let legacy_table_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('farm_events', 'mail')",
                [],
                |r| r.get(0),
            )?;
            if legacy_table_count > 0 {
                return Ok(true);
            }
            let has_cursors: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = 'cursors'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .unwrap_or(false);
            if has_cursors {
                let mut stmt = conn.prepare("PRAGMA table_info(cursors)")?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let col_name: String = row.get(1)?;
                    if col_name == "seq" {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        })?;

        if has_legacy {
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::other("legacy database detected; run quaqua bus migrate"),
            )));
        }

        crate::engine::sqlite::retry_while_busy(|| {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS threads (
                    id              TEXT PRIMARY KEY,
                    owner           TEXT NOT NULL,
                    state           TEXT NOT NULL CHECK (state IN ('open','closed')),
                    last_trigger_ms INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS triggers (
                    id           INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts_ms        INTEGER NOT NULL,
                    source       TEXT NOT NULL CHECK (source IN ('herdr','mail','watchdog','worker','github-ci')),
                    kind         TEXT NOT NULL,
                    actor        TEXT NOT NULL,
                    to_name      TEXT,
                    thread_id    TEXT NOT NULL REFERENCES threads(id),
                    summary      TEXT NOT NULL CHECK (length(summary) <= 80),
                    ref          TEXT,
                    payload_json TEXT,
                    dedupe_key   TEXT UNIQUE,
                    farm         TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS triggers_thread ON triggers(thread_id, id);

                CREATE TABLE IF NOT EXISTS subscriptions (
                    thread_id TEXT NOT NULL REFERENCES threads(id),
                    consumer  TEXT NOT NULL,
                    until_ms  INTEGER NOT NULL,
                    PRIMARY KEY (thread_id, consumer)
                );

                CREATE TABLE IF NOT EXISTS cursors (
                    consumer   TEXT PRIMARY KEY,
                    trigger_id INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS source_state (
                    source TEXT PRIMARY KEY,
                    value  TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS bus_migration (
                    name TEXT PRIMARY KEY,
                    watermark INTEGER NOT NULL
                );
                INSERT OR IGNORE INTO bus_migration (name, watermark) VALUES ('superagent-v1', 1);
                "#,
            )?;
            Ok(())
        })?;

        let events_watermark: u64 = conn
            .query_row(
                "SELECT watermark FROM bus_migration WHERE name = 'farm_events_seq'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64;

        let mail_watermark: u64 = conn
            .query_row(
                "SELECT watermark FROM bus_migration WHERE name = 'mail_id'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64;

        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
            farm: local_farm,
            events_watermark,
            mail_watermark,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn farm(&self) -> &str {
        &self.farm
    }

    pub fn events_watermark(&self) -> u64 {
        self.events_watermark
    }

    pub fn mail_watermark(&self) -> u64 {
        self.mail_watermark
    }
    pub fn conn_for_test(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn.lock().unwrap()
    }

    /// Records an event in `farm_events`.
    ///
    /// If `event.incident_key` is set and a row with that key already exists,
    /// the insert is ignored and this method returns `Ok(false)`.
    /// Otherwise, the event is inserted and this method returns `Ok(true)`.
    pub fn record(&self, event: &FarmEvent) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let farm_str = event.farm.as_deref().unwrap_or(&self.farm);
        crate::engine::sqlite::retry_while_busy(|| {
            let has_triggers: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='triggers'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .unwrap_or(false);

            if has_triggers {
                let actor_str = event.actor.as_deref().unwrap_or("worker");
                let thread_name = event
                    .subject
                    .as_deref()
                    .or(event.owner.as_deref())
                    .unwrap_or("unknown");
                let thread_id = format!("agent:{thread_name}");
                conn.execute(
                    "INSERT OR IGNORE INTO threads (id, owner, state, last_trigger_ms) VALUES (?1, ?2, 'open', ?3)",
                    rusqlite::params![&thread_id, thread_name, event.ts_ms as i64],
                )?;
                conn.execute(
                    "UPDATE threads SET last_trigger_ms = MAX(last_trigger_ms, ?1) WHERE id = ?2",
                    rusqlite::params![event.ts_ms as i64, &thread_id],
                )?;
                let summary = format!(
                    "{} {}",
                    event.event_type,
                    event.subject.as_deref().unwrap_or_default()
                );
                let summary_trunc: String = summary.trim().chars().take(80).collect();
                let summary_final = if summary_trunc.is_empty() {
                    event.event_type.chars().take(80).collect()
                } else {
                    summary_trunc
                };
                let source = if actor_str == "herdr" || event.event_type.starts_with("agent_") {
                    "herdr"
                } else if actor_str == "watchdog" || event.event_type == "stalled" {
                    "watchdog"
                } else if actor_str == "github" || event.event_type.contains("github") {
                    "github-ci"
                } else {
                    "worker"
                };
                let mut stmt = conn.prepare_cached(
                    r#"
                    INSERT OR IGNORE INTO triggers (
                        ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm
                    ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, NULL, ?7, ?8, ?9)
                    "#,
                )?;
                let rows = stmt.execute(rusqlite::params![
                    event.ts_ms as i64,
                    source,
                    event.event_type,
                    actor_str,
                    thread_id,
                    summary_final,
                    event.payload_json,
                    event.incident_key,
                    farm_str,
                ])?;
                Ok(rows > 0)
            } else {
                let mut stmt = conn.prepare_cached(
                    r#"
                    INSERT OR IGNORE INTO farm_events (
                        ts_ms, type, actor, subject, owner, project, incident_key, payload_json, farm
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                    "#,
                )?;
                let rows = stmt.execute(rusqlite::params![
                    event.ts_ms as i64,
                    event.event_type,
                    event.actor,
                    event.subject,
                    event.owner,
                    event.project,
                    event.incident_key,
                    event.payload_json,
                    farm_str,
                ])?;
                Ok(rows > 0)
            }
        })
    }

    /// Loads all recorded events ordered by `seq` ASC, optionally filtered to `seq > since_seq`.
    pub fn load_events(&self, since_seq: u64) -> rusqlite::Result<Vec<FarmEventRecord>> {
        let conn = self.conn.lock().unwrap();
        crate::engine::sqlite::retry_while_busy(|| {
            let has_triggers: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='triggers'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .unwrap_or(false);

            if has_triggers {
                let mut stmt = conn.prepare_cached(
                    r#"
                    SELECT id, ts_ms, kind, actor, thread_id, dedupe_key, payload_json, farm
                    FROM triggers
                    WHERE id > ?1
                    ORDER BY id ASC
                    "#,
                )?;
                let rows = stmt.query_map(rusqlite::params![since_seq as i64], |row| {
                    let thread_id: String = row.get(4)?;
                    let subject = thread_id
                        .strip_prefix("agent:")
                        .unwrap_or(&thread_id)
                        .to_string();
                    Ok(FarmEventRecord {
                        seq: row.get::<_, i64>(0)? as u64,
                        ts_ms: row.get::<_, i64>(1)? as u64,
                        event_type: row.get(2)?,
                        actor: Some(row.get(3)?),
                        subject: Some(subject),
                        owner: None,
                        project: None,
                        incident_key: row.get(5)?,
                        payload_json: row
                            .get::<_, Option<String>>(6)?
                            .unwrap_or_else(|| "{}".to_string()),
                        farm: row.get(7)?,
                    })
                })?;
                let mut res = Vec::new();
                for r in rows {
                    res.push(r?);
                }
                Ok(res)
            } else {
                let mut stmt = conn.prepare_cached(
                    r#"
                    SELECT seq, ts_ms, type, actor, subject, owner, project, incident_key, payload_json, farm
                    FROM farm_events
                    WHERE seq > ?1
                    ORDER BY seq ASC
                    "#,
                )?;
                let rows = stmt.query_map(rusqlite::params![since_seq as i64], |row| {
                    Ok(FarmEventRecord {
                        seq: row.get::<_, i64>(0)? as u64,
                        ts_ms: row.get::<_, i64>(1)? as u64,
                        event_type: row.get(2)?,
                        actor: row.get(3)?,
                        subject: row.get(4)?,
                        owner: row.get(5)?,
                        project: row.get(6)?,
                        incident_key: row.get(7)?,
                        payload_json: row.get(8)?,
                        farm: row.get(9)?,
                    })
                })?;
                let mut res = Vec::new();
                for r in rows {
                    res.push(r?);
                }
                Ok(res)
            }
        })
    }

    /// Returns the cursor for `consumer`, if set.
    pub fn get_cursor(&self, consumer: &str) -> rusqlite::Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();
        crate::engine::sqlite::retry_while_busy(|| {
            let has_trigger_id: bool = {
                let mut stmt = conn.prepare("PRAGMA table_info(cursors)")?;
                let mut rows = stmt.query([])?;
                let mut found = false;
                while let Some(row) = rows.next()? {
                    let col_name: String = row.get(1)?;
                    if col_name == "trigger_id" {
                        found = true;
                        break;
                    }
                }
                found
            };
            if has_trigger_id {
                let mut stmt =
                    conn.prepare_cached("SELECT trigger_id FROM cursors WHERE consumer = ?1")?;
                let mut rows = stmt.query(rusqlite::params![consumer])?;
                if let Some(row) = rows.next()? {
                    let id: i64 = row.get(0)?;
                    Ok(Some(id as u64))
                } else {
                    Ok(None)
                }
            } else {
                let mut stmt =
                    conn.prepare_cached("SELECT seq FROM cursors WHERE consumer = ?1")?;
                let mut rows = stmt.query(rusqlite::params![consumer])?;
                if let Some(row) = rows.next()? {
                    let seq: i64 = row.get(0)?;
                    Ok(Some(seq as u64))
                } else {
                    Ok(None)
                }
            }
        })
    }

    /// Sets or updates the cursor for `consumer`.
    pub fn set_cursor(&self, consumer: &str, seq: u64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        crate::engine::sqlite::retry_while_busy(|| {
            let has_trigger_id: bool = {
                let mut stmt = conn.prepare("PRAGMA table_info(cursors)")?;
                let mut rows = stmt.query([])?;
                let mut found = false;
                while let Some(row) = rows.next()? {
                    let col_name: String = row.get(1)?;
                    if col_name == "trigger_id" {
                        found = true;
                        break;
                    }
                }
                found
            };
            if has_trigger_id {
                let mut stmt = conn.prepare_cached(
                    r#"
                    INSERT INTO cursors (consumer, trigger_id) VALUES (?1, ?2)
                    ON CONFLICT(consumer) DO UPDATE SET trigger_id = excluded.trigger_id
                    "#,
                )?;
                stmt.execute(rusqlite::params![consumer, seq as i64])?;
            } else {
                let mut stmt = conn.prepare_cached(
                    r#"
                    INSERT INTO cursors (consumer, seq, mail_id) VALUES (?1, ?2, 0)
                    ON CONFLICT(consumer) DO UPDATE SET seq = excluded.seq
                    "#,
                )?;
                stmt.execute(rusqlite::params![consumer, seq as i64])?;
            }
            Ok(())
        })
    }

    /// Returns the mail cursor (last seen mail id) for `consumer`, if set.
    pub fn get_mail_cursor(&self, consumer: &str) -> rusqlite::Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();
        crate::engine::sqlite::retry_while_busy(|| {
            let mut stmt =
                conn.prepare_cached("SELECT mail_id FROM cursors WHERE consumer = ?1")?;
            let mut rows = stmt.query(rusqlite::params![consumer])?;
            if let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                Ok(Some(id as u64))
            } else {
                Ok(None)
            }
        })
    }

    /// Sets or updates the mail cursor (last seen mail id) for `consumer`.
    pub fn set_mail_cursor(&self, consumer: &str, mail_id: u64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        crate::engine::sqlite::retry_while_busy(|| {
            let mut stmt = conn.prepare_cached(
                r#"
                INSERT INTO cursors (consumer, seq, mail_id) VALUES (?1, 0, ?2)
                ON CONFLICT(consumer) DO UPDATE SET mail_id = excluded.mail_id
                "#,
            )?;
            stmt.execute(rusqlite::params![consumer, mail_id as i64])?;
            Ok(())
        })
    }
}

/// Safely records a farm event to the database at `db_path`.
/// Logs on error and returns `false`, ensuring bus write failures never break callers.
pub fn record_event_safely(db_path: &Path, event: &FarmEvent) -> bool {
    if crate::workspace::live_db_open_refused(db_path) {
        eprintln!(
            "warning: bus refused write to live workspace database under test: {}",
            db_path.display()
        );
        return false;
    }
    match FarmBus::open(db_path) {
        Ok(bus) => match bus.record(event) {
            Ok(inserted) => inserted,
            Err(e) => {
                eprintln!(
                    "warning: bus failed to record event '{}': {e}",
                    event.event_type
                );
                false
            }
        },
        Err(e) => {
            eprintln!("warning: bus failed to open at {}: {e}", db_path.display());
            false
        }
    }
}

/// Safely records a farm event into `<runs_dir>/quaqua.db`.
pub fn record_event_safely_in_runs(runs_dir: &Path, event: &FarmEvent) -> bool {
    record_event_safely(&runs_dir.join("quaqua.db"), event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    struct TempDb(PathBuf);
    impl Drop for TempDb {
        fn drop(&mut self) {
            if let Some(parent) = self.0.parent() {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }

    fn temp_db(tag: &str) -> (PathBuf, TempDb) {
        static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("quaqua-bus-{tag}-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("quaqua.db");
        (path.clone(), TempDb(path))
    }

    #[test]
    fn test_concurrent_inserts_give_monotonic_seq() {
        let (db_path, _cleaner) = temp_db("concurrent-seq");
        let bus = Arc::new(FarmBus::open(&db_path).expect("open bus"));

        let num_threads = 8;
        let events_per_thread = 20;
        let mut handles = Vec::new();

        for t in 0..num_threads {
            let bus = Arc::clone(&bus);
            handles.push(thread::spawn(move || {
                for i in 0..events_per_thread {
                    let ev = FarmEvent::new(
                        "spawned",
                        Some(format!("thread-{t}")),
                        Some(format!("worker-{t}-{i}")),
                        Some("cos"),
                        Some("quaqua"),
                    );
                    let inserted = bus.record(&ev).expect("record event");
                    assert!(inserted, "new plain event must be inserted");
                }
            }));
        }

        for h in handles {
            h.join().expect("thread join");
        }

        let events = bus.load_events(0).expect("load events");
        assert_eq!(
            events.len(),
            num_threads * events_per_thread,
            "all concurrent events must be recorded"
        );

        for (i, ev) in events.iter().enumerate() {
            let expected_seq = (i + 1) as u64;
            assert_eq!(
                ev.seq, expected_seq,
                "seq must be strictly consecutive and monotonic"
            );
        }
    }

    #[test]
    fn test_duplicate_incident_key_inserts_once() {
        let (db_path, _cleaner) = temp_db("dup-incident");
        let bus = FarmBus::open(&db_path).expect("open bus");

        let key = "stall:sude-task:1000";

        // 1. First insert with incident key must succeed
        let ev1 = FarmEvent::new(
            "stalled",
            Some("watchdog"),
            Some("sude-task"),
            Some("cos"),
            Some("quaqua"),
        )
        .with_incident_key(key)
        .with_payload_json(r#"{"attempt": 1}"#);
        let inserted1 = bus.record(&ev1).expect("record ev1");
        assert!(inserted1, "first incident_key insert must return true");

        // 2. Duplicate insert with identical incident key must be ignored and return false
        let ev2 = FarmEvent::new(
            "stalled",
            Some("watchdog"),
            Some("sude-task"),
            Some("cos"),
            Some("quaqua"),
        )
        .with_incident_key(key)
        .with_payload_json(r#"{"attempt": 2}"#);
        let inserted2 = bus.record(&ev2).expect("record ev2");
        assert!(
            !inserted2,
            "duplicate incident_key insert must return false"
        );

        // 3. Different incident key must succeed
        let ev3 = FarmEvent::new(
            "stalled",
            Some("watchdog"),
            Some("other-task"),
            Some("cos"),
            Some("quaqua"),
        )
        .with_incident_key("stall:other-task:1000");
        let inserted3 = bus.record(&ev3).expect("record ev3");
        assert!(inserted3, "different incident_key must return true");

        // 4. Events with None incident_key must always succeed
        let ev4 = FarmEvent::new(
            "spawned",
            Some("cos"),
            Some("task-a"),
            Some("cos"),
            Some("quaqua"),
        );
        let ev5 = FarmEvent::new(
            "spawned",
            Some("cos"),
            Some("task-b"),
            Some("cos"),
            Some("quaqua"),
        );
        assert!(bus.record(&ev4).expect("record ev4"));
        assert!(bus.record(&ev5).expect("record ev5"));

        // Verify total count: 4 events (ev1, ev3, ev4, ev5); ev2 was ignored
        let events = bus.load_events(0).expect("load events");
        assert_eq!(
            events.len(),
            4,
            "duplicate incident key must not produce a row"
        );
    }

    #[test]
    fn test_quaqua_status_output_unchanged_with_new_tables_present() {
        let _lock = crate::test_env::lock_env();
        use crate::engine::journal::{EventRecord, Journal};
        use crate::engine::journal_sqlite::SqliteJournal;
        use crate::herdr::Herdr;
        use crate::status::render_status;

        let (db_path, _cleaner) = temp_db("status-unchanged");
        let runs_dir = db_path.parent().unwrap();

        // Populate standard journal data (events table)
        {
            let mut journal = SqliteJournal::open(&db_path).expect("open journal");
            let ev1 = EventRecord::new(1, 1000, "workflow_started", "{}");
            let ev2 = EventRecord::new(2, 2000, "workflow_completed", r#"{"exit":0}"#);
            journal
                .append_event("wf-status-test", &ev1)
                .expect("append ev1");
            journal
                .append_event("wf-status-test", &ev2)
                .expect("append ev2");
        }

        let mock_herdr = Herdr::new();
        let status_before =
            render_status(&db_path, runs_dir, &mock_herdr).expect("render status before");

        // Now open FarmBus on the same database, creating farm_events and cursors tables,
        // and insert several farm events.
        {
            let bus = FarmBus::open(&db_path).expect("open bus");
            bus.record(&FarmEvent::new(
                "spawned",
                Some("cos"),
                Some("w1"),
                Some("cos"),
                Some("proj"),
            ))
            .expect("rec 1");
            bus.record(&FarmEvent::new(
                "report_written",
                Some("w1"),
                Some("w1"),
                Some("cos"),
                Some("proj"),
            ))
            .expect("rec 2");
            bus.set_cursor("cos", 42).expect("set cursor");
        }

        // Render status again: must be byte-identical!
        let status_after =
            render_status(&db_path, runs_dir, &mock_herdr).expect("render status after");

        assert_eq!(
            status_before, status_after,
            "quaqua status output must be byte-unchanged when farm_events/cursors tables and rows exist"
        );
    }

    #[test]
    fn test_farm_bus_legacy_refusal_without_dropping() {
        let (db_path, _cleaner) = temp_db("bus-legacy-refusal");
        let _ = std::fs::remove_file(&db_path);

        // 1. Manually initialize a database with legacy schema
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open legacy db");
            conn.execute_batch(
                r#"
                CREATE TABLE farm_events (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts_ms INTEGER NOT NULL,
                    type TEXT NOT NULL,
                    actor TEXT,
                    subject TEXT,
                    owner TEXT,
                    project TEXT,
                    incident_key TEXT UNIQUE,
                    payload_json TEXT NOT NULL DEFAULT '{}'
                );
                CREATE TABLE mail (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    msg_key     TEXT NOT NULL UNIQUE,
                    ts_ms       INTEGER NOT NULL,
                    from_name   TEXT NOT NULL,
                    from_role   TEXT NOT NULL,
                    to_name     TEXT NOT NULL,
                    kind        TEXT NOT NULL,
                    project     TEXT,
                    reply_to    INTEGER REFERENCES mail(id),
                    body        TEXT NOT NULL CHECK (length(body) <= 4096),
                    deliveries  INTEGER NOT NULL DEFAULT 0,
                    last_delivered_ms INTEGER,
                    acked_ms    INTEGER,
                    acked_by    TEXT
                );
                INSERT INTO farm_events (ts_ms, type, actor, subject, owner, project, incident_key)
                VALUES (1000, 'blocked', 'herdr', 'w1:p1', 'cos', 'projA', 'k1');
                INSERT INTO farm_events (ts_ms, type, actor, subject, owner, project, incident_key)
                VALUES (2000, 'blocked', 'herdr', 'w7:p2', NULL, NULL, 'k2');
                "#,
            )
            .expect("create legacy schema");
        }

        // Count rows before open
        let count_before: i64 = {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.query_row("SELECT count(*) FROM farm_events", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count_before, 2);

        // 2. Open via FarmBus -> must return Err refusing legacy database
        let open_res = FarmBus::open(&db_path);
        assert!(
            open_res.is_err(),
            "legacy database must be refused by FarmBus::open"
        );
        let err_msg = open_res.err().unwrap().to_string();
        assert!(
            err_msg.contains("legacy database detected; run quaqua bus migrate"),
            "error message must advise running migration: {err_msg}"
        );

        // Invariant: no row deleted or dropped
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count_after: i64 = conn
            .query_row("SELECT count(*) FROM farm_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count_after, count_before,
            "refusal must not delete any row"
        );
    }

    #[test]
    fn test_open_bus_fresh_creates_new_tables() {
        let (db_path, _cleaner) = temp_db("bus-fresh-new-tables");
        let bus = open_bus(&db_path).expect("open fresh bus");
        let conn = bus.conn_for_test();

        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('triggers', 'threads', 'subscriptions', 'cursors', 'source_state', 'bus_migration')",
                [],
                |r| r.get(0),
            )
            .expect("query tables");
        assert_eq!(table_count, 6, "all 6 superagent tables must exist");

        let watermark: i64 = conn
            .query_row(
                "SELECT watermark FROM bus_migration WHERE name='superagent-v1'",
                [],
                |r| r.get(0),
            )
            .expect("query watermark");
        assert_eq!(watermark, 1);
    }
}
