use quaqua::bus::migrate::{migrate_bus, MigrationSummary};
use quaqua::bus::{open_bus, FarmBus};
use quaqua::herdr::HerdrOps;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn hermetic_temp_dir(prefix: &str) -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "quaqua-bus-migrate-test-{prefix}-{}-{id}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create hermetic temp dir");
    dir
}

struct MockHerdr {
    agent_list_result: std::io::Result<String>,
}

impl MockHerdr {
    fn with_agents(agents_json: &str) -> Self {
        Self {
            agent_list_result: Ok(format!(r#"{{"result":{{"agents":{agents_json}}}}}"#)),
        }
    }

    fn with_raw(raw: &str) -> Self {
        Self {
            agent_list_result: Ok(raw.to_string()),
        }
    }

    fn with_err(err: std::io::Error) -> Self {
        Self {
            agent_list_result: Err(err),
        }
    }

    fn idle_clean() -> Self {
        Self::with_agents("[]")
    }
}

impl HerdrOps for MockHerdr {
    fn run(&self, _args: &[&str]) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn agent_list(&self) -> std::io::Result<String> {
        self.agent_list_result
            .as_ref()
            .map(|s| s.clone())
            .map_err(|e| std::io::Error::new(e.kind(), e.to_string()))
    }
    fn agent_get(&self, _name: &str) -> std::io::Result<(bool, String)> {
        unimplemented!()
    }
    fn agent_read(&self, _name: &str, _source: &str, _lines: u32) -> std::io::Result<String> {
        unimplemented!()
    }
    fn agent_start(
        &self,
        _name: &str,
        _kind: &str,
        _pane: &str,
        _timeout_ms: u32,
        _extra_args: &[&str],
    ) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn pane_split(
        &self,
        _cwd: &str,
        _owner: &str,
        _envs: &[(&str, &str)],
    ) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn pane_close(&self, _pane: &str) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn pane_report_metadata(&self, _pane: &str, _tokens: &[&str]) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn pane_wait_output(&self, _pane: &str, _pattern: &str, _timeout_ms: u32) -> bool {
        false
    }
    fn pane_read(&self, _pane: &str, _source: &str, _lines: u32) -> std::io::Result<String> {
        unimplemented!()
    }
    fn pane_send_keys(&self, _pane: &str, _key: &str) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn agent_send_keys(&self, _name: &str, _key: &str) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn agent_wait_idle(&self, _name: &str, _timeout_ms: u32) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn agent_prompt_nowait(&self, _name: &str, _prompt: &str) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn agent_prompt_wait_background(
        &self,
        _name: &str,
        _prompt: &str,
        _timeout_ms: u32,
        _log_file: std::fs::File,
    ) -> std::io::Result<std::process::Child> {
        unimplemented!()
    }
}

fn create_legacy_fixture_db(db_path: &Path) {
    let conn = Connection::open(db_path).expect("create legacy db");
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
            payload_json TEXT NOT NULL DEFAULT '{}',
            farm TEXT
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
            acked_by    TEXT,
            farm        TEXT
        );
        CREATE TABLE cursors (
            consumer TEXT PRIMARY KEY,
            seq INTEGER NOT NULL,
            mail_id INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE bus_migration (
            name TEXT PRIMARY KEY,
            watermark INTEGER NOT NULL
        );
        "#,
    )
    .expect("legacy schema batch");
}

#[test]
fn test_fresh_db_creates_new_tables_and_succeeds() {
    let tmp = hermetic_temp_dir("fresh-db");
    let db_path = tmp.join("quaqua.db");

    // Open via open_bus
    let bus = open_bus(&db_path).expect("open_bus must succeed on fresh path");
    assert_eq!(bus.path(), db_path);

    // Open via FarmBus::open
    let bus2 = FarmBus::open(&db_path).expect("FarmBus::open must succeed idempotently");
    let conn = bus2.conn_for_test();

    // Verify all 6 superagent tables exist
    let table_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('triggers', 'threads', 'subscriptions', 'cursors', 'source_state', 'bus_migration')",
            [],
            |r| r.get(0),
        )
        .expect("query tables");
    assert_eq!(table_count, 6, "all 6 superagent tables must exist");

    // Verify columns on triggers
    let mut stmt = conn.prepare("PRAGMA table_info(triggers)").unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut cols = Vec::new();
    while let Some(r) = rows.next().unwrap() {
        cols.push(r.get::<_, String>(1).unwrap());
    }
    assert!(cols.contains(&"id".to_string()));
    assert!(cols.contains(&"ts_ms".to_string()));
    assert!(cols.contains(&"source".to_string()));
    assert!(cols.contains(&"kind".to_string()));
    assert!(cols.contains(&"actor".to_string()));
    assert!(cols.contains(&"to_name".to_string()));
    assert!(cols.contains(&"thread_id".to_string()));
    assert!(cols.contains(&"summary".to_string()));
    assert!(cols.contains(&"ref".to_string()));
    assert!(cols.contains(&"payload_json".to_string()));
    assert!(cols.contains(&"dedupe_key".to_string()));
    assert!(cols.contains(&"farm".to_string()));

    // Verify cursors table has trigger_id column and NOT seq
    let mut stmt = conn.prepare("PRAGMA table_info(cursors)").unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut cursor_cols = Vec::new();
    while let Some(r) = rows.next().unwrap() {
        cursor_cols.push(r.get::<_, String>(1).unwrap());
    }
    assert!(cursor_cols.contains(&"consumer".to_string()));
    assert!(cursor_cols.contains(&"trigger_id".to_string()));
    assert!(!cursor_cols.contains(&"seq".to_string()));

    // Verify bus_migration watermark
    let watermark: i64 = conn
        .query_row(
            "SELECT watermark FROM bus_migration WHERE name='superagent-v1'",
            [],
            |r| r.get(0),
        )
        .expect("query watermark");
    assert_eq!(watermark, 1);
}

#[test]
fn test_legacy_db_open_refused_without_dropping() {
    let tmp = hermetic_temp_dir("legacy-refused");
    let db_path = tmp.join("quaqua.db");
    create_legacy_fixture_db(&db_path);

    // Insert legacy data
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO farm_events (ts_ms, type, actor, subject, owner, incident_key, payload_json) VALUES (1000, 'blocked', 'herdr', 'w1:p1', 'cos', 'k1', '{\"msg\":1}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mail (msg_key, ts_ms, from_name, from_role, to_name, kind, body) VALUES ('m1', 2000, 'cos', 'cos', 'w1', 'directive', 'do work')",
            [],
        )
        .unwrap();
    }

    // Attempt FarmBus::open
    let open_res = FarmBus::open(&db_path);
    assert!(
        open_res.is_err(),
        "FarmBus::open must return Err on legacy DB"
    );
    let err_msg = open_res.err().unwrap().to_string();
    assert!(
        err_msg.contains("legacy database detected; run quaqua bus migrate"),
        "error must advise running quaqua bus migrate: {err_msg}"
    );

    // Attempt open_bus
    let open_bus_res = open_bus(&db_path);
    assert!(
        open_bus_res.is_err(),
        "open_bus must return Err on legacy DB"
    );

    // Verify legacy tables and rows are completely untouched
    let conn = Connection::open(&db_path).unwrap();
    let fe_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM farm_events", [], |r| r.get(0))
        .unwrap();
    let mail_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM mail", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fe_count, 1, "farm_events rows must not be dropped");
    assert_eq!(mail_count, 1, "mail rows must not be dropped");
}

#[test]
fn test_migrate_live_worker_refused() {
    let tmp = hermetic_temp_dir("live-worker-refused");
    let db_path = tmp.join("quaqua.db");
    create_legacy_fixture_db(&db_path);

    let herdr_active =
        MockHerdr::with_agents(r#"[{"name":"sude-sa-t1","agent_status":"working"}]"#);
    let res = migrate_bus(&db_path, None, false, &herdr_active);
    assert!(res.is_err());
    let err_msg = res.err().unwrap();
    assert!(
        err_msg.contains("cannot migrate: active workers running: sude-sa-t1 (working)"),
        "must list active worker: {err_msg}"
    );

    // Verify database was not changed
    let conn = Connection::open(&db_path).unwrap();
    let has_fe: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='farm_events'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap();
    assert!(has_fe, "legacy tables must remain untouched");
}
#[test]
fn test_migrate_live_worker_herdr_error_fails_closed() {
    let tmp = hermetic_temp_dir("live-worker-err-closed");
    let db_path = tmp.join("quaqua.db");
    create_legacy_fixture_db(&db_path);

    let herdr_err = MockHerdr::with_err(std::io::Error::other("herdr daemon down"));
    let res = migrate_bus(&db_path, None, false, &herdr_err);
    assert!(res.is_err(), "must fail closed when herdr errors");
    let err_msg = res.err().unwrap();
    assert!(
        err_msg.contains("cannot verify live workers: herdr agent list failed: herdr daemon down"),
        "unexpected error message: {err_msg}"
    );
}

#[test]
fn test_migrate_live_worker_malformed_json_fails_closed() {
    let tmp = hermetic_temp_dir("live-worker-malformed-json");
    let db_path = tmp.join("quaqua.db");
    create_legacy_fixture_db(&db_path);

    let herdr_bad = MockHerdr::with_raw("not valid json");
    let res = migrate_bus(&db_path, None, false, &herdr_bad);
    assert!(
        res.is_err(),
        "must fail closed on unparseable herdr response"
    );
    let err_msg = res.err().unwrap();
    assert!(
        err_msg.contains("cannot verify live workers: invalid herdr agent list JSON"),
        "unexpected error message: {err_msg}"
    );
}

#[test]
fn test_migrate_dry_run_allowed_with_live_workers() {
    let tmp = hermetic_temp_dir("dry-run-live-worker");
    let db_path = tmp.join("quaqua.db");
    create_legacy_fixture_db(&db_path);

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO farm_events (ts_ms, type, actor, subject) VALUES (100, 'spawned', 'cos', 'w1')",
            [],
        )
        .unwrap();
    }

    // Active worker present
    let herdr_active =
        MockHerdr::with_agents(r#"[{"name":"worker-running","agent_status":"working"}]"#);
    // dry-run must succeed and report counts
    let summary = migrate_bus(&db_path, None, true, &herdr_active)
        .expect("dry-run must be allowed even when live workers exist");
    assert!(summary.dry_run);
    assert_eq!(summary.farm_events_count, 1);
    assert_eq!(summary.triggers_count, 1);
}

#[test]
fn test_migrate_backup_vacuum_into() {
    let tmp = hermetic_temp_dir("backup-vacuum");
    let db_path = tmp.join("quaqua.db");
    let raw_backup_dir = tmp.join("raw");
    create_legacy_fixture_db(&db_path);

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO farm_events (ts_ms, type, actor, subject, owner, incident_key) VALUES (1000, 'spawned', 'cos', 'w1', 'cos', 'k1')",
            [],
        )
        .unwrap();
    }

    let herdr = MockHerdr::idle_clean();
    let summary: MigrationSummary =
        migrate_bus(&db_path, Some(&raw_backup_dir), false, &herdr).expect("migrate succeeds");

    assert!(summary.backup_path.is_some());
    let backup_path = summary.backup_path.unwrap();
    assert!(backup_path.exists(), "backup file must exist on disk");

    // Open backup file and verify it contains original legacy table and row
    let conn_bak = Connection::open(&backup_path).expect("open backup db");
    let fe_count: i64 = conn_bak
        .query_row("SELECT COUNT(*) FROM farm_events", [], |r| r.get(0))
        .expect("query farm_events in backup");
    assert_eq!(fe_count, 1, "backup must preserve legacy data");
}

#[test]
fn test_migrate_1_to_1_row_mapping_and_order() {
    let tmp = hermetic_temp_dir("mapping-1to1");
    let db_path = tmp.join("quaqua.db");
    let backup_dir = tmp.join("raw");
    create_legacy_fixture_db(&db_path);

    // Insert interleaving rows
    {
        let conn = Connection::open(&db_path).unwrap();
        // farm_event 1: ts=100
        conn.execute(
            "INSERT INTO farm_events (ts_ms, type, actor, subject, owner, incident_key, payload_json, farm) VALUES (100, 'spawned', 'cos', 'w1', 'cos', 'inc:1', '{\"worker\":\"w1\"}', 'farmA')",
            [],
        ).unwrap();
        // mail 1: ts=200
        conn.execute(
            "INSERT INTO mail (msg_key, ts_ms, from_name, from_role, to_name, kind, body, farm) VALUES ('m:1', 200, 'cos', 'cos', 'w1', 'directive', 'Fix bug\nDetails here', 'farmA')",
            [],
        ).unwrap();
        // farm_event 2: ts=300
        conn.execute(
            "INSERT INTO farm_events (ts_ms, type, actor, subject, owner, incident_key, payload_json, farm) VALUES (300, 'stalled', 'watchdog', 'w1', 'cos', 'inc:2', '{}', 'farmA')",
            [],
        ).unwrap();
        // mail 2: ts=400 (multibyte body)
        conn.execute(
            "INSERT INTO mail (msg_key, ts_ms, from_name, from_role, to_name, kind, body, farm) VALUES ('m:2', 400, 'w1', 'worker', 'cos', 'handoff', 'Tiến độ hoàn thành 100% tiếng Việt có dấu\nNội dung chi tiết', 'farmA')",
            [],
        ).unwrap();
    }

    let herdr = MockHerdr::idle_clean();
    let summary = migrate_bus(&db_path, Some(&backup_dir), false, &herdr).expect("migrate");

    assert_eq!(summary.farm_events_count, 2);
    assert_eq!(summary.mail_count, 2);
    assert_eq!(summary.triggers_count, 4, "exact 1:1 mapping (2 + 2 = 4)");

    // Inspect migrated DB
    let conn = Connection::open(&db_path).unwrap();

    // Legacy tables must be dropped
    let has_fe: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='farm_events'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap();
    let has_mail: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='mail'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap();
    assert!(!has_fe, "legacy farm_events must be dropped");
    assert!(!has_mail, "legacy mail must be dropped");

    // Read triggers in order
    struct TrigRow {
        id: i64,
        ts_ms: i64,
        source: String,
        kind: String,
        actor: String,
        to_name: Option<String>,
        thread_id: String,
        summary: String,
        dedupe_key: Option<String>,
        farm: String,
    }

    let mut stmt = conn
        .prepare("SELECT id, ts_ms, source, kind, actor, to_name, thread_id, summary, dedupe_key, farm FROM triggers ORDER BY id ASC")
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok(TrigRow {
                id: r.get(0)?,
                ts_ms: r.get(1)?,
                source: r.get(2)?,
                kind: r.get(3)?,
                actor: r.get(4)?,
                to_name: r.get(5)?,
                thread_id: r.get(6)?,
                summary: r.get(7)?,
                dedupe_key: r.get(8)?,
                farm: r.get(9)?,
            })
        })
        .unwrap();

    let trigs: Vec<TrigRow> = rows.map(|r| r.unwrap()).collect();
    assert_eq!(trigs.len(), 4);

    // Verify trigger 1: farm_event (ts=100)
    assert_eq!(trigs[0].id, 1);
    assert_eq!(trigs[0].ts_ms, 100);
    assert_eq!(trigs[0].source, "worker"); // actor 'cos' -> 'worker'
    assert_eq!(trigs[0].kind, "spawned");
    assert_eq!(trigs[0].actor, "cos");
    assert_eq!(trigs[0].to_name, None);
    assert_eq!(trigs[0].thread_id, "agent:w1");
    assert_eq!(trigs[0].summary, "spawned w1");
    assert_eq!(trigs[0].dedupe_key.as_deref(), Some("inc:1"));
    assert_eq!(trigs[0].farm, "farmA");

    // Verify trigger 2: mail (ts=200)
    assert_eq!(trigs[1].id, 2);
    assert_eq!(trigs[1].ts_ms, 200);
    assert_eq!(trigs[1].source, "mail");
    assert_eq!(trigs[1].kind, "directive");
    assert_eq!(trigs[1].actor, "cos");
    assert_eq!(trigs[1].to_name.as_deref(), Some("w1"));
    assert_eq!(trigs[1].thread_id, "agent:w1");
    assert_eq!(trigs[1].summary, "Fix bug");
    assert_eq!(trigs[1].dedupe_key, None);
    assert_eq!(trigs[1].farm, "farmA");

    // Verify trigger 3: farm_event (ts=300)
    assert_eq!(trigs[2].id, 3);
    assert_eq!(trigs[2].ts_ms, 300);
    assert_eq!(trigs[2].source, "watchdog");
    assert_eq!(trigs[2].kind, "stalled");
    assert_eq!(trigs[2].actor, "watchdog");
    assert_eq!(trigs[2].dedupe_key.as_deref(), Some("inc:2"));

    // Verify trigger 4: mail (ts=400)
    assert_eq!(trigs[3].id, 4);
    assert_eq!(trigs[3].ts_ms, 400);
    assert_eq!(trigs[3].source, "mail");
    assert_eq!(trigs[3].kind, "handoff");
    assert_eq!(trigs[3].actor, "w1");
    assert_eq!(trigs[3].to_name.as_deref(), Some("cos"));
    assert_eq!(trigs[3].thread_id, "agent:cos");
    assert_eq!(
        trigs[3].summary,
        "Tiến độ hoàn thành 100% tiếng Việt có dấu"
    );

    // Verify threads
    // Verify threads: exactly agent:w1 and agent:cos (no phantom agent:unknown)
    let thread_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM threads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        thread_count, 2,
        "threads must only contain agent:w1 and agent:cos"
    );
    let thread_ids: Vec<String> = conn
        .prepare("SELECT id FROM threads ORDER BY id ASC")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(thread_ids, vec!["agent:cos", "agent:w1"]);
}

#[test]
fn test_migrate_cursor_prefix_rule() {
    let tmp = hermetic_temp_dir("cursor-prefix");
    let db_path = tmp.join("quaqua.db");
    let backup_dir = tmp.join("raw");
    create_legacy_fixture_db(&db_path);

    {
        let conn = Connection::open(&db_path).unwrap();
        // Triggers will be:
        // id 1: fe seq 1 (ts 100)
        // id 2: mail id 1 (ts 200)
        // id 3: fe seq 2 (ts 300)
        // id 4: mail id 2 (ts 400)
        conn.execute(
            "INSERT INTO farm_events (seq, ts_ms, type, actor, subject) VALUES (1, 100, 'spawned', 'cos', 'w1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO mail (id, msg_key, ts_ms, from_name, from_role, to_name, kind, body) VALUES (1, 'm1', 200, 'cos', 'cos', 'w1', 'd', 'msg 1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO farm_events (seq, ts_ms, type, actor, subject) VALUES (2, 300, 'spawned', 'cos', 'w2')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO mail (id, msg_key, ts_ms, from_name, from_role, to_name, kind, body) VALUES (2, 'm2', 400, 'cos', 'cos', 'w2', 'd', 'msg 2')",
            [],
        ).unwrap();

        // Consumers:
        // c_all: seq=2, mail_id=2 -> all 4 triggers read -> cursor = 4
        // c_none: seq=0, mail_id=0 -> trigger 1 unread -> cursor = 0
        // c_first_two: seq=1, mail_id=1 -> trigger 1 & 2 read, trigger 3 unread -> cursor = 2
        // c_gap: seq=1, mail_id=2 -> trigger 1 read (seq 1<=1), trigger 2 read (mail 1<=2), trigger 3 UNREAD (seq 2 > 1), trigger 4 read (mail 2<=2)
        //   Prefix rule: first unread trigger is id 3. Cursor must be 2!
        conn.execute(
            "INSERT INTO cursors (consumer, seq, mail_id) VALUES ('c_all', 2, 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursors (consumer, seq, mail_id) VALUES ('c_none', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursors (consumer, seq, mail_id) VALUES ('c_first_two', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursors (consumer, seq, mail_id) VALUES ('c_gap', 1, 2)",
            [],
        )
        .unwrap();
    }

    let herdr = MockHerdr::idle_clean();
    let summary = migrate_bus(&db_path, Some(&backup_dir), false, &herdr).expect("migrate");

    let cursor_map: std::collections::HashMap<String, u64> = summary
        .consumer_cursors
        .into_iter()
        .map(|(c, _, _, new_c)| (c, new_c))
        .collect();

    assert_eq!(cursor_map.get("c_all"), Some(&4));
    assert_eq!(cursor_map.get("c_none"), Some(&0));
    assert_eq!(cursor_map.get("c_first_two"), Some(&2));
    assert_eq!(
        cursor_map.get("c_gap"),
        Some(&2),
        "gap in read events must stop cursor at prefix 2"
    );

    // Verify stored cursors in database match
    let conn = Connection::open(&db_path).unwrap();
    let mut stmt = conn
        .prepare("SELECT consumer, trigger_id FROM cursors")
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        })
        .unwrap();
    let db_cursors: std::collections::HashMap<String, u64> = rows.map(|r| r.unwrap()).collect();

    assert_eq!(db_cursors.get("c_all"), Some(&4));
    assert_eq!(db_cursors.get("c_none"), Some(&0));
    assert_eq!(db_cursors.get("c_first_two"), Some(&2));
    assert_eq!(db_cursors.get("c_gap"), Some(&2));
}

#[test]
fn test_migrate_idempotent_second_run() {
    let tmp = hermetic_temp_dir("idempotent-2nd-run");
    let db_path = tmp.join("quaqua.db");
    let backup_dir = tmp.join("raw");
    create_legacy_fixture_db(&db_path);

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO farm_events (ts_ms, type, actor, subject) VALUES (100, 'spawned', 'cos', 'w1')",
            [],
        )
        .unwrap();
    }

    let herdr = MockHerdr::idle_clean();
    // First run
    let sum1 = migrate_bus(&db_path, Some(&backup_dir), false, &herdr).expect("first run");
    assert!(!sum1.already_migrated);
    assert_eq!(sum1.triggers_count, 1);

    // Second run
    let sum2 = migrate_bus(&db_path, Some(&backup_dir), false, &herdr).expect("second run");
    assert!(
        sum2.already_migrated,
        "second run must be no-op with already_migrated true"
    );

    // FarmBus::open succeeds on migrated DB
    let bus = FarmBus::open(&db_path).expect("FarmBus::open must succeed on migrated DB");
    assert_eq!(bus.path(), db_path);
}

#[test]
fn test_migrate_dry_run_leaves_db_untouched() {
    let tmp = hermetic_temp_dir("dry-run-untouched");
    let db_path = tmp.join("quaqua.db");
    create_legacy_fixture_db(&db_path);

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO farm_events (ts_ms, type, actor, subject) VALUES (100, 'spawned', 'cos', 'w1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mail (msg_key, ts_ms, from_name, from_role, to_name, kind, body) VALUES ('m1', 200, 'cos', 'cos', 'w1', 'd', 'msg')",
            [],
        )
        .unwrap();
    }

    let bytes_before = fs::read(&db_path).unwrap();

    let herdr = MockHerdr::idle_clean();
    let summary = migrate_bus(&db_path, None, true, &herdr).expect("dry run");
    assert!(summary.dry_run);
    assert_eq!(summary.farm_events_count, 1);
    assert_eq!(summary.mail_count, 1);
    assert_eq!(summary.triggers_count, 2);

    let bytes_after = fs::read(&db_path).unwrap();
    assert_eq!(
        bytes_before, bytes_after,
        "dry-run must leave database byte-identical"
    );

    // Legacy tables still exist
    let conn = Connection::open(&db_path).unwrap();
    let fe_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM farm_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fe_count, 1);
}

#[test]
fn test_legacy_db_cli_refusal_exits_code_2() {
    let tmp = hermetic_temp_dir("cli-exit-2");
    let db_path = tmp.join("quaqua.db");
    create_legacy_fixture_db(&db_path);

    let bin = env!("CARGO_BIN_EXE_quaqua");

    // Case 1: `quaqua inbox` on legacy DB
    let out = std::process::Command::new(bin)
        .arg("inbox")
        .env("QUAQUA_RUNS_DIR", &tmp)
        .output()
        .expect("run quaqua inbox");
    assert_eq!(
        out.status.code(),
        Some(2),
        "quaqua inbox on legacy DB must exit with code 2"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("legacy database detected; run quaqua bus migrate"),
        "stderr must mention running quaqua bus migrate: {stderr}"
    );

    // Case 2: `quaqua inbox --all` on legacy DB
    let out_all = std::process::Command::new(bin)
        .arg("inbox")
        .arg("--all")
        .env("QUAQUA_RUNS_DIR", &tmp)
        .output()
        .expect("run quaqua inbox --all");
    assert_eq!(
        out_all.status.code(),
        Some(2),
        "quaqua inbox --all on legacy DB must exit with code 2"
    );

    // Case 3: `quaqua mail inbox` on legacy DB
    let out_mail = std::process::Command::new(bin)
        .arg("mail")
        .arg("inbox")
        .env("QUAQUA_RUNS_DIR", &tmp)
        .output()
        .expect("run quaqua mail inbox");
    assert_eq!(
        out_mail.status.code(),
        Some(2),
        "quaqua mail inbox on legacy DB must exit with code 2"
    );
}
