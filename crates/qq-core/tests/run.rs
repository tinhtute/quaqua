//! R19: a run is recorded whole on every driver and printed by `run show` /
//! `run list`; cost is `unknown` unless the harness reported it for every message.

use std::path::Path;

use qq_core::cli::Env;
use qq_core::config;
use qq_core::db;
use qq_core::driver::Registry;
use qq_core::run::{self, capture, show};
use qq_core::testkit::TestWorkspace;
use rusqlite::Connection;

const SHA: &str = "0123abc4567def890123abc4567def890123abcd";

fn seeded(ws: &TestWorkspace) -> Connection {
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    let team = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
    )
    .unwrap();
    config::seed(&mut conn, &team).expect("seed");
    conn.execute("INSERT INTO project(slug, created_ms) VALUES ('p', 1)", []).unwrap();
    conn.execute("INSERT INTO project(slug, created_ms) VALUES ('other', 1)", []).unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS events (workflow_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         ts_ms INTEGER NOT NULL, kind TEXT NOT NULL, payload_json TEXT NOT NULL, \
         PRIMARY KEY (workflow_id, seq))",
        [],
    )
    .unwrap();
    conn
}

/// Opens the run of one worker on `harness` in project `project_id`.
fn open_run(conn: &mut Connection, name: &str, harness: &str, project_id: i64) -> String {
    conn.execute(
        "INSERT INTO agent (name, role, kind, project_id, started_ms) VALUES (?1, 'peer', 'ni', ?2, 1)",
        rusqlite::params![name, project_id],
    )
    .unwrap();
    let agent_id = conn.last_insert_rowid();
    let wf = format!("worker:{name}");
    conn.execute(
        "INSERT INTO workflow (id, kind, state, started_ms, updated_ms) VALUES (?1, 'worker', 'running', 1, 1)",
        [&wf],
    )
    .unwrap();
    run::open(
        conn,
        &run::NewRun {
            workflow_id: &wf,
            agent_id,
            task_id: None,
            kind: "ni",
            harness,
            model: "m-1",
            effort: Some("low"),
            base: Some("main"),
            now_ms: 1_000,
        },
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (workflow_id, seq, ts_ms, kind, payload_json) VALUES (?1, 1, 1000, 'workflow_started', '{}')",
        [&wf],
    )
    .unwrap();
    wf
}

const CLAUDE_SESSION: &str = r#"{"type":"user","timestamp":"2026-11-21T10:00:00.000Z","message":{"role":"user","content":"fix the bug"}}
{"type":"assistant","timestamp":"2026-11-21T10:00:01.000Z","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"looking"}],"usage":{"input_tokens":10,"output_tokens":5}}}
{"type":"assistant","timestamp":"2026-11-21T10:00:02.000Z","message":{"id":"m1","role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}],"usage":{"input_tokens":10,"output_tokens":5}}}
{"type":"user","timestamp":"2026-11-21T10:00:03.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a.rs","is_error":false}]}}
{"type":"assistant","timestamp":"2026-11-21T10:00:04.000Z","message":{"id":"m2","role":"assistant","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":20,"output_tokens":7}}}
{"type":"assistant","half written
"#;

const PI_SESSION: &str = r#"{"type":"message","timestamp":"2026-11-21T10:00:00.000Z","message":{"role":"user","content":[{"type":"text","text":"fix the bug"}]}}
{"type":"message","timestamp":"2026-11-21T10:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"on it"},{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"ls"}}],"usage":{"input":100,"output":9,"cacheRead":1,"cacheWrite":2,"cost":{"total":0.25}}}}
{"type":"message","timestamp":"2026-11-21T10:00:02.000Z","message":{"role":"toolResult","toolCallId":"c1","content":[{"type":"text","text":"a.rs"}],"isError":true}}
{"type":"message","timestamp":"2026-11-21T10:00:03.000Z","message":{"role":"assistant","content":[{"type":"text","text":"finished"}],"usage":{"input":50,"output":4,"cost":{"total":0.5}}}}
"#;

fn driver(conn: &Connection, harness: &str) -> std::sync::Arc<dyn qq_core::driver::HarnessDriver> {
    let h = config::harness(conn, harness).unwrap().expect("harness row");
    Registry::native().resolve(&h).expect("driver")
}

#[test]
fn run_record_complete_all_drivers() {
    let ws = TestWorkspace::new("run-complete");
    let mut conn = seeded(&ws);
    let claude_file = ws.root().join("claude.jsonl");
    let pi_file = ws.root().join("pi.jsonl");
    std::fs::write(&claude_file, CLAUDE_SESSION).unwrap();
    std::fs::write(&pi_file, PI_SESSION).unwrap();

    for (name, harness, session, project) in [
        ("c1", "claude", Some(claude_file.to_str().unwrap()), 1),
        ("p1", "pi", Some(pi_file.to_str().unwrap()), 1),
        ("g1", "opencode", None, 1),
        ("o1", "claude", None, 2),
    ] {
        let wf = open_run(&mut conn, name, harness, project);
        let d = driver(&conn, harness);
        let got = capture::capture(&mut conn, &wf, &*d, session, None).unwrap();
        // A repeat replaces and converges.
        let again = capture::capture(&mut conn, &wf, &*d, session, None).unwrap();
        assert_eq!(got, again);
        let id: i64 = conn
            .query_row("SELECT id FROM run WHERE workflow_id = ?1", [&wf], |r| r.get(0))
            .unwrap();
        capture::add_artifact(&conn, id, "report", Some("runs/x.md"), None).unwrap();
        capture::add_artifact(&conn, id, "report", Some("runs/x.md"), None).unwrap();
        assert!(run::close(&conn, &wf, "verified", Some(SHA), 9_000).unwrap());
        assert_eq!(got.is_some(), session.is_some(), "{harness}");
    }

    // claude: transcript, the tool call paired with its result, tokens counted once per message.
    let full = show::render(&show::load(&conn, "worker:c1").unwrap().unwrap());
    for want in [
        "run worker:c1",
        "harness claude",
        "outcome verified",
        &format!("candidate_sha {SHA}"),
        "tokens in=30 out=12",
        "cost unknown",
        "base main",
        "report runs/x.md",
        "events (1)",
        "workflow_started",
        "transcript (3)",
        "user: fix the bug",
        "assistant: done",
        "tool calls (1)",
        "Bash ok args={\"command\":\"ls\"} result=\"a.rs\"",
    ] {
        assert!(full.contains(want), "claude run lacks {want:?}:\n{full}");
    }
    assert_eq!(full.matches("report runs/x.md").count(), 1);

    // pi: usage and cost reported, a failed tool call shows as error.
    let full = show::render(&show::load(&conn, "worker:p1").unwrap().unwrap());
    for want in [
        "harness pi",
        "tokens in=150 out=13",
        "cost $0.7500",
        "transcript (3)",
        "assistant: finished",
        "bash error args={\"command\":\"ls\"}",
    ] {
        assert!(full.contains(want), "pi run lacks {want:?}:\n{full}");
    }

    // generic: no session source, still whole where it can be.
    let full = show::render(&show::load(&conn, "worker:g1").unwrap().unwrap());
    for want in [
        "harness opencode",
        "tokens in=unknown out=unknown",
        "cost unknown",
        "events (1)",
        "transcript (0)",
        "tool calls (0)",
        &format!("candidate_sha {SHA}"),
    ] {
        assert!(full.contains(want), "generic run lacks {want:?}:\n{full}");
    }

    // run list --project, through the command.
    let env = Env { runs_dir: ws.runs_dir() };
    let listed = show::run_command(&env, &["list".into(), "--project".into(), "p".into()]).unwrap();
    assert_eq!(listed.lines().count(), 3, "{listed}");
    assert!(listed.contains("worker:c1") && !listed.contains("worker:o1"), "{listed}");
    let all = show::run_command(&env, &["list".into()]).unwrap();
    assert_eq!(all.lines().count(), 4);
    assert!(show::run_command(&env, &["list".into(), "--project".into(), "nope".into()]).is_err());
    assert!(show::run_command(&env, &["show".into(), "worker:zz".into()]).is_err());
    let by_id = show::run_command(&env, &["show".into(), "1".into()]).unwrap();
    assert!(by_id.starts_with("run worker:c1"), "{by_id}");
}

#[test]
fn cost_unknown_when_unreported() {
    let ws = TestWorkspace::new("run-cost");
    let mut conn = seeded(&ws);
    let state = |conn: &Connection, wf: &str| -> (String, Option<f64>, Option<i64>) {
        conn.query_row(
            "SELECT cost_state, cost_usd, tokens_in FROM run WHERE workflow_id = ?1",
            [wf],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };

    // A fresh run has no cost: unknown, never 0.
    let wf = open_run(&mut conn, "n1", "claude", 1);
    assert_eq!(state(&conn, &wf), ("unknown".into(), None, None));

    // claude reports tokens but no cost.
    let f = ws.root().join("c.jsonl");
    std::fs::write(&f, CLAUDE_SESSION).unwrap();
    let d = driver(&conn, "claude");
    capture::capture(&mut conn, &wf, &*d, f.to_str(), None).unwrap();
    assert_eq!(state(&conn, &wf), ("unknown".into(), None, Some(30)));

    // pi with one assistant message missing its cost: the total is unknown, not partial.
    let wf = open_run(&mut conn, "n2", "pi", 1);
    let partial = PI_SESSION.replace(r#","cost":{"total":0.5}"#, "");
    let f = ws.root().join("p.jsonl");
    std::fs::write(&f, partial).unwrap();
    let d = driver(&conn, "pi");
    capture::capture(&mut conn, &wf, &*d, f.to_str(), None).unwrap();
    assert_eq!(state(&conn, &wf), ("unknown".into(), None, Some(150)));
    let shown = show::render(&show::load(&conn, &wf).unwrap().unwrap());
    assert!(shown.contains("cost unknown") && !shown.contains("cost $0"), "{shown}");

    // pi with every cost reported.
    let wf = open_run(&mut conn, "n3", "pi", 1);
    std::fs::write(&f, PI_SESSION).unwrap();
    let d = driver(&conn, "pi");
    capture::capture(&mut conn, &wf, &*d, f.to_str(), None).unwrap();
    assert_eq!(state(&conn, &wf), ("reported".into(), Some(0.75), Some(150)));

    // A generic driver never reads a session, even when one is named.
    let wf = open_run(&mut conn, "n4", "opencode", 1);
    let d = driver(&conn, "opencode");
    let got = capture::capture(&mut conn, &wf, &*d, f.to_str(), None).unwrap();
    assert!(got.is_none());
    assert_eq!(state(&conn, &wf), ("unknown".into(), None, None));
}
