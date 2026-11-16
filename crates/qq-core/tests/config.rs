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

    // The shipped template names omp: refused, typed, and nothing written.
    match config::seed(&mut conn, &template()) {
        Err(ConfigError::UnknownHarness { kind, harness }) => {
            assert_eq!((kind.as_str(), harness.as_str()), ("sude", "omp"));
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
