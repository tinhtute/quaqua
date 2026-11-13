//! R1 (#214): the GUI read contract. Hermetic fixture DBs only.

use quaqua::bus::events::{DeliverFormat, DeliverMode};
use quaqua::bus::{open_bus, FarmBus};
use quaqua::engine::child::SingleWorkerInput;
use quaqua::engine::journal_sqlite::SqliteJournal;
use quaqua::worker::prompt::workflow::SingleWorkerState;
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_db() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("quaqua-gui-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("quaqua.db")
}

fn columns(conn: &Connection, view: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("SELECT name FROM pragma_table_info('{view}')"))
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn add_trigger(conn: &Connection, farm: &str, id: i64, to: &str, thread: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO threads (id, owner, state, last_trigger_ms) VALUES (?1, 'x', 'open', 1)",
        [thread],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO triggers (id, ts_ms, source, kind, actor, to_name, thread_id, summary, farm) \
         VALUES (?1, ?1, 'mail', 'handoff', 'lead', ?2, ?3, 's', ?4)",
        rusqlite::params![id, to, thread, farm],
    )
    .unwrap();
}

#[test]
fn gui_view_columns_and_version_are_pinned_on_a_fresh_db() {
    let db = temp_db();
    let _bus = open_bus(&db).unwrap();
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        columns(&conn, "v_gui_workflows"),
        [
            "workflow_id",
            "phase",
            "snap_seq",
            "last_event_kind",
            "last_event_seq",
            "last_event_ts_ms"
        ]
    );
    assert_eq!(
        columns(&conn, "v_gui_inbox"),
        [
            "id",
            "ts_ms",
            "source",
            "kind",
            "from_name",
            "to_name",
            "summary",
            "thread_id",
            "ref",
            "payload_json",
            "farm"
        ]
    );
    assert_eq!(
        columns(&conn, "v_gui_workers"),
        [
            "name",
            "kind",
            "workflow_id",
            "budget_sec",
            "cost_budget",
            "start_ms",
            "agent_status",
            "hook_active",
            "errors",
            "lead_waiting",
            "snap_seq",
            "last_event_kind",
            "last_event_ts_ms"
        ]
    );
    assert_eq!(columns(&conn, "gui_schema_version"), ["version"]);
    assert_eq!(count(&conn, "SELECT version FROM gui_schema_version"), 1);
    // Empty tables give empty, queryable views.
    for v in ["v_gui_workflows", "v_gui_inbox", "v_gui_workers"] {
        assert_eq!(count(&conn, &format!("SELECT COUNT(*) FROM {v}")), 0, "{v}");
    }
}

#[test]
fn gui_views_exist_when_the_journal_opened_the_db_first() {
    let db = temp_db();
    drop(SqliteJournal::open(&db).unwrap());
    drop(open_bus(&db).unwrap());
    let conn = Connection::open(&db).unwrap();
    assert_eq!(count(&conn, "SELECT version FROM gui_schema_version"), 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM v_gui_workflows"), 0);
}

#[test]
fn gui_workflows_report_phase_per_workflow_and_survive_bad_rows() {
    let db = temp_db();
    drop(open_bus(&db).unwrap());
    let conn = Connection::open(&db).unwrap();
    for (wf, phase) in [
        ("p-build", "build"),
        ("p-review", "review"),
        ("p-done", "done"),
    ] {
        conn.execute(
            "INSERT INTO snapshots VALUES (?1, 1, ?2)",
            rusqlite::params![wf, format!("{{\"phase\":\"{phase}\"}}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events VALUES (?1, 1, 1000, 'step_done', '{}')",
            [wf],
        )
        .unwrap();
    }
    // Latest snapshot wins; a cancelled event overrides the phase.
    conn.execute(
        "INSERT INTO snapshots VALUES ('p-build', 2, '{\"phase\":\"fix\"}')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO snapshots VALUES ('p-cancel', 1, '{\"phase\":\"build\"}')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events VALUES ('p-cancel', 1, 2000, 'workflow_cancelled', '{}')",
        [],
    )
    .unwrap();
    // One malformed snapshot and one event-only workflow must not break the view.
    conn.execute("INSERT INTO snapshots VALUES ('p-junk', 1, 'not json')", [])
        .unwrap();
    conn.execute(
        "INSERT INTO events VALUES ('p-evonly', 1, 3000, 'workflow_started', '{}')",
        [],
    )
    .unwrap();

    let phase = |wf: &str| -> Option<String> {
        conn.query_row(
            "SELECT phase FROM v_gui_workflows WHERE workflow_id = ?1",
            [wf],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(phase("p-build").as_deref(), Some("fix"));
    assert_eq!(phase("p-review").as_deref(), Some("review"));
    assert_eq!(phase("p-done").as_deref(), Some("done"));
    assert_eq!(phase("p-cancel").as_deref(), Some("cancelled"));
    assert_eq!(phase("p-junk"), None);
    assert_eq!(phase("p-evonly"), None);
    let (kind, ts): (String, i64) = conn
        .query_row(
            "SELECT last_event_kind, last_event_ts_ms FROM v_gui_workflows WHERE workflow_id = 'p-evonly'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), ts), ("workflow_started", 3000));
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM v_gui_workflows"), 6);
}

#[test]
fn gui_workers_come_from_real_worker_state_only() {
    let db = temp_db();
    drop(open_bus(&db).unwrap());
    let conn = Connection::open(&db).unwrap();
    let input = SingleWorkerInput::new("sude-x", "builder", "/r", "brief", 5400, Some(7.5));
    let mut state = SingleWorkerState::new(input);
    state.start_ms = 4242;
    state.errors = 3;
    state.hook_active = true;
    state.agent_status = "working".to_string();
    conn.execute(
        "INSERT INTO snapshots VALUES ('worker-sude-x', 1, ?1)",
        [serde_json::to_string(&state).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events VALUES ('worker-sude-x', 1, 9000, 'step_done', '{}')",
        [],
    )
    .unwrap();
    // A pipeline snapshot and a malformed one are not workers.
    conn.execute(
        "INSERT INTO snapshots VALUES ('pipe', 1, '{\"phase\":\"build\"}')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO snapshots VALUES ('junk', 1, '{oops')", [])
        .unwrap();

    assert_eq!(count(&conn, "SELECT COUNT(*) FROM v_gui_workers"), 1);
    #[allow(clippy::type_complexity)]
    let row: (String, String, i64, f64, i64, String, i64, i64, String, i64) = conn
        .query_row(
            "SELECT name, kind, budget_sec, cost_budget, start_ms, agent_status, hook_active, errors, \
             last_event_kind, last_event_ts_ms FROM v_gui_workers",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "sude-x".into(),
            "builder".into(),
            5400,
            7.5,
            4242,
            "working".into(),
            1,
            3,
            "step_done".into(),
            9000
        )
    );
}

#[test]
fn gui_inbox_lists_the_unread_set_the_engine_digest_loads_for_cos() {
    let db = temp_db();
    let bus = open_bus(&db).unwrap();
    let farm = bus.farm().to_string();
    {
        let conn = bus.conn_for_test();
        add_trigger(&conn, &farm, 1, "cos", "t1"); // acked by the cursor below
        add_trigger(&conn, &farm, 2, "cos", "t2");
        add_trigger(&conn, &farm, 3, "someone-else", "t3");
        add_trigger(&conn, &farm, 4, "nobody", "t4"); // reaches cos only through a subscription
        add_trigger(&conn, &farm, 5, "cos", "t5");
        conn.execute("INSERT INTO cursors VALUES ('cos', 1)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO subscriptions VALUES ('t4', 'cos', 99999999999999)",
            [],
        )
        .unwrap();
    }
    let mut sink = Vec::new();
    let digest = bus
        .deliver_page("cos", DeliverMode::Peek, DeliverFormat::Plain, &mut sink)
        .unwrap();
    let mut engine_ids: Vec<u64> = digest.triggers.iter().map(|t| t.id).collect();
    engine_ids.sort();
    assert_eq!(engine_ids, [2, 4, 5]);

    let conn = Connection::open(&db).unwrap();
    let mut stmt = conn
        .prepare("SELECT id FROM v_gui_inbox ORDER BY id")
        .unwrap();
    let view_ids: Vec<u64> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(view_ids, engine_ids);

    // Ack (cursor moves) and the card set shrinks on the next read.
    conn.execute(
        "UPDATE cursors SET trigger_id = 5 WHERE consumer = 'cos'",
        [],
    )
    .unwrap();
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM v_gui_inbox"), 0);
}

#[test]
fn gui_views_are_idempotent_and_a_stale_definition_converges() {
    let db = temp_db();
    drop(open_bus(&db).unwrap());
    let schema_cookie = |c: &Connection| count(c, "PRAGMA schema_version");
    let conn = Connection::open(&db).unwrap();
    let before = schema_cookie(&conn);
    drop(open_bus(&db).unwrap());
    assert_eq!(schema_cookie(&conn), before, "second open must not write");

    conn.execute_batch(
        "DROP VIEW v_gui_inbox; CREATE VIEW v_gui_inbox AS SELECT 1 AS id; \
         DROP VIEW gui_schema_version; CREATE VIEW gui_schema_version AS SELECT 0 AS version;",
    )
    .unwrap();
    drop(open_bus(&db).unwrap());
    assert_eq!(columns(&conn, "v_gui_inbox").len(), 11);
    assert_eq!(count(&conn, "SELECT version FROM gui_schema_version"), 1);
}

#[test]
fn gui_views_are_read_only_and_readable_by_absolute_path() {
    let db = temp_db();
    drop(open_bus(&db).unwrap());
    let rw = Connection::open(&db).unwrap();
    assert!(rw
        .execute("INSERT INTO v_gui_inbox (id) VALUES (1)", [])
        .is_err());
    assert!(rw.execute("DELETE FROM gui_schema_version", []).is_err());

    let abs = db.canonicalize().unwrap();
    assert!(abs.is_absolute());
    let ro = Connection::open_with_flags(&abs, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(count(&ro, "SELECT version FROM gui_schema_version"), 1);
    assert_eq!(count(&ro, "SELECT COUNT(*) FROM v_gui_workflows"), 0);
}

#[test]
fn gui_views_are_not_planted_in_a_legacy_database() {
    let db = temp_db();
    let legacy = concat!("farm_", "events");
    Connection::open(&db)
        .unwrap()
        .execute_batch(&format!(
            "CREATE TABLE {legacy} (seq INTEGER PRIMARY KEY, ts_ms INTEGER, event_type TEXT, payload_json TEXT)"
        ))
        .unwrap();
    assert!(FarmBus::open(&db).is_err());
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '%gui%'"
        ),
        0
    );
}
