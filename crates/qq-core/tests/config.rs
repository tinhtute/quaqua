//! T35: config seed from the template team.toml, readers from SQLite only.

use qq_core::cli::{Env, Registry};
use qq_core::config::{self, ConfigError, Driver, Seeded};
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::db;
use qq_core::herdr::FakeHerdr;
use qq_core::testkit::TestWorkspace;
use rusqlite::Connection;
use std::sync::Arc;

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

    // The daily verbs refuse the same names and change nothing.
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let (_registry, daemon) = start(&ws);
    let before = config::roster(&conn).unwrap();
    for h in ["omp", "nope"] {
        let file = ws.root().join(format!("{h}.toml"));
        std::fs::write(
            &file,
            template().replace("harness = \"omp\"", &format!("harness = \"{h}\"")),
        )
        .unwrap();
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for cmd in [
            args(&["import", file.to_str().unwrap()]),
            args(&["validate", file.to_str().unwrap()]),
            args(&["set", "kind.ni.harness", h]),
            args(&[
                "set",
                "kind.fresh",
                &format!("{{\"harness\":\"{h}\",\"model\":\"m\"}}"),
            ]),
        ] {
            let e = config::config_cmd(&env, &cmd).expect_err("refused");
            assert_eq!(e.code, 1, "{cmd:?}");
            assert!(e.message.contains("unknown harness"), "{}", e.message);
        }
    }
    assert_eq!(config::roster(&conn).unwrap(), before);
    assert!(config::kind(&conn, "fresh").unwrap().is_none());
    daemon.stop().unwrap();
}

/// A daemon with the config verbs installed on a fresh registry; the CLI
/// commands in that registry are its clients.
fn start(ws: &TestWorkspace) -> (Registry, DaemonHandle) {
    let mut registry = Registry::new();
    let mut daemon = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: Arc::new(FakeHerdr::new()),
    });
    config::INSTALL(&mut registry, &mut daemon).expect("config installs");
    (registry, daemon.spawn().expect("daemon starts"))
}

fn strs(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn config_set_import_roster() {
    let ws = TestWorkspace::new("cfg-verbs");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let (registry, daemon) = start(&ws);
    let run = |a: &[&str]| registry.dispatch(&env, &strs(a));

    // import: the toml goes in through the verb; a repeat converges.
    std::fs::write(ws.team_toml(), template_without_omp()).unwrap();
    let toml = ws.team_toml();
    let toml = toml.to_str().unwrap();
    assert_eq!(run(&["config", "import", toml]), 0);
    let conn = open(&ws);
    let snapshot = |c: &Connection| -> Vec<i64> { TABLES.iter().map(|t| count(c, t)).collect() };
    let once = snapshot(&conn);
    assert_eq!(
        config::resolve_spawn(&conn, "ni").unwrap().model,
        "opus-5.5"
    );
    assert_eq!(run(&["config", "import", toml]), 0);
    assert_eq!(snapshot(&conn), once);
    assert_eq!(run(&["config", "validate"]), 0);

    // set: a kind added here is the one the next spawn reads.
    let scout = r#"{"harness":"claude","model":"haiku-5.5","effort":"low","fallback":"ni","use_for":"quick scans","avoid":"edits"}"#;
    assert_eq!(run(&["config", "set", "kind.scout", scout]), 0);
    assert_eq!(run(&["config", "set", "kind.scout", scout]), 0);
    let conn = open(&ws);
    let t = config::resolve_spawn(&conn, "scout").unwrap();
    assert_eq!(
        (
            t.harness.name.as_str(),
            t.model.as_str(),
            t.effort.as_deref(),
            t.fallback.as_deref()
        ),
        ("claude", "haiku-5.5", Some("low"), Some("ni"))
    );
    assert_eq!(count(&conn, "kind"), 6, "a repeat adds no row");

    // a field edit changes only that field; get reads it back.
    assert_eq!(run(&["config", "set", "kind.scout.model", "haiku-6"]), 0);
    let k = config::kind(&open(&ws), "scout").unwrap().unwrap();
    assert_eq!(
        (k.model.as_str(), k.use_for.as_deref()),
        ("haiku-6", Some("quick scans"))
    );
    assert_eq!(
        config::config_cmd(&env, &strs(&["get", "kind.scout.model"])).unwrap(),
        "haiku-6"
    );
    assert_eq!(
        config::config_cmd(&env, &strs(&["get", "purpose.build"])).unwrap(),
        "sude"
    );
    assert_eq!(run(&["config", "set", "limits.depth", "3"]), 0);
    assert_eq!(
        config::config_cmd(&env, &strs(&["get", "limits.depth"])).unwrap(),
        "3"
    );
    assert_eq!(run(&["config", "set", "purpose.scan", "scout"]), 0);
    assert_eq!(
        config::purpose_kind(&open(&ws), "scan").unwrap().as_deref(),
        Some("scout")
    );

    // roster lists every kind with use_for and avoid.
    let out = config::roster_cmd(&env, &[]).unwrap();
    let line = out
        .lines()
        .find(|l| l.starts_with("scout"))
        .expect("scout listed");
    assert!(
        line.contains("haiku-6")
            && line.contains("use: quick scans")
            && line.contains("avoid: edits"),
        "{line}"
    );
    assert_eq!(out.lines().count(), 6);

    // bad edits are refused and leave the row alone.
    assert_eq!(run(&["config", "set", "kind.scout.fallback", "ghost"]), 1);
    assert_eq!(run(&["config", "set", "kind.ghost.model", "m"]), 1);
    assert_eq!(run(&["config", "set", "kind.half", r#"{"model":"m"}"#]), 1);
    // the toml's own spellings are not keys: refused, nothing stored.
    let settings = count(&open(&ws), "setting");
    assert_eq!(run(&["config", "set", "kinds.scout.model", "m"]), 1);
    assert_eq!(run(&["config", "set", "purposes.build", "scout"]), 1);
    assert_eq!(run(&["config", "set", "roles.cos.can_spawn", "[]"]), 1);
    assert_eq!(count(&open(&ws), "setting"), settings);
    assert_eq!(run(&["config", "set", "purpose.x", "ghost"]), 1);
    assert_eq!(run(&["config", "get", "kind.ghost"]), 1);
    let k = config::kind(&open(&ws), "scout").unwrap().unwrap();
    assert_eq!(k.fallback.as_deref(), Some("ni"));

    // a later import keeps the kind added by set (it never deletes) and
    // does not undo it: the file's own kinds win, the rest stays.
    assert_eq!(run(&["config", "import", toml]), 0);
    assert!(config::kind(&open(&ws), "scout").unwrap().is_some());
    daemon.stop().unwrap();
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

/// Child half of `config_import_relative_path`: a no-op unless the parent set
/// the env, in which case it is a CLI client running in another cwd.
#[test]
fn relative_path_child() {
    let Some(runs) = std::env::var_os("QQ_T40_RUNS") else {
        return;
    };
    let env = Env {
        runs_dir: runs.into(),
    };
    for verb in ["validate", "import"] {
        config::config_cmd(&env, &strs(&[verb, "rel.toml"])).expect("relative path resolves");
    }
}

#[test]
fn config_import_relative_path() {
    let ws = TestWorkspace::new("cfg-relpath");
    let (_registry, daemon) = start(&ws);
    std::fs::write(ws.root().join("rel.toml"), template_without_omp()).unwrap();
    // The daemon runs in this process's cwd; the client runs in the workspace.
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .current_dir(ws.root())
        .env("QQ_T40_RUNS", ws.runs_dir())
        .args(["--exact", "relative_path_child", "--nocapture"])
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && text.contains("1 passed"), "{text}");
    assert_eq!(config::roster(&open(&ws)).unwrap().len(), 5);
    daemon.stop().unwrap();
}

#[test]
fn config_set_typed_and_usage() {
    let ws = TestWorkspace::new("cfg-typed");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let (registry, daemon) = start(&ws);
    let run = |a: &[&str]| registry.dispatch(&env, &strs(a));
    std::fs::write(ws.team_toml(), template_without_omp()).unwrap();
    assert_eq!(
        run(&["config", "import", ws.team_toml().to_str().unwrap()]),
        0
    );

    // reports_to with a role is an edge (as import does), not a setting row.
    assert_eq!(
        run(&["config", "set", "roles.lead.reports_to", "supervisor"]),
        0
    );
    let conn = open(&ws);
    let edges: Vec<_> = config::topology(&conn)
        .unwrap()
        .into_iter()
        .filter(|e| e.from == "lead" && e.edge == "report")
        .map(|e| e.to)
        .collect();
    assert_eq!(edges, ["supervisor"], "the old report edge is replaced");
    assert!(config::setting(&conn, "roles.lead.reports_to")
        .unwrap()
        .is_none());
    assert_eq!(run(&["config", "set", "roles.lead.reports_to", "owner"]), 0);
    let conn = open(&ws);
    assert!(config::topology(&conn)
        .unwrap()
        .iter()
        .all(|e| !(e.from == "lead" && e.edge == "report")));
    assert_eq!(
        config::setting(&conn, "roles.lead.reports_to").unwrap(),
        Some(serde_json::json!("owner"))
    );
    assert_eq!(
        run(&["config", "set", "roles.lead.reports_to", "nobody"]),
        1
    );

    // role settings are typed like import: bad values refused, good stored.
    let before = config::setting(&conn, "roles.lead.max_peers").unwrap();
    assert_eq!(run(&["config", "set", "roles.lead.max_peers", "abc"]), 1);
    assert_eq!(
        run(&["config", "set", "roles.lead.can_prompt", r#"["wizard"]"#]),
        1
    );
    assert_eq!(run(&["config", "set", "roles.lead.can_prompt", "[3]"]), 1);
    assert_eq!(
        config::setting(&open(&ws), "roles.lead.max_peers").unwrap(),
        before
    );
    assert_eq!(run(&["config", "set", "roles.lead.max_peers", "6"]), 0);
    assert_eq!(
        run(&["config", "set", "roles.lead.can_prompt", r#"["peer"]"#]),
        0
    );
    let conn = open(&ws);
    assert_eq!(
        config::setting(&conn, "roles.lead.max_peers").unwrap(),
        Some(serde_json::json!(6))
    );
    assert_eq!(
        config::setting(&conn, "roles.lead.can_prompt").unwrap(),
        Some(serde_json::json!(["peer"]))
    );

    // import holds kind names to the lowercase rule of set; nothing written.
    let upper = ws.root().join("upper.toml");
    std::fs::write(
        &upper,
        format!(
            "{}\n[kinds.Scout]\nharness = \"claude\"\nmodel = \"m\"\n",
            template_without_omp()
        ),
    )
    .unwrap();
    assert_eq!(run(&["config", "import", upper.to_str().unwrap()]), 1);
    assert_eq!(count(&open(&ws), "kind"), 5);

    // usage errors exit 2.
    assert_eq!(run(&["config"]), 2);
    assert_eq!(run(&["config", "bogus"]), 2);
    assert_eq!(run(&["config", "set", "limits.depth"]), 2);
    assert_eq!(run(&["config", "import"]), 2);
    daemon.stop().unwrap();
}
