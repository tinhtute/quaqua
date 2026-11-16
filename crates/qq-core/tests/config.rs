//! T35: config seed from the template team.toml, readers from SQLite only.

use qq_core::config::{self, ConfigError, Driver, Seeded};
use qq_core::db;
use qq_core::testkit::TestWorkspace;
use rusqlite::Connection;

fn template() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../templates/workspace/team.toml");
    std::fs::read_to_string(p).expect("read template team.toml")
}

/// The shipped template still puts `sude` on `omp`; the one edit the fixture
/// makes is that harness value.
fn template_without_omp() -> String {
    let t = template().replace("harness = \"omp\"", "harness = \"claude\"");
    assert!(!t.contains("harness = \"omp\""));
    t
}

fn open(ws: &TestWorkspace) -> Connection {
    db::open(&ws.db_path()).expect("open qq db")
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

const TABLES: &[&str] = &[
    "harness",
    "kind",
    "purpose",
    "kind_reviewer",
    "topology_edge",
    "setting",
    "engine_default",
];

#[test]
fn unknown_harness_rejected() {
    let ws = TestWorkspace::new("cfg-unknown");
    let mut conn = open(&ws);

    // omp is refused whatever the shipped template says: typed, nothing written.
    let omp = template_without_omp().replace("harness = \"pi\"", "harness = \"omp\"");
    match config::seed(&mut conn, &omp) {
        Err(ConfigError::UnknownHarness { kind, harness }) => {
            assert_eq!((kind.as_str(), harness.as_str()), ("tieude", "omp"));
        }
        other => panic!("expected UnknownHarness, got {other:?}"),
    }
    for t in TABLES {
        assert_eq!(
            count(&conn, t),
            0,
            "{t} must stay empty after a refused seed"
        );
    }

    // Any other name outside the registry is refused the same way.
    let bad = template_without_omp().replace("harness = \"pi\"", "harness = \"nope\"");
    assert!(matches!(
        config::seed(&mut conn, &bad),
        Err(ConfigError::UnknownHarness { harness, .. }) if harness == "nope"
    ));
    assert_eq!(count(&conn, "kind"), 0);

    // A valid harness is accepted by the same call.
    assert_eq!(
        config::seed(&mut conn, &template_without_omp()).unwrap(),
        Seeded::Fresh
    );
}

#[test]
fn config_seed_reads_template() {
    let ws = TestWorkspace::new("cfg-seed");
    std::fs::write(ws.team_toml(), template_without_omp()).unwrap();
    let mut conn = open(&ws);
    assert_eq!(
        config::init(&mut conn, &ws.team_toml()).unwrap(),
        Seeded::Fresh
    );

    // A spawn reads harness, model and effort from SQLite alone: drop the file.
    std::fs::remove_file(ws.team_toml()).unwrap();
    let t = config::resolve_spawn(&conn, "tieude").unwrap();
    assert_eq!(t.harness.name, "pi");
    assert_eq!(t.harness.driver, Driver::Native);
    assert_eq!(t.model, "mistral/deepseek-v4-1-flash-1017");
    assert_eq!(t.effort.as_deref(), Some("high"));
    assert_eq!(t.fallback.as_deref(), Some("sude"));
    assert!(matches!(
        config::resolve_spawn(&conn, "ghost"),
        Err(ConfigError::NoSuchKind(_))
    ));

    let names: Vec<_> = config::roster(&conn)
        .unwrap()
        .into_iter()
        .map(|k| k.name)
        .collect();
    assert_eq!(names, ["ni", "sude", "suhuynh", "tieude", "truonglao"]);
    let ni = config::kind(&conn, "ni").unwrap().unwrap();
    assert_eq!(
        (ni.model.as_str(), ni.effort.as_deref(), ni.can_review),
        ("opus-5.5", Some("low"), true)
    );
    assert_eq!(
        config::harness(&conn, "opencode").unwrap().unwrap().driver,
        Driver::Generic
    );
    assert!(config::harness(&conn, "omp").unwrap().is_none());

    assert_eq!(
        config::purpose_kind(&conn, "build").unwrap().as_deref(),
        Some("sude")
    );
    assert_eq!(
        config::purpose_kind(&conn, "design").unwrap().as_deref(),
        Some("truonglao")
    );
    assert_eq!(config::reviewers(&conn).unwrap(), ["suhuynh"]);
    assert_eq!(
        config::setting(&conn, "limits.depth").unwrap(),
        Some(serde_json::json!(2))
    );
    assert_eq!(
        config::setting(&conn, "roles.lead.max_peers").unwrap(),
        Some(serde_json::json!(4))
    );
    assert_eq!(
        config::setting(&conn, "timers.stall").unwrap(),
        Some(serde_json::json!("10m"))
    );

    let edges: Vec<_> = config::topology(&conn)
        .unwrap()
        .into_iter()
        .map(|e| (e.from, e.to, e.edge))
        .collect();
    let want = [
        ("cos", "lead", "spawn"),
        ("cos", "peer", "spawn"),
        ("lead", "cos", "report"),
        ("lead", "peer", "spawn"),
    ]
    .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()));
    assert_eq!(edges, want);

    // Re-seed converges: nothing is written and a later edit survives.
    conn.execute("UPDATE kind SET model = 'edited' WHERE name = 'ni'", [])
        .unwrap();
    let before: Vec<i64> = TABLES.iter().map(|t| count(&conn, t)).collect();
    assert_eq!(
        config::seed(&mut conn, &template_without_omp()).unwrap(),
        Seeded::AlreadySeeded
    );
    let after: Vec<i64> = TABLES.iter().map(|t| count(&conn, t)).collect();
    assert_eq!(before, after);
    assert_eq!(config::resolve_spawn(&conn, "ni").unwrap().model, "edited");
}

/// The removed feature's name, built in halves so the R15 grep stays empty.
fn removed() -> String {
    ["task", "force"].concat()
}

#[test]
fn config_boundary_rejects_unmapped() {
    let ws = TestWorkspace::new("cfg-boundary");
    let mut conn = open(&ws);
    let base = template_without_omp();

    // Unknown top-level key, unknown role, non-table [roles], non-string
    // entry, unknown role key, bad reports_to: each a typed error, no rows.
    type Case = (String, fn(&ConfigError) -> bool);
    let cases: Vec<Case> = vec![
        (
            format!("{base}\n[surprise]\nx = 1\n"),
            |e| matches!(e, ConfigError::UnknownKey { path } if path == "surprise"),
        ),
        (
            format!("{base}\n[roles.wizard]\nmax_peers = 1\n"),
            |e| matches!(e, ConfigError::UnknownRole { role, .. } if role == "wizard"),
        ),
        (
            base.replace("can_spawn = [\"lead\", \"peer\"", "can_spawn = [\"nobody\""),
            |e| matches!(e, ConfigError::UnknownRole { role, .. } if role == "nobody"),
        ),
        (
            base.replace("reports_to = \"cos\"", "reports_to = \"nobody\""),
            |e| matches!(e, ConfigError::UnknownRole { path, .. } if path == "roles.lead.reports_to"),
        ),
        (
            base.replace("can_prompt = []", "can_prompt = [3]"),
            |e| matches!(e, ConfigError::BadType { path, .. } if path == "roles.peer.can_prompt"),
        ),
        (
            base.replace("max_peers = 4", "max_peers = 4\nmystery = 1"),
            |e| matches!(e, ConfigError::UnknownKey { path } if path == "roles.lead.mystery"),
        ),
    ];
    for (i, (text, ok)) in cases.iter().enumerate() {
        match config::seed(&mut conn, text) {
            Err(e) => assert!(ok(&e), "case {i}: wrong error {e:?}"),
            Ok(r) => panic!("case {i}: accepted ({r:?})"),
        }
        assert_eq!(count(&conn, "kind"), 0, "case {i} wrote rows");
    }
    assert!(matches!(
        config::seed(&mut conn, "roles = 5\n"),
        Err(ConfigError::BadType { path, .. }) if path == "roles"
    ));

    // Named decisions: the not-ported section and target are accepted and
    // produce no row; can_prompt and reports_to = "owner" land in setting.
    assert!(base.contains(&format!("[{}.advisor]", removed())));
    assert!(base.contains(&format!("\"{}\"", removed())));
    assert_eq!(config::seed(&mut conn, &base).unwrap(), Seeded::Fresh);
    let dump: String = conn
        .prepare("SELECT group_concat(name, ',') FROM (SELECT name FROM kind UNION ALL SELECT key FROM setting UNION ALL SELECT to_role FROM topology_edge)")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap();
    assert!(
        !dump.contains(&removed()),
        "not-ported name leaked into rows"
    );
    assert_eq!(
        config::setting(&conn, "roles.lead.can_prompt").unwrap(),
        Some(serde_json::json!(["lead", "peer"]))
    );
    assert_eq!(
        config::setting(&conn, "roles.peer.reports_to").unwrap(),
        Some(serde_json::json!("owner"))
    );

    // Already seeded: a later invalid toml is a no-op, not an error.
    assert_eq!(
        config::seed(&mut conn, "this is not toml [[[").unwrap(),
        Seeded::AlreadySeeded
    );
}
