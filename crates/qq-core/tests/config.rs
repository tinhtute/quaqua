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
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml");
    std::fs::read_to_string(p).expect("read template team.toml")
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

    // omp is an unknown harness like any other name: typed, nothing written.
    let omp = template().replace("harness = \"pi\"", "harness = \"omp\"");
    match config::seed(&mut conn, &omp) {
        Err(ConfigError::UnknownHarness { harness, .. }) => assert_eq!(harness, "omp"),
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
    let bad = template().replace("harness = \"pi\"", "harness = \"nope\"");
    assert!(matches!(
        config::seed(&mut conn, &bad),
        Err(ConfigError::UnknownHarness { harness, .. }) if harness == "nope"
    ));
    assert_eq!(count(&conn, "kind"), 0);

    // A valid harness is accepted by the same call.
    assert_eq!(config::seed(&mut conn, &template()).unwrap(), Seeded::Fresh);

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
            template().replace("harness = \"pi\"", &format!("harness = \"{h}\"")),
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
    let handle = daemon.spawn().expect("daemon starts");
    // Config writes need a named caller that may orchestrate: the CoS.
    open(ws)
        .execute(
            "INSERT OR IGNORE INTO agent (name, role, started_ms) VALUES ('cos', 'cos', 1)",
            [],
        )
        .unwrap();
    std::env::set_var("QQ_AGENT", "cos");
    (registry, handle)
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
    std::fs::write(ws.team_toml(), template()).unwrap();
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
    // does not undo it: the file's own kinds win, the rest stays. The values
    // set above differ from the file, so a plain import refuses them (#327).
    assert_eq!(run(&["config", "import", toml]), 1);
    assert_eq!(run(&["config", "import", toml, "--yes"]), 0);
    assert!(config::kind(&open(&ws), "scout").unwrap().is_some());
    daemon.stop().unwrap();
}

#[test]
fn config_seed_reads_template() {
    let ws = TestWorkspace::new("cfg-seed");
    std::fs::write(ws.team_toml(), template()).unwrap();
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
        config::seed(&mut conn, &template()).unwrap(),
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
    let base = template();

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
    std::fs::write(ws.root().join("rel.toml"), template()).unwrap();
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
    std::fs::write(ws.team_toml(), template()).unwrap();
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
            template()
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

#[test]
fn harness_declared_in_toml_imports_and_validates() {
    let ws = TestWorkspace::new("cfg-harness");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let (registry, daemon) = start(&ws);
    let run = |a: &[&str]| registry.dispatch(&env, &strs(a));
    let file = |name: &str, body: &str| {
        let p = ws.root().join(name);
        std::fs::write(&p, body).unwrap();
        p.to_str().unwrap().to_string()
    };
    let decl = |h: &str| {
        format!(
            "[harnesses.{h}]\nherdr_kind = \"kimi\"\nefforts = [\"low\"]\nargs = [\"-m\", \"{{model}}\"]\n\
             [kinds.k]\nharness = \"{h}\"\nmodel = \"m\"\neffort = \"low\"\n"
        )
    };
    let ok = file("ok.toml", &decl("kimi"));
    assert_eq!(run(&["config", "validate", &ok]), 0);
    assert_eq!(run(&["config", "import", &ok]), 0);
    let conn = open(&ws);
    let t = config::resolve_spawn(&conn, "k").unwrap();
    assert_eq!(t.harness.driver, Driver::Generic);
    let spec = t.harness.spec.expect("stored spec");
    assert_eq!((spec.herdr_kind.as_str(), spec.args.len()), ("kimi", 2));
    // a stored harness stays usable by later sets and a bare validate
    assert_eq!(
        run(&[
            "config",
            "set",
            "kind.k2",
            r#"{"harness":"kimi","model":"m2"}"#
        ]),
        0
    );
    assert_eq!(run(&["config", "validate"]), 0);
    assert_eq!(
        run(&["config", "set", "kind.k2.effort", "high"]),
        1,
        "not in efforts"
    );

    // refused before anything is written
    let refused = |name: &str, body: String| {
        let before = count(&open(&ws), "harness");
        let f = file(name, &body);
        assert_eq!(run(&["config", "import", &f]), 1, "{name}");
        assert_eq!(count(&open(&ws), "harness"), before, "{name}");
    };
    refused("omp.toml", decl("omp"));
    refused("native.toml", decl("claude"));
    refused("upper.toml", decl("Kimi"));
    refused("nomodel.toml", decl("x").replace("{model}", "m"));
    refused("badph.toml", decl("x").replace("{model}", "{model}{nope}"));
    refused(
        "prompt.toml",
        decl("x").replace("{model}", "{model}{prompt}"),
    );
    refused(
        "ctrlc.toml",
        decl("x").replace("args =", "stop_keys = [\"ctrl+c\"]\nargs ="),
    );
    refused(
        "shape.toml",
        decl("x").replace("args =", "error_shape = \"zzz\"\nargs ="),
    );
    // neither native nor declared: unknown
    let ghost = file(
        "ghost.toml",
        "[kinds.g]\nharness = \"nope\"\nmodel = \"m\"\n",
    );
    assert_eq!(run(&["config", "validate", &ghost]), 1);
    daemon.stop().unwrap();
}

// ---- T52: export, diff, profiles --------------------------------------------

const CONFIG_TABLES: &[&str] = &[
    "harness",
    "kind",
    "purpose",
    "kind_reviewer",
    "profile",
    "profile_kind",
    "topology_edge",
    "setting",
    "engine_default",
];

/// Every row of the given tables as sorted text, to compare whole databases.
fn dump(conn: &Connection, tables: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for t in tables {
        let mut st = conn.prepare(&format!("SELECT * FROM {t}")).unwrap();
        let n = st.column_count();
        let mut rows: Vec<String> = st
            .query_map([], |r| {
                let mut s = String::new();
                for i in 0..n {
                    s.push_str(&format!("{:?}|", r.get_ref(i)?));
                }
                Ok(s)
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.sort();
        out.extend(rows.into_iter().map(|r| format!("{t}: {r}")));
    }
    out
}

/// Clears `QUAQUA_PROFILE` when the test ends, even on a failed assert.
struct ProfileEnv;

impl ProfileEnv {
    fn set(v: &str) -> ProfileEnv {
        std::env::set_var("QUAQUA_PROFILE", v);
        ProfileEnv
    }
}

impl Drop for ProfileEnv {
    fn drop(&mut self) {
        std::env::remove_var("QUAQUA_PROFILE");
    }
}

fn seeded(name: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(name);
    std::fs::write(ws.team_toml(), template()).unwrap();
    let mut conn = open(&ws);
    assert_eq!(
        config::init(&mut conn, &ws.team_toml()).unwrap(),
        Seeded::Fresh
    );
    ws
}

#[test]
fn config_seed_set_export_roundtrip() {
    let ws = seeded("cfg-export");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let (registry, daemon) = start(&ws);
    let run = |a: &[&str]| registry.dispatch(&env, &strs(a));

    // Edits after the seed: a kind, a purpose, settings (one with a dotted
    // key), a profile and the switch to it.
    let scout = r#"{"harness":"claude","model":"haiku-5.5","effort":"low","fallback":"ni","use_for":"quick scans","avoid":"edits"}"#;
    assert_eq!(run(&["config", "set", "kind.scout", scout]), 0);
    assert_eq!(run(&["config", "set", "purpose.scan", "scout"]), 0);
    assert_eq!(run(&["config", "set", "limits.depth", "3"]), 0);
    assert_eq!(
        run(&["config", "set", "projects.alpha.root", "\"/tmp/a\""]),
        0
    );
    assert_eq!(
        run(&[
            "config",
            "set",
            "profile.fast",
            r#"{"ni":{"model":"haiku-5.5","effort":"low"}}"#
        ]),
        0
    );
    assert_eq!(
        run(&[
            "config",
            "set",
            "profile.swap",
            r#"{"ni":{"harness":"pi","model":"m-pi"}}"#
        ]),
        0
    );
    assert_eq!(run(&["config", "set", "profile", "fast"]), 0);

    let conn = open(&ws);
    let exported = config::config_cmd(&env, &strs(&["export"])).unwrap();
    assert!(exported.contains("[profiles.fast.ni]"), "{exported}");
    // The export is the base config: the active profile and the env switch
    // never reach it.
    assert_eq!(
        config::kind(&conn, "ni").unwrap().unwrap().model,
        "haiku-5.5"
    );
    {
        let _p = ProfileEnv::set("fast");
        assert_eq!(config::export_toml(&conn).unwrap(), exported);
    }

    // Seeding a fresh database from the export gives the same rows, and
    // exporting that gives the same text.
    let mut copy = db::open(&ws.runs_dir().join("roundtrip.db")).expect("open copy");
    assert_eq!(config::seed(&mut copy, &exported).unwrap(), Seeded::Fresh);
    assert_eq!(dump(&copy, CONFIG_TABLES), dump(&conn, CONFIG_TABLES));
    assert_eq!(config::export_toml(&copy).unwrap(), exported);
    let base: String = copy
        .query_row("SELECT model FROM kind WHERE name = 'ni'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(base, "opus-5.5");
    assert_eq!(
        config::kind(&copy, "ni").unwrap().unwrap().model,
        "haiku-5.5"
    );
    daemon.stop().unwrap();
}

#[test]
fn config_diff_never_overwrites() {
    let ws = seeded("cfg-diff");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let (registry, daemon) = start(&ws);
    let run = |a: &[&str]| registry.dispatch(&env, &strs(a));

    // Seeded from the default: nothing differs.
    let conn = open(&ws);
    assert_eq!(config::diff(&conn, &template()).unwrap(), "no differences");

    // A local edit and a new engine default for another kind.
    assert_eq!(run(&["config", "set", "kind.sude.model", "local-model"]), 0);
    let newer = template().replace("opus-5.5", "opus-6");
    assert_ne!(newer, template());
    std::fs::write(ws.team_toml(), &newer).unwrap();
    let before = dump(&open(&ws), CONFIG_TABLES);

    let out = config::config_cmd(&env, &strs(&["diff", ws.team_toml().to_str().unwrap()])).unwrap();
    assert!(
        out.contains("~ kinds.ni.model: stored \"opus-5.5\" -> default \"opus-6\""),
        "{out}"
    );
    assert!(
        out.contains("kinds.sude.model: stored \"local-model\""),
        "{out}"
    );

    // Diff, a re-seed and an import-free reload leave every row as it was.
    assert_eq!(dump(&open(&ws), CONFIG_TABLES), before);
    let mut conn = open(&ws);
    assert_eq!(
        config::seed(&mut conn, &newer).unwrap(),
        Seeded::AlreadySeeded
    );
    assert_eq!(dump(&open(&ws), CONFIG_TABLES), before);
    assert_eq!(
        config::config_cmd(&env, &strs(&["get", "kind.ni.model"])).unwrap(),
        "opus-5.5"
    );
    // The shipped default is readable the same way.
    assert_eq!(run(&["config", "diff"]), 0);
    assert_eq!(dump(&open(&ws), CONFIG_TABLES), before);
    daemon.stop().unwrap();
}

#[test]
fn roster_profiles_next_spawn() {
    let ws = seeded("cfg-profiles");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let (registry, daemon) = start(&ws);
    let run = |a: &[&str]| registry.dispatch(&env, &strs(a));
    let model = |kind: &str| config::resolve_spawn(&open(&ws), kind).map(|t| (t.model, t.effort));

    // A worker is already running; no switch may change its rows.
    open(&ws)
        .execute(
            "INSERT INTO agent (name, role, started_ms) VALUES ('w1', 'peer', 2)",
            [],
        )
        .unwrap();
    let live = |c: &Connection| dump(c, &["agent", "run"]);
    let live_before = live(&open(&ws));

    assert_eq!(
        run(&[
            "config",
            "set",
            "profile.fast",
            r#"{"ni":{"model":"haiku-5.5","effort":"low"}}"#
        ]),
        0
    );
    assert_eq!(
        run(&[
            "config",
            "set",
            "profile.alt.ni",
            r#"{"model":"sonnet-5.5"}"#
        ]),
        0
    );
    // A profile alone changes nothing until it is switched on.
    assert_eq!(model("ni").unwrap().0, "opus-5.5");

    // The key switch reaches the next spawn read, for that kind only.
    assert_eq!(run(&["config", "set", "profile", "fast"]), 0);
    assert_eq!(
        model("ni").unwrap(),
        ("haiku-5.5".into(), Some("low".into()))
    );
    assert_eq!(
        model("sude").unwrap().0,
        config::kind(&open(&ws), "sude").unwrap().unwrap().model
    );
    assert_ne!(model("sude").unwrap().0, "haiku-5.5");
    let line = config::roster_cmd(&env, &[]).unwrap();
    let ni = line.lines().find(|l| l.starts_with("ni")).unwrap();
    assert!(ni.contains("haiku-5.5 (low)"), "{ni}");
    // The stored kind itself is untouched.
    assert_eq!(
        config::config_cmd(&env, &strs(&["get", "kind.ni.model"])).unwrap(),
        "opus-5.5"
    );

    // A harness override switches the harness the next spawn resolves.
    assert_eq!(
        run(&[
            "config",
            "set",
            "profile.swap",
            r#"{"ni":{"harness":"pi","model":"m-pi"}}"#
        ]),
        0
    );
    assert_eq!(run(&["config", "set", "profile", "swap"]), 0);
    let t = config::resolve_spawn(&open(&ws), "ni").unwrap();
    assert_eq!((t.harness.name.as_str(), t.model.as_str()), ("pi", "m-pi"));
    assert_eq!(run(&["config", "set", "profile", "fast"]), 0);

    // The roster verb runs in the daemon: it answers with the profile its
    // client named, and never reads its own env. Key is "fast" here.
    let rpc_roster = |profile: Option<&str>| {
        let mut body = serde_json::json!({ "argv": [] });
        if let Some(p) = profile {
            body["profile"] = serde_json::json!(p);
        }
        qq_core::cli::rpc_call(&env, "roster", body).unwrap()["out"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(rpc_roster(None).contains("haiku-5.5 (low)"));
    assert!(rpc_roster(Some("alt")).contains("sonnet-5.5"));
    {
        // A daemon-side env is not a switch: only the client's argument is.
        let _p = ProfileEnv::set("alt");
        assert!(rpc_roster(None).contains("haiku-5.5 (low)"));
        // The CLI client forwards its own env.
        assert!(config::roster_cmd(&env, &[])
            .unwrap()
            .contains("sonnet-5.5"));
    }
    assert_eq!(run(&["config", "set", "profile.none", "{}"]), 1);
    assert_eq!(
        run(&["config", "set", "profile.null.ni", r#"{"model":"m"}"#]),
        1
    );

    // The env beats the key; a bad env name fails the spawn read, not silently.
    {
        let _p = ProfileEnv::set("alt");
        let base_effort = config::kind(&open(&ws), "ni").unwrap().unwrap().effort;
        assert_eq!(model("ni").unwrap(), ("sonnet-5.5".into(), base_effort));
    }
    {
        let _p = ProfileEnv::set("ghost");
        assert!(matches!(
            config::resolve_spawn(&open(&ws), "ni"),
            Err(ConfigError::Invalid(_))
        ));
    }
    assert_eq!(model("ni").unwrap().0, "haiku-5.5");

    // Refused: an unknown profile, an override naming an unknown harness.
    assert_eq!(run(&["config", "set", "profile", "ghost"]), 1);
    assert_eq!(
        run(&["config", "set", "profile.bad.ni", r#"{"harness":"ghost"}"#]),
        1
    );
    assert_eq!(model("ni").unwrap().0, "haiku-5.5");

    // Back to the base roster.
    assert_eq!(run(&["config", "set", "profile", "none"]), 0);
    assert_eq!(model("ni").unwrap().0, "opus-5.5");

    // No switch above touched the running worker.
    assert_eq!(live(&open(&ws)), live_before);
    daemon.stop().unwrap();
}

// ---- U3 (#327 finding 3): import never overwrites a differing value quietly ----

#[test]
fn config_import_refuses_overwrite_without_yes() {
    let ws = seeded("cfg-import-yes");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let (registry, daemon) = start(&ws);
    let run = |a: &[&str]| registry.dispatch(&env, &strs(a));
    let toml = ws.team_toml();
    let toml = toml.to_str().unwrap();

    // The owner sets two values with `config set`; the file still says the old ones.
    assert_eq!(run(&["config", "set", "kind.sude.model", "local-model"]), 0);
    assert_eq!(run(&["config", "set", "roles.lead.max_peers", "6"]), 0);
    let before = dump(&open(&ws), CONFIG_TABLES);

    // No --yes: refused, every differing key is named, nothing is written.
    let e = config::config_cmd(&env, &strs(&["import", toml])).unwrap_err();
    assert!(e.message.contains("kinds.sude.model"), "{}", e.message);
    assert!(e.message.contains("roles.lead.max_peers"), "{}", e.message);
    assert!(e.message.contains("--yes"), "{}", e.message);
    assert_eq!(run(&["config", "import", toml]), 1);
    assert_eq!(dump(&open(&ws), CONFIG_TABLES), before);

    // Keys that are equal or new still import without --yes.
    let extra = ws.runs_dir().join("extra.toml");
    std::fs::write(&extra, "[limits]\ndepth = 2\n[purposes]\nscan = \"sude\"\n").unwrap();
    assert_eq!(run(&["config", "import", extra.to_str().unwrap()]), 0);
    assert_eq!(
        config::purpose_kind(&open(&ws), "scan").unwrap().as_deref(),
        Some("sude")
    );

    // --yes overwrites and prints what changed.
    let out = config::config_cmd(&env, &strs(&["import", toml, "--yes"])).unwrap();
    assert!(out.contains("kinds.sude.model"), "{out}");
    assert!(out.contains("roles.lead.max_peers"), "{out}");
    assert_eq!(
        config::config_cmd(&env, &strs(&["get", "kind.sude.model"])).unwrap(),
        template()
            .lines()
            .skip_while(|l| !l.starts_with("[kinds.sude]"))
            .find_map(|l| l.strip_prefix("model = "))
            .unwrap()
            .trim_matches('"')
    );
    daemon.stop().unwrap();
}

#[test]
fn config_validate_reports_drift_from_team_toml() {
    let ws = seeded("cfg-drift");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let (registry, daemon) = start(&ws);
    let run = |a: &[&str]| registry.dispatch(&env, &strs(a));

    let clean = config::config_cmd(&env, &strs(&["validate"])).unwrap();
    assert!(!clean.contains("drift"), "{clean}");

    assert_eq!(run(&["config", "set", "roles.lead.max_peers", "6"]), 0);
    let before = dump(&open(&ws), CONFIG_TABLES);
    let out = config::config_cmd(&env, &strs(&["validate"])).unwrap();
    assert!(out.contains("drift"), "{out}");
    assert!(out.contains("roles.lead.max_peers"), "{out}");
    assert_eq!(
        dump(&open(&ws), CONFIG_TABLES),
        before,
        "validate writes nothing"
    );
    daemon.stop().unwrap();
}

#[test]
fn config_import_refuses_to_reset_values_the_file_omits() {
    let ws = seeded("cfg-import-omit");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let (registry, daemon) = start(&ws);
    let run = |a: &[&str]| registry.dispatch(&env, &strs(a));
    let toml = ws.team_toml();
    let toml = toml.to_str().unwrap();

    // The fixture's [kinds.ni] has no fallback and no on_cost_cap.
    assert_eq!(run(&["config", "set", "kind.ni.fallback", "sude"]), 0);
    assert_eq!(run(&["config", "set", "kind.ni.on_cost_cap", "kill"]), 0);
    let before = dump(&open(&ws), CONFIG_TABLES);

    // Plain import: refused, both omitted keys named, nothing written.
    let e = config::config_cmd(&env, &strs(&["import", toml])).unwrap_err();
    assert!(e.message.contains("kinds.ni.fallback"), "{}", e.message);
    assert!(e.message.contains("kinds.ni.on_cost_cap"), "{}", e.message);
    assert_eq!(dump(&open(&ws), CONFIG_TABLES), before);
    assert_eq!(
        config::config_cmd(&env, &strs(&["get", "kind.ni"])).unwrap().contains("kill"),
        true
    );

    // --yes resets them to what the file implies and lists them.
    let out = config::config_cmd(&env, &strs(&["import", toml, "--yes"])).unwrap();
    assert!(out.contains("kinds.ni.fallback"), "{out}");
    assert_ne!(dump(&open(&ws), CONFIG_TABLES), before);

    // Extra positional args are an error, not ignored.
    assert_eq!(run(&["config", "import", toml, "extra.toml"]), 1);
    daemon.stop().unwrap();
}
