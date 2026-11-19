//! T32: schema, migrations, path and live guard, old-DB reader.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use qq_core::db::migrate::{latest_version, LADDER};
use qq_core::db::old::{read_runs_dir, OldDb, OldDbError};
use qq_core::db::path::{
    default_db_path, is_live_db, is_test_environment, live_db_files, live_runs_dir,
    old_db_path, resolve_workspace_root,
};
use qq_core::db::{open, DbError};
use rusqlite::Connection;

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "qq-t32-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }
    fn join(&self, p: &str) -> PathBuf {
        self.0.join(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn user_version(conn: &Connection) -> u32 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap()
}

fn tables(conn: &Connection) -> BTreeSet<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.map(|r| r.unwrap()).collect()
}

/// Runs `sql` inside a savepoint that is always rolled back.
fn attempt(conn: &Connection, sql: &str) -> rusqlite::Result<usize> {
    conn.execute_batch("SAVEPOINT attempt").unwrap();
    let res = conn.execute(sql, []);
    conn.execute_batch("ROLLBACK TO attempt; RELEASE attempt").unwrap();
    res
}

const SEED: &str = "
INSERT INTO harness(name, driver) VALUES ('claude', 'native');
INSERT INTO kind(name, harness, model) VALUES ('k1', 'claude', 'm');
INSERT INTO profile(name) VALUES ('pf');
INSERT INTO project(id, slug, created_ms) VALUES (1, 'p', 0);
INSERT INTO agent(id, name, role, kind, project_id, started_ms) VALUES (1, 'a', 'peer', 'k1', 1, 0);
INSERT INTO proposal(id, loop, risk, created_ms) VALUES (1, 1, 'low', 0);
INSERT INTO task(id, project_id, title, disposition, state, created_ms) VALUES (1, 1, 't', 'engineer', 'new', 0);
INSERT INTO task(id, project_id, title, disposition, state, created_ms) VALUES (2, 1, 't', 'engineer', 'new', 0);
INSERT INTO workflow(id, kind, state, started_ms, updated_ms) VALUES ('w1', 'k', 's', 0, 0);
INSERT INTO run(id, task_id, agent_id, kind, harness, model, started_ms, cost_state)
    VALUES (1, 1, 1, 'k1', 'h', 'm', 0, 'unknown');
";

/// One row per foreign key: `table.column`, a statement with `@V` in that
/// column, a value that exists in the seed and one that does not.
const FOREIGN_KEYS: &[(&str, &str, &str, &str)] = &[
    // R2, R5, R6 (config)
    ("kind.harness", "INSERT INTO kind(name, harness, model) VALUES ('k2', @V, 'm')", "'claude'", "'ghost'"),
    ("kind.fallback", "INSERT INTO kind(name, harness, model, fallback) VALUES ('k2', 'claude', 'm', @V)", "'k1'", "'ghost'"),
    ("purpose.kind", "INSERT INTO purpose(name, kind) VALUES ('x', @V)", "'k1'", "'ghost'"),
    ("kind_reviewer.kind", "INSERT INTO kind_reviewer(position, kind) VALUES (1, @V)", "'k1'", "'ghost'"),
    ("profile_kind.profile", "INSERT INTO profile_kind(profile, kind) VALUES (@V, 'k1')", "'pf'", "'ghost'"),
    ("profile_kind.kind", "INSERT INTO profile_kind(profile, kind) VALUES ('pf', @V)", "'k1'", "'ghost'"),
    ("profile_kind.harness", "INSERT INTO profile_kind(profile, kind, harness) VALUES ('pf', 'k1', @V)", "'claude'", "'ghost'"),
    // R8 (agents belong to a project, a Peer has a spawner)
    ("agent.kind", "INSERT INTO agent(name, role, kind, started_ms) VALUES ('a', 'peer', @V, 0)", "'k1'", "'ghost'"),
    ("agent.project_id", "INSERT INTO agent(name, role, project_id, started_ms) VALUES ('a', 'peer', @V, 0)", "1", "999"),
    ("agent.spawner_id", "INSERT INTO agent(name, role, spawner_id, started_ms) VALUES ('a', 'peer', @V, 0)", "1", "999"),
    // R23 (proposal, verdict, task link)
    ("proposal.project_id", "INSERT INTO proposal(loop, risk, project_id, created_ms) VALUES (1, 'low', @V, 0)", "1", "999"),
    ("decision.proposal_id", "INSERT INTO decision(proposal_id, decider_role, verdict, ts_ms) VALUES (@V, 'cos', 'approve', 0)", "1", "999"),
    ("task.proposal_id", "INSERT INTO task(project_id, title, disposition, state, proposal_id, created_ms) VALUES (1, 't', 'engineer', 'new', @V, 0)", "1", "999"),
    // R8, R9, R22 (tasks)
    ("task.project_id", "INSERT INTO task(project_id, title, disposition, state, created_ms) VALUES (@V, 't', 'engineer', 'new', 0)", "1", "999"),
    ("task.kind", "INSERT INTO task(project_id, title, kind, disposition, state, created_ms) VALUES (1, 't', @V, 'engineer', 'new', 0)", "'k1'", "'ghost'"),
    ("task_dep.task_id", "INSERT INTO task_dep(task_id, parent_id) VALUES (@V, 1)", "2", "999"),
    ("task_dep.parent_id", "INSERT INTO task_dep(task_id, parent_id) VALUES (2, @V)", "1", "999"),
    // engine workflow
    ("workflow.project_id", "INSERT INTO workflow(id, kind, state, project_id, started_ms, updated_ms) VALUES ('w2', 'k', 's', @V, 0, 0)", "1", "999"),
    ("workflow.task_id", "INSERT INTO workflow(id, kind, state, task_id, started_ms, updated_ms) VALUES ('w2', 'k', 's', @V, 0, 0)", "1", "999"),
    // R19 (run capture), R13/R23 (score)
    ("run.task_id", "INSERT INTO run(task_id, agent_id, kind, harness, model, started_ms, cost_state) VALUES (@V, 1, 'k1', 'h', 'm', 0, 'unknown')", "1", "999"),
    ("run.agent_id", "INSERT INTO run(agent_id, kind, harness, model, started_ms, cost_state) VALUES (@V, 'k1', 'h', 'm', 0, 'unknown')", "1", "999"),
    ("run.kind", "INSERT INTO run(agent_id, kind, harness, model, started_ms, cost_state) VALUES (1, @V, 'h', 'm', 0, 'unknown')", "'k1'", "'ghost'"),
    ("run.workflow_id", "INSERT INTO run(agent_id, kind, workflow_id, harness, model, started_ms, cost_state) VALUES (1, 'k1', @V, 'h', 'm', 0, 'unknown')", "'w1'", "'ghost'"),
    ("run_transcript.run_id", "INSERT INTO run_transcript(run_id, seq, ts_ms, role, content) VALUES (@V, 1, 0, 'u', 'x')", "1", "999"),
    ("run_tool_call.run_id", "INSERT INTO run_tool_call(run_id, seq, ts_ms, tool) VALUES (@V, 1, 0, 't')", "1", "999"),
    ("run_artifact.run_id", "INSERT INTO run_artifact(run_id, kind) VALUES (@V, 'report')", "1", "999"),
    ("score.run_id", "INSERT INTO score(run_id, rubric, value, ts_ms) VALUES (@V, 'r', 1.0, 0)", "1", "999"),
];

fn declared_foreign_keys(conn: &Connection) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for t in tables(conn) {
        let mut stmt = conn
            .prepare(&format!("SELECT \"from\" FROM pragma_foreign_key_list('{t}')"))
            .unwrap();
        for col in stmt.query_map([], |r| r.get::<_, String>(0)).unwrap() {
            out.insert(format!("{t}.{}", col.unwrap()));
        }
    }
    out
}

#[test]
fn ontology_schema_relations() {
    let tmp = Tmp::new();
    let conn = open(&tmp.join("runs/qq.db")).unwrap();

    let on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
    assert_eq!(on, 1, "the opener turns foreign keys on");

    // R20 entities exist.
    let have = tables(&conn);
    for t in [
        "project", "task", "agent", "kind", "run", "rule", "skill", "score", "decision", "proposal",
    ] {
        assert!(have.contains(t), "missing entity table {t}");
    }

    // Every declared foreign key has a row below, and no row names a missing key.
    let expected: BTreeSet<String> = FOREIGN_KEYS.iter().map(|k| k.0.to_string()).collect();
    assert_eq!(declared_foreign_keys(&conn), expected);

    conn.execute_batch(SEED).unwrap();
    for (label, sql, valid, bogus) in FOREIGN_KEYS {
        let good = attempt(&conn, &sql.replace("@V", valid));
        assert!(good.is_ok(), "{label}: valid reference refused: {good:?}");
        let bad = attempt(&conn, &sql.replace("@V", bogus)).unwrap_err();
        assert!(
            bad.to_string().contains("FOREIGN KEY"),
            "{label}: orphan not refused by a foreign key: {bad}"
        );
    }
}

#[test]
fn db_task_dep_rejects_self_parent() {
    let tmp = Tmp::new();
    let conn = open(&tmp.join("qq.db")).unwrap();
    conn.execute_batch(SEED).unwrap();
    assert!(attempt(&conn, "INSERT INTO task_dep(task_id, parent_id) VALUES (1, 1)").is_err());
    assert!(attempt(&conn, "INSERT INTO task_dep(task_id, parent_id) VALUES (2, 1)").is_ok());
}

#[test]
fn db_agent_active_lead_unique() {
    let tmp = Tmp::new();
    let conn = open(&tmp.join("qq.db")).unwrap();
    conn.execute_batch(SEED).unwrap();
    conn.execute_batch(
        "INSERT INTO project(id, slug, created_ms) VALUES (2, 'q', 0);
         INSERT INTO agent(name, role, project_id, started_ms) VALUES ('l1', 'lead', 1, 0);",
    )
    .unwrap();
    let second = "INSERT INTO agent(name, role, project_id, started_ms) VALUES ('l2', 'lead', 1, 1)";
    assert!(attempt(&conn, second).is_err(), "second active Lead in a project");
    assert!(
        attempt(&conn, "INSERT INTO agent(name, role, project_id, started_ms) VALUES ('l2', 'lead', 2, 1)").is_ok(),
        "a Lead in another project"
    );
    assert!(
        attempt(&conn, "INSERT INTO agent(name, role, project_id, started_ms) VALUES ('p2', 'peer', 1, 1)").is_ok(),
        "Peers are not limited"
    );
    conn.execute("UPDATE agent SET ended_ms = 5 WHERE name = 'l1'", []).unwrap();
    assert!(attempt(&conn, second).is_ok(), "a new Lead after the old one ended");
}

#[test]
fn db_run_cost_state_check() {
    let tmp = Tmp::new();
    let conn = open(&tmp.join("qq.db")).unwrap();
    conn.execute_batch(SEED).unwrap();
    let run = |state: &str, cost: &str| {
        attempt(
            &conn,
            &format!(
                "INSERT INTO run(agent_id, kind, harness, model, started_ms, cost_state, cost_usd) \
                 VALUES (1, 'k1', 'h', 'm', 0, '{state}', {cost})"
            ),
        )
    };
    assert!(run("unknown", "NULL").is_ok());
    assert!(run("reported", "1.5").is_ok());
    assert!(run("unknown", "1.5").is_err(), "unknown cost must carry no number");
    assert!(run("reported", "NULL").is_err(), "reported cost must carry a number");
}

#[test]
fn db_rejects_omp_harness() {
    let tmp = Tmp::new();
    let conn = open(&tmp.join("qq.db")).unwrap();
    let ins = |name: &str| {
        attempt(&conn, &format!("INSERT INTO harness(name, driver) VALUES ('{name}', 'generic')"))
    };
    assert!(ins("opencode").is_ok());
    assert!(ins("omp").is_err());
    assert!(ins("OMP").is_err());
    // A kind cannot name a harness that is not a row.
    assert!(attempt(&conn, "INSERT INTO kind(name, harness, model) VALUES ('s', 'omp', 'm')").is_err());
}

#[test]
fn db_migrate_idempotent_beside_journal_tables() {
    let tmp = Tmp::new();
    let path = tmp.join("qq.db");

    // Journal tables made by another writer before the first open survive.
    {
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch(
            "CREATE TABLE events (workflow_id TEXT NOT NULL, seq INTEGER NOT NULL, ts_ms INTEGER NOT NULL,
                kind TEXT NOT NULL, payload_json TEXT NOT NULL, PRIMARY KEY (workflow_id, seq));
             INSERT INTO events VALUES ('w', 1, 0, 'k', '{}');",
        )
        .unwrap();
    }
    let conn = open(&path).unwrap();
    assert_eq!(user_version(&conn), latest_version());
    assert_eq!(latest_version() as usize, LADDER.len());
    conn.execute("INSERT INTO project(slug, created_ms) VALUES ('keep', 0)", []).unwrap();
    drop(conn);

    let again = open(&path).unwrap();
    assert_eq!(user_version(&again), latest_version());
    let n: i64 = again.query_row("SELECT COUNT(*) FROM project", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "reopen keeps rows");
    let ev: i64 = again.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
    assert_eq!(ev, 1, "journal rows survive the migration");

    // A fresh database gets no journal tables: the journal creates its own.
    let fresh = open(&tmp.join("fresh.db")).unwrap();
    let have = tables(&fresh);
    assert!(!have.contains("events") && !have.contains("snapshots"));
}

#[test]
fn db_migrate_refuses_newer_schema() {
    let tmp = Tmp::new();
    let path = tmp.join("qq.db");
    {
        let raw = Connection::open(&path).unwrap();
        raw.pragma_update(None, "user_version", latest_version() + 1).unwrap();
    }
    match open(&path) {
        Err(DbError::TooNew { found, known }) => {
            assert_eq!((found, known), (latest_version() + 1, latest_version()));
        }
        other => panic!("expected TooNew, got {:?}", other.map(|_| ())),
    }
    let raw = Connection::open(&path).unwrap();
    assert_eq!(user_version(&raw), latest_version() + 1);
    assert!(tables(&raw).is_empty(), "a newer database is left as it was");
}

fn exists_state(paths: &[PathBuf]) -> Vec<bool> {
    paths.iter().map(|p| p.exists()).collect()
}

#[test]
fn db_live_guard_refuses_before_fs_access() {
    // Pure core, with a fake live file.
    let tmp = Tmp::new();
    std::fs::create_dir_all(tmp.join("runs")).unwrap();
    let live = vec![tmp.join("runs/qq.db")];
    assert!(is_live_db(&tmp.join("runs/qq.db"), &live));
    assert!(is_live_db(&tmp.join("runs/../runs/qq.db"), &live), "dot-dot path");
    assert!(!is_live_db(&tmp.join("runs/other.db"), &live));
    assert!(!is_live_db(&tmp.join("qq.db"), &live));

    assert!(is_test_environment(), "cargo test must count as test mode");

    // The owner's real files: refused, and nothing is created or touched.
    let live_files = live_db_files();
    assert_eq!(live_files.len(), 2);
    let runs = live_runs_dir().unwrap();
    if runs.starts_with(std::env::temp_dir()) {
        eprintln!("skip: passwd home is under the temp dir, guard exempts it");
        return;
    }
    let mut watch: Vec<PathBuf> = vec![runs.clone()];
    for f in &live_files {
        let s = f.to_string_lossy().into_owned();
        watch.extend([f.clone(), PathBuf::from(format!("{s}-wal")), PathBuf::from(format!("{s}-shm"))]);
    }
    let before = exists_state(&watch);
    for f in &live_files {
        assert!(matches!(open(f), Err(DbError::LiveDb(_))), "open {}", f.display());
        assert!(
            matches!(OldDb::open(f), Err(OldDbError::LiveDb(_))),
            "OldDb::open {}",
            f.display()
        );
    }
    assert_eq!(exists_state(&watch), before);
}

#[test]
fn db_default_path_is_runs_qq_db() {
    assert_eq!(default_db_path().file_name().unwrap(), "qq.db");
    assert_eq!(old_db_path().file_name().unwrap(), "quaqua.db");
    assert_eq!(default_db_path().parent(), old_db_path().parent());
    // Under test mode the default never lands on a live file.
    assert!(!is_live_db(&default_db_path(), &live_db_files()));
    assert!(!is_live_db(&old_db_path(), &live_db_files()));

    let p = |s: &str| Some(PathBuf::from(s));
    let (h, r, home) = (p("/h"), p("/r"), p("/u"));
    let root = |a: &Option<PathBuf>, b: &Option<PathBuf>, c: &Option<PathBuf>| {
        resolve_workspace_root(a.as_deref(), b.as_deref(), c.as_deref())
    };
    assert_eq!(root(&h, &r, &home), Path::new("/h"));
    assert_eq!(root(&None, &r, &home), Path::new("/r"));
    assert_eq!(root(&None, &None, &home), Path::new("/u/quaqua"));
    assert_eq!(root(&None, &None, &None), Path::new("."));
}

// ---- old database -------------------------------------------------------

const OLD_SCHEMA: &str = "
CREATE TABLE threads (id TEXT PRIMARY KEY, owner TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open','closed')), last_trigger_ms INTEGER NOT NULL);
CREATE TABLE triggers (id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('herdr','mail','watchdog','worker','github-ci')),
    kind TEXT NOT NULL, actor TEXT NOT NULL, to_name TEXT,
    thread_id TEXT NOT NULL REFERENCES threads(id),
    summary TEXT NOT NULL CHECK (length(summary) <= 80), ref TEXT, payload_json TEXT,
    dedupe_key TEXT UNIQUE, farm TEXT NOT NULL);
CREATE TABLE subscriptions (thread_id TEXT NOT NULL REFERENCES threads(id),
    consumer TEXT NOT NULL, until_ms INTEGER NOT NULL, PRIMARY KEY (thread_id, consumer));
CREATE TABLE cursors (consumer TEXT PRIMARY KEY, trigger_id INTEGER NOT NULL);
CREATE TABLE trigger_acks (trigger_id INTEGER PRIMARY KEY REFERENCES triggers(id),
    acked_ms INTEGER NOT NULL, acked_by TEXT);
CREATE TABLE source_state (source TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE events (workflow_id TEXT NOT NULL, seq INTEGER NOT NULL, ts_ms INTEGER NOT NULL,
    kind TEXT NOT NULL, payload_json TEXT NOT NULL, PRIMARY KEY (workflow_id, seq));
CREATE TABLE snapshots (workflow_id TEXT NOT NULL, seq INTEGER NOT NULL, state_json TEXT NOT NULL,
    PRIMARY KEY (workflow_id, seq));

INSERT INTO events VALUES ('wf-1', 1, 1000, 'spawned',
    '{\"name\":\"sude-1\",\"kind\":\"sude\",\"harness\":\"omp\"}');
INSERT INTO events VALUES ('wf-1', 2, 2000, 'verdict',
    '{\"branch\":\"quaqua/feat-1\",\"sha\":\"abc123456789\",\"level\":\"unit-test-verified\",
      \"verifier\":\"sude-worker\",\"builder_kind\":\"sude\",\"harness\":\"omp\"}');
INSERT INTO snapshots VALUES ('wf-1', 2, '{\"harness\":\"omp\"}');
INSERT INTO threads VALUES ('t1', 'cos', 'open', 3000);
INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm)
    VALUES (3000, 'herdr', 'blocked', 'sude-1', NULL, 't1', 'sude-1 blocked', 'w1:p1',
            '{\"harness\":\"omp\"}', 'k1', 'quaqua');
INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm)
    VALUES (3100, 'github-ci', 'failed', 'ci', 'cos', 't1', 'ci failed', NULL, NULL, 'k2', 'quaqua');
INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm)
    VALUES (3200, 'mail', 'question', 'sude-1', 'cos', 't1', 'need a call', NULL,
            '{\"body\":\"run on omp?\"}', 'mail:m1', 'quaqua');
INSERT INTO subscriptions VALUES ('t1', 'cos', 9000);
INSERT INTO cursors VALUES ('cos', 2);
INSERT INTO trigger_acks VALUES (3, 3300, 'cos');
INSERT INTO source_state VALUES ('github-ci', 'etag-1');
";

fn old_fixture(path: &Path, wal: bool) {
    let c = Connection::open(path).unwrap();
    if wal {
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
    }
    c.execute_batch(OLD_SCHEMA).unwrap();
}

fn fingerprint(path: &Path) -> (Vec<u8>, std::time::SystemTime) {
    (std::fs::read(path).unwrap(), std::fs::metadata(path).unwrap().modified().unwrap())
}

fn listing(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn legacy_omp_sude_rows_read() {
    let tmp = Tmp::new();
    let path = tmp.join("quaqua.db");
    old_fixture(&path, false);
    let before = fingerprint(&path);
    let files = listing(&tmp.0);

    let db = OldDb::open(&path).unwrap();

    let events = db.events().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].payload().unwrap()["harness"], "omp");
    assert_eq!(events[0].payload().unwrap()["kind"], "sude");
    assert_eq!(events[1].kind, "verdict");
    assert_eq!(events[1].payload().unwrap()["builder_kind"], "sude");
    assert_eq!(events[1].payload().unwrap()["harness"], "omp");
    assert_eq!(db.snapshots().unwrap()[0].state_json, "{\"harness\":\"omp\"}");

    let triggers = db.triggers().unwrap();
    let sources: Vec<&str> = triggers.iter().map(|t| t.source.as_str()).collect();
    assert_eq!(sources, ["herdr", "github-ci", "mail"]);
    assert_eq!(triggers[0].actor, "sude-1");
    assert_eq!(triggers[0].payload().unwrap().unwrap()["harness"], "omp");
    assert_eq!(triggers[1].to_name.as_deref(), Some("cos"));
    assert!(triggers[1].payload().is_none());
    assert_eq!(triggers[2].reference, None);
    assert_eq!(triggers[0].reference.as_deref(), Some("w1:p1"));

    assert_eq!(db.threads().unwrap()[0].owner, "cos");
    assert_eq!(db.subscriptions().unwrap()[0].until_ms, 9000);
    assert_eq!(db.cursors().unwrap()[0].trigger_id, 2);
    assert_eq!(db.acks().unwrap()[0].acked_by.as_deref(), Some("cos"));
    assert_eq!(db.source_state().unwrap()[0].value, "etag-1");
    drop(db);

    assert_eq!(fingerprint(&path), before, "the old database is not written");
    assert_eq!(listing(&tmp.0), files, "no file is created beside it");
}

#[test]
fn legacy_omp_sude_rows_read_wal_fixture_bytes_unchanged() {
    let tmp = Tmp::new();
    let path = tmp.join("quaqua.db");
    old_fixture(&path, true);
    let before = std::fs::read(&path).unwrap();
    let db = OldDb::open(&path).unwrap();
    assert_eq!(db.events().unwrap().len(), 2);
    assert_eq!(db.triggers().unwrap().len(), 3);
    drop(db);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn legacy_omp_sude_rows_read_runs_dir() {
    let tmp = Tmp::new();
    let runs = tmp.join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    for (name, body) in [
        ("sude-1.owner", "cos\n"),
        ("sude-1.pipeline", "t32 sude omp\n"),
        ("sude-1.repo", "/r\n"),
        (".sude-1.state", "working 1000\n"),
        ("omp-w.grid", "tab-3\n"),
        // not sidecars: never read
        ("sude-1.md", "report"),
        (".sude-1.settings.json", "{}"),
        ("sude-1.state", "visible state is not a sidecar"),
        ("notes.txt", "x"),
    ] {
        std::fs::write(runs.join(name), body).unwrap();
    }
    std::fs::create_dir_all(runs.join("sub.owner")).unwrap();

    let rows = read_runs_dir(&runs).unwrap();
    let got: Vec<(&str, &str, &str)> =
        rows.iter().map(|r| (r.worker.as_str(), r.file.as_str(), r.content.as_str())).collect();
    assert_eq!(
        got,
        [
            ("omp-w", "grid", "tab-3\n"),
            ("sude-1", "owner", "cos\n"),
            ("sude-1", "pipeline", "t32 sude omp\n"),
            ("sude-1", "repo", "/r\n"),
            ("sude-1", "state", "working 1000\n"),
        ]
    );
}

#[test]
fn legacy_omp_sude_rows_read_refusals() {
    let tmp = Tmp::new();

    // Missing: an error, and no file appears.
    let missing = tmp.join("none.db");
    assert!(matches!(OldDb::open(&missing), Err(OldDbError::NotFound(_))));
    assert!(!missing.exists());

    // Pre-bus schema is not read.
    let pre = tmp.join("pre.db");
    Connection::open(&pre)
        .unwrap()
        .execute_batch("CREATE TABLE farm_events (seq INTEGER PRIMARY KEY); CREATE TABLE mail (id INTEGER PRIMARY KEY);")
        .unwrap();
    assert!(matches!(OldDb::open(&pre), Err(OldDbError::PreBus)));

    // A journal-only file reads; bus tables are simply empty.
    let jo = tmp.join("journal.db");
    Connection::open(&jo)
        .unwrap()
        .execute_batch(
            "CREATE TABLE events (workflow_id TEXT, seq INTEGER, ts_ms INTEGER, kind TEXT, payload_json TEXT);
             INSERT INTO events VALUES ('w', 1, 0, 'k', '{}');",
        )
        .unwrap();
    let db = OldDb::open(&jo).unwrap();
    assert_eq!(db.events().unwrap().len(), 1);
    assert!(db.triggers().unwrap().is_empty());

    // Writing through the reader is impossible.
    let path = tmp.join("quaqua.db");
    old_fixture(&path, false);
    let db = OldDb::open(&path).unwrap();
    assert!(db.events().is_ok());
}
